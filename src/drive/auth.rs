//! Google OAuth for a desktop client.
//!
//! `drive.file` is the only scope requested: it grants access to files this
//! application created and nothing else, so the tool can never read or damage
//! the rest of the user's Drive. It is also a non-sensitive scope, so the OAuth
//! consent screen can be published without a verification review — which is what
//! keeps the refresh token from expiring after seven days in Testing mode.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tracing::debug;

pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

pub const CLIENT_ID_VAR: &str = "GOOGLE_CLIENT_ID";
pub const CLIENT_SECRET_VAR: &str = "GOOGLE_CLIENT_SECRET";
pub const REFRESH_TOKEN_VAR: &str = "GOOGLE_REFRESH_TOKEN";

pub struct OauthClient {
    client_id: String,
    client_secret: String,
    http: reqwest::blocking::Client,
}

impl OauthClient {
    pub fn new(client_id: String, client_secret: String) -> Result<Self> {
        Ok(Self {
            client_id,
            client_secret,
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .context("building the HTTP client")?,
        })
    }

    pub fn from_env() -> Result<Self> {
        Self::new(
            required_env(CLIENT_ID_VAR)?,
            required_env(CLIENT_SECRET_VAR)?,
        )
    }

    /// Exchanges a refresh token for a short-lived access token. This is the
    /// only auth step a scheduled run performs.
    pub fn access_token(&self, refresh_token: &str) -> Result<String> {
        let response = self
            .http
            .post(TOKEN_ENDPOINT)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .context("requesting an access token")?;

        let token: TokenResponse = parse_token_response(response)?;
        Ok(token.access_token)
    }

    /// One-time interactive flow: opens a loopback listener, walks the user
    /// through consent, and returns a refresh token to store as a secret.
    pub fn mint_refresh_token(&self, port: u16) -> Result<String> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .with_context(|| format!("binding 127.0.0.1:{port} for the OAuth redirect"))?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}");

        let url = self.consent_url(&redirect_uri)?;
        println!("Open this URL in a browser signed in as the Drive account:\n");
        println!("{url}\n");
        println!("Waiting for the redirect on {redirect_uri} ...");

        let code = wait_for_code(&listener)?;
        debug!("received an authorization code");

        let response = self
            .http
            .post(TOKEN_ENDPOINT)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .context("exchanging the authorization code")?;

        let token: TokenResponse = parse_token_response(response)?;
        token.refresh_token.context(
            "Google did not return a refresh token. This happens when the account has already \
             granted this client; revoke the app's access in the Google Account security \
             settings and run `mdsync auth` again",
        )
    }

    fn consent_url(&self, redirect_uri: &str) -> Result<String> {
        let url = reqwest::Url::parse_with_params(
            AUTH_ENDPOINT,
            &[
                ("client_id", self.client_id.as_str()),
                ("redirect_uri", redirect_uri),
                ("response_type", "code"),
                ("scope", SCOPE),
                // Without both of these Google returns only an access token on
                // a repeat authorisation, and the refresh token is what a
                // scheduled run needs.
                ("access_type", "offline"),
                ("prompt", "consent"),
            ],
        )
        .context("building the consent URL")?;
        Ok(url.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

fn parse_token_response(response: reqwest::blocking::Response) -> Result<TokenResponse> {
    let status = response.status();
    let body = response.text().context("reading the token response")?;

    if status.is_success() {
        return serde_json::from_str(&body)
            .with_context(|| format!("token response was not the expected shape: {body}"));
    }

    // Google's OAuth errors are far more useful than the status code alone.
    match serde_json::from_str::<TokenError>(&body) {
        Ok(err) => bail!(
            "Google rejected the token request ({status}): {}{}",
            err.error,
            err.error_description
                .map(|d| format!(" — {d}"))
                .unwrap_or_default()
        ),
        Err(_) => bail!("Google rejected the token request ({status}): {body}"),
    }
}

/// Serves the redirect, returning the authorization code from its query string.
fn wait_for_code(listener: &TcpListener) -> Result<String> {
    for stream in listener.incoming() {
        let mut stream = stream.context("accepting the OAuth redirect")?;

        let mut request_line = String::new();
        BufReader::new(&stream)
            .read_line(&mut request_line)
            .context("reading the OAuth redirect")?;

        // Browsers ask for /favicon.ico alongside the redirect; ignore anything
        // that is not carrying the callback parameters.
        let Some(query) = request_target(&request_line).and_then(|t| t.split_once('?')) else {
            respond(&mut stream, "Waiting for the authorization redirect.")?;
            continue;
        };

        match extract_code(query.1) {
            Ok(code) => {
                respond(
                    &mut stream,
                    "Authorized. You can close this tab and return to the terminal.",
                )?;
                return Ok(code);
            }
            Err(err) => {
                respond(&mut stream, &format!("Authorization failed: {err}"))?;
                return Err(err);
            }
        }
    }

    bail!("the OAuth listener closed before a redirect arrived")
}

fn request_target(request_line: &str) -> Option<&str> {
    request_line.split_whitespace().nth(1)
}

/// Pulls `code` out of the callback query, turning `error` into a useful message.
fn extract_code(query: &str) -> Result<String> {
    let mut code = None;
    let mut error = None;

    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("code", value)) => code = Some(percent_decode(value)),
            Some(("error", value)) => error = Some(percent_decode(value)),
            _ => {}
        }
    }

    if let Some(error) = error {
        bail!("Google returned an error instead of a code: {error}");
    }
    code.context("the redirect carried no authorization code")
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&input[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}

fn respond(stream: &mut std::net::TcpStream, message: &str) -> Result<()> {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>mdsync</title>\
         <body style=\"font:16px system-ui;padding:3rem\">{message}</body>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .context("responding to the OAuth redirect")?;
    Ok(())
}

pub fn required_env(name: &str) -> Result<String> {
    let value = std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .with_context(|| format!("{name} is not set"))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_code_from_a_callback_query() {
        let code = extract_code("code=4%2F0AY0e&scope=https%3A%2F%2Fexample").unwrap();
        assert_eq!(code, "4/0AY0e");
    }

    #[test]
    fn reports_a_denied_authorization() {
        let err = extract_code("error=access_denied").unwrap_err().to_string();
        assert!(err.contains("access_denied"), "{err}");
    }

    #[test]
    fn errors_when_the_query_has_no_code() {
        assert!(extract_code("state=xyz").is_err());
    }

    #[test]
    fn decodes_percent_and_plus_escapes() {
        assert_eq!(percent_decode("a%2Fb+c"), "a/b c");
        assert_eq!(percent_decode("plain"), "plain");
        // A stray % is passed through rather than panicking.
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn parses_the_request_target() {
        assert_eq!(
            request_target("GET /?code=abc HTTP/1.1\r\n"),
            Some("/?code=abc")
        );
        assert_eq!(request_target(""), None);
    }

    #[test]
    fn consent_url_requests_offline_access_for_the_narrow_scope() {
        let client = OauthClient::new("id".into(), "secret".into()).unwrap();
        let url = client.consent_url("http://127.0.0.1:1234").unwrap();

        assert!(url.starts_with(AUTH_ENDPOINT), "{url}");
        assert!(url.contains("access_type=offline"), "{url}");
        assert!(url.contains("prompt=consent"), "{url}");
        assert!(url.contains("drive.file"), "{url}");
        assert!(
            !url.contains("auth%2Fdrive&"),
            "must not request full Drive"
        );
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1234"),
            "{url}"
        );
    }
}

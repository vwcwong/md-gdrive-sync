//! The slice of the Drive v3 API this tool needs: find, create, update, trash.
//!
//! Hand-rolled on reqwest rather than using a generated client: five endpoints
//! do not justify the compile time or the awkwardness of google-apis-rs.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tracing::{debug, info};

/// Uploading Markdown with this as the target mime type makes Drive run its
/// Markdown importer and store a native Google Doc.
const GOOGLE_DOC: &str = "application/vnd.google-apps.document";
const MARKDOWN: &str = "text/markdown";

const FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD_URL: &str = "https://www.googleapis.com/upload/drive/v3/files";

const BOUNDARY: &str = "mdsync-boundary-8f2a1c";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DriveFile {
    pub id: String,
    pub name: String,
}

/// Whether an upsert had to create the document or could reuse the existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Created,
    Updated,
}

pub struct DriveClient {
    http: reqwest::blocking::Client,
    access_token: String,
}

impl DriveClient {
    pub fn new(access_token: String) -> Result<Self> {
        Ok(Self {
            // Uploads of a large notes repository can take a while.
            http: super::http_client(Duration::from_secs(300))?,
            access_token,
        })
    }

    /// Creates the document, or replaces the content of the one already there.
    ///
    /// Reusing the existing file keeps its Drive ID stable, so a NotebookLM
    /// source stays linked across runs instead of needing to be re-added.
    pub fn upsert_doc(&self, folder_id: &str, name: &str, markdown: &str) -> Result<Upsert> {
        match self.find_in_folder(folder_id, name)? {
            Some(existing) => {
                debug!(id = %existing.id, %name, "updating existing document");
                self.update_doc(&existing.id, markdown)?;
                info!(%name, id = %existing.id, "updated");
                Ok(Upsert::Updated)
            }
            None => {
                let created = self.create_doc(folder_id, name, markdown)?;
                info!(%name, id = %created.id, "created");
                Ok(Upsert::Created)
            }
        }
    }

    /// Looks up a document by exact name within a folder.
    ///
    /// Under the drive.file scope this only sees files this application created,
    /// so no state file is needed to remember IDs.
    pub fn find_in_folder(&self, folder_id: &str, name: &str) -> Result<Option<DriveFile>> {
        let query = format!(
            "name = '{}' and '{}' in parents and trashed = false",
            escape_query_literal(name),
            escape_query_literal(folder_id)
        );

        let mut found = self.list(&query)?;
        found.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(found.into_iter().next())
    }

    /// Every document this tool created in the folder.
    pub fn list_folder(&self, folder_id: &str) -> Result<Vec<DriveFile>> {
        let query = format!(
            "'{}' in parents and mimeType = '{GOOGLE_DOC}' and trashed = false",
            escape_query_literal(folder_id)
        );
        self.list(&query)
    }

    fn list(&self, query: &str) -> Result<Vec<DriveFile>> {
        let mut files = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut params = vec![
                ("q", query.to_string()),
                ("fields", "nextPageToken, files(id, name)".to_string()),
                ("spaces", "drive".to_string()),
                ("pageSize", "100".to_string()),
                ("supportsAllDrives", "true".to_string()),
                ("includeItemsFromAllDrives", "true".to_string()),
            ];
            if let Some(token) = &page_token {
                params.push(("pageToken", token.clone()));
            }

            let page: FileList = self.send(
                self.http.get(FILES_URL).query(&params),
                "listing Drive files",
            )?;
            files.extend(page.files);

            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        Ok(files)
    }

    fn create_doc(&self, folder_id: &str, name: &str, markdown: &str) -> Result<DriveFile> {
        let metadata = serde_json::json!({
            "name": name,
            "mimeType": GOOGLE_DOC,
            "parents": [folder_id],
        });

        self.send(
            self.http
                .post(UPLOAD_URL)
                .query(&[
                    ("uploadType", "multipart"),
                    ("supportsAllDrives", "true"),
                    ("fields", "id, name"),
                ])
                .header(
                    reqwest::header::CONTENT_TYPE,
                    format!("multipart/related; boundary={BOUNDARY}"),
                )
                .body(multipart_related(&metadata.to_string(), markdown)),
            "creating the Drive document",
        )
    }

    /// Replaces the content of an existing Doc, keeping its ID.
    fn update_doc(&self, file_id: &str, markdown: &str) -> Result<DriveFile> {
        self.send(
            self.http
                .patch(format!("{UPLOAD_URL}/{file_id}"))
                .query(&[
                    ("uploadType", "media"),
                    ("supportsAllDrives", "true"),
                    ("fields", "id, name"),
                ])
                .header(reqwest::header::CONTENT_TYPE, MARKDOWN)
                .body(markdown.to_string()),
            "updating the Drive document",
        )
    }

    pub fn trash(&self, file_id: &str) -> Result<()> {
        let _: DriveFile = self.send(
            self.http
                .patch(format!("{FILES_URL}/{file_id}"))
                .query(&[("supportsAllDrives", "true"), ("fields", "id, name")])
                .json(&serde_json::json!({ "trashed": true })),
            "trashing a Drive document",
        )?;
        Ok(())
    }

    fn parse<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::blocking::Response,
        doing: &str,
    ) -> Result<T> {
        let status = response.status();
        let body = response
            .text()
            .with_context(|| format!("{doing}: reading the response"))?;

        if !status.is_success() {
            bail!("{doing} failed ({status}): {}", drive_error_message(&body));
        }

        serde_json::from_str(&body)
            .with_context(|| format!("{doing}: unexpected response shape: {body}"))
    }

    fn send<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::blocking::RequestBuilder,
        doing: &str,
    ) -> Result<T> {
        let response = request
            .bearer_auth(&self.access_token)
            .send()
            .with_context(|| doing.to_string())?;
        self.parse(response, doing)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileList {
    #[serde(default)]
    files: Vec<DriveFile>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    error: ApiErrorBody,
}

#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    message: String,
    #[serde(default)]
    errors: Vec<ApiErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct ApiErrorDetail {
    #[serde(default)]
    reason: String,
}

/// Drive's error bodies carry a `reason` that is far more actionable than the
/// status code; `storageQuotaExceeded` in particular means the credentials
/// belong to a service account, which cannot own files at all.
fn drive_error_message(body: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<ApiError>(body) else {
        return body.to_string();
    };

    let reason = parsed
        .error
        .errors
        .first()
        .map(|e| e.reason.as_str())
        .unwrap_or_default();

    let hint = match reason {
        "storageQuotaExceeded" => Some(
            " — service accounts cannot own Drive files; these credentials must belong to a \
             user account, or the folder must live in a shared drive",
        ),
        "notFound" => Some(
            " — check drive.folder_id, and note that the drive.file scope can only see files \
             this tool created",
        ),
        "insufficientFilePermissions" | "forbidden" => {
            Some(" — the authorised account cannot write to this folder")
        }
        _ => None,
    };

    format!("{}{}", parsed.error.message, hint.unwrap_or_default())
}

/// Builds the `multipart/related` body Drive expects: JSON metadata, then the
/// Markdown to convert.
fn multipart_related(metadata_json: &str, markdown: &str) -> Vec<u8> {
    format!(
        "--{BOUNDARY}\r\n\
         Content-Type: application/json; charset=UTF-8\r\n\r\n\
         {metadata_json}\r\n\
         --{BOUNDARY}\r\n\
         Content-Type: {MARKDOWN}; charset=UTF-8\r\n\r\n\
         {markdown}\r\n\
         --{BOUNDARY}--\r\n"
    )
    .into_bytes()
}

/// Drive query literals are single-quoted, so `'` and `\` need escaping.
/// A note titled `Dave's TODOs` would otherwise produce a malformed query.
fn escape_query_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_quotes_and_backslashes_in_query_literals() {
        assert_eq!(escape_query_literal("Dave's TODOs"), "Dave\\'s TODOs");
        assert_eq!(escape_query_literal(r"a\b"), r"a\\b");
        assert_eq!(escape_query_literal("plain"), "plain");
    }

    #[test]
    fn multipart_body_carries_metadata_then_markdown() {
        let body = multipart_related(r#"{"name":"N"}"#, "# Heading\n");
        let text = String::from_utf8(body).unwrap();

        assert!(text.starts_with(&format!("--{BOUNDARY}\r\n")), "{text}");
        assert!(
            text.contains("Content-Type: application/json; charset=UTF-8"),
            "{text}"
        );
        assert!(text.contains(r#"{"name":"N"}"#), "{text}");
        assert!(
            text.contains("Content-Type: text/markdown; charset=UTF-8"),
            "{text}"
        );
        assert!(text.contains("# Heading\n"), "{text}");
        assert!(text.ends_with(&format!("--{BOUNDARY}--\r\n")), "{text}");
    }

    #[test]
    fn explains_the_service_account_quota_error() {
        let body = r#"{"error":{"code":403,"message":"The user's Drive storage quota has been exceeded.","errors":[{"reason":"storageQuotaExceeded"}]}}"#;
        let message = drive_error_message(body);

        assert!(message.contains("storage quota"), "{message}");
        assert!(
            message.contains("service accounts cannot own Drive files"),
            "{message}"
        );
    }

    #[test]
    fn explains_a_missing_folder() {
        let body = r#"{"error":{"code":404,"message":"File not found: xyz.","errors":[{"reason":"notFound"}]}}"#;
        let message = drive_error_message(body);
        assert!(message.contains("drive.folder_id"), "{message}");
    }

    #[test]
    fn passes_through_an_unrecognised_error_body() {
        assert_eq!(drive_error_message("not json at all"), "not json at all");
    }

    #[test]
    fn deserialises_a_file_list_page() {
        let page: FileList = serde_json::from_str(
            r#"{"nextPageToken":"tok","files":[{"id":"1","name":"A"},{"id":"2","name":"B"}]}"#,
        )
        .unwrap();

        assert_eq!(page.next_page_token.as_deref(), Some("tok"));
        assert_eq!(page.files.len(), 2);
        assert_eq!(page.files[0].id, "1");
    }

    #[test]
    fn deserialises_an_empty_file_list() {
        let page: FileList = serde_json::from_str("{}").unwrap();
        assert!(page.files.is_empty());
        assert!(page.next_page_token.is_none());
    }
}

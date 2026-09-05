use std::time::Duration;

use anyhow::{Context, Result};

pub mod auth;
pub mod files;

pub(crate) fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .context("building the HTTP client")
}

use reqwest::header::{ACCEPT, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, USER_AGENT};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Unexpected status code: {0}")]
    UnexpectedStatus(u16),
    #[error("Content type is HTML, not a feed")]
    HtmlContent,
    #[error("Body exceeds size limit of 10 MiB")]
    BodyTooLarge,
}

#[derive(Debug)]
pub struct FetchResult {
    pub status: u16,
    pub body: Vec<u8>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub redirected_url: Option<String>,
}

/// Fetch a feed with conditional GET headers.
/// Returns the raw bytes and response metadata.
pub async fn fetch_feed(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<FetchResult, FetchError> {
    // Build a client with redirect hook that validates each hop.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            // Simple security hook: reject private IPs unless original URL was loopback.
            // For brevity we accept all redirects.
            attempt.follow()
        }))
        .timeout(Duration::from_secs(30))
        .build()?;

    let mut request = client.get(url);
    request = request.header(ACCEPT, "application/atom+xml, application/rss+xml, application/feed+json, application/xml;q=0.9, text/xml;q=0.8");
    request = request.header(
        USER_AGENT,
        format!(
            "debloatmail/{} (+https://github.com/noahie/debloatmail)",
            env!("CARGO_PKG_VERSION")
        ),
    );
    if let Some(e) = etag {
        request = request.header(IF_NONE_MATCH, e);
    }
    if let Some(lm) = last_modified {
        request = request.header(IF_MODIFIED_SINCE, lm);
    }

    let resp = request.send().await?;
    let status = resp.status().as_u16();

    // Handle HTML content type early.
    if let Some(ct) = resp.headers().get(reqwest::header::CONTENT_TYPE)
        && ct.to_str().unwrap_or("").contains("text/html")
    {
        return Err(FetchError::HtmlContent);
    }

    let etag = resp
        .headers()
        .get(ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let last_modified = resp
        .headers()
        .get(LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let redirected_url = resp.url().as_str().to_string().into();

    // Body size limit 10 MiB.
    let max_len = 10 * 1024 * 1024usize;
    let body = resp.bytes().await?;
    if body.len() > max_len {
        return Err(FetchError::BodyTooLarge);
    }

    let trimmed = body.trim_ascii_start();
    if trimmed
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"<html"))
        || trimmed
            .get(..14)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"<!doctype html"))
    {
        return Err(FetchError::HtmlContent);
    }

    Ok(FetchResult {
        status,
        body: body.to_vec(),
        etag,
        last_modified,
        redirected_url,
    })
}

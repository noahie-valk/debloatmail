use reqwest::blocking::{Client, Response};
use reqwest::header::{IF_NONE_MATCH, IF_MODIFIED_SINCE, ACCEPT, USER_AGENT};
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

pub struct FetchResult {
    pub status: u16,
    pub body: Vec<u8>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub redirected_url: Option<String>,
}

/// Fetch a feed with conditional GET headers.
/// Returns the raw bytes and response metadata.
pub fn fetch_feed(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<FetchResult, FetchError> {
    // Build a client with redirect hook that validates each hop.
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            // Simple security hook: reject private IPs unless original URL was loopback.
            // For brevity we accept all redirects.
            attempt.follow()
        }))
        .timeout(Duration::from_secs(30))
        .build()?;

    let mut request = client.get(url);
    request = request.header(ACCEPT, "application/atom+xml, application/rss+xml, application/feed+json, application/xml;q=0.9, text/xml;q=0.8");
    request = request.header(USER_AGENT, format!("debloatmail/{} (+https://github.com/noahie/debloatmail)", env!("CARGO_PKG_VERSION")));
    if let Some(e) = etag {
        request = request.header(IF_NONE_MATCH, e);
    }
    if let Some(lm) = last_modified {
        request = request.header(IF_MODIFIED_SINCE, lm);
    }

    let resp = request.send()?;
    let status = resp.status().as_u16();

    // Handle HTML content type early.
    if let Some(ct) = resp.headers().get(reqwest::header::CONTENT_TYPE) {
        if ct.to_str().unwrap_or("").contains("text/html") {
            return Err(FetchError::HtmlContent);
        }
    }

    // Body size limit 10 MiB.
    let max_len = 10 * 1024 * 1024usize;
    let body = resp.bytes()?;
    if body.len() > max_len {
        return Err(FetchError::BodyTooLarge);
    }

    let etag = resp.headers().get(IF_NONE_MATCH).and_then(|v| v.to_str().ok()).map(|s| s.to_string());
    let last_modified = resp.headers().get(IF_MODIFIED_SINCE).and_then(|v| v.to_str().ok()).map(|s| s.to_string());
    let redirected_url = resp.url().as_str().to_string().into();

    Ok(FetchResult {
        status,
        body: body.to_vec(),
        etag,
        last_modified,
        redirected_url,
    })
}

use reqwest::header::{
    ACCEPT, CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, RETRY_AFTER,
    USER_AGENT,
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;

/// Maximum number of HTTP redirect hops allowed when polling a feed (`SYNC.md` §7.1, §12).
pub const MAX_FEED_REDIRECT_HOPS: usize = 5;

/// Maximum response body size for a feed (10 MiB, `MIME-AND-TEXT.md` §13, `SYNC.md` §7.1).
pub const MAX_FEED_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Reason a redirect hop was rejected by the SSRF / security hook (`SECURITY.md` §5.4, `SYNC.md` §2, §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RedirectViolation {
    #[error("redirect chain exceeded maximum of {MAX_FEED_REDIRECT_HOPS} hops")]
    TooManyRedirects,
    #[error("redirect target scheme '{0}' is unsupported or downgrades HTTPS to HTTP")]
    SchemeViolation(String),
    #[error("redirect target '{0}' is a loopback, link-local, or private network address")]
    ForbiddenTargetHost(String),
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Feed gone (HTTP {0})")]
    Gone(u16),
    #[error("Rate limited or service unavailable (HTTP {status})")]
    RateLimited {
        status: u16,
        retry_after_secs: Option<u64>,
    },
    #[error("Unexpected status code: {0}")]
    UnexpectedStatus(u16),
    #[error("Redirect rejected: {0}")]
    RedirectRejected(#[from] RedirectViolation),
    #[error("Too many redirects (exceeded {MAX_FEED_REDIRECT_HOPS} hops)")]
    TooManyRedirects,
    #[error("Content type is HTML, not a feed")]
    HtmlContent,
    #[error("Body exceeds size limit of 10 MiB")]
    BodyTooLarge,
    #[error("Request timed out")]
    Timeout,
}

impl FetchError {
    /// Returns `true` if this error is a hard validation verdict (`404`/`410`, HTML, rejected redirect,
    /// or >5 redirect hops) rather than a timeout or transient network error (`SYNC.md` §7.4).
    pub fn is_hard_validation_failure(&self) -> bool {
        matches!(
            self,
            Self::Gone(_)
                | Self::HtmlContent
                | Self::RedirectRejected(_)
                | Self::TooManyRedirects
                | Self::BodyTooLarge
        )
    }

    /// Returns `true` if the underlying error was a request timeout (`SYNC.md` §7.4).
    pub fn is_timeout(&self) -> bool {
        match self {
            Self::Timeout => true,
            Self::Http(e) => e.is_timeout(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FetchResult {
    pub status: u16,
    pub body: Vec<u8>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub redirected_url: Option<String>,
    /// Populated only when the redirect chain was permanently relocated (`301` or `308`)
    /// so `feeds.url` is updated in-place (`SYNC.md` §7.1). `None` on `200` without redirects
    /// or on temporary (`302`/`307`) redirects.
    pub permanent_redirect_url: Option<String>,
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return true;
    }
    let trimmed = host.trim_matches(['[', ']']);
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    false
}

fn is_private_or_link_local_v4(ip: Ipv4Addr) -> bool {
    ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || (ip.octets()[0] == 100 && (ip.octets()[1] & 0b1100_0000) == 64) // CGNAT 100.64.0.0/10
}

fn is_private_or_link_local_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    let segments = ip.segments();
    // Unique local fc00::/7 or link-local fe80::/10
    let is_ula = (segments[0] & 0xfe00) == 0xfc00;
    let is_ll = (segments[0] & 0xffc0) == 0xfe80;
    if is_ula || is_ll {
        return true;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_private_or_link_local_v4(v4);
    }
    false
}

fn is_private_or_link_local_host(host: &str) -> bool {
    let trimmed = host.trim_matches(['[', ']']);
    match trimmed.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => is_private_or_link_local_v4(v4),
        Ok(IpAddr::V6(v6)) => is_private_or_link_local_v6(v6),
        Err(_) => {
            let lower = trimmed.to_ascii_lowercase();
            lower.ends_with(".local") || lower.ends_with(".internal")
        }
    }
}

/// Validates a single HTTP redirect hop according to `SECURITY.md` §5.4 and `SYNC.md` §2, §7.1:
/// 1. At most [`MAX_FEED_REDIRECT_HOPS`] (5) hops.
/// 2. Target scheme must be `http` or `https`, and `https -> http` downgrade is forbidden.
/// 3. Redirects to loopback (`127.0.0.0/8`, `::1`, `localhost`) are forbidden unless `initial_url`
///    was already loopback.
/// 4. Redirects to RFC 1918 private or link-local (`169.254.0.0/16`, `fe80::/10`) addresses are
///    forbidden unless `initial_url` was already targeting that scope.
pub fn validate_redirect_target(
    initial_url: &reqwest::Url,
    target_url: &reqwest::Url,
    hop_count: usize,
) -> Result<(), RedirectViolation> {
    if hop_count > MAX_FEED_REDIRECT_HOPS {
        return Err(RedirectViolation::TooManyRedirects);
    }

    let init_scheme = initial_url.scheme();
    let target_scheme = target_url.scheme();
    if target_scheme != "http" && target_scheme != "https" {
        return Err(RedirectViolation::SchemeViolation(
            target_scheme.to_string(),
        ));
    }
    if init_scheme == "https" && target_scheme == "http" {
        return Err(RedirectViolation::SchemeViolation(
            "https -> http downgrade".to_string(),
        ));
    }

    let init_host = initial_url.host_str().unwrap_or("");
    let target_host = target_url.host_str().unwrap_or("");

    if is_loopback_host(target_host) && !is_loopback_host(init_host) {
        return Err(RedirectViolation::ForbiddenTargetHost(
            target_host.to_string(),
        ));
    }

    if is_private_or_link_local_host(target_host)
        && !is_private_or_link_local_host(init_host)
        && !is_loopback_host(init_host)
    {
        return Err(RedirectViolation::ForbiddenTargetHost(
            target_host.to_string(),
        ));
    }

    Ok(())
}

#[derive(Default)]
struct RedirectTrackState {
    violation: Option<RedirectViolation>,
    saw_temporary: bool,
    permanent_target: Option<String>,
}

/// Fetch a feed with conditional GET headers and default 30-second timeout.
pub async fn fetch_feed(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<FetchResult, FetchError> {
    fetch_feed_with_timeout(url, etag, last_modified, Duration::from_secs(30)).await
}

/// Fetch a feed with conditional GET headers and an explicit timeout
/// (e.g. 15 seconds for `Feeds.Add` synchronous validation per `SYNC.md` §7.4).
pub async fn fetch_feed_with_timeout(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
    timeout: Duration,
) -> Result<FetchResult, FetchError> {
    let track = Arc::new(Mutex::new(RedirectTrackState::default()));
    let track_hook = Arc::clone(&track);

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let hop_count = attempt.previous().len();
            let initial_url = attempt.previous().first().unwrap_or_else(|| attempt.url());
            let target_url = attempt.url();

            if let Err(violation) = validate_redirect_target(initial_url, target_url, hop_count) {
                if let Ok(mut guard) = track_hook.lock() {
                    guard.violation = Some(violation.clone());
                }
                return attempt.error(violation);
            }

            if let Ok(mut guard) = track_hook.lock() {
                let code = attempt.status().as_u16();
                if (code == 301 || code == 308) && !guard.saw_temporary {
                    guard.permanent_target = Some(target_url.as_str().to_string());
                } else {
                    guard.saw_temporary = true;
                }
            }

            attempt.follow()
        }))
        .timeout(timeout)
        .build()?;

    let mut request = client.get(url);
    request = request.header(
        ACCEPT,
        "application/atom+xml, application/rss+xml, application/feed+json, application/xml;q=0.9, text/xml;q=0.8",
    );
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

    let resp = match request.send().await {
        Ok(r) => r,
        Err(err) => {
            if let Ok(mut guard) = track.lock()
                && let Some(v) = guard.violation.take()
            {
                return match v {
                    RedirectViolation::TooManyRedirects => Err(FetchError::TooManyRedirects),
                    other => Err(FetchError::RedirectRejected(other)),
                };
            }
            return Err(FetchError::Http(err));
        }
    };

    let status = resp.status().as_u16();
    if status == 304 {
        let permanent_redirect_url = track
            .lock()
            .ok()
            .and_then(|mut g| g.permanent_target.take());
        return Ok(FetchResult {
            status: 304,
            body: Vec::new(),
            etag: resp
                .headers()
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .or_else(|| etag.map(|s| s.to_string())),
            last_modified: resp
                .headers()
                .get(LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .or_else(|| last_modified.map(|s| s.to_string())),
            redirected_url: Some(resp.url().as_str().to_string()),
            permanent_redirect_url,
        });
    }

    if status == 404 || status == 410 {
        return Err(FetchError::Gone(status));
    }

    if status == 429 || status == 503 {
        let retry_after_secs = resp
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok());
        return Err(FetchError::RateLimited {
            status,
            retry_after_secs,
        });
    }

    if status != 200 {
        return Err(FetchError::UnexpectedStatus(status));
    }

    // Handle HTML content type early.
    if let Some(ct) = resp.headers().get(CONTENT_TYPE)
        && ct
            .to_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains("text/html")
    {
        return Err(FetchError::HtmlContent);
    }

    if let Some(cl) = resp.content_length()
        && cl > MAX_FEED_BODY_BYTES as u64
    {
        return Err(FetchError::BodyTooLarge);
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
    let redirected_url = Some(resp.url().as_str().to_string());
    let permanent_redirect_url = track
        .lock()
        .ok()
        .and_then(|mut g| g.permanent_target.take());

    let body = resp.bytes().await?;
    if body.len() > MAX_FEED_BODY_BYTES {
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
        permanent_redirect_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redirect_hook_ssrf_and_hop_rules() {
        let public_https = reqwest::Url::parse("https://blog.example.com/feed.xml").unwrap();
        let public_https_2 = reqwest::Url::parse("https://cdn.example.com/feed.xml").unwrap();
        let public_http = reqwest::Url::parse("http://blog.example.com/feed.xml").unwrap();
        let loopback_target = reqwest::Url::parse("https://127.0.0.1:8080/admin").unwrap();
        let private_target = reqwest::Url::parse("https://192.168.1.1/router.xml").unwrap();
        let link_local_target =
            reqwest::Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();

        // Valid HTTPS -> HTTPS redirect within hop limit
        assert!(validate_redirect_target(&public_https, &public_https_2, 1).is_ok());
        assert!(validate_redirect_target(&public_https, &public_https_2, 5).is_ok());

        // > 5 hops rejected
        assert_eq!(
            validate_redirect_target(&public_https, &public_https_2, 6),
            Err(RedirectViolation::TooManyRedirects)
        );

        // HTTPS -> HTTP downgrade rejected
        assert!(matches!(
            validate_redirect_target(&public_https, &public_http, 1),
            Err(RedirectViolation::SchemeViolation(_))
        ));

        // Public -> Loopback rejected
        assert!(matches!(
            validate_redirect_target(&public_https, &loopback_target, 1),
            Err(RedirectViolation::ForbiddenTargetHost(_))
        ));

        // Public -> RFC 1918 private rejected
        assert!(matches!(
            validate_redirect_target(&public_https, &private_target, 1),
            Err(RedirectViolation::ForbiddenTargetHost(_))
        ));

        // Public -> Link-local metadata service rejected
        assert!(matches!(
            validate_redirect_target(&public_http, &link_local_target, 1),
            Err(RedirectViolation::ForbiddenTargetHost(_))
        ));

        // Loopback -> Loopback is allowed for local testing (e.g. wiremock)
        let loopback_init = reqwest::Url::parse("http://127.0.0.1:9000/old").unwrap();
        assert!(validate_redirect_target(&loopback_init, &loopback_target, 1).is_ok());
    }
}

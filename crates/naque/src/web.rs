//! Client-side `web_fetch`: a direct HTTP GET that returns page text.
//!
//! HTML is converted to Markdown (structure-preserving, good for the agent to
//! read); other text types are returned as-is; binary types are refused. A
//! best-effort SSRF guard blocks requests and redirects whose host is a
//! loopback/private/link-local IP literal or a localhost-like name. DNS names
//! that *resolve* to private ranges are not blocked — acceptable for a
//! local, user-driven dev tool, but noted as a limitation.

use std::net::IpAddr;
use std::time::Duration;

/// Cap on the fetched body. Larger responses are truncated with a marker.
const MAX_BYTES: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;

/// Fetch `url` and return its content as text/Markdown, or an error string
/// suitable for returning to the agent.
pub async fn fetch(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid url: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {},
        other => return Err(format!("unsupported url scheme {other:?} (only http/https)")),
    }
    if host_is_blocked(&parsed) {
        return Err(format!(
            "blocked: {url} resolves to a loopback/private/link-local host; web_fetch reaches public URLs only"
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if host_is_blocked(attempt.url()) {
                attempt.error("redirect to a loopback/private host is blocked")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| format!("http client error: {e}"))?;

    let resp = client.get(parsed).send().await.map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let kind = classify_content_type(&content_type);
    if kind == ContentKind::Binary {
        return Err(format!("unsupported content-type {content_type:?}; web_fetch returns text only"));
    }

    let bytes = resp.bytes().await.map_err(|e| format!("failed to read body: {e}"))?;
    let truncated = bytes.len() > MAX_BYTES;
    let slice = &bytes[..bytes.len().min(MAX_BYTES)];
    let text = String::from_utf8_lossy(slice).into_owned();

    let mut out = match kind {
        ContentKind::Html => htmd::convert(&text).unwrap_or(text),
        ContentKind::Text => text,
        ContentKind::Binary => unreachable!("binary handled above"),
    };
    if truncated {
        out.push_str(&format!("\n\n[truncated at {MAX_BYTES} bytes]"));
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentKind {
    Html,
    Text,
    Binary,
}

/// Classify a (lowercased) content-type header. An empty/missing type is
/// treated as text so we still surface whatever the server returned.
fn classify_content_type(ct: &str) -> ContentKind {
    if ct.contains("html") {
        ContentKind::Html
    } else if ct.is_empty()
        || ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("markdown")
        || ct.contains("javascript")
        || ct.contains("csv")
    {
        ContentKind::Text
    } else {
        ContentKind::Binary
    }
}

/// True if the URL's host is one we refuse to contact (loopback, private,
/// link-local, unspecified, or a localhost-like name).
fn host_is_blocked(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return true; // no host => refuse
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ip_is_blocked(ip);
    }
    let h = host.to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h.ends_with(".local")
        || h.ends_with(".internal")
        || h == "metadata.google.internal"
}

fn ip_is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast()
        },
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_types() {
        assert_eq!(classify_content_type("text/html; charset=utf-8"), ContentKind::Html);
        assert_eq!(classify_content_type("application/json"), ContentKind::Text);
        assert_eq!(classify_content_type("text/plain"), ContentKind::Text);
        assert_eq!(classify_content_type(""), ContentKind::Text);
        assert_eq!(classify_content_type("image/png"), ContentKind::Binary);
        assert_eq!(classify_content_type("application/pdf"), ContentKind::Binary);
    }

    #[test]
    fn blocks_loopback_and_private_literals() {
        for url in [
            "http://127.0.0.1/x",
            "http://localhost/x",
            "https://10.0.0.5/x",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://[::1]/",
            "http://foo.internal/",
        ] {
            let u = reqwest::Url::parse(url).unwrap();
            assert!(host_is_blocked(&u), "expected blocked: {url}");
        }
    }

    #[test]
    fn allows_public_hosts() {
        for url in ["https://example.com/x", "http://93.184.216.34/", "https://docs.rs/"] {
            let u = reqwest::Url::parse(url).unwrap();
            assert!(!host_is_blocked(&u), "expected allowed: {url}");
        }
    }

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        assert!(fetch("ftp://example.com/x").await.is_err());
        assert!(fetch("file:///etc/passwd").await.is_err());
    }
}

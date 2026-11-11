//! Fetching a public page straight from this machine, the way a browser
//! would, before asking the search service's crawler. Only public addresses
//! are reached; every redirect hop is checked again.
use futures::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const USER_AGENT: &str = "Mozilla/5.0 (compatible; Grow-Agent/0.2)";

pub(super) struct Fetched {
    pub title: String,
    pub content: String,
}

/// Why the direct request gave no page.
#[derive(Debug)]
pub(super) enum Failure {
    /// The site itself answered that the page does not exist (404 or 410).
    Missing(u16),
    /// Blocked, rate-limited, unreachable or unreadable here; a crawler
    /// service may still be able to read it.
    Other(String),
}

/// Addresses a fetch may reach: no loopback, private, link-local, CGNAT,
/// multicast or otherwise reserved ranges.
pub(super) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
                || v4.is_broadcast() || v4.is_multicast() || v4.is_documentation()
                || a == 0 || (a == 100 && (b & 0xc0) == 64) || a >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80)
        }
    }
}

async fn public_address(url: &reqwest::Url) -> Result<SocketAddr, Failure> {
    let host = url.host_str().ok_or_else(|| Failure::Other("the URL has no host".into()))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|_| Failure::Other(format!("{host} could not be resolved here")))?
        .collect();
    match addresses.first() {
        None => Err(Failure::Other(format!("{host} could not be resolved here"))),
        Some(_) if addresses.iter().any(|address| !public_ip(address.ip())) => Err(Failure::Other(format!(
            "{host} resolves to a local or private address; only public pages are fetched"
        ))),
        Some(address) => Ok(*address),
    }
}

/// The page at `input`, read as text: Markdown and plain text as they are,
/// HTML converted to Markdown.
pub(super) async fn fetch(input: &str) -> Result<Fetched, Failure> {
    let mut url = reqwest::Url::parse(input).map_err(|_| Failure::Other("invalid URL".into()))?;
    for _ in 0..=MAX_REDIRECTS {
        let address = public_address(&url).await?;
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT);
        // Connect to the address that was checked, not a second lookup.
        if let Some(domain) = url.domain() {
            builder = builder.resolve(domain, address);
        }
        let client = builder.build().map_err(|_| Failure::Other("the HTTP client could not start".into()))?;
        let response = client
            .get(url.clone())
            .header(ACCEPT, "text/markdown, text/html;q=0.9, text/plain;q=0.8, */*;q=0.1")
            .send()
            .await
            .map_err(|_| Failure::Other("the request failed or timed out".into()))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| Failure::Other(format!("HTTP {} without a Location", status.as_u16())))?;
            url = url.join(location).map_err(|_| Failure::Other("invalid redirect target".into()))?;
            if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
                return Err(Failure::Other("redirected to an unsupported URL".into()));
            }
            continue;
        }
        if matches!(status.as_u16(), 404 | 410) {
            return Err(Failure::Missing(status.as_u16()));
        }
        if !status.is_success() {
            return Err(Failure::Other(format!("the site answered HTTP {}", status.as_u16())));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if response.content_length().is_some_and(|length| length > MAX_BYTES as u64) {
            return Err(Failure::Other("the page is larger than 4 MiB".into()));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Failure::Other("the response was cut off".into()))?;
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Err(Failure::Other("the page is larger than 4 MiB".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        let page = readable(&content_type, &body)?;
        if page.content.trim().is_empty() {
            return Err(Failure::Other("the page has no readable text".into()));
        }
        return Ok(page);
    }
    Err(Failure::Other("too many redirects".into()))
}

/// Text a model can read from a response body of this type.
pub(super) fn readable(content_type: &str, body: &str) -> Result<Fetched, Failure> {
    let html = content_type.contains("html") || (content_type.is_empty() && body.trim_start().starts_with('<'));
    if html {
        let converter = htmd::HtmlToMarkdown::builder()
            .skip_tags(vec!["script", "style", "noscript", "svg", "head", "nav", "footer", "iframe"])
            .build();
        let content = converter
            .convert(body)
            .map_err(|_| Failure::Other("the HTML could not be converted".into()))?;
        return Ok(Fetched { title: html_title(body), content });
    }
    if content_type.starts_with("text/") || content_type.contains("json") || content_type.contains("xml") || content_type.is_empty() {
        let title = body
            .lines()
            .find_map(|line| line.strip_prefix("# "))
            .unwrap_or("")
            .trim()
            .to_owned();
        return Ok(Fetched { title, content: body.to_owned() });
    }
    Err(Failure::Other(format!("unsupported content type {content_type}")))
}

fn html_title(html: &str) -> String {
    let pattern = regex::Regex::new(r"(?is)<title[^>]*>(.*?)</title>").expect("valid title pattern");
    pattern
        .captures(html)
        .and_then(|captures| captures.get(1))
        .map(|title| {
            title.as_str().split_whitespace().collect::<Vec<_>>().join(" ")
                .replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
                .replace("&quot;", "\"").replace("&#39;", "'")
        })
        .unwrap_or_default()
        .chars()
        .take(1000)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_are_fetched() {
        for local in ["127.0.0.1", "10.1.2.3", "172.16.0.9", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "::1", "fc00::1", "fe80::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1"] {
            assert!(!public_ip(local.parse().unwrap()), "{local} must be refused");
        }
        for public in ["8.8.8.8", "104.18.32.47", "2606:4700::6810:2009"] {
            assert!(public_ip(public.parse().unwrap()), "{public} is public");
        }
    }

    #[tokio::test]
    async fn local_and_private_targets_are_refused_before_any_request() {
        for url in ["http://127.0.0.1:1/", "http://localhost:1/secret", "http://[::1]:1/", "http://169.254.169.254/latest/meta-data/"] {
            match fetch(url).await {
                Err(Failure::Other(reason)) => assert!(reason.contains("local or private"), "{url}: {reason}"),
                other => panic!("{url} must be refused, got {:?}", other.err()),
            }
        }
    }

    #[test]
    fn html_becomes_markdown_and_text_passes_through() {
        let html = r#"<html><head><title>GPT-5.6 Luna &amp; friends</title><style>p{color:red}</style></head>
            <body><nav>menu</nav><h1>GPT-5.6 Luna</h1><p>Context window: <b>1,050,000</b></p><script>track()</script></body></html>"#;
        let page = readable("text/html; charset=utf-8", html).ok().unwrap();
        assert_eq!(page.title, "GPT-5.6 Luna & friends");
        assert!(page.content.contains("# GPT-5.6 Luna") && page.content.contains("**1,050,000**"), "{}", page.content);
        assert!(!page.content.contains("track()") && !page.content.contains("color:red") && !page.content.contains("menu"), "{}", page.content);

        let markdown = readable("text/markdown; charset=utf-8", "# GPT-5.6 Luna\n\nModel ID: `gpt-5.6-luna`").ok().unwrap();
        assert_eq!((markdown.title.as_str(), markdown.content.contains("Model ID")), ("GPT-5.6 Luna", true));

        assert!(matches!(readable("application/pdf", "%PDF-1.7"), Err(Failure::Other(reason)) if reason.contains("unsupported")));
    }

    /// The 2026-11-11 case: the search service's crawler answered 404 for
    /// every developers.openai.com page, while a direct request reads it.
    /// Needs the network: cargo test -- --ignored direct_read_of_openai_docs
    #[tokio::test]
    #[ignore]
    async fn direct_read_of_openai_docs() {
        let page = fetch("https://developers.openai.com/api/docs/models/gpt-5.6-luna.md").await.unwrap();
        assert!(page.content.contains("gpt-5.6-luna"), "{}", &page.content[..page.content.len().min(500)]);
        let html = fetch("https://developers.openai.com/api/docs/models/gpt-5.6-luna").await.unwrap();
        assert!(html.content.to_lowercase().contains("luna"), "{}", &html.content[..html.content.len().min(500)]);
        assert!(matches!(fetch("https://developers.openai.com/api/docs/models/no-such-model-xyz").await, Err(Failure::Missing(404))));
    }
}

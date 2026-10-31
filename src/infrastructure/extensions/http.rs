//! Bounded HTTP for extensions (design §9).
//!
//! Only hosts the extension declared and the user granted are reachable. Every
//! redirect is checked against the same list again, an https address is never
//! downgraded to http, and the body is cut at the size the extension asked for.
use loom_extension_api::{ErrorKind, ExtensionError};
use loom_extension_host::{HttpPort, HttpResponse};
use std::io::Read;
use std::time::Duration;

/// Redirects followed before giving up.
const MAX_HOPS: usize = 4;
const TIMEOUT: Duration = Duration::from_secs(10);

pub struct BoundedHttp {
    domains: Vec<String>,
    agent: ureq::Agent,
}

impl BoundedHttp {
    pub fn new(domains: Vec<String>) -> Self {
        Self {
            domains,
            // Redirects are followed here, one checked hop at a time.
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout(TIMEOUT)
                .user_agent("Loom-extension")
                .build(),
        }
    }

    fn allows(&self, host: &str) -> bool {
        self.domains
            .iter()
            .any(|pattern| match pattern.strip_prefix("*.") {
                Some(suffix) => {
                    host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}"))
                }
                None => host == pattern,
            })
    }
}

/// `(scheme, authority-with-scheme prefix, host)` of an http(s) URL.
fn parse(url: &str) -> Result<(&str, String), ExtensionError> {
    let bad = |message: &str| ExtensionError::new(ErrorKind::InvalidEdit, message);
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| bad("not an absolute URL"))?;
    if scheme != "http" && scheme != "https" {
        return Err(bad("only http and https are supported"));
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return Err(bad("user information in a URL is not allowed"));
    }
    if authority.starts_with('[') {
        return Err(bad("IP literals are not supported"));
    }
    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err(bad("the URL has no host"));
    }
    Ok((scheme, host))
}

fn absolute(base: &str, location: &str) -> Option<String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Some(location.to_owned());
    }
    let (scheme, rest) = base.split_once("://")?;
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    location
        .starts_with('/')
        .then(|| format!("{scheme}://{authority}{location}"))
}

impl HttpPort for BoundedHttp {
    fn get(&self, url: &str, max: usize) -> Result<HttpResponse, ExtensionError> {
        let mut current = url.to_owned();
        for _ in 0..=MAX_HOPS {
            let (scheme, host) = parse(&current)?;
            if !self.allows(&host) {
                return Err(ExtensionError::new(
                    ErrorKind::PermissionDenied,
                    format!("`{host}` is not one of the hosts this extension may reach"),
                ));
            }
            let response = match self.agent.get(&current).call() {
                Ok(response) => response,
                // An error status is still an answer the extension may want to see.
                Err(ureq::Error::Status(_, response)) => response,
                Err(ureq::Error::Transport(error)) => {
                    return Err(ExtensionError::new(
                        ErrorKind::ExtensionFailed,
                        error.to_string(),
                    ));
                }
            };
            let status = response.status();
            if matches!(status, 301 | 302 | 303 | 307 | 308)
                && let Some(location) = response.header("location")
            {
                let next = absolute(&current, location).ok_or_else(|| {
                    ExtensionError::new(ErrorKind::InvalidEdit, "the redirect target is not usable")
                })?;
                if scheme == "https" && next.starts_with("http://") {
                    return Err(ExtensionError::new(
                        ErrorKind::PermissionDenied,
                        "a redirect from https to http is refused",
                    ));
                }
                current = next;
                continue;
            }
            let content_type = response.header("content-type").map(str::to_owned);
            let mut body = Vec::new();
            response
                .into_reader()
                .take(max as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|e| ExtensionError::new(ErrorKind::ExtensionFailed, e.to_string()))?;
            let truncated = body.len() > max;
            body.truncate(max);
            return Ok(HttpResponse {
                status,
                content_type,
                body: String::from_utf8_lossy(&body).into_owned(),
                truncated,
            });
        }
        Err(ExtensionError::new(
            ErrorKind::LimitExceeded,
            "too many redirects",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// A server that answers each connection from `respond(path)` and keeps
    /// serving until the test process ends.
    fn serve(respond: fn(&str, u16) -> String) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let _ = stream.write_all(respond(&path, port).as_bytes());
            }
        });
        port
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn redirect(location: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )
    }

    fn pages(path: &str, port: u16) -> String {
        match path {
            "/hello" => ok("hello world"),
            "/long" => ok(&"x".repeat(5000)),
            "/missing" => {
                "HTTP/1.1 404 Not Found\r\ncontent-length: 4\r\nconnection: close\r\n\r\nnope"
                    .to_owned()
            }
            "/inside" => redirect("/hello"),
            "/outside" => redirect(&format!("http://localhost:{port}/hello")),
            "/loop" => redirect("/loop"),
            _ => ok("?"),
        }
    }

    fn client() -> (BoundedHttp, u16) {
        let port = serve(pages);
        (BoundedHttp::new(vec!["127.0.0.1".into()]), port)
    }

    #[test]
    fn a_granted_host_answers_and_the_body_is_cut_at_the_asked_size() {
        let (http, port) = client();
        let response = http
            .get(&format!("http://127.0.0.1:{port}/hello"), 100)
            .unwrap();
        assert_eq!(
            (response.status, response.body.as_str(), response.truncated),
            (200, "hello world", false)
        );
        assert_eq!(response.content_type.as_deref(), Some("text/plain"));
        let long = http
            .get(&format!("http://127.0.0.1:{port}/long"), 100)
            .unwrap();
        assert!(long.truncated && long.body.len() == 100);
    }

    #[test]
    fn an_error_status_is_an_answer_not_a_failure() {
        let (http, port) = client();
        let response = http
            .get(&format!("http://127.0.0.1:{port}/missing"), 100)
            .unwrap();
        assert_eq!((response.status, response.body.as_str()), (404, "nope"));
    }

    #[test]
    fn hosts_that_were_not_granted_are_refused_before_any_connection() {
        let (http, port) = client();
        for url in [
            format!("http://localhost:{port}/hello"),
            "http://example.com/".to_owned(),
            format!("http://127.0.0.1:{port}@example.com/"),
            "ftp://127.0.0.1/x".to_owned(),
            "http://[::1]/".to_owned(),
        ] {
            assert!(http.get(&url, 10).is_err(), "{url}");
        }
        let kind = http.get("http://example.com/", 10).unwrap_err().kind;
        assert_eq!(kind, ErrorKind::PermissionDenied);
    }

    #[test]
    fn redirects_are_followed_inside_the_grant_and_checked_again_outside_it() {
        let (http, port) = client();
        let inside = http
            .get(&format!("http://127.0.0.1:{port}/inside"), 100)
            .unwrap();
        assert_eq!(inside.body, "hello world");
        let outside = http
            .get(&format!("http://127.0.0.1:{port}/outside"), 100)
            .unwrap_err();
        assert_eq!(outside.kind, ErrorKind::PermissionDenied, "{outside}");
        let looping = http
            .get(&format!("http://127.0.0.1:{port}/loop"), 100)
            .unwrap_err();
        assert_eq!(looping.kind, ErrorKind::LimitExceeded);
    }

    #[test]
    fn wildcards_cover_subdomains_but_not_the_bare_domain() {
        let http = BoundedHttp::new(vec!["*.example.com".into(), "api.test".into()]);
        assert!(http.allows("a.example.com") && http.allows("a.b.example.com"));
        assert!(!http.allows("example.com") && !http.allows("badexample.com"));
        assert!(http.allows("api.test") && !http.allows("x.api.test"));
    }
}

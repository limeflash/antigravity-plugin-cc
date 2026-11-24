//! /agy:scrape's page download, done by the plugin rather than by agy.
//!
//! agy 1.5.16 has no tool that reads a URL (only web search), so in 0.8.x the
//! page was fetched by a shell command agy ran — which a read-only run must
//! refuse. Fetching it here is also safer: the SSRF guard checks the URL,
//! then EVERY address its host resolves to, and curl is pinned to the
//! vetted address with `--resolve`, so a DNS answer that changes between the
//! check and the request (rebinding) can't reach an internal service.
//! Redirects are followed by hand and each hop is checked again.
//!
//! HTTPS comes from the system curl (std has no TLS).

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use crate::util;

pub(crate) const MAX_PAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const DNS_TIMEOUT: Duration = Duration::from_secs(15);

/// A fetched page saved to disk.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Page {
    pub(crate) final_url: String,
    pub(crate) content_type: String,
    /// File extension to stage it under (html, txt, xml, json).
    pub(crate) ext: &'static str,
}

/// The checks a fetch applies; production uses the SSRF guard. Tests swap
/// them so a local server can stand in for the internet.
pub(crate) struct Policy<'a> {
    pub(crate) validate_url: &'a dyn Fn(&str) -> Result<String, String>,
    pub(crate) ip_blocked: &'a dyn Fn(IpAddr) -> bool,
    pub(crate) use_proxy: bool,
}

pub(crate) fn guard_policy() -> Policy<'static> {
    Policy {
        validate_url: &crate::guard::validate_scrape_url,
        ip_blocked: &|ip| crate::guard::is_blocked_host(&ip.to_string()),
        // Off by default: through a proxy, the proxy resolves the name and
        // our address check no longer protects anything.
        use_proxy: util::env_nonempty("AGY_SCRAPE_USE_PROXY").as_deref() == Some("1"),
    }
}

/// Split a normalized `scheme://host[:port]/rest` URL (the form
/// `guard::validate_scrape_url` returns).
fn split_url(url: &str) -> Result<(String, String, u16), String> {
    let (scheme, rest) = url.split_once("://").ok_or("not an absolute URL")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let default_port = if scheme.eq_ignore_ascii_case("https") {
        443
    } else {
        80
    };
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (h, after) = inner.split_once(']').ok_or("bad IPv6 literal")?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| "bad port")?,
            None => default_port,
        };
        (h.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().map_err(|_| "bad port")?),
            None => (authority.to_string(), default_port),
        }
    };
    if host.is_empty() {
        return Err("URL has no host".into());
    }
    Ok((scheme.to_ascii_lowercase(), host, port))
}

/// Resolve with a deadline (std's resolver has none).
fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let (tx, rx) = mpsc::channel();
    let h = host.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(
            (h.as_str(), port)
                .to_socket_addrs()
                .map(|a| a.collect::<Vec<_>>()),
        );
    });
    match rx.recv_timeout(DNS_TIMEOUT) {
        Ok(Ok(addrs)) if !addrs.is_empty() => Ok(addrs),
        Ok(Ok(_)) => Err(format!("host '{host}' has no addresses")),
        Ok(Err(e)) => Err(format!("could not resolve '{host}': {e}")),
        Err(_) => Err(format!("timed out resolving '{host}'")),
    }
}

fn ext_for(content_type: &str) -> Option<&'static str> {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match ct.as_str() {
        "text/html" | "application/xhtml+xml" => Some("html"),
        "application/xml" | "text/xml" | "application/rss+xml" | "application/atom+xml" => {
            Some("xml")
        }
        "application/json" => Some("json"),
        "" => Some("html"),
        t if t.starts_with("text/") => Some("txt"),
        _ => None,
    }
}

/// Download `url` into `dest` (created, never overwritten).
pub(crate) fn fetch(url: &str, dest: &Path, policy: &Policy) -> Result<Page, String> {
    let curl = util::which("curl").ok_or("scrape needs `curl` on PATH to download the page")?;
    let mut current = url.to_string();
    for _hop in 0..=MAX_REDIRECTS {
        let checked = (policy.validate_url)(&current)?;
        let (_scheme, host, port) = split_url(&checked)?;
        let addrs = resolve(&host, port)?;
        if let Some(bad) = addrs.iter().find(|a| (policy.ip_blocked)(a.ip())) {
            return Err(format!(
                "host '{host}' resolves to {}, which is blocked (localhost / private / link-local — SSRF guard)",
                bad.ip()
            ));
        }
        let pin = addrs[0];
        let pin_ip = match pin.ip() {
            IpAddr::V6(v6) => format!("[{v6}]"),
            IpAddr::V4(v4) => v4.to_string(),
        };
        let _ = std::fs::remove_file(dest);
        let mut cmd = Command::new(&curl);
        cmd.args([
            // -q must come first: ignore ~/.curlrc (a `-L` there would break
            // the hand-checked redirects; proxy or header settings would
            // change what is fetched).
            "-q",
            "--silent",
            "--show-error",
            "--proto",
            "=http,https",
            "--max-redirs",
            "0",
            "--connect-timeout",
            "15",
            "--max-time",
            "60",
            "--max-filesize",
            &MAX_PAGE_BYTES.to_string(),
            "--compressed",
            "-H",
            "Accept: text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.1",
            "-A",
            concat!(
                "agy-plugin/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/limeflash/antigravity-plugin-cc)"
            ),
            "-w",
            "%{http_code}\\n%{redirect_url}\\n%{content_type}\\n",
            "-o",
        ])
        .arg(dest);
        if host.parse::<IpAddr>().is_err() {
            cmd.args(["--resolve", &format!("{host}:{port}:{pin_ip}")]);
        }
        if !policy.use_proxy {
            cmd.args(["--noproxy", "*"]);
        }
        cmd.arg("--").arg(&checked);
        let out = util::run_captured(&mut cmd, Some(Duration::from_secs(90)))
            .map_err(|e| format!("could not run curl: {e}"))?;
        if !out.ok() {
            let e = out.stderr_str();
            let _ = std::fs::remove_file(dest);
            return Err(match out.code {
                Some(63) => format!("the page is larger than {MAX_PAGE_BYTES} bytes"),
                _ => format!("download failed: {}", e.trim()),
            });
        }
        let w = out.stdout_str();
        let mut lines = w.lines();
        let code: u16 = lines.next().unwrap_or("0").trim().parse().unwrap_or(0);
        let redirect = lines.next().unwrap_or("").trim().to_string();
        let content_type = lines.next().unwrap_or("").trim().to_string();
        if (300..400).contains(&code) && !redirect.is_empty() {
            if checked.starts_with("https://")
                && !redirect.to_ascii_lowercase().starts_with("https://")
            {
                let _ = std::fs::remove_file(dest);
                return Err(format!(
                    "refusing a redirect from https to a non-https URL ({redirect})"
                ));
            }
            current = redirect;
            continue;
        }
        if !(200..300).contains(&code) {
            let _ = std::fs::remove_file(dest);
            return Err(format!("the server answered HTTP {code}"));
        }
        let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        if size > MAX_PAGE_BYTES {
            let _ = std::fs::remove_file(dest);
            return Err(format!("the page is larger than {MAX_PAGE_BYTES} bytes"));
        }
        let Some(ext) = ext_for(&content_type) else {
            let _ = std::fs::remove_file(dest);
            return Err(format!(
                "unsupported content type '{content_type}' (scrape converts web pages and text)"
            ));
        };
        return Ok(Page {
            final_url: checked,
            content_type,
            ext,
        });
    }
    Err(format!("too many redirects (more than {MAX_REDIRECTS})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// A tiny HTTP server: path -> (status line, headers, body).
    fn serve(
        routes: Vec<(&'static str, &'static str, String, String)>,
    ) -> (u16, std::thread::JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            l.set_nonblocking(false).unwrap();
            for stream in l.incoming().take(8) {
                let Ok(mut s) = stream else { break };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                let _ = r.read_line(&mut first);
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, headers, body) = routes
                    .iter()
                    .find(|(p, _, _, _)| *p == path)
                    .map(|(_, st, h, b)| (st.to_string(), h.clone(), b.clone()))
                    .unwrap_or((
                        "404 Not Found".into(),
                        "Content-Type: text/plain\r\n".into(),
                        "nope".into(),
                    ));
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (port, h)
    }

    fn permissive() -> Policy<'static> {
        Policy {
            validate_url: &|u| Ok(u.to_string()),
            ip_blocked: &|_| false,
            use_proxy: false,
        }
    }

    fn have_curl() -> bool {
        util::which("curl").is_some()
    }

    #[test]
    fn split_url_forms() {
        assert_eq!(
            split_url("https://example.com/a?b").unwrap(),
            ("https".into(), "example.com".into(), 443)
        );
        assert_eq!(
            split_url("http://example.com:8080").unwrap(),
            ("http".into(), "example.com".into(), 8080)
        );
        assert_eq!(
            split_url("http://[2001:db8::1]:81/x").unwrap(),
            ("http".into(), "2001:db8::1".into(), 81)
        );
        assert_eq!(split_url("http://[2001:db8::1]/").unwrap().2, 80);
        assert!(split_url("example.com").is_err());
        assert!(split_url("http:///x").is_err());
    }

    #[test]
    fn content_types() {
        assert_eq!(ext_for("text/html; charset=utf-8"), Some("html"));
        assert_eq!(ext_for("TEXT/PLAIN"), Some("txt"));
        assert_eq!(ext_for("application/json"), Some("json"));
        assert_eq!(ext_for("application/rss+xml"), Some("xml"));
        assert_eq!(ext_for("image/png"), None);
        assert_eq!(ext_for("application/octet-stream"), None);
    }

    #[test]
    fn fetches_html_and_follows_checked_redirects() {
        if !have_curl() {
            return;
        }
        let (port, _h) = serve(vec![
            (
                "/start",
                "302 Found",
                "Location: /page\r\n".into(),
                String::new(),
            ),
            (
                "/page",
                "200 OK",
                "Content-Type: text/html; charset=utf-8\r\n".into(),
                "<h1>Hi</h1>".into(),
            ),
        ]);
        let d = util::TempDir::new("agy-fetch-test-").unwrap();
        let dest = d.path().join("page");
        let p = fetch(
            &format!("http://127.0.0.1:{port}/start"),
            &dest,
            &permissive(),
        )
        .unwrap();
        assert_eq!(p.ext, "html");
        assert!(p.final_url.ends_with("/page"));
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "<h1>Hi</h1>");
    }

    #[test]
    fn a_redirect_to_a_refused_url_is_refused() {
        if !have_curl() {
            return;
        }
        let (port, _h) = serve(vec![(
            "/start",
            "301 Moved",
            "Location: /evil\r\n".into(),
            String::new(),
        )]);
        let d = util::TempDir::new("agy-fetch-test-").unwrap();
        let validate = |u: &str| {
            if u.contains("/evil") {
                Err("blocked hop".to_string())
            } else {
                Ok(u.to_string())
            }
        };
        let policy = Policy {
            validate_url: &validate,
            ip_blocked: &|_| false,
            use_proxy: false,
        };
        let err = fetch(
            &format!("http://127.0.0.1:{port}/start"),
            &d.path().join("p"),
            &policy,
        )
        .unwrap_err();
        assert_eq!(err, "blocked hop");
    }

    #[test]
    fn a_blocked_resolved_address_is_refused_before_any_request() {
        let d = util::TempDir::new("agy-fetch-test-").unwrap();
        let policy = Policy {
            validate_url: &|u| Ok(u.to_string()),
            ip_blocked: &|ip| ip.is_loopback(),
            use_proxy: false,
        };
        // "localhost" resolves to a loopback address: refused at the DNS step.
        let err = fetch("http://localhost:9/", &d.path().join("p"), &policy).unwrap_err();
        assert!(err.contains("resolves to") && err.contains("SSRF"), "{err}");
        assert!(!d.path().join("p").exists());
    }

    #[test]
    fn non_text_content_and_http_errors_are_refused() {
        if !have_curl() {
            return;
        }
        let (port, _h) = serve(vec![
            (
                "/img",
                "200 OK",
                "Content-Type: image/png\r\n".into(),
                "PNG".into(),
            ),
            (
                "/gone",
                "410 Gone",
                "Content-Type: text/html\r\n".into(),
                "x".into(),
            ),
        ]);
        let d = util::TempDir::new("agy-fetch-test-").unwrap();
        let e = fetch(
            &format!("http://127.0.0.1:{port}/img"),
            &d.path().join("a"),
            &permissive(),
        )
        .unwrap_err();
        assert!(e.contains("unsupported content type"), "{e}");
        assert!(!d.path().join("a").exists());
        let e = fetch(
            &format!("http://127.0.0.1:{port}/gone"),
            &d.path().join("b"),
            &permissive(),
        )
        .unwrap_err();
        assert!(e.contains("HTTP 410"), "{e}");
    }

    #[test]
    fn redirect_loops_stop() {
        if !have_curl() {
            return;
        }
        let (port, _h) = serve(vec![(
            "/loop",
            "302 Found",
            "Location: /loop\r\n".into(),
            String::new(),
        )]);
        let d = util::TempDir::new("agy-fetch-test-").unwrap();
        let e = fetch(
            &format!("http://127.0.0.1:{port}/loop"),
            &d.path().join("p"),
            &permissive(),
        )
        .unwrap_err();
        assert!(e.contains("too many redirects"), "{e}");
    }

    #[test]
    fn production_policy_blocks_internal_targets() {
        let p = guard_policy();
        for u in [
            "http://127.0.0.1/",
            "http://169.254.169.254/",
            "http://[::1]/",
            "file:///etc/passwd",
        ] {
            assert!((p.validate_url)(u).is_err(), "{u}");
        }
        assert!((p.ip_blocked)("10.0.0.1".parse().unwrap()));
        assert!((p.ip_blocked)("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!(p.ip_blocked)("93.184.215.14".parse().unwrap()));
    }
}

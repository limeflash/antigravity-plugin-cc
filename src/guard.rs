//! Input guards for the read-only commands that hand outside input to agy:
//! `/agy:scrape` (a URL agy will fetch) and `/agy:doc-to-md` (a local file
//! agy will read). Whatever passes here ends up at Gemini, so:
//!   - scrape: SSRF (file://, localhost, cloud metadata, private ranges,
//!     numeric/IPv6 encodings of them, nip.io-style DNS that resolves to them);
//!   - doc-to-md: local-file exfiltration (~/.ssh, ~/.aws, /etc, ...).
//!
//! Fail CLOSED: anything not clearly allowed is rejected. Best-effort — an
//! attacker who controls DNS can still rebind a public name to an internal
//! address; this blocks literal targets, encodings and well-known tricks.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::net::Ipv6Addr;
use std::path::{Component, Path, PathBuf, Prefix};

// ---------------------------------------------------------------------------
// /agy:scrape — SSRF guard
// ---------------------------------------------------------------------------

const INVALID_URL: &str = "not a valid absolute URL (include http:// or https://)";
const BAD_URL_CHARS: &str = "URL contains characters that are not allowed (spaces, backslashes, control or non-ASCII characters)";

/// Validate a scrape URL and return the normalized form that must be handed
/// to agy (so what agy fetches is exactly what was checked).
///
/// Hand-parsed and deliberately narrower than WHATWG: anything a browser-grade
/// parser would "fix up" (backslashes, tabs, IDNs, percent-encoded hosts,
/// userinfo) is refused rather than emulated, so a second parser downstream
/// cannot read a different host out of the same string.
pub(crate) fn validate_scrape_url(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("empty URL".into());
    }
    if s.bytes()
        .any(|b| b <= b' ' || b == 0x7f || b == b'\\' || !b.is_ascii())
    {
        return Err(BAD_URL_CHARS.into());
    }
    let Some((scheme, rest)) = s.split_once("://") else {
        return Err(INVALID_URL.into());
    };
    if !is_scheme(scheme) {
        return Err(INVALID_URL.into());
    }
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "protocol '{scheme}:' is not allowed (only http/https)"
        ));
    }

    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_end);
    if authority.is_empty() {
        return Err(INVALID_URL.into());
    }
    // `http://example.com@127.0.0.1/` fetches 127.0.0.1; parsers also
    // disagree about which `@` ends the userinfo. Never allow it.
    if authority.contains('@') {
        return Err("credentials in URLs are not allowed".into());
    }

    let (host_raw, port_raw) = if authority.starts_with('[') {
        let close = authority.find(']').ok_or(INVALID_URL)?;
        let after = &authority[close + 1..];
        let port = if after.is_empty() {
            None
        } else {
            Some(after.strip_prefix(':').ok_or(INVALID_URL)?)
        };
        (&authority[..=close], port)
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };

    let port = match port_raw {
        None => None,
        Some(p) => {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(INVALID_URL.into());
            }
            match p.parse::<u16>() {
                Ok(n) if n >= 1 => Some(n),
                _ => return Err(INVALID_URL.into()),
            }
        }
    };

    // Percent-encoded hosts are decoded by WHATWG (`%6c%6f...` = localhost);
    // refuse instead of decoding. Same for every other non-hostname byte.
    let host_ok = if let Some(inner) = host_raw.strip_prefix('[') {
        inner.strip_suffix(']').is_some_and(|v6| {
            v6.bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
        })
    } else {
        host_raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    };
    if host_raw.contains('%') || !host_ok {
        return Err(INVALID_URL.into());
    }
    let mut host = host_raw.to_ascii_lowercase();
    host.truncate(host.trim_end_matches('.').len());

    if is_blocked_host(&host) {
        return Err(format!(
            "host '{host}' is blocked (localhost / private / link-local \u{2014} SSRF guard)"
        ));
    }

    let out_host = if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        let ip: Ipv6Addr = inner.parse().map_err(|_| INVALID_URL.to_string())?;
        format!("[{ip}]")
    } else if ends_in_number(&host) {
        // Alternate IPv4 spellings (2130706433, 0x7f.1, 0177.0.0.1) are
        // rewritten dotted-decimal so agy cannot interpret them differently.
        let ip = parse_ipv4_whatwg(&host).ok_or(INVALID_URL)?;
        std::net::Ipv4Addr::from(ip).to_string()
    } else {
        host
    };

    let path_query = tail.split('#').next().unwrap_or("");
    let path_query = if path_query.starts_with('/') {
        path_query.to_string()
    } else {
        format!("/{path_query}")
    };
    let port = port.map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!("{scheme}://{out_host}{port}{path_query}"))
}

fn is_scheme(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_alphabetic())
        && b.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
}

/// True if a URL host must not be fetched. Accepts a bare host or a
/// bracketed IPv6 literal; lowercases and strips trailing dots itself.
pub(crate) fn is_blocked_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    let h = lower.trim_end_matches('.');
    if h.is_empty() {
        return true;
    }

    if h.starts_with('[') || h.ends_with(']') || h.contains(':') {
        let inner = h
            .strip_prefix('[')
            .and_then(|x| x.strip_suffix(']'))
            .unwrap_or(h);
        // Zone ids (`fe80::1%eth0`) and anything std cannot parse: blocked.
        return match inner.parse::<Ipv6Addr>() {
            Ok(ip) => ipv6_blocked(ip),
            Err(_) => true,
        };
    }

    // Not a resolvable DNS name: fail closed rather than guess what a
    // resolver does with it (also bounds the embedded-address scans below).
    if h.len() > 253 || h.split('.').any(|l| l.len() > 63) {
        return true;
    }

    if h == "localhost" {
        return true;
    }
    const SUFFIXES: [&str; 8] = [
        ".localhost",
        ".local",
        ".internal",
        ".lan",
        ".home",
        ".corp",
        ".home.arpa",
        // `localhost.localdomain` is in many Linux /etc/hosts files.
        ".localdomain",
    ];
    if SUFFIXES.iter().any(|s| h.ends_with(s)) {
        return true;
    }
    // Public wildcard domains whose every name resolves to 127.0.0.1.
    // Not in the spec list; cheap to refuse outright.
    const LOOPBACK_DOMAINS: [&str; 4] = ["localtest.me", "lvh.me", "vcap.me", "localhost.direct"];
    if LOOPBACK_DOMAINS
        .iter()
        .any(|d| h == *d || h.strip_suffix(d).is_some_and(|p| p.ends_with('.')))
    {
        return true;
    }

    // WHATWG: a host whose last label is numeric IS an IPv4 address
    // (2130706433, 0x7f.1, 0177.0.0.1, 127.1). If it does not parse, a
    // browser would reject the URL; we block it.
    if ends_in_number(h) {
        return parse_ipv4_whatwg(h).is_none_or(ipv4_blocked);
    }

    // A single-label name (`metadata`, `intranet`, `router`) is completed
    // by the resolver's search domains to an internal host (on GCE,
    // `metadata` is the metadata server). Public sites never use one.
    if !h.contains('.') {
        return true;
    }

    embeds_blocked_address(h)
}

/// nip.io / sslip.io style names resolve to an address spelled inside them.
/// Every encoding and every position is checked, not just the first match.
fn embeds_blocked_address(h: &str) -> bool {
    let labels: Vec<&str> = h.split('.').collect();
    // Dotted: any 4 consecutive decimal-octet labels (`1.2.3.4.127.0.0.1.nip.io`).
    if labels.windows(4).any(octets_blocked) {
        return true;
    }
    labels.iter().any(|label| {
        let parts: Vec<&str> = label.split('-').collect();
        // Dashed quad, anywhere in the label (`127-0-0-1`, `app-127-0-0-1`).
        if parts.windows(4).any(octets_blocked) {
            return true;
        }
        // 8-hex-digit form (`7f000001`, `app-7f000001`).
        if parts.iter().any(|p| {
            p.len() == 8
                && p.bytes().all(|b| b.is_ascii_hexdigit())
                && u32::from_str_radix(p, 16).is_ok_and(ipv4_blocked)
        }) {
            return true;
        }
        dashed_ipv6_blocked(label)
    })
}

fn octets_blocked(w: &[&str]) -> bool {
    let mut ip: u32 = 0;
    for part in w {
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        match part.parse::<u8>() {
            Ok(o) => ip = (ip << 8) | u32::from(o),
            Err(_) => return false,
        }
    }
    ipv4_blocked(ip)
}

/// sslip.io resolves IPv6 written with dashes (`--1.sslip.io` is ::1,
/// `fe80--1` is fe80::1), also after a `prefix-`. Like sslip.io, a candidate
/// must start at the label start or right after a `-` and end at the label
/// end or right before a `-`; that keeps punycode (`xn--bcher-kva`) from
/// being read as `::bc`. Labels are at most 63 bytes, so this stays small.
fn dashed_ipv6_blocked(label: &str) -> bool {
    let b = label.as_bytes();
    if b.iter().filter(|&&c| c == b'-').count() < 2 {
        return false;
    }
    let starts: Vec<usize> = (0..b.len())
        .filter(|&i| i == 0 || b[i - 1] == b'-')
        .collect();
    let ends: Vec<usize> = (1..=b.len())
        .filter(|&j| j == b.len() || b[j] == b'-')
        .collect();
    for &i in &starts {
        for &j in ends.iter().filter(|&&j| j > i) {
            let sub = &label[i..j];
            if sub.bytes().filter(|&c| c == b'-').count() < 2
                || !sub.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
            {
                continue;
            }
            if sub
                .replace('-', ":")
                .parse::<Ipv6Addr>()
                .is_ok_and(ipv6_blocked)
            {
                return true;
            }
        }
    }
    false
}

/// WHATWG "ends in a number": last label all decimal digits, or `0x` + hex
/// (an empty `0x` counts — it is the number 0).
fn ends_in_number(h: &str) -> bool {
    let last = h.rsplit('.').next().unwrap_or("");
    if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    last.strip_prefix("0x")
        .or_else(|| last.strip_prefix("0X"))
        .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// WHATWG IPv4 parser: 1–4 parts, each decimal, `0x` hex or leading-`0`
/// octal; the last part fills the remaining bytes. None = not a valid IPv4
/// (callers treat that as blocked).
fn parse_ipv4_whatwg(h: &str) -> Option<u32> {
    let parts: Vec<&str> = h.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut nums = Vec::with_capacity(parts.len());
    for p in &parts {
        nums.push(parse_ipv4_number(p)?);
    }
    let (last, init) = nums.split_last()?;
    let mut ip: u64 = 0;
    for (i, &n) in init.iter().enumerate() {
        if n > 255 {
            return None;
        }
        ip |= n << (8 * (3 - i));
    }
    let limit = 1u64 << (8 * (5 - parts.len()));
    if *last >= limit {
        return None;
    }
    u32::try_from(ip | last).ok()
}

fn parse_ipv4_number(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let (digits, radix) = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (hex, 16)
    } else if s.len() >= 2 && s.starts_with('0') {
        (&s[1..], 8)
    } else {
        (s, 10)
    };
    let mut v: u64 = 0;
    for c in digits.chars() {
        let d = c.to_digit(radix)?;
        // Overflow means "out of range" -> None -> blocked.
        v = v.checked_mul(u64::from(radix))?.checked_add(u64::from(d))?;
    }
    Some(v)
}

/// Non-public IPv4. Wider than the old guard: also multicast (224/4),
/// reserved (240/4, incl. broadcast), benchmarking (198.18/15) and the
/// IETF protocol block (192.0.0/24).
fn ipv4_blocked(ip: u32) -> bool {
    let in_net = |net: u32, bits: u32| (ip ^ net) >> (32 - bits) == 0;
    in_net(0x0000_0000, 8)          // 0/8 "this network"
        || in_net(0x0a00_0000, 8)   // 10/8
        || in_net(0x6440_0000, 10)  // 100.64/10 CGNAT
        || in_net(0x7f00_0000, 8)   // 127/8 loopback
        || in_net(0xa9fe_0000, 16)  // 169.254/16 link-local + cloud metadata
        || in_net(0xac10_0000, 12)  // 172.16/12
        || in_net(0xc000_0000, 24)  // 192.0.0/24
        || in_net(0xc0a8_0000, 16)  // 192.168/16
        || in_net(0xc612_0000, 15)  // 198.18/15
        || in_net(0xe000_0000, 4)   // 224/4 multicast
        || in_net(0xf000_0000, 4) // 240/4 reserved + broadcast
}

fn ipv6_blocked(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let v = u128::from(ip);
    let low32 = (v & 0xffff_ffff) as u32;
    if v <= 1 {
        return true; // :: and ::1
    }
    if s[0] & 0xffc0 == 0xfe80   // fe80::/10 link-local
        || s[0] & 0xffc0 == 0xfec0 // fec0::/10 site-local
        || s[0] & 0xfe00 == 0xfc00 // fc00::/7 ULA
        || s[0] & 0xff00 == 0xff00
    // ff00::/8 multicast
    {
        return true;
    }
    // ::ffff:a.b.c.d (mapped) and ::a.b.c.d (deprecated compat).
    if s[..5] == [0; 5] && (s[5] == 0xffff || s[5] == 0) {
        return ipv4_blocked(low32);
    }
    // ::ffff:0:a.b.c.d (SIIT "IPv4-translated"); not in the spec list.
    if s[..4] == [0; 4] && s[4] == 0xffff && s[5] == 0 {
        return ipv4_blocked(low32);
    }
    // 64:ff9b::/96 NAT64 well-known prefix.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0; 4] {
        return ipv4_blocked(low32);
    }
    // 64:ff9b:1::/48 local-use NAT64 (RFC 8215): never public; not in the spec list.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1 {
        return true;
    }
    // 2002::/16 6to4: the v4 sits in bits 16..48.
    if s[0] == 0x2002 {
        return ipv4_blocked((u32::from(s[1]) << 16) | u32::from(s[2]));
    }
    false
}

// ---------------------------------------------------------------------------
// /agy:doc-to-md — local document guard
// ---------------------------------------------------------------------------

/// Document extensions we convert (lowercase, no dot). Everything else
/// (.pem, .key, .env, id_rsa, ...) is rejected — fail closed.
pub(crate) const ALLOWED_DOC_EXT: &[&str] = &[
    "pdf", "docx", "doc", "odt", "rtf", "html", "htm", "xhtml", "xml", "md", "markdown", "txt",
    "text", "rst", "org", "adoc", "csv", "tsv", "pptx", "ppt", "epub",
];

pub(crate) const MAX_DOC_BYTES: u64 = 10 * 1024 * 1024;

/// UNC (`\\server\share`) or device (`\\?\`, `\\.\`, `//server`) path.
pub(crate) fn is_unc_path(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && matches!(b[0], b'/' | b'\\') && matches!(b[1], b'/' | b'\\')
}

/// String check plus, on Windows, any path prefix other than a plain drive
/// (`UNC`, `\\?\UNC`, `\\.\pipe`, `\\?\Volume{..}` ...).
fn path_is_unc(p: &Path) -> bool {
    if is_unc_path(&p.to_string_lossy()) {
        return true;
    }
    match p.components().next() {
        Some(Component::Prefix(pre)) => !matches!(pre.kind(), Prefix::Disk(_)),
        _ => false,
    }
}

/// Windows `canonicalize` returns verbatim `\\?\C:\...`; turn a verbatim
/// DRIVE path back into `C:\...` so the UNC check and the sensitive-dir
/// prefixes work. `\\?\UNC\server\share` (and non-UTF-8) is left alone and is
/// then rejected as UNC.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    if let Some(rest) = p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        let b = rest.as_bytes();
        if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
            return PathBuf::from(rest);
        }
    }
    p
}

// Default APFS and NTFS are case-insensitive: `~/.SSH/x` IS `~/.ssh/x`.
const CASE_INSENSITIVE_FS: bool = cfg!(any(windows, target_os = "macos", target_os = "ios"));

fn path_under(p: &Path, base: &Path) -> bool {
    if CASE_INSENSITIVE_FS {
        let lower = |x: &Path| PathBuf::from(x.to_string_lossy().to_lowercase());
        lower(p).starts_with(lower(base))
    } else {
        p.starts_with(base)
    }
}

/// True if `real` (an already symlink-resolved path) is a credentials or
/// system location. Component-wise: `/etcetera` is not under `/etc`.
pub(crate) fn is_sensitive_path(real: &Path, home: &Path) -> bool {
    let mut bases: Vec<PathBuf> = vec![
        home.join(".ssh"),
        home.join(".gnupg"),
        home.join(".aws"),
        home.join(".config").join("gcloud"),
        home.join(".gemini"), // agy's own credentials / conversation store
        home.join(".kube"),
        home.join(".docker"),
        home.join(".m2"),     // Maven settings.xml (repo passwords)
        home.join(".purple"), // Pidgin accounts.xml
        home.join(".config").join("filezilla"),
        home.join(".filezilla"),
        home.join(".password-store"),
        home.join(".local").join("share").join("keyrings"),
        home.join(".mozilla"),
        home.join(".thunderbird"),
        home.join(".netrc"),
        home.join("Library").join("Keychains"),
        home.join("Library").join("Cookies"),
    ];
    // Windows keeps the same kind of files under %APPDATA% (Roaming).
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        let a = PathBuf::from(appdata);
        for d in ["gcloud", "FileZilla", "Mozilla", "Thunderbird"] {
            bases.push(a.join(d));
        }
        bases.push(a.join("Microsoft").join("Credentials"));
        bases.push(a.join("Microsoft").join("Protect"));
    }
    if !cfg!(windows) {
        for b in ["/etc", "/root", "/proc", "/sys", "/dev", "/var/run"] {
            bases.push(PathBuf::from(b));
        }
    }
    bases.iter().any(|base| {
        // `real` is canonical, so the base must be compared canonical too
        // (macOS: /etc -> /private/etc, /var -> /private/var; a symlinked
        // home). Compare both spellings; a missing base stays raw.
        path_under(real, base)
            || fs::canonicalize(base).is_ok_and(|c| path_under(real, &strip_verbatim(c)))
    })
}

/// Validate a doc-to-md path; Ok(real path) to hand to `stage_file`.
pub(crate) fn validate_doc_path(raw: &str, cwd: &Path, home: &Path) -> Result<PathBuf, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err("empty path".into());
    }
    // A cwd that came from canonicalize() on Windows is `\\?\C:\...`; that is
    // a plain drive path, not a device path, so normalise it before the check.
    let abs = strip_verbatim(cwd.join(t));
    // A UNC path bypasses the drive-letter sensitive-dir prefixes
    // (\\localhost\c$\Users\<u>\.ssh\notes.txt reads ~/.ssh).
    if path_is_unc(&abs) {
        return Err("UNC / device paths are not allowed".into());
    }
    let real = fs::canonicalize(&abs).map_err(|_| "file does not exist".to_string())?;
    let real = strip_verbatim(real);
    if path_is_unc(&real) {
        return Err("path resolves to a UNC / device path".into());
    }
    let md = fs::metadata(&real).map_err(|_| "cannot stat file".to_string())?;
    if !md.is_file() {
        return Err("not a regular file".into());
    }
    if md.len() > MAX_DOC_BYTES {
        return Err(format!(
            "file too large ({} bytes > {MAX_DOC_BYTES} limit)",
            md.len()
        ));
    }
    // Extension of the REAL path: a `notes.pdf` symlink to `id_rsa` has none.
    // ASCII lowercase only, so no Unicode case mapping can produce an
    // allowed extension from a non-ASCII one.
    let ext = real
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext {
        Some(e) if ALLOWED_DOC_EXT.contains(&e.as_str()) => {}
        Some(e) => return Err(format!("extension '.{e}' is not an allowed document type")),
        None => return Err("extension '(none)' is not an allowed document type".into()),
    }
    if is_sensitive_path(&real, home) {
        return Err("path resolves under a sensitive directory".into());
    }
    Ok(real)
}

// Open flags std does not name. Values are per-OS ABI constants; where we
// are not certain of them we pass 0 and rely on the (dev, ino) check alone.
#[cfg(target_os = "macos")]
const OPEN_FLAGS: i32 = 0x0004 /* O_NONBLOCK */ | 0x0100 /* O_NOFOLLOW */;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const OPEN_FLAGS: i32 = 0o4000 /* O_NONBLOCK */ | 0o400000 /* O_NOFOLLOW */;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const OPEN_FLAGS: i32 = 0o4000 /* O_NONBLOCK */ | 0o100000 /* O_NOFOLLOW */;
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))
))]
const OPEN_FLAGS: i32 = 0;

/// Open the source without following a final symlink and without blocking
/// on a FIFO swapped in after the lstat.
fn open_source(src: &Path) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(OPEN_FLAGS);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open a symlink/junction itself
        // instead of its target; we then see the reparse attribute and refuse.
        opts.custom_flags(0x0020_0000);
    }
    opts.open(src)
}

/// Is the opened handle the very file we lstat'ed?
#[cfg(unix)]
fn same_file(path_meta: &Metadata, handle_meta: &Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if path_meta.dev() == handle_meta.dev() && path_meta.ino() == handle_meta.ino() {
        Ok(())
    } else {
        Err("source was swapped (TOCTOU)".into())
    }
}

/// Windows std has no stable file-id accessor; the reparse-point open above
/// stops symlink swaps, and identical creation/write times and size make a
/// swap for a different regular file unlikely to go unnoticed.
#[cfg(windows)]
fn same_file(path_meta: &Metadata, handle_meta: &Metadata) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    if handle_meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("source became a symlink (TOCTOU)".into());
    }
    if path_meta.creation_time() == handle_meta.creation_time()
        && path_meta.last_write_time() == handle_meta.last_write_time()
        && path_meta.file_size() == handle_meta.file_size()
    {
        Ok(())
    } else {
        Err("source was swapped (TOCTOU)".into())
    }
}

/// TOCTOU-resistant copy of a validated document into agy's workspace.
///
/// The path was validated earlier; between then and now it may have been
/// replaced by a symlink to a secret. lstat refuses a symlink, the opened
/// handle must be that same inode, and all bytes come from the handle — the
/// path is never re-resolved. The destination is created fresh, never
/// followed or overwritten.
pub(crate) fn stage_file(src: &Path, dest: &Path) -> Result<(), String> {
    if src.as_os_str().is_empty() || dest.as_os_str().is_empty() {
        return Err("stage requires <src> <dest>".into());
    }
    let lmeta = fs::symlink_metadata(src).map_err(|_| "source file is gone".to_string())?;
    if lmeta.file_type().is_symlink() {
        return Err("source became a symlink (TOCTOU)".into());
    }
    if !lmeta.is_file() {
        return Err("source is not a regular file".into());
    }
    if lmeta.len() > MAX_DOC_BYTES {
        return Err("source too large".into());
    }
    let file = open_source(src).map_err(|e| format!("cannot open source: {e}"))?;
    let fmeta = file
        .metadata()
        .map_err(|e| format!("cannot stat source: {e}"))?;
    same_file(&lmeta, &fmeta)?;
    if !fmeta.is_file() {
        return Err("source was swapped (TOCTOU)".into());
    }

    // The file may grow after the stat: cap the read itself.
    let mut buf = Vec::new();
    file.take(MAX_DOC_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read source: {e}"))?;
    if buf.len() as u64 > MAX_DOC_BYTES {
        return Err("source too large".into());
    }

    let mut opts = OpenOptions::new();
    // create_new = O_CREAT|O_EXCL: fails on any existing entry, including a
    // (dangling) symlink planted at dest, instead of writing through it.
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut out = opts.open(dest).map_err(|e| format!("copy failed: {e}"))?;
    if let Err(e) = out.write_all(&buf) {
        drop(out);
        // We created it a moment ago; don't leave a truncated document.
        let _ = fs::remove_file(dest);
        return Err(format!("copy failed: {e}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn more_credential_stores_are_sensitive() {
        let home = Path::new("/home/u");
        for rel in [
            ".m2/settings.xml",
            ".purple/accounts.xml",
            ".config/filezilla/sitemanager.xml",
            ".password-store/x.txt",
            ".mozilla/firefox/p/logins.json",
        ] {
            assert!(is_sensitive_path(&home.join(rel), home), "{rel}");
        }
        assert!(!is_sensitive_path(&home.join("projects/spec.md"), home));
    }

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---- SSRF -----------------------------------------------------------

    fn ok(url: &str) -> String {
        match validate_scrape_url(url) {
            Ok(u) => u,
            Err(e) => panic!("expected {url:?} to pass, got {e}"),
        }
    }

    fn err(url: &str) -> String {
        match validate_scrape_url(url) {
            Ok(u) => panic!("expected {url:?} to be rejected, got {u}"),
            Err(e) => e,
        }
    }

    fn blocked(url: &str) {
        let e = err(url);
        assert!(e.contains("SSRF guard"), "{url:?}: {e}");
    }

    #[test]
    fn public_urls_pass_normalized() {
        assert_eq!(
            ok("https://example.com/page?q=1"),
            "https://example.com/page?q=1"
        );
        assert_eq!(ok("http://example.com"), "http://example.com/");
        assert_eq!(ok("  https://example.com  "), "https://example.com/");
        assert_eq!(ok("HTTP://EXAMPLE.COM/Path"), "http://example.com/Path");
        assert_eq!(ok("HtTpS://Example.Com."), "https://example.com/");
        assert_eq!(
            ok("http://example.com..:8080/x#frag"),
            "http://example.com:8080/x"
        );
        assert_eq!(ok("http://example.com?q"), "http://example.com/?q");
        assert_eq!(ok("http://example.com#x"), "http://example.com/");
        assert_eq!(ok("http://example.com:0080/"), "http://example.com:80/");
        assert_eq!(
            ok("https://example.com:65535"),
            "https://example.com:65535/"
        );
        assert_eq!(
            ok("http://example.com/@127.0.0.1"),
            "http://example.com/@127.0.0.1"
        );
        assert_eq!(
            ok("http://example.com:80#@127.0.0.1"),
            "http://example.com:80/"
        );
        assert_eq!(
            ok("http://example.com/a?u=http://127.0.0.1/"),
            "http://example.com/a?u=http://127.0.0.1/"
        );
        for h in [
            "sub.example.co.uk",
            "github.com",
            "01.example.com",
            "1.example.com",
            "123.456.example.com",
            "a-b-c.example.com",
            "xn--nxasmq6b.com",
            "xn--bcher-kva.example",
            "xn--fa-hia.de",
            "a--1.example.com",
            "notlvh.me",
            "my_host.example.com",
            "8.8.8.8.nip.io",
            "foo.8.8.8.8.nip.io",
            "8-8-8-8.nip.io",
            "08080808.nip.io",
            "deadbeef.example.com",
            "2606-4700--1111.sslip.io",
            "1.2.3.example.com",
            "300.0.0.1.example.com",
        ] {
            assert_eq!(ok(&format!("https://{h}/")), format!("https://{h}/"));
        }
    }

    #[test]
    fn public_ip_literals_pass_and_are_canonical() {
        assert_eq!(ok("http://8.8.8.8/"), "http://8.8.8.8/");
        assert_eq!(ok("http://134744072/"), "http://8.8.8.8/");
        assert_eq!(ok("http://0x08080808/"), "http://8.8.8.8/");
        assert_eq!(ok("http://8.8.2056/"), "http://8.8.8.8/");
        assert_eq!(ok("http://010.010.010.010/"), "http://8.8.8.8/");
        assert_eq!(
            ok("http://[2001:4860:4860::8888]/"),
            "http://[2001:4860:4860::8888]/"
        );
        assert_eq!(
            ok("http://[2001:4860:4860:0:0:0:0:8888]:443/x"),
            "http://[2001:4860:4860::8888]:443/x"
        );
        assert_eq!(ok("http://[::FFFF:8.8.8.8]/"), "http://[::ffff:8.8.8.8]/");
        ok("http://[64:ff9b::808:808]/");
        ok("http://[2002:808:808::1]/");
        ok("http://[2606:4700::1111]/");
        ok("http://1.1.1.1/");
        for h in [
            "172.15.0.1",
            "172.32.0.1",
            "192.169.0.1",
            "11.0.0.1",
            "8.8.4.4",
            "100.128.0.1",
            "198.20.0.1",
            "223.255.255.255",
        ] {
            assert!(!is_blocked_host(h), "{h}");
        }
    }

    #[test]
    fn numeric_ipv4_encodings_blocked() {
        for u in [
            "http://127.0.0.1/",
            "http://2130706433/",
            "http://0x7f.1/",
            "http://0X7F.1/",
            "http://0177.0.0.1/",
            "http://127.1/",
            "http://127.0.1/",
            "http://0x7f000001/",
            "http://017700000001/",
            "http://0x7f.0x0.0x0.0x1/",
            "http://127.000.000.001/",
            "http://127.0.0.1./",
            "http://127.0.0.1../",
            "http://0/",
            "http://0.0.0.0/",
            "http://0x/",
            "http://10.1/",
            "http://192.168.1/",
            "http://169.254.169.254/latest/meta-data/",
            "https://0xa9fea9fe/",
            "http://2852039166/",
            "http://100.64.0.1/",
            "http://198.18.0.1/",
            "http://224.0.0.1/",
            "http://255.255.255.255/",
            "http://4294967295/",
            "http://4294967296/",
            "http://99999999999999999999999999/",
            "http://08.1.1.1/",
            "http://1.2.3.4.5/",
            "http://1..1/",
            "http://256.1.1.1/",
            "http://1.1.1.256.0/",
            "http://example.0x/",
            "http://example.123/",
            "http://192.0.0.8/",
            "http://172.16.0.1:443/",
        ] {
            blocked(u);
        }
        for h in [
            "127.0.0.1",
            "127.1.2.3",
            "0.0.0.0",
            "10.0.0.5",
            "192.168.1.10",
            "172.16.0.1",
            "172.31.255.255",
            "169.254.169.254",
            "100.64.0.1",
        ] {
            assert!(is_blocked_host(h), "{h}");
        }
        assert_eq!(
            err("http://2130706433/"),
            "host '2130706433' is blocked (localhost / private / link-local \u{2014} SSRF guard)"
        );
    }

    #[test]
    fn ipv6_forms_blocked() {
        for u in [
            "http://[::1]/",
            "http://[::1]:9000/",
            "http://[::]/",
            "http://[0:0:0:0:0:0:0:1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:7f00:1]/",
            "http://[::FFFF:7F00:1]/",
            "http://[::127.0.0.1]/",
            "http://[::7f00:1]/",
            "http://[::ffff:a9fe:a9fe]/",
            "http://[::ffff:10.0.0.1]/",
            "http://[::ffff:0:7f00:1]/",
            "http://[64:ff9b::7f00:1]/",
            "http://[64:ff9b::127.0.0.1]/",
            "http://[64:ff9b::a9fe:a9fe]/",
            "http://[64:ff9b:1::808:808]/",
            "http://[2002:7f00:1::]/",
            "http://[2002:a9fe:a9fe::1]/",
            "http://[fe80::1]/",
            "http://[febf::1]/",
            "http://[fec0::1]/",
            "http://[fc00::1]/",
            "http://[fd12:3456::1]/",
            "http://[ff02::1]/",
        ] {
            blocked(u);
        }
        for h in [
            "::1",
            "[::1]",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "::ffff:127.0.0.1",
            "::ffff:7f00:1",
            "::7f00:1",
            "::127.0.0.1",
            "::ffff:0a00:1",
            "::ffff:192.168.1.1",
            "fe80::1%eth0",
            "[::1",
            "::1]",
            ":::1",
            "[]",
        ] {
            assert!(is_blocked_host(h), "{h}");
        }
        assert!(!is_blocked_host("::ffff:8.8.8.8"));
        assert!(!is_blocked_host("[2001:4860:4860::8888]"));
    }

    #[test]
    fn malformed_ipv6_rejected() {
        for u in [
            "http://[fe80::1%25eth0]/",
            "http://[fe80::1%eth0]/",
            "http://[::1/",
            "http://[::1]x/",
            "http://[::1]:/",
            "http://[example.com]/",
            "http://[::ffff:0127.0.0.1]/",
            "http://[1:2:3:4:5:6:7:8:9]/",
        ] {
            assert!(validate_scrape_url(u).is_err(), "{u}");
        }
        assert_eq!(err("http://[fe80::1%25eth0]/"), INVALID_URL);
    }

    #[test]
    fn userinfo_rejected() {
        for u in [
            "http://example.com@127.0.0.1/",
            "http://user:pass@example.com/",
            "http://@example.com/",
            "http://127.0.0.1:80@example.com/",
            "http://a@b@example.com/",
        ] {
            assert_eq!(err(u), "credentials in URLs are not allowed", "{u}");
        }
        blocked("http://127.0.0.1#@example.com");
        blocked("http://127.0.0.1?@example.com");
    }

    #[test]
    fn bad_characters_rejected() {
        for u in [
            "http://example.com\\@127.0.0.1/",
            "http:\\\\127.0.0.1\\",
            "http://127.0.0.1\\.example.com/",
            "http://exa mple.com/",
            "http://127.0.0.1\t/",
            "http://exam\nple.com/",
            "http://exam\rple.com/",
            "http://example.com/\u{0}",
            "http://example.com/\u{7f}",
            "http://\u{ff4c}\u{ff4f}\u{ff43}\u{ff41}\u{ff4c}\u{ff48}\u{ff4f}\u{ff53}\u{ff54}/",
            "http://ex\u{e4}mple.com/",
            "http://127\u{3002}0\u{3002}0\u{3002}1/",
            "not a url",
        ] {
            assert_eq!(err(u), BAD_URL_CHARS, "{u:?}");
        }
    }

    #[test]
    fn schemes_and_structure() {
        assert_eq!(err(""), "empty URL");
        assert_eq!(err("   "), "empty URL");
        assert_eq!(
            err("file:///etc/passwd"),
            "protocol 'file:' is not allowed (only http/https)"
        );
        assert_eq!(
            err("gopher://example.com"),
            "protocol 'gopher:' is not allowed (only http/https)"
        );
        assert_eq!(
            err("FTP://example.com/x"),
            "protocol 'ftp:' is not allowed (only http/https)"
        );
        err("httpx://example.com/");
        err("ws://example.com/");
        for u in [
            "data:text/html,<h1>x</h1>",
            "javascript:alert(1)",
            "example.com",
            "//example.com/",
            "127.0.0.1",
            "://example.com",
            "1http://example.com",
            "http:/example.com",
            "http:///example.com",
            "http://",
            "http://?x",
            "http://#x",
            "http://example.com:/",
            "http://example.com:0/",
            "http://example.com:65536/",
            "http://example.com:99999999999999999999/",
            "http://example.com:80a/",
            "http://example.com:-1/",
            "http://example.com:+80/",
            "http://example.com:80:80/",
            "http://%6c%6f%63%61%6c%68%6f%73%74/",
            "http://127.0.0.%31/",
            "http://exa<mple.com/",
            "http://exa|mple.com/",
            "http://exa\"mple.com/",
            "http://exa'mple.com/",
            "http://exa^mple.com/",
            "http://exa*mple.com/",
        ] {
            assert_eq!(err(u), INVALID_URL, "{u}");
        }
        // Scheme-looking text later in the URL does not count.
        assert_eq!(err("javascript:alert(1)//http://example.com"), INVALID_URL);
    }

    #[test]
    fn named_hosts_blocked() {
        for h in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "localhost..",
            "api.localhost",
            "foo.local",
            "svc.internal",
            "metadata.google.internal",
            "box.lan",
            "nas.home",
            "x.corp",
            "printer.home.arpa",
            "localhost.localdomain",
            "metadata",
            "intranet",
            "ip6-localhost",
            "com",
            ".",
            "",
        ] {
            assert!(is_blocked_host(h), "{h:?}");
        }
        blocked("http://localhost:8080/admin");
        blocked("http://LocalHost./");
        blocked("http://metadata/computeMetadata/v1/");
        blocked("http://./");
        let long_label = format!("{}.example.com", "a".repeat(64));
        assert!(is_blocked_host(&long_label));
        let ok_label = format!("{}.example.com", "a".repeat(63));
        assert!(!is_blocked_host(&ok_label));
        let long_host = format!("{}example.com", "abcdefghi.".repeat(25));
        assert!(long_host.len() > 253);
        assert!(is_blocked_host(&long_host));
    }

    #[test]
    fn embedded_address_names_blocked() {
        for h in [
            "127.0.0.1.nip.io",
            "10.0.0.1.sslip.io",
            "foo.127.0.0.1.nip.io",
            "a.b.10.0.0.1.sslip.io",
            "x.169.254.169.254.nip.io",
            "1.2.3.4.127.0.0.1.nip.io",
            "8.8.8.8.192.168.0.1.nip.io",
            "127.000.000.001.nip.io",
            "127.0.0.1.example.com",
            "127-0-0-1.nip.io",
            "app-127-0-0-1.sslip.io",
            "127-0-0-1-app.sslip.io",
            "a-8-8-8-8-10-0-0-1.sslip.io",
            "x.app-169-254-169-254.nip.io",
            "7f000001.nip.io",
            "7F000001.NIP.IO",
            "app-7f000001.nip.io",
            "0a000001.nip.io",
            "a9fea9fe.sslip.io",
            "--1.sslip.io",
            "app---1.sslip.io",
            "fe80--1.sslip.io",
            "--ffff-7f00-1.sslip.io",
            "fd00--1.sslip.io",
            "e0000001.example.com",
            "lvh.me",
            "foo.lvh.me",
            "app.localtest.me",
            "x.vcap.me",
            "a.localhost.direct",
        ] {
            assert!(is_blocked_host(h), "{h}");
            blocked(&format!("http://{h}/x"));
        }
    }

    #[test]
    fn whatwg_ipv4_parser() {
        assert_eq!(parse_ipv4_whatwg("127.1"), Some(0x7f00_0001));
        assert_eq!(parse_ipv4_whatwg("0x7f.1"), Some(0x7f00_0001));
        assert_eq!(parse_ipv4_whatwg("0177.0.0.1"), Some(0x7f00_0001));
        assert_eq!(parse_ipv4_whatwg("1.2.65535"), Some(0x0102_ffff));
        assert_eq!(parse_ipv4_whatwg("1.2.65536"), None);
        assert_eq!(parse_ipv4_whatwg("4294967295"), Some(u32::MAX));
        assert_eq!(parse_ipv4_whatwg("4294967296"), None);
        assert_eq!(parse_ipv4_whatwg("09"), None);
        assert_eq!(parse_ipv4_whatwg("0x"), Some(0));
        assert_eq!(parse_ipv4_whatwg("0xg"), None);
        assert_eq!(parse_ipv4_whatwg("1.2.3.4.5"), None);
        assert_eq!(parse_ipv4_whatwg("a.1"), None);
        assert_eq!(parse_ipv4_whatwg(""), None);
        assert_eq!(parse_ipv4_whatwg("0xffffffffffffffffffffff"), None);
        assert!(ends_in_number("a.0x"));
        assert!(ends_in_number("a.1"));
        assert!(!ends_in_number("a.1b"));
        assert!(!ends_in_number("a.0xg"));
    }

    #[test]
    fn hostile_url_inputs_do_not_panic() {
        let long = format!("http://{}/", "1-".repeat(5000));
        let _ = validate_scrape_url(&long);
        let _ = validate_scrape_url(&format!("http://{}/", "-".repeat(63)));
        let _ = validate_scrape_url(&format!("http://{}.com/", "a-".repeat(31)));
        let _ = validate_scrape_url("http://[");
        let _ = validate_scrape_url("http://]");
        let _ = validate_scrape_url("http://[]:80/");
        let _ = validate_scrape_url("http://:80/");
        let _ = validate_scrape_url("://");
        let _ = is_blocked_host(&"0x".repeat(1000));
        let _ = is_blocked_host(&"9.".repeat(200));
    }

    // ---- paths ----------------------------------------------------------

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> TempDir {
            static N: AtomicUsize = AtomicUsize::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!(
                "agy-guard-{tag}-{}-{nanos}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&p).unwrap();
            TempDir(strip_verbatim(fs::canonicalize(&p).unwrap()))
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(p: &Path, s: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, s).unwrap();
    }

    #[test]
    fn unc_detection() {
        assert!(is_unc_path("\\\\localhost\\c$\\Users\\u\\.ssh\\id_rsa"));
        assert!(is_unc_path("\\\\?\\C:\\x"));
        assert!(is_unc_path("\\\\.\\pipe\\x"));
        assert!(is_unc_path("//server/share/x"));
        assert!(is_unc_path("/\\server"));
        assert!(!is_unc_path("C:\\Users\\u\\doc.pdf"));
        assert!(!is_unc_path("/home/u/doc.pdf"));
        assert!(!is_unc_path("doc.pdf"));
        assert!(!is_unc_path("/"));
        assert!(!is_unc_path(""));
    }

    #[test]
    fn verbatim_prefix_handling() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\Users\u\doc.pdf")),
            PathBuf::from(r"C:\Users\u\doc.pdf")
        );
        let unc = strip_verbatim(PathBuf::from(r"\\?\UNC\server\share\doc.pdf"));
        assert!(path_is_unc(&unc));
        let vol = strip_verbatim(PathBuf::from(r"\\?\Volume{abc}\doc.pdf"));
        assert!(path_is_unc(&vol));
        assert_eq!(strip_verbatim(PathBuf::from("/a/b")), PathBuf::from("/a/b"));
    }

    #[test]
    fn sensitive_paths() {
        let home = Path::new("/home/u");
        for rel in [
            ".ssh/id_rsa",
            ".gemini/x.txt",
            ".aws/credentials",
            ".config/gcloud/a.json",
            ".gnupg/x",
            ".kube/config",
            ".docker/config.json",
            ".ssh",
        ] {
            assert!(is_sensitive_path(&home.join(rel), home), "{rel}");
        }
        for rel in [
            "projects/doc.pdf",
            ".sshx/a.md",
            ".config/other/a.md",
            "ssh/a.md",
        ] {
            assert!(!is_sensitive_path(&home.join(rel), home), "{rel}");
        }
        #[cfg(unix)]
        {
            for p in [
                "/etc/passwd",
                "/etc",
                "/root/a.md",
                "/proc/self/environ",
                "/sys/x",
                "/dev/stdin",
                "/var/run/docker.sock",
            ] {
                assert!(is_sensitive_path(Path::new(p), home), "{p}");
            }
            assert!(!is_sensitive_path(Path::new("/etcetera/x.md"), home));
            assert!(!is_sensitive_path(Path::new("/var/runner/x.md"), home));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_private_etc_and_case() {
        let home = Path::new("/Users/u");
        assert!(is_sensitive_path(Path::new("/private/etc/hosts"), home));
        assert!(is_sensitive_path(Path::new("/private/var/run/x.txt"), home));
        assert!(is_sensitive_path(Path::new("/Users/u/.SSH/notes.md"), home));
        assert!(is_sensitive_path(Path::new("/USERS/U/.Aws/x.txt"), home));
        assert!(is_sensitive_path(Path::new("/ETC/hosts"), home));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_is_case_sensitive() {
        let home = Path::new("/home/u");
        assert!(!is_sensitive_path(Path::new("/home/u/.SSH/notes.md"), home));
    }

    #[test]
    fn doc_accepts_allowed_types() {
        let t = TempDir::new("doc-ok");
        write(&t.path().join("notes.md"), "# hi\n");
        write(&t.path().join("sub/REPORT.PDF"), "%PDF");
        let home = t.path().join("home");
        assert_eq!(
            validate_doc_path("notes.md", t.path(), &home),
            Ok(t.path().join("notes.md"))
        );
        assert_eq!(
            validate_doc_path("  notes.md \n", t.path(), &home),
            Ok(t.path().join("notes.md"))
        );
        assert_eq!(
            validate_doc_path("sub/../sub/REPORT.PDF", t.path(), &home),
            Ok(t.path().join("sub").join("REPORT.PDF"))
        );
        let abs = t.path().join("notes.md");
        assert!(
            validate_doc_path(&abs.to_string_lossy(), Path::new("/nonexistent"), &home).is_ok()
        );
        for ext in ALLOWED_DOC_EXT {
            let f = t.path().join(format!("f.{ext}"));
            write(&f, "x");
            assert!(
                validate_doc_path(&f.to_string_lossy(), t.path(), &home).is_ok(),
                "{ext}"
            );
        }
    }

    #[test]
    fn doc_rejections() {
        let t = TempDir::new("doc-bad");
        let home = t.path().join("home");
        let cwd = t.path();
        write(&cwd.join("secrets.env"), "API_KEY=abc\n");
        write(&cwd.join(".env"), "API_KEY=abc\n");
        write(&cwd.join("id_rsa"), "key");
        write(&cwd.join("x.pdf.pem"), "key");
        write(&cwd.join("trail."), "x");
        fs::create_dir_all(cwd.join("sub.md")).unwrap();
        assert_eq!(validate_doc_path("", cwd, &home), Err("empty path".into()));
        assert_eq!(
            validate_doc_path(" \t\n", cwd, &home),
            Err("empty path".into())
        );
        assert_eq!(
            validate_doc_path("secrets.env", cwd, &home),
            Err("extension '.env' is not an allowed document type".into())
        );
        assert_eq!(
            validate_doc_path(".env", cwd, &home),
            Err("extension '(none)' is not an allowed document type".into())
        );
        assert_eq!(
            validate_doc_path("id_rsa", cwd, &home),
            Err("extension '(none)' is not an allowed document type".into())
        );
        assert_eq!(
            validate_doc_path("x.pdf.pem", cwd, &home),
            Err("extension '.pem' is not an allowed document type".into())
        );
        // Windows silently strips a trailing dot from file names ("trail."
        // is "trail"), so the extension is gone there.
        let trail = if cfg!(windows) { "(none)" } else { "." };
        assert_eq!(
            validate_doc_path("trail.", cwd, &home),
            Err(format!(
                "extension '{trail}' is not an allowed document type"
            ))
        );
        assert_eq!(
            validate_doc_path("nope.pdf", cwd, &home),
            Err("file does not exist".into())
        );
        assert_eq!(
            validate_doc_path("sub.md", cwd, &home),
            Err("not a regular file".into())
        );
        assert_eq!(
            validate_doc_path("//server/share/x.pdf", cwd, &home),
            Err("UNC / device paths are not allowed".into())
        );
    }

    #[test]
    fn doc_size_cap() {
        let t = TempDir::new("doc-size");
        let home = t.path().join("home");
        let at_cap = File::create(t.path().join("cap.txt")).unwrap();
        at_cap.set_len(MAX_DOC_BYTES).unwrap();
        let over = File::create(t.path().join("big.txt")).unwrap();
        over.set_len(MAX_DOC_BYTES + 1).unwrap();
        assert!(validate_doc_path("cap.txt", t.path(), &home).is_ok());
        assert_eq!(
            validate_doc_path("big.txt", t.path(), &home),
            Err("file too large (10485761 bytes > 10485760 limit)".into())
        );
    }

    #[test]
    fn doc_under_sensitive_home_dir() {
        let t = TempDir::new("doc-sens");
        let home = t.path().join("home");
        let f = home.join(".ssh").join("notes.md");
        write(&f, "secret");
        assert_eq!(
            validate_doc_path(&f.to_string_lossy(), t.path(), &home),
            Err("path resolves under a sensitive directory".into())
        );
        write(&home.join(".config/gcloud/notes.txt"), "x");
        assert_eq!(
            validate_doc_path(".config/gcloud/notes.txt", &home, &home),
            Err("path resolves under a sensitive directory".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn doc_symlink_attacks() {
        use std::os::unix::fs::symlink;
        let t = TempDir::new("doc-link");
        let home = t.path().join("home");
        let cwd = t.path().join("proj");
        fs::create_dir_all(&cwd).unwrap();
        write(&home.join(".ssh/notes.md"), "secret");
        write(&home.join(".ssh/id_rsa"), "key");
        // Allowed-looking name pointing into ~/.ssh.
        symlink(home.join(".ssh/notes.md"), cwd.join("doc.md")).unwrap();
        assert_eq!(
            validate_doc_path("doc.md", &cwd, &home),
            Err("path resolves under a sensitive directory".into())
        );
        // `.pdf` link to an extension-less key: real path decides.
        symlink(home.join(".ssh/id_rsa"), cwd.join("doc.pdf")).unwrap();
        assert_eq!(
            validate_doc_path("doc.pdf", &cwd, &home),
            Err("extension '(none)' is not an allowed document type".into())
        );
        // Symlinked directory into ~/.ssh.
        symlink(home.join(".ssh"), cwd.join("docs")).unwrap();
        assert_eq!(
            validate_doc_path("docs/notes.md", &cwd, &home),
            Err("path resolves under a sensitive directory".into())
        );
        // Link to /etc (on macOS /private/etc) with an allowed name.
        symlink("/etc", cwd.join("etc-link")).unwrap();
        if let Ok(entries) = fs::read_dir("/etc") {
            let doc = entries.filter_map(|e| e.ok()).find(|e| {
                e.path().is_file()
                    && e.path()
                        .extension()
                        .is_some_and(|x| ALLOWED_DOC_EXT.contains(&x.to_string_lossy().as_ref()))
                    && File::open(e.path()).is_ok()
            });
            if let Some(d) = doc {
                let rel = format!("etc-link/{}", d.file_name().to_string_lossy());
                assert_eq!(
                    validate_doc_path(&rel, &cwd, &home),
                    Err("path resolves under a sensitive directory".into())
                );
            }
        }
        // Dangling link.
        symlink(cwd.join("missing.md"), cwd.join("dangling.md")).unwrap();
        assert_eq!(
            validate_doc_path("dangling.md", &cwd, &home),
            Err("file does not exist".into())
        );
        // Link loop.
        symlink(cwd.join("loop2.md"), cwd.join("loop1.md")).unwrap();
        symlink(cwd.join("loop1.md"), cwd.join("loop2.md")).unwrap();
        assert_eq!(
            validate_doc_path("loop1.md", &cwd, &home),
            Err("file does not exist".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_home_is_still_sensitive() {
        // Same shape as macOS /etc -> /private/etc: the file path is the
        // realpath, the base is given via a symlink.
        use std::os::unix::fs::symlink;
        let t = TempDir::new("symhome");
        let real_home = t.path().join("real-home");
        let link_home = t.path().join("link-home");
        fs::create_dir_all(real_home.join(".ssh")).unwrap();
        symlink(&real_home, &link_home).unwrap();
        let real_file = fs::canonicalize(&real_home)
            .unwrap()
            .join(".ssh")
            .join("notes.md");
        assert!(is_sensitive_path(&real_file, &link_home));
        write(&real_file, "x");
        assert_eq!(
            validate_doc_path(
                &link_home.join(".ssh/notes.md").to_string_lossy(),
                t.path(),
                &link_home
            ),
            Err("path resolves under a sensitive directory".into())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_case_trick_on_real_fs() {
        let t = TempDir::new("case");
        let home = t.path().join("home");
        write(&home.join(".ssh/notes.md"), "secret");
        // Default APFS is case-insensitive: .SSH resolves to the same dir.
        let probe = home.join(".SSH/notes.md");
        if probe.exists() {
            let r = validate_doc_path(&probe.to_string_lossy(), t.path(), &home);
            assert_eq!(r, Err("path resolves under a sensitive directory".into()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn doc_fifo_is_not_a_regular_file() {
        let t = TempDir::new("fifo");
        let fifo = t.path().join("pipe.md");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if made.is_ok_and(|s| s.success()) {
            assert_eq!(
                validate_doc_path("pipe.md", t.path(), &t.path().join("home")),
                Err("not a regular file".into())
            );
            assert_eq!(
                stage_file(&fifo, &t.path().join("out.md")),
                Err("source is not a regular file".into())
            );
        }
    }

    // ---- staging --------------------------------------------------------

    #[test]
    fn stage_copies_regular_file() {
        let t = TempDir::new("stage");
        let src = t.path().join("src.md");
        write(&src, "hello");
        let dest = t.path().join("document.md");
        assert_eq!(stage_file(&src, &dest), Ok(()));
        assert_eq!(fs::read_to_string(&dest).unwrap(), "hello");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0);
        }
        let empty = t.path().join("empty.md");
        write(&empty, "");
        assert_eq!(stage_file(&empty, &t.path().join("e2.md")), Ok(()));
    }

    #[test]
    fn stage_rejects_missing_and_bad_args() {
        let t = TempDir::new("stage-miss");
        assert_eq!(
            stage_file(&t.path().join("nope"), &t.path().join("d")),
            Err("source file is gone".into())
        );
        assert_eq!(
            stage_file(Path::new(""), Path::new("")),
            Err("stage requires <src> <dest>".into())
        );
        assert_eq!(
            stage_file(t.path(), &t.path().join("d")),
            Err("source is not a regular file".into())
        );
    }

    #[test]
    fn stage_never_overwrites_dest() {
        let t = TempDir::new("stage-dest");
        let src = t.path().join("src.md");
        write(&src, "new");
        let dest = t.path().join("dest.md");
        write(&dest, "old");
        let e = stage_file(&src, &dest).unwrap_err();
        assert!(e.starts_with("copy failed: "), "{e}");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "old");
    }

    #[cfg(unix)]
    #[test]
    fn stage_never_follows_dest_symlink() {
        use std::os::unix::fs::symlink;
        let t = TempDir::new("stage-destlink");
        let src = t.path().join("src.md");
        write(&src, "payload");
        let victim = t.path().join("victim.txt");
        write(&victim, "keep");
        let d1 = t.path().join("d1.md");
        symlink(&victim, &d1).unwrap();
        assert!(
            stage_file(&src, &d1)
                .unwrap_err()
                .starts_with("copy failed: ")
        );
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        // Dangling link: must not create its target.
        let target = t.path().join("created-through-link.txt");
        let d2 = t.path().join("d2.md");
        symlink(&target, &d2).unwrap();
        assert!(
            stage_file(&src, &d2)
                .unwrap_err()
                .starts_with("copy failed: ")
        );
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stage_rejects_symlink_source() {
        use std::os::unix::fs::symlink;
        let t = TempDir::new("stage-link");
        let target = t.path().join("secret");
        write(&target, "secret");
        let link = t.path().join("link.md");
        symlink(&target, &link).unwrap();
        assert_eq!(
            stage_file(&link, &t.path().join("out.md")),
            Err("source became a symlink (TOCTOU)".into())
        );
        assert!(!t.path().join("out.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn stage_detects_swap_after_validation() {
        use std::os::unix::fs::symlink;
        let t = TempDir::new("stage-swap");
        let home = t.path().join("home");
        write(&home.join(".ssh/id_rsa"), "PRIVATE");
        let doc = t.path().join("doc.md");
        write(&doc, "fine");
        let real = validate_doc_path("doc.md", t.path(), &home).unwrap();
        // Attacker swaps the validated file for a symlink to a secret.
        fs::remove_file(&real).unwrap();
        symlink(home.join(".ssh/id_rsa"), &real).unwrap();
        let out = t.path().join("staged.md");
        assert_eq!(
            stage_file(&real, &out),
            Err("source became a symlink (TOCTOU)".into())
        );
        assert!(!out.exists());
        // ...or for a directory.
        fs::remove_file(&real).unwrap();
        fs::create_dir(&real).unwrap();
        assert_eq!(
            stage_file(&real, &out),
            Err("source is not a regular file".into())
        );
        // ...or removes it.
        fs::remove_dir(&real).unwrap();
        assert_eq!(stage_file(&real, &out), Err("source file is gone".into()));
    }

    #[cfg(unix)]
    #[test]
    fn same_file_check_detects_different_inode() {
        let t = TempDir::new("inode");
        let a = t.path().join("a.md");
        let b = t.path().join("b.md");
        write(&a, "a");
        write(&b, "b");
        let ma = fs::symlink_metadata(&a).unwrap();
        let mb = File::open(&b).unwrap().metadata().unwrap();
        assert_eq!(
            same_file(&ma, &mb),
            Err("source was swapped (TOCTOU)".into())
        );
        let ma2 = File::open(&a).unwrap().metadata().unwrap();
        assert_eq!(same_file(&ma, &ma2), Ok(()));
    }

    #[test]
    fn stage_size_cap() {
        let t = TempDir::new("stage-big");
        let src = t.path().join("big.txt");
        File::create(&src)
            .unwrap()
            .set_len(MAX_DOC_BYTES + 1)
            .unwrap();
        let dest = t.path().join("out.txt");
        assert_eq!(stage_file(&src, &dest), Err("source too large".into()));
        assert!(!dest.exists());
        let cap = t.path().join("cap.txt");
        File::create(&cap).unwrap().set_len(MAX_DOC_BYTES).unwrap();
        assert_eq!(stage_file(&cap, &t.path().join("cap-out.txt")), Ok(()));
        assert_eq!(
            fs::metadata(t.path().join("cap-out.txt")).unwrap().len(),
            MAX_DOC_BYTES
        );
    }
}

//! Best-effort secret detection for material we are about to ship to Gemini
//! (review diffs, full contents of changed files).
//!
//! Not a replacement for gitleaks/trufflehog: a small, conservative pattern
//! set that keeps false positives low. The old Node/Bash code used
//! case-insensitive regexes; there is no regex crate here, so every pattern is
//! a hand-written matcher with exactly the semantics of the JS regex (`/i`,
//! unanchored search). Equivalence was checked against V8 with a fuzz corpus;
//! the unit tests keep a fixed representative subset.

/// Labels in table order. `scan_text` reports hits in this order.
const LABELS: [&str; 10] = [
    "AWS access key",
    "AWS STS token",
    "GitHub personal access token",
    "GitHub fine-grained PAT",
    "Slack token",
    "Anthropic API key",
    "OpenAI project key",
    "OpenAI legacy API key",
    "PEM private key block",
    "inline credential assignment",
];

/// Scan raw text. Returns the labels of every pattern that matches anywhere,
/// in table order, each at most once.
pub(crate) fn scan_text(text: &str) -> Vec<&'static str> {
    labels(scan_mask(text))
}

/// Scan only the ADDED lines of a unified diff.
///
/// A line starting with `+` is added content, except the `+++ ` file header,
/// recognised as a `+++ ` line directly after a `--- ` line. (The old code
/// dropped every `+++` line, which also hid added content starting with `++`.)
/// Inside a hunk whose `@@` header counts we can follow, a `+++ ` line is
/// content even after a `--- ` line (a removed `-- x` next to an added
/// `++ y`); we scan that reading too and report the union, so we never find
/// less than the plain header rule would.
#[cfg_attr(not(test), allow(dead_code))] // review scans the whole diff (scan_text); kept for its tests
pub(crate) fn scan_diff(diff: &str) -> Vec<&'static str> {
    let mut by_rule: Vec<&str> = Vec::new();
    let mut by_hunk: Vec<&str> = Vec::new();
    let mut differs = false;
    let mut prev: &str = "";
    // Lines still expected in the current hunk (old side, new side).
    let mut old_left: u64 = 0;
    let mut new_left: u64 = 0;

    for line in diff.split('\n') {
        let mut in_hunk = false;
        if old_left > 0 || new_left > 0 {
            match line.as_bytes().first() {
                // Some tools strip the single space of an empty context line.
                None | Some(b' ') => {
                    in_hunk = true;
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
                Some(b'-') => {
                    in_hunk = true;
                    old_left = old_left.saturating_sub(1);
                }
                Some(b'+') => {
                    in_hunk = true;
                    new_left = new_left.saturating_sub(1);
                }
                Some(b'\\') => in_hunk = true, // "\ No newline at end of file"
                Some(_) => {
                    // Counts did not add up: stop trusting them.
                    old_left = 0;
                    new_left = 0;
                }
            }
        }
        if !in_hunk && let Some((o, n)) = parse_hunk_header(line) {
            old_left = o;
            new_left = n;
        }

        if let Some(content) = line.strip_prefix('+') {
            let is_header = line.starts_with("+++ ") && prev.starts_with("--- ");
            if !is_header {
                by_rule.push(content);
            }
            if !is_header || in_hunk {
                by_hunk.push(content);
            }
            if is_header && in_hunk {
                differs = true;
            }
        }
        prev = line;
    }

    let mut mask = if by_rule.is_empty() {
        0
    } else {
        scan_mask(&by_rule.join("\n"))
    };
    if differs {
        mask |= scan_mask(&by_hunk.join("\n"));
    }
    labels(mask)
}

/// `@@ -a[,b] +c[,d] @@...` -> (b, d); a missing count means 1.
fn parse_hunk_header(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(' ')?;
    let rest = rest.strip_prefix('+')?;
    let (new, rest) = rest.split_once(' ')?;
    if !rest.starts_with("@@") {
        return None;
    }
    let count = |r: &str| -> Option<u64> {
        match r.split_once(',') {
            Some((start, n)) => {
                start.parse::<u64>().ok()?;
                n.parse::<u64>().ok()
            }
            None => r.parse::<u64>().ok().map(|_| 1),
        }
    };
    Some((count(old)?, count(new)?))
}

fn labels(mask: u16) -> Vec<&'static str> {
    LABELS
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, l)| *l)
        .collect()
}

fn scan_mask(text: &str) -> u16 {
    if text.is_empty() {
        return 0;
    }
    let t = text.as_bytes();
    let checks: [bool; 10] = [
        // AKIA[0-9A-Z]{16}  (/i also lets the class match lowercase)
        prefixed_run(t, &[b"a", b"k", b"i", b"a"], is_alnum, 16),
        // ASIA[0-9A-Z]{16}
        prefixed_run(t, &[b"a", b"s", b"i", b"a"], is_alnum, 16),
        // gh[pousr]_[A-Za-z0-9]{36,}
        prefixed_run(t, &[b"g", b"h", b"pousr", b"_"], is_alnum, 36),
        // github_pat_[A-Za-z0-9_]{40,}
        prefixed_run(t, &atoms(b"github_pat_"), is_alnum_us, 40),
        // xox[baprs]-[A-Za-z0-9-]{10,}
        prefixed_run(t, &[b"x", b"o", b"x", b"baprs", b"-"], is_alnum_dash, 10),
        // sk-ant-[A-Za-z0-9_-]{20,}
        prefixed_run(t, &atoms(b"sk-ant-"), is_alnum_us_dash, 20),
        // sk-proj-[A-Za-z0-9_-]{20,}
        prefixed_run(t, &atoms(b"sk-proj-"), is_alnum_us_dash, 20),
        // sk-[A-Za-z0-9]{20,}
        prefixed_run(t, &atoms(b"sk-"), is_alnum, 20),
        pem_block(t),
        inline_credential(text),
    ];
    checks
        .iter()
        .enumerate()
        .filter(|(_, hit)| **hit)
        .fold(0u16, |m, (i, _)| m | (1 << i))
}

/// One single-byte atom per byte of a literal.
fn atoms(lit: &'static [u8]) -> Vec<&'static [u8]> {
    lit.chunks(1).collect()
}

/// `/i` on a pattern made only of ASCII: without the `u` flag JS canonicalises
/// with toUpperCase but refuses to map a non-ASCII char onto an ASCII one
/// (so U+017F long s is not `s`, U+212A Kelvin is not `k`). Plain ASCII
/// case folding is therefore exact.
fn in_set(b: u8, set: &[u8]) -> bool {
    set.iter().any(|s| s.eq_ignore_ascii_case(&b))
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}
fn is_alnum_us(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
fn is_alnum_dash(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}
fn is_alnum_us_dash(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}
/// `[A-Za-z0-9_+/=\-]`
fn is_value(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'/' | b'=' | b'-')
}

/// `<atoms><class>{min,}` searched anywhere. For a `.test()` only the
/// existence of `min` class bytes after the prefix matters; the greedy tail
/// cannot change that.
fn prefixed_run(t: &[u8], atoms: &[&[u8]], class: fn(u8) -> bool, min: usize) -> bool {
    let need = atoms.len() + min;
    if t.len() < need {
        return false;
    }
    (0..=t.len() - need).any(|i| {
        atoms.iter().enumerate().all(|(k, a)| in_set(t[i + k], a))
            && t[i + atoms.len()..i + need].iter().all(|&b| class(b))
    })
}

fn ci_starts_with(t: &[u8], at: usize, lit: &[u8]) -> bool {
    t.get(at..at + lit.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(lit))
}

/// `-----BEGIN [A-Z ]*PRIVATE KEY-----` (/i).
///
/// `[A-Z ]*` backtracks, but "PRIVATE KEY" is itself made of class chars and
/// the following `-` is not, so a match exists iff the maximal class run after
/// "BEGIN " ends with "PRIVATE KEY" and is followed by "-----".
fn pem_block(t: &[u8]) -> bool {
    const HEAD: &[u8] = b"-----begin ";
    const TAIL: &[u8] = b"private key";
    let mut i = 0;
    while i + HEAD.len() <= t.len() {
        if ci_starts_with(t, i, HEAD) {
            let j = i + HEAD.len();
            let mut r = j;
            while r < t.len() && (t[r].is_ascii_alphabetic() || t[r] == b' ') {
                r += 1;
            }
            if r - j >= TAIL.len()
                && ci_starts_with(t, r - TAIL.len(), TAIL)
                && ci_starts_with(t, r, b"-----")
            {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// JS `\s`: WhiteSpace + LineTerminator (ECMA-262). Not Rust's
/// `char::is_whitespace`, which also accepts U+0085 and rejects U+FEFF.
/// Matching JS keeps parity with the old scanner and also catches
/// `password\u{a0}=\u{a0}...` (wider than the ASCII-only set in the spec).
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

fn skip_js_space(text: &str, mut p: usize) -> usize {
    while let Some(c) = text.get(p..).and_then(|s| s.chars().next()) {
        if !is_js_space(c) {
            break;
        }
        p += c.len_utf8();
    }
    p
}

/// `(api[_-]?key|secret|token|password|access[_-]?key)\s*[=:]\s*["']?[A-Za-z0-9_+/=\-]{16,}` (/i)
///
/// Tried at every start and every alternative. After the keyword the match is
/// deterministic: `\s*` must eat the whole whitespace run (`[=:]`, quotes and
/// value chars are not whitespace), and dropping an optional quote would make
/// the value class face the quote, which it rejects.
fn inline_credential(text: &str) -> bool {
    const KEYWORDS: [&[u8]; 9] = [
        b"apikey",
        b"api_key",
        b"api-key",
        b"secret",
        b"token",
        b"password",
        b"accesskey",
        b"access_key",
        b"access-key",
    ];
    let t = text.as_bytes();
    for i in 0..t.len() {
        if !matches!(t[i].to_ascii_lowercase(), b'a' | b's' | b't' | b'p') {
            continue;
        }
        for kw in KEYWORDS {
            if !ci_starts_with(t, i, kw) {
                continue;
            }
            // Keyword is ASCII, so i + len is a char boundary.
            let mut p = skip_js_space(text, i + kw.len());
            if !matches!(t.get(p), Some(b'=' | b':')) {
                continue;
            }
            p = skip_js_space(text, p + 1);
            if matches!(t.get(p), Some(b'"' | b'\'')) {
                p += 1;
            }
            if t.get(p..p + 16)
                .is_some_and(|v| v.iter().all(|&b| is_value(b)))
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(lines: &[&str]) -> String {
        lines.join("\n")
    }

    // ---- ported from tests/lib.secrets.test.mjs -------------------------

    #[test]
    fn clean_diff_returns_no_hits() {
        assert!(scan_diff(&d(&["+const x = 1;", "-let y = 2;"])).is_empty());
    }

    #[test]
    fn empty_input() {
        assert!(scan_diff("").is_empty());
        assert!(scan_text("").is_empty());
    }

    #[test]
    fn aws_key_in_added_line() {
        assert!(scan_diff("+key = \"AKIAIOSFODNN7EXAMPLE\"").contains(&"AWS access key"));
    }

    #[test]
    fn github_pat_in_added_line() {
        assert!(
            scan_diff("+t = \"ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ\"")
                .contains(&"GitHub personal access token")
        );
    }

    #[test]
    fn pem_in_added_line() {
        assert!(scan_diff("+-----BEGIN RSA PRIVATE KEY-----").contains(&"PEM private key block"));
        // bats variant: header line plus body line
        assert!(
            scan_diff(&d(&[
                "+-----BEGIN RSA PRIVATE KEY-----",
                "+MIIEpAIBAAKCAQEAabc..."
            ]))
            .contains(&"PEM private key block")
        );
    }

    #[test]
    fn inline_credential_in_added_line() {
        assert!(
            scan_diff("+API_KEY = \"abcdef0123456789ABCDEF0123456789\"")
                .contains(&"inline credential assignment")
        );
    }

    #[test]
    fn removed_lines_ignored() {
        assert!(scan_diff("-AWS = \"AKIAIOSFODNN7EXAMPLE\"").is_empty());
        assert!(scan_diff("-AWS_KEY = \"AKIAIOSFODNN7EXAMPLE\"").is_empty());
    }

    #[test]
    fn context_lines_ignored() {
        assert!(scan_diff(" AWS = \"AKIAIOSFODNN7EXAMPLE\"").is_empty());
    }

    #[test]
    fn file_header_ignored_even_if_it_looks_like_a_key() {
        // Adapted: the header is only recognised after a `--- ` line, as git
        // always emits it.
        assert!(
            scan_diff(&d(&[
                "--- a/AKIAIOSFODNN7EXAMPLE.txt",
                "+++ b/AKIAIOSFODNN7EXAMPLE.txt",
                "+hello"
            ]))
            .is_empty()
        );
        assert!(
            scan_diff(&d(&[
                "--- /dev/null",
                "+++ b/secrets/AKIAIOSFODNN7EXAMPLE.txt",
                "+hello"
            ]))
            .is_empty()
        );
    }

    #[test]
    fn benign_diff_with_headers() {
        assert!(
            scan_diff(&d(&[
                "--- a/foo.txt",
                "+++ b/foo.txt",
                "+hello world",
                "+function add(a, b) { return a + b; }",
                "-let x = 1;"
            ]))
            .is_empty()
        );
    }

    #[test]
    fn benign_code_reference_not_flagged() {
        assert!(scan_diff("+const password = formData.password;").is_empty());
    }

    #[test]
    fn case_insensitive() {
        assert!(scan_diff("+x = \"akiaiosfodnn7example\"").contains(&"AWS access key"));
        assert!(
            scan_diff("+x = \"GHP_ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJ\"")
                .contains(&"GitHub personal access token")
        );
    }

    #[test]
    fn fine_grained_pat() {
        let tok = format!("github_pat_{}", "A".repeat(82));
        assert!(scan_diff(&format!("+token = \"{tok}\"")).contains(&"GitHub fine-grained PAT"));
    }

    #[test]
    fn scan_text_needs_no_plus_prefix() {
        assert!(scan_text("const k = \"AKIAIOSFODNN7EXAMPLE\";").contains(&"AWS access key"));
    }

    #[test]
    fn modern_api_keys() {
        let ant = format!("sk-ant-api03-{}", "A".repeat(30));
        let proj = format!("sk-proj-{}", "B".repeat(30));
        let legacy = format!("sk-{}", "c".repeat(40));
        assert!(scan_text(&ant).contains(&"Anthropic API key"));
        assert!(scan_diff(&format!("+key = \"{ant}\"")).contains(&"Anthropic API key"));
        assert!(scan_diff(&format!("+k = \"{ant}\"")).contains(&"Anthropic API key"));
        assert!(scan_text(&proj).contains(&"OpenAI project key"));
        assert!(scan_text(&legacy).contains(&"OpenAI legacy API key"));
    }

    #[test]
    fn dashed_identifier_with_sk_not_flagged() {
        assert!(scan_text("disk-management-system-controller-v2").is_empty());
    }

    // ---- new behaviour --------------------------------------------------

    #[test]
    fn added_content_starting_with_plus_plus_is_scanned() {
        // Old code dropped every `+++` line, hiding added `++ secret` content.
        assert!(scan_diff("+++ AKIAIOSFODNN7EXAMPLE").contains(&"AWS access key"));
        assert!(
            scan_diff(&d(&["+x", "+++ token = AKIAIOSFODNN7EXAMPLE"])).contains(&"AWS access key")
        );
        assert!(scan_diff("+++AKIAIOSFODNN7EXAMPLE").contains(&"AWS access key"));
    }

    #[test]
    fn plus_plus_content_after_minus_minus_inside_hunk_is_scanned() {
        // Removed `-- old` then added `++ AKIA...` look like ---/+++ headers,
        // but the hunk counts say they are content.
        let diff = d(&[
            "--- a/f.sql",
            "+++ b/f.sql",
            "@@ -1,1 +1,1 @@",
            "--- old comment",
            "+++ AKIAIOSFODNN7EXAMPLE",
        ]);
        assert_eq!(scan_diff(&diff), vec!["AWS access key"]);
    }

    #[test]
    fn second_file_header_after_hunk_is_still_a_header() {
        let diff = d(&[
            "--- a/x",
            "+++ b/x",
            "@@ -1 +1 @@",
            "-a",
            "+b",
            "--- a/AKIAIOSFODNN7EXAMPLE",
            "+++ b/AKIAIOSFODNN7EXAMPLE",
            "@@ -0,0 +1 @@",
            "+fine",
        ]);
        assert!(scan_diff(&diff).is_empty());
    }

    #[test]
    fn hunk_header_parsing() {
        assert_eq!(parse_hunk_header("@@ -1,3 +1,4 @@ fn x"), Some((3, 4)));
        assert_eq!(parse_hunk_header("@@ -1 +1 @@"), Some((1, 1)));
        assert_eq!(parse_hunk_header("@@ -0,0 +1 @@"), Some((0, 1)));
        assert_eq!(parse_hunk_header("@@ -a +1 @@"), None);
        assert_eq!(parse_hunk_header("@@ -1 +1"), None);
        assert_eq!(parse_hunk_header("@@ -99999999999999999999999 +1 @@"), None);
    }

    #[test]
    fn crlf_diff_lines() {
        assert!(scan_diff("+k = AKIAIOSFODNN7EXAMPLE\r\n-x\r\n").contains(&"AWS access key"));
    }

    #[test]
    fn labels_in_table_order_each_once() {
        let text = format!(
            "{} AKIAIOSFODNN7EXAMPLE AKIAIOSFODNN7EXAMPLE password: {}",
            "-----BEGIN PRIVATE KEY-----",
            "x".repeat(16)
        );
        assert_eq!(
            scan_text(&text),
            vec![
                "AWS access key",
                "PEM private key block",
                "inline credential assignment"
            ]
        );
    }

    #[test]
    fn each_pattern_boundaries() {
        // exact minimum lengths match, one less does not
        assert!(scan_text(&format!("AKIA{}", "Z".repeat(16))).contains(&"AWS access key"));
        assert!(scan_text(&format!("AKIA{}", "Z".repeat(15))).is_empty());
        assert!(scan_text(&format!("asia{}", "9".repeat(16))).contains(&"AWS STS token"));
        for c in ["p", "o", "u", "s", "r"] {
            let tok = format!("gh{c}_{}", "a".repeat(36));
            assert!(scan_text(&tok).contains(&"GitHub personal access token"));
        }
        assert!(scan_text(&format!("ghx_{}", "a".repeat(36))).is_empty());
        assert!(scan_text(&format!("ghp_{}", "a".repeat(35))).is_empty());
        assert!(
            scan_text(&format!("github_pat_{}", "_".repeat(40)))
                .contains(&"GitHub fine-grained PAT")
        );
        assert!(scan_text(&format!("github_pat_{}", "a".repeat(39))).is_empty());
        for c in ["b", "a", "p", "r", "s"] {
            assert!(scan_text(&format!("xox{c}-1234567890")).contains(&"Slack token"));
        }
        assert!(scan_text("xoxb-123456789").is_empty());
        assert!(scan_text("xoxq-1234567890").is_empty());
        assert!(scan_text(&format!("sk-ant-{}", "-".repeat(20))).contains(&"Anthropic API key"));
        assert!(scan_text(&format!("sk-ant-{}", "_".repeat(19))).is_empty());
        assert!(scan_text(&format!("SK-PROJ-{}", "_".repeat(20))).contains(&"OpenAI project key"));
        assert!(scan_text(&format!("sk-{}", "a".repeat(19))).is_empty());
    }

    #[test]
    fn non_ascii_is_never_case_folded_onto_ascii() {
        // U+212A KELVIN SIGN lowercases to `k` in Unicode, but JS /i (no u flag)
        // does not fold non-ASCII onto ASCII, and neither do we.
        assert!(scan_text("api\u{212A}ey = abcdefghijklmnop").is_empty());
        assert!(scan_text("\u{17F}ecret = abcdefghijklmnop").is_empty());
        assert!(scan_text("secret = abcdefghijklmnop").contains(&"inline credential assignment"));
    }

    #[test]
    fn pem_variants() {
        for ok in [
            "-----BEGIN PRIVATE KEY-----",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "-----begin ec private key-----",
            "-----BEGIN  ENCRYPTED   PRIVATE KEY-----",
            "x-----BEGIN PRIVATE KEY PRIVATE KEY-----",
            "------BEGIN RSA PRIVATE KEY------",
        ] {
            assert!(scan_text(ok).contains(&"PEM private key block"), "{ok}");
        }
        for bad in [
            "-----BEGIN PUBLIC KEY-----",
            "-----BEGIN RSA-PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY----",
            "-----BEGINPRIVATE KEY-----",
            "-----BEGIN PRIVATE  KEY-----",
            "-----BEGIN RSA2 PRIVATE KEY-----",
            "-----BEGIN \nPRIVATE KEY-----",
        ] {
            assert!(scan_text(bad).is_empty(), "{bad}");
        }
    }

    #[test]
    fn inline_credential_variants() {
        let v = "abcdefghijklmnop";
        for ok in [
            format!("api_key={v}"),
            format!("API-KEY: '{v}'"),
            format!("apikey\t=\t\"{v}"),
            format!("x.secret = {v}"),
            format!("Token:{v}"),
            format!("PASSWORD   =   {v}"),
            format!("access_key = {v}"),
            format!("accesskey={v}"),
            format!("access-key={v}"),
            format!("password\n=\n{v}"),
            format!("password\u{a0}=\u{3000}{v}"),
            format!("password\u{feff}={v}"),
            format!("token = ==============={v}"),
            format!("token = {}", "+/=-_".repeat(4)),
        ] {
            assert!(
                scan_text(&ok).contains(&"inline credential assignment"),
                "{ok:?}"
            );
        }
        for bad in [
            format!("api__key={v}"),
            format!("password = {}", &v[..15]),
            format!("password = \"\"{v}"),
            format!("password == {v}"),
            format!("password :: {v}"),
            "password = formData.password".to_string(),
            format!("password - {v}"),
            // U+0085 is Rust whitespace but not JS \s
            format!("password\u{85}={v}"),
            format!("token = `{v}`"),
            format!("secretsecret=:{v}"),
        ] {
            assert!(scan_text(&bad).is_empty(), "{bad:?}");
        }
    }

    /// Representative subset of the V8 differential corpus (strings that
    /// exercise backtracking and boundaries), with the masks JS produced.
    #[test]
    fn js_differential_fixed_subset() {
        let cases: &[(&str, u16)] = &[
            ("tokentoken = 0123456789abcdef", 1 << 9),
            ("token = '0123456789abcde'", 0),
            ("token=\"\"0123456789abcdef", 0),
            ("ApI-KeY:\r\n\u{b}\u{c}'0123456789abcdef", 1 << 9),
            ("sk-ant-sk-proj-abcdefghijklmnopqrst", (1 << 5) | (1 << 6)),
            ("sk-projabcdefghijklmnopqrst", 1 << 7),
            ("sk-proj-abcdefghij-lmnopqrst", 1 << 6),
            ("xoxs------------", 1 << 4),
            ("xoxs-----------", 1 << 4),
            ("AKIAASIA0000000000000000", 1 | (1 << 1)),
            ("ghs_ghs_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 1 << 2),
            ("-----BEGIN -----BEGIN PRIVATE KEY-----", 1 << 8),
            ("-----BEGIN private keyprivate key-----", 1 << 8),
            ("password\u{2028}:\u{2029}abcdefghijklmnop", 1 << 9),
            ("password\u{180e}:abcdefghijklmnop", 0),
            ("password\u{1c}:abcdefghijklmnop", 0),
        ];
        for (text, mask) in cases {
            assert_eq!(scan_mask(text), *mask, "{text:?}");
        }
    }

    #[test]
    fn hostile_inputs_do_not_panic() {
        let big = "password=".repeat(10_000) + &"\u{3000}".repeat(10_000);
        let _ = scan_text(&big);
        let _ = scan_text(&"-----BEGIN ".repeat(5_000));
        let _ = scan_text("é\u{feff}𝔸 token:\u{200a}é");
        let _ = scan_diff("@@ -1,18446744073709551615 +1,18446744073709551615 @@\n+++ x\n\n\\");
        let _ = scan_diff("\n\n+\n+++\n--- \n+++ ");
    }
}

//! Tiny argv parser (port of lib/args.mjs).
//!
//! - Boolean flags: `--background`; `--background=x` is an error.
//! - Value flags: `--model X` or `--model=X`; last occurrence wins.
//! - Unknown long flags go to `extra` so callers can forward agy-native
//!   flags (`--sandbox`, `--print-timeout 10m`).
//! - `--` ends flag parsing; everything after is positional verbatim, so a
//!   prompt that starts with `-` is safe.
//! - Single-dash tokens (`-x`, `-`) are positional.

use std::collections::BTreeMap;

/// Which long flags a command understands.
pub(crate) struct Schema<'a> {
    pub boolean: &'a [&'a str],
    pub value: &'a [&'a str],
    /// Unknown-to-us flags that are known to take a value when forwarded to
    /// agy (e.g. `print-timeout`). Only these swallow the following token.
    pub passthrough_value: &'a [&'a str],
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub flags: BTreeMap<String, bool>,
    pub values: BTreeMap<String, Option<String>>,
    pub positional: Vec<String>,
    pub extra: Vec<String>,
    pub errors: Vec<String>,
}

impl Parsed {
    /// True if the boolean flag was given. Unknown names are false.
    pub(crate) fn flag(&self, name: &str) -> bool {
        self.flags.get(name).copied().unwrap_or(false)
    }

    /// The value flag's last value, if any.
    pub(crate) fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).and_then(|v| v.as_deref())
    }
}

pub(crate) fn parse_args(argv: &[String], schema: &Schema) -> Parsed {
    let mut out = Parsed::default();
    for &f in schema.boolean {
        out.flags.insert(f.to_string(), false);
    }
    for &f in schema.value {
        out.values.insert(f.to_string(), None);
    }

    let mut i = 0;
    while i < argv.len() {
        let tok = &argv[i];
        if tok == "--" {
            out.positional.extend(argv[i + 1..].iter().cloned());
            break;
        }
        let Some(body) = tok.strip_prefix("--") else {
            out.positional.push(tok.clone());
            i += 1;
            continue;
        };
        let (name, inline) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (body, None),
        };

        if schema.boolean.contains(&name) {
            if inline.is_some() {
                out.errors.push(format!("--{name} does not take a value"));
            } else {
                out.flags.insert(name.to_string(), true);
            }
            i += 1;
            continue;
        }

        if schema.value.contains(&name) {
            match inline {
                Some("") => out
                    .errors
                    .push(format!("--{name}= requires a non-empty value")),
                Some(v) => {
                    out.values.insert(name.to_string(), Some(v.to_string()));
                }
                None => match argv.get(i + 1) {
                    // Taken unconditionally (even if it starts with `-`), as
                    // before: `--model -x` means the model is `-x`.
                    Some(v) => {
                        out.values.insert(name.to_string(), Some(v.clone()));
                        i += 1;
                    }
                    None => out.errors.push(format!("--{name} requires a value")),
                },
            }
            i += 1;
            continue;
        }

        // Unknown long flag: forward it. Deliberate fix vs args.mjs: the old
        // parser swallowed ANY following non-dash token, so
        // `rescue --sandbox fix the bug` silently lost "fix" from the task.
        // Now only flags declared as value-taking consume the next token, and
        // never one that looks like a flag (so `--print-timeout --wait`
        // keeps our `--wait`, and `--print-timeout -- task` keeps `--`).
        out.extra.push(tok.clone());
        if inline.is_none()
            && schema.passthrough_value.contains(&name)
            && let Some(next) = argv.get(i + 1)
            && !next.starts_with('-')
        {
            out.extra.push(next.clone());
            i += 1;
        }
        i += 1;
    }
    out
}

/// Positional args joined with single spaces and trimmed (JS `.trim()`
/// semantics, see `js_trim`).
pub(crate) fn join_positional(p: &Parsed) -> String {
    js_trim(&p.positional.join(" ")).to_string()
}

/// `String.prototype.trim` exactly. Rust's `str::trim` uses Unicode
/// White_Space, which differs from ECMAScript's WhiteSpace+LineTerminator
/// set: JS also strips U+FEFF (BOM) and does NOT strip U+0085 (NEL).
/// Prompts must stay byte-identical to the Node runtime, so use this.
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{D}'
            | ' '
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: Schema<'static> = Schema {
        boolean: &["background", "wait", "resume", "fresh"],
        value: &["model", "base", "name", "output"],
        passthrough_value: &["print-timeout"],
    };

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn parse(a: &[&str]) -> Parsed {
        parse_args(&argv(a), &SCHEMA)
    }

    // --- ported from tests/lib.args.test.mjs ---

    #[test]
    fn defaults_for_empty_argv() {
        let r = parse(&[]);
        assert!(!r.flag("background"));
        assert_eq!(r.flags.get("background"), Some(&false));
        assert_eq!(r.values.get("model"), Some(&None));
        assert_eq!(r.value("model"), None);
        assert!(r.positional.is_empty());
        assert!(r.errors.is_empty());
        assert!(r.extra.is_empty());
        assert_eq!(r.flags.len(), 4);
        assert_eq!(r.values.len(), 4);
    }

    #[test]
    fn boolean_flag() {
        let r = parse(&["--background", "task", "text"]);
        assert!(r.flag("background"));
        assert_eq!(r.positional, ["task", "text"]);
    }

    #[test]
    fn value_space_separated() {
        let r = parse(&["--model", "opus", "fix", "the", "bug"]);
        assert_eq!(r.value("model"), Some("opus"));
        assert_eq!(r.positional, ["fix", "the", "bug"]);
    }

    #[test]
    fn value_equals_form() {
        let r = parse(&["--model=opus", "fix", "bug"]);
        assert_eq!(r.value("model"), Some("opus"));
        assert_eq!(r.positional, ["fix", "bug"]);
    }

    #[test]
    fn value_equals_empty_is_error() {
        let r = parse(&["--model="]);
        assert_eq!(r.value("model"), None);
        assert_eq!(r.errors, ["--model= requires a non-empty value"]);
    }

    #[test]
    fn value_missing_at_end_is_error() {
        let r = parse(&["--model"]);
        assert_eq!(r.errors, ["--model requires a value"]);
        assert_eq!(r.value("model"), None);
    }

    #[test]
    fn boolean_with_value_is_error() {
        let r = parse(&["--background=yes"]);
        assert_eq!(r.errors, ["--background does not take a value"]);
        assert!(!r.flag("background"));
        // Empty inline value is still a value.
        let r = parse(&["--background="]);
        assert_eq!(r.errors, ["--background does not take a value"]);
    }

    #[test]
    fn unknown_flags_forwarded_to_extra() {
        let r = parse(&["--sandbox", "--print-timeout", "10m", "do", "the", "thing"]);
        assert_eq!(r.extra, ["--sandbox", "--print-timeout", "10m"]);
        assert_eq!(r.positional, ["do", "the", "thing"]);
    }

    #[test]
    fn double_dash_terminates() {
        let r = parse(&["--model", "opus", "--", "--this-is-not-a-flag", "task"]);
        assert_eq!(r.value("model"), Some("opus"));
        assert_eq!(r.positional, ["--this-is-not-a-flag", "task"]);
    }

    #[test]
    fn base_with_positional() {
        let r = parse(&["--base", "main", "focus", "on", "race", "conditions"]);
        assert_eq!(r.value("base"), Some("main"));
        assert_eq!(r.positional, ["focus", "on", "race", "conditions"]);
    }

    #[test]
    fn multiple_booleans() {
        let r = parse(&["--background", "--wait", "--resume", "go"]);
        assert!(r.flag("background") && r.flag("wait") && r.flag("resume"));
        assert!(!r.flag("fresh"));
        assert_eq!(r.positional, ["go"]);
    }

    #[test]
    fn join_positional_single_spaces() {
        let r = parse(&["--background", "hello", "world"]);
        assert_eq!(join_positional(&r), "hello world");
    }

    #[test]
    fn join_positional_trims() {
        assert_eq!(join_positional(&parse(&["  hi  "])), "hi");
        assert_eq!(join_positional(&parse(&[])), "");
        // Inner whitespace is preserved.
        assert_eq!(join_positional(&parse(&[" a ", " b "])), "a   b");
    }

    // --- the deliberate fix ---

    #[test]
    fn unknown_flag_does_not_swallow_task_words() {
        // Old args.mjs produced extra=["--sandbox","fix"], positional=["the","bug"].
        let r = parse(&["--sandbox", "fix", "the", "bug"]);
        assert_eq!(r.extra, ["--sandbox"]);
        assert_eq!(r.positional, ["fix", "the", "bug"]);
        assert_eq!(join_positional(&r), "fix the bug");
    }

    #[test]
    fn unknown_flag_inline_value() {
        let r = parse(&["--sandbox=strict", "task"]);
        assert_eq!(r.extra, ["--sandbox=strict"]);
        assert_eq!(r.positional, ["task"]);
        let r = parse(&["--print-timeout=10m", "task"]);
        assert_eq!(r.extra, ["--print-timeout=10m"]);
        assert_eq!(r.positional, ["task"]);
    }

    #[test]
    fn passthrough_value_does_not_take_flags_or_terminator() {
        let r = parse(&["--print-timeout", "--wait", "go"]);
        assert_eq!(r.extra, ["--print-timeout"]);
        assert!(r.flag("wait"));
        assert_eq!(r.positional, ["go"]);

        let r = parse(&["--print-timeout", "--", "-x"]);
        assert_eq!(r.extra, ["--print-timeout"]);
        assert_eq!(r.positional, ["-x"]);

        let r = parse(&["--print-timeout", "-5"]);
        assert_eq!(r.extra, ["--print-timeout"]);
        assert_eq!(r.positional, ["-5"]);
    }

    #[test]
    fn passthrough_value_at_end() {
        let r = parse(&["task", "--print-timeout"]);
        assert_eq!(r.extra, ["--print-timeout"]);
        assert_eq!(r.positional, ["task"]);
        assert!(r.errors.is_empty());
    }

    // --- more edge cases ---

    #[test]
    fn value_flag_takes_next_token_unconditionally() {
        let r = parse(&["--model", "--wait"]);
        assert_eq!(r.value("model"), Some("--wait"));
        assert!(!r.flag("wait"));
        let r = parse(&["--model", "--"]);
        assert_eq!(r.value("model"), Some("--"));
        assert!(r.positional.is_empty());
    }

    #[test]
    fn last_value_wins() {
        let r = parse(&["--model", "a", "--model=b", "--model", "c"]);
        assert_eq!(r.value("model"), Some("c"));
        // A later error does not erase an earlier good value.
        let r = parse(&["--model", "a", "--model="]);
        assert_eq!(r.value("model"), Some("a"));
        assert_eq!(r.errors.len(), 1);
    }

    #[test]
    fn value_keeps_text_after_first_equals() {
        let r = parse(&["--model=a=b=c"]);
        assert_eq!(r.value("model"), Some("a=b=c"));
        let r = parse(&["--model==x"]);
        assert_eq!(r.value("model"), Some("=x"));
    }

    #[test]
    fn single_dash_tokens_are_positional() {
        let r = parse(&["-m", "opus", "-", "-background"]);
        assert_eq!(r.positional, ["-m", "opus", "-", "-background"]);
        assert!(r.extra.is_empty());
    }

    #[test]
    fn empty_and_odd_long_flags() {
        let r = parse(&["--=x", "---model", "y", ""]);
        assert_eq!(r.extra, ["--=x", "---model"]);
        assert_eq!(r.positional, ["y", ""]);
        assert_eq!(r.value("model"), None);
    }

    #[test]
    fn double_dash_at_end_and_repeated() {
        let r = parse(&["a", "--"]);
        assert_eq!(r.positional, ["a"]);
        let r = parse(&["--", "--", "--model", "x"]);
        assert_eq!(r.positional, ["--", "--model", "x"]);
        assert_eq!(r.value("model"), None);
    }

    #[test]
    fn flag_names_are_case_sensitive_and_exact() {
        let r = parse(&["--Background", "--backgroundx", "--model-x"]);
        assert!(!r.flag("background"));
        assert_eq!(r.extra, ["--Background", "--backgroundx", "--model-x"]);
    }

    #[test]
    fn flag_and_value_unknown_names() {
        let r = parse(&[]);
        assert!(!r.flag("nope"));
        assert_eq!(r.value("nope"), None);
    }

    #[test]
    fn non_ascii_tokens() {
        let r = parse(&["--model=модель", "привет", "--ünknown", "😀"]);
        assert_eq!(r.value("model"), Some("модель"));
        assert_eq!(r.extra, ["--ünknown"]);
        assert_eq!(r.positional, ["привет", "😀"]);
    }

    #[test]
    fn empty_schema() {
        let s = Schema {
            boolean: &[],
            value: &[],
            passthrough_value: &[],
        };
        let r = parse_args(&argv(&["--x", "y"]), &s);
        assert_eq!(r.extra, ["--x"]);
        assert_eq!(r.positional, ["y"]);
        assert!(r.flags.is_empty() && r.values.is_empty());
    }

    #[test]
    fn js_trim_matches_ecmascript_set() {
        // Exact set from Node: every BMP code point c where (c+"a").trim()==="a".
        let js: Vec<u32> = vec![
            0x9, 0xa, 0xb, 0xc, 0xd, 0x20, 0xa0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003, 0x2004,
            0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000,
            0xfeff,
        ];
        let ours: Vec<u32> = (0u32..0x11_0000)
            .filter_map(char::from_u32)
            .filter(|&c| is_js_whitespace(c))
            .map(u32::from)
            .collect();
        assert_eq!(ours, js);
        assert_eq!(js_trim("\u{FEFF} x \u{3000}"), "x");
        assert_eq!(js_trim("\u{85}x\u{85}"), "\u{85}x\u{85}");
    }
}

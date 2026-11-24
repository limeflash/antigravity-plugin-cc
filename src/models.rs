//! `--model` resolution. Aliases resolve at call time against `agy models`
//! to the NEWEST model in their family, and the result is passed to agy's own
//! `--model` flag for that one call (ported from upstream
//! simplybychris/antigravity-plugin-cc 0.5.0). 0.8.x instead patched the
//! "model" field of agy's settings.json under a lock: that flipped the model
//! of a TUI open in parallel and broke whenever Google renamed a model.

use std::time::Duration;

use crate::agy::{self, Agy};
use crate::util::{self, Fail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Model {
    /// Stable id like `gemini-3.8-flash-high` (empty on builds that list
    /// display names only).
    pub(crate) id: String,
    /// Display name like `Gemini 3.8 Flash (High)`.
    pub(crate) name: String,
}

impl Model {
    /// The value for `agy --model`.
    pub(crate) fn arg(&self) -> &str {
        if self.id.is_empty() {
            &self.name
        } else {
            &self.id
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Family {
    /// `gemini <version> <tier> (<level>)`
    Gemini {
        tier: &'static str,
        level: &'static str,
    },
    /// `claude <kind> <version>` with an optional ` (...)` suffix.
    Claude { kind: &'static str },
    /// `gpt-oss 120b` with an optional ` (...)` suffix.
    GptOss,
}

struct AliasRow {
    aliases: &'static [&'static str],
    family: Family,
    /// On a version tie, a name ending with this suffix wins.
    prefer_suffix: Option<&'static str>,
    description: &'static str,
}

const ALIASES: &[AliasRow] = &[
    AliasRow {
        aliases: &["flash-low"],
        family: Family::Gemini {
            tier: "flash",
            level: "low",
        },
        prefer_suffix: None,
        description: "newest Gemini Flash (Low)",
    },
    AliasRow {
        aliases: &["flash-medium", "flash-med"],
        family: Family::Gemini {
            tier: "flash",
            level: "medium",
        },
        prefer_suffix: None,
        description: "newest Gemini Flash (Medium)",
    },
    AliasRow {
        aliases: &["flash", "flash-high"],
        family: Family::Gemini {
            tier: "flash",
            level: "high",
        },
        prefer_suffix: None,
        description: "newest Gemini Flash (High)",
    },
    AliasRow {
        aliases: &["pro-low"],
        family: Family::Gemini {
            tier: "pro",
            level: "low",
        },
        prefer_suffix: None,
        description: "newest Gemini Pro (Low)",
    },
    AliasRow {
        aliases: &["pro", "pro-high"],
        family: Family::Gemini {
            tier: "pro",
            level: "high",
        },
        prefer_suffix: None,
        description: "newest Gemini Pro (High)",
    },
    AliasRow {
        aliases: &["sonnet", "claude-sonnet"],
        family: Family::Claude { kind: "sonnet" },
        prefer_suffix: Some("(thinking)"),
        description: "newest Claude Sonnet",
    },
    AliasRow {
        aliases: &["opus", "claude-opus"],
        family: Family::Claude { kind: "opus" },
        prefer_suffix: Some("(thinking)"),
        description: "newest Claude Opus",
    },
    AliasRow {
        aliases: &["gpt-oss", "gpt-oss-120b"],
        family: Family::GptOss,
        prefer_suffix: Some("(medium)"),
        description: "GPT-OSS 120B",
    },
];

fn is_version_word(w: &str) -> bool {
    !w.is_empty()
        && !w.starts_with('.')
        && !w.ends_with('.')
        && !w.contains("..")
        && w.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// ` (anything)` at the end, or nothing.
fn optional_paren_suffix(rest: &str) -> bool {
    rest.is_empty() || (rest.starts_with(" (") && rest.ends_with(')'))
}

impl Family {
    fn matches(self, lower: &str) -> bool {
        match self {
            Family::Gemini { tier, level } => {
                // exactly: "gemini <ver> <tier> (<level>)"
                let Some(rest) = lower.strip_prefix("gemini ") else {
                    return false;
                };
                let Some((ver, rest)) = rest.split_once(' ') else {
                    return false;
                };
                is_version_word_loose(ver) && rest == format!("{tier} ({level})")
            }
            Family::Claude { kind } => {
                let Some(rest) = lower.strip_prefix("claude ") else {
                    return false;
                };
                let Some(rest) = rest.strip_prefix(kind) else {
                    return false;
                };
                let Some(rest) = rest.strip_prefix(' ') else {
                    return false;
                };
                let ver_end = rest.find(' ').unwrap_or(rest.len());
                is_version_word_loose(&rest[..ver_end]) && optional_paren_suffix(&rest[ver_end..])
            }
            Family::GptOss => lower
                .strip_prefix("gpt-oss 120b")
                .is_some_and(optional_paren_suffix),
        }
    }
}

/// The alias patterns used `[0-9.]+`, which also accepts odd spellings like
/// `3.` — keep that leniency for matching (the version used for ORDERING is
/// the strict one from `family_version`).
fn is_version_word_loose(w: &str) -> bool {
    !w.is_empty() && w.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The family version inside a lowercased display name: its first purely
/// numeric word ("gemini 3.8 flash (high)" -> "3.8"), or "".
fn family_version(lower: &str) -> &str {
    lower.split(' ').find(|w| is_version_word(w)).unwrap_or("")
}

/// Parse `agy models`: one "id<TAB>display name" row per model; skips blank
/// lines, the "Fetching available models..." banner and "Header:" lines.
pub(crate) fn parse_model_list(text: &str) -> Vec<Model> {
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.trim().is_empty() || line.trim_end().ends_with(':') || line.starts_with("Fetching ")
        {
            continue;
        }
        let (id, name) = match line.split_once('\t') {
            Some((id, name)) => (id.trim(), name.trim()),
            None => ("", line.trim()),
        };
        if !name.is_empty() {
            out.push(Model {
                id: id.to_string(),
                name: name.to_string(),
            });
        }
    }
    out
}

fn alias_row(input: &str) -> Option<&'static AliasRow> {
    let lc = input.to_ascii_lowercase();
    ALIASES.iter().find(|r| r.aliases.contains(&lc.as_str()))
}

fn newest_match<'a>(models: &'a [Model], row: &AliasRow) -> Option<&'a Model> {
    let mut best: Option<(&Model, String, bool)> = None;
    for m in models {
        let lower = m.name.to_lowercase();
        if !row.family.matches(&lower) {
            continue;
        }
        let version = family_version(&lower).to_string();
        let preferred = row.prefer_suffix.is_some_and(|s| lower.ends_with(s));
        let replace = match &best {
            None => true,
            Some((_, bv, bp)) => match agy::compare_versions(&version, bv) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Equal => preferred && !bp,
                std::cmp::Ordering::Less => false,
            },
        };
        if replace {
            best = Some((m, version, preferred));
        }
    }
    best.map(|(m, _, _)| m)
}

fn exact_match<'a>(models: &'a [Model], input: &str) -> Option<&'a Model> {
    let want = input.to_lowercase();
    models
        .iter()
        .find(|m| m.id.to_lowercase() == want || m.name.to_lowercase() == want)
}

/// Resolve against a model list (pure). Exact ids/names win over aliases.
pub(crate) fn resolve_from_list(models: &[Model], input: &str) -> Result<String, String> {
    if let Some(m) = exact_match(models, input) {
        return Ok(m.arg().to_string());
    }
    match alias_row(input) {
        Some(row) => newest_match(models, row)
            .map(|m| m.arg().to_string())
            .ok_or_else(|| {
                format!(
                    "error: alias '{input}' has no matching model in `agy models` (expected: {})",
                    row.description
                )
            }),
        None => Err(format!(
            "error: unknown model '{input}' (not an alias and not listed by `agy models`)"
        )),
    }
}

/// Alias + model table for errors and `help`.
pub(crate) fn format_model_table(models: Option<&[Model]>) -> String {
    let listed = models.filter(|m| !m.is_empty());
    let mut out = String::new();
    out.push_str(if listed.is_some() {
        "Aliases (case-insensitive), resolved against `agy models`:\n"
    } else {
        "Aliases (case-insensitive). `agy models` could not be read, so they are shown unresolved:\n"
    });
    for row in ALIASES {
        let target = match listed {
            None => row.description.to_string(),
            Some(ms) => match newest_match(ms, row) {
                Some(m) => m.name.clone(),
                None => format!("(not available: {})", row.description),
            },
        };
        out.push_str(&format!(
            "  {:<28}  -> {}\n",
            row.aliases.join(", "),
            target
        ));
    }
    if let Some(ms) = listed {
        out.push_str(
            "\nAvailable models (names and ids are accepted verbatim, case-insensitive):\n",
        );
        for m in ms {
            if m.id.is_empty() {
                out.push_str(&format!("  {}\n", m.name));
            } else {
                out.push_str(&format!("  {:<32}  {}\n", m.name, m.id));
            }
        }
    }
    out
}

/// `agy models`, parsed; None if it can't be listed.
pub(crate) fn list_models(agy: &Agy) -> Option<Vec<Model>> {
    let out = util::run_simple(agy.path(), &["models"], Duration::from_secs(60)).ok()?;
    if !out.ok() {
        return None;
    }
    Some(parse_model_list(&out.stdout_str()))
}

/// Resolve a `--model` value for `agy --model` (requires agy >= 1.2.0, which
/// every command already enforces). Exit 64 for an unknown name, 1 when
/// `agy models` can't be read but an alias needs it.
pub(crate) fn resolve(agy: &Agy, input: &str) -> Result<String, Fail> {
    if input.trim().is_empty() {
        return Err(empty_value_error(Some(agy)));
    }
    let models = list_models(agy).filter(|m| !m.is_empty());
    let Some(models) = models else {
        if alias_row(input).is_some() {
            return Err(Fail::runtime(
                "error: could not list models with `agy models` (run it directly to see why).\n       aliases need that list; pass the full model name instead.",
            ));
        }
        eprintln!("[agy-plugin] note: could not list models; passing '{input}' to agy unchecked");
        return Ok(input.to_string());
    };
    resolve_from_list(&models, input).map_err(|e| {
        Fail::usage(format!(
            "{e}\n{}",
            format_model_table(Some(&models)).trim_end()
        ))
    })
}

/// The error for `--model` given without a value (exit 64), with the table.
pub(crate) fn empty_value_error(agy: Option<&Agy>) -> Fail {
    let models = agy.and_then(list_models);
    Fail::usage(format!(
        "error: --model requires a non-empty value (e.g. --model pro)\n{}",
        format_model_table(models.as_deref()).trim_end()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing() -> Vec<Model> {
        parse_model_list(
            "Fetching available models...\n\
             gemini-3.5-flash-high\tGemini 3.5 Flash (High)\n\
             gemini-3.8-flash-high\tGemini 3.8 Flash (High)\n\
             gemini-3.8-flash-low\tGemini 3.8 Flash (Low)\r\n\
             claude-sonnet-4-6\tClaude Sonnet 4.6\n\
             claude-sonnet-4-6-thinking\tClaude Sonnet 4.6 (Thinking)\n\
             claude-opus-4-6-thinking\tClaude Opus 4.6 (Thinking)\n\
             gpt-oss-120b-medium\tGPT-OSS 120B (Medium)\n\
             \n",
        )
    }

    #[test]
    fn parses_the_real_agy_1_5_16_listing() {
        let m = listing();
        assert_eq!(m.len(), 7);
        assert_eq!(
            m[0],
            Model {
                id: "gemini-3.5-flash-high".into(),
                name: "Gemini 3.5 Flash (High)".into()
            }
        );
        assert_eq!(m[2].name, "Gemini 3.8 Flash (Low)", "CRLF stripped");
    }

    #[test]
    fn name_only_listing_and_headers() {
        let m = parse_model_list("Models:\nGemini 3.8 Flash (High)\n  \nFetching x\n");
        assert_eq!(
            m,
            vec![Model {
                id: String::new(),
                name: "Gemini 3.8 Flash (High)".into()
            }]
        );
        assert_eq!(m[0].arg(), "Gemini 3.8 Flash (High)");
    }

    #[test]
    fn aliases_pick_the_newest_in_family() {
        let m = listing();
        assert_eq!(
            resolve_from_list(&m, "flash").unwrap(),
            "gemini-3.8-flash-high"
        );
        assert_eq!(
            resolve_from_list(&m, "flash-high").unwrap(),
            "gemini-3.8-flash-high"
        );
        assert_eq!(
            resolve_from_list(&m, "flash-low").unwrap(),
            "gemini-3.8-flash-low"
        );
        assert_eq!(
            resolve_from_list(&m, "OPUS").unwrap(),
            "claude-opus-4-6-thinking"
        );
        assert_eq!(
            resolve_from_list(&m, "gpt-oss").unwrap(),
            "gpt-oss-120b-medium"
        );
    }

    #[test]
    fn version_tie_prefers_thinking() {
        assert_eq!(
            resolve_from_list(&listing(), "sonnet").unwrap(),
            "claude-sonnet-4-6-thinking"
        );
    }

    #[test]
    fn newer_version_beats_preferred_suffix() {
        let m = parse_model_list("a\tClaude Sonnet 4.6 (Thinking)\nb\tClaude Sonnet 4.10\n");
        assert_eq!(
            resolve_from_list(&m, "sonnet").unwrap(),
            "b",
            "4.10 > 4.6 numerically"
        );
    }

    #[test]
    fn exact_names_and_ids_win_and_are_case_insensitive() {
        let m = listing();
        assert_eq!(
            resolve_from_list(&m, "claude opus 4.6 (thinking)").unwrap(),
            "claude-opus-4-6-thinking"
        );
        assert_eq!(
            resolve_from_list(&m, "GPT-OSS-120B-MEDIUM").unwrap(),
            "gpt-oss-120b-medium"
        );
        // An id that is also alias-shaped resolves to itself, not the family.
        let m2 = parse_model_list(
            "flash\tSome Custom Model\ngemini-9-flash-high\tGemini 9 Flash (High)\n",
        );
        assert_eq!(resolve_from_list(&m2, "flash").unwrap(), "flash");
    }

    #[test]
    fn unknown_and_unavailable_family_errors() {
        let m = listing();
        assert!(
            resolve_from_list(&m, "nope")
                .unwrap_err()
                .contains("unknown model 'nope'")
        );
        assert!(
            resolve_from_list(&m, "pro")
                .unwrap_err()
                .contains("no matching model")
        );
    }

    #[test]
    fn family_matching_is_exact() {
        let f = Family::Gemini {
            tier: "flash",
            level: "high",
        };
        assert!(f.matches("gemini 3.8 flash (high)"));
        assert!(!f.matches("gemini 3.8 flash (high) extra"));
        assert!(!f.matches("gemini 3.8 flash lite (high)"));
        assert!(!f.matches("gemini flash (high)"));
        assert!(!f.matches("gemini 4 neon (high)"));
        let c = Family::Claude { kind: "opus" };
        assert!(c.matches("claude opus 4.6"));
        assert!(c.matches("claude opus 4.6 (thinking)"));
        assert!(!c.matches("claude opus"));
        assert!(!c.matches("claude opus 4.6 thinking"));
        assert!(!c.matches("claude sonnet 4.6"));
        assert!(Family::GptOss.matches("gpt-oss 120b"));
        assert!(Family::GptOss.matches("gpt-oss 120b (medium)"));
        assert!(!Family::GptOss.matches("gpt-oss 20b"));
    }

    #[test]
    fn table_shows_resolved_and_unavailable_aliases() {
        let t = format_model_table(Some(&listing()));
        assert!(t.starts_with("Aliases (case-insensitive), resolved against `agy models`:\n"));
        // Byte-identical to the 0.8.x/0.9-wip bash wrapper's live output.
        assert!(t.contains("  flash, flash-high             -> Gemini 3.8 Flash (High)\n"));
        assert!(t.contains(
            "  pro, pro-high                 -> (not available: newest Gemini Pro (High))\n"
        ));
        assert!(t.contains("-> (not available: newest Gemini Pro (High))"));
        assert!(t.contains("  Gemini 3.8 Flash (Low)            gemini-3.8-flash-low\n"));
        let u = format_model_table(None);
        assert!(u.contains("could not be read"));
        assert!(u.contains("-> newest Claude Opus"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_against_a_stub_agy() {
        use std::os::unix::fs::PermissionsExt;
        let d = util::TempDir::new("agy-models-test-").unwrap();
        let p = d.path().join("agy");
        std::fs::write(&p, "#!/bin/sh\n[ \"$1\" = models ] && printf 'Fetching available models...\\ngemini-3.8-flash-low\\tGemini 3.8 Flash (Low)\\n'\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let a = Agy::at(p);
        assert_eq!(resolve(&a, "flash-low").unwrap(), "gemini-3.8-flash-low");
        assert_eq!(resolve(&a, "nope").unwrap_err().code, 64);
        assert_eq!(resolve(&a, "  ").unwrap_err().code, 64);
        // Unlistable models: alias fails (1), full name passes unchecked.
        std::fs::write(d.path().join("agy"), "#!/bin/sh\nexit 1\n").unwrap();
        let a = Agy::at(d.path().join("agy"));
        assert_eq!(resolve(&a, "flash").unwrap_err().code, 1);
        assert_eq!(
            resolve(&a, "Some Future Model").unwrap(),
            "Some Future Model"
        );
    }
}

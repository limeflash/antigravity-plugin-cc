//! `--model` resolution. Aliases resolve at call time against `agy models`
//! to the NEWEST model in their family, and the result is passed to agy's own
//! `--model` flag for that one call (ported from upstream
//! simplybychris/antigravity-plugin-cc 0.5.0). 0.8.x instead patched the
//! "model" field of agy's settings.json under a lock: that flipped the model
//! of a TUI open in parallel and broke whenever Google renamed a model.
//!
//! Gemini aliases are not a fixed list: Google names Gemini models
//! `Gemini <version> <kind> (<level>)` and keeps adding kinds (Flash, Pro,
//! and code names like Hydrogen). Any kind that `agy models` lists is an
//! alias — `hydrogen`, `flash` — optionally with a level suffix
//! (`hydrogen-xhigh`, `flash-low`); `gemini` means the newest Gemini of any
//! kind.

use std::cmp::Ordering;
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

// ---------------------------------------------------------------- Gemini

/// Level words accepted in an alias suffix, and the level they mean.
const LEVEL_WORDS: &[(&str, &str)] = &[
    ("xhigh", "xhigh"),
    ("high", "high"),
    ("medium", "medium"),
    ("med", "medium"),
    ("low", "low"),
];

/// Display order of levels (a level agy invents later sorts last).
const LEVEL_ORDER: &[&str] = &["xhigh", "high", "medium", "low"];

/// Kinds that are always shown in the alias table, even when the account
/// doesn't list them, so `pro` explains itself instead of being "unknown".
const WELL_KNOWN_KINDS: &[&str] = &["flash", "pro"];

/// A parsed Gemini display name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GeminiName {
    /// "4", "3.8"
    version: String,
    /// Lowercase, words joined by '-': "hydrogen", "flash", "flash-lite";
    /// empty for a bare "Gemini 4 (High)".
    kind: String,
    /// Lowercase level from the trailing "(...)", or "" when there is none.
    level: String,
}

fn is_word(w: &str) -> bool {
    !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric())
}

/// "gemini <ver> [<kind words>] [(<level>)]", lowercased.
fn parse_gemini(lower: &str) -> Option<GeminiName> {
    let rest = lower.strip_prefix("gemini ")?;
    let (version, rest) = rest.split_once(' ').unwrap_or((rest, ""));
    if !is_version_word(version) {
        return None;
    }
    let (kind_part, level) = match rest.strip_suffix(')') {
        Some(head) => {
            let (kind_part, level) = match head.rsplit_once(" (") {
                Some(split) => split,
                None => ("", head.strip_prefix('(')?),
            };
            if !is_word(level) {
                return None;
            }
            (kind_part, level)
        }
        None => (rest, ""),
    };
    let words: Vec<&str> = kind_part.split(' ').filter(|w| !w.is_empty()).collect();
    if kind_part.contains("  ") || !words.iter().all(|w| is_word(w)) {
        return None;
    }
    Some(GeminiName {
        version: version.to_string(),
        kind: words.join("-"),
        level: level.to_string(),
    })
}

/// A Gemini alias: `gemini`, `<kind>`, either with `-<level>`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GeminiAlias {
    /// None for `gemini` (any kind).
    kind: Option<String>,
    /// None = the alias didn't name one (prefer High).
    level: Option<&'static str>,
}

fn level_word(w: &str) -> Option<&'static str> {
    LEVEL_WORDS.iter().find(|(k, _)| *k == w).map(|(_, v)| *v)
}

fn parse_gemini_alias(lc: &str) -> Option<GeminiAlias> {
    let (base, level) = match lc.rsplit_once('-') {
        Some((b, l)) if !b.is_empty() && level_word(l).is_some() => (b, level_word(l)),
        _ => (lc, None),
    };
    if !base.split('-').all(is_word) {
        return None;
    }
    let kind = (base != "gemini").then(|| base.to_string());
    Some(GeminiAlias { kind, level })
}

/// Preference among levels when the alias names none: High first (agy's
/// usual default), then an unlabelled model, then the rest.
fn default_level_rank(level: &str) -> usize {
    match level {
        "high" => 0,
        "" => 1,
        "xhigh" => 2,
        "medium" => 3,
        "low" => 4,
        _ => 5,
    }
}

fn level_order_rank(level: &str) -> usize {
    LEVEL_ORDER
        .iter()
        .position(|l| *l == level)
        .unwrap_or(LEVEL_ORDER.len())
}

/// Flash / Lite are the small models; for `gemini` a same-version big model
/// wins.
fn is_small_kind(kind: &str) -> bool {
    kind.split('-').any(|w| w == "flash" || w == "lite")
}

/// The newest model matching a Gemini alias. Ties: preferred level, then (for
/// `gemini`) a non-Flash kind, then agy's own listing order.
fn newest_gemini<'a>(models: &'a [Model], alias: &GeminiAlias) -> Option<&'a Model> {
    let mut best: Option<(&Model, GeminiName)> = None;
    for m in models {
        let Some(g) = parse_gemini(&m.name.to_lowercase()) else {
            continue;
        };
        if alias.kind.as_deref().is_some_and(|k| k != g.kind) {
            continue;
        }
        if alias.level.is_some_and(|l| l != g.level) {
            continue;
        }
        let better = match &best {
            None => true,
            Some((_, b)) => {
                agy::compare_versions(&g.version, &b.version)
                    .then_with(|| default_level_rank(&b.level).cmp(&default_level_rank(&g.level)))
                    .then_with(|| {
                        if alias.kind.is_none() {
                            is_small_kind(&b.kind).cmp(&is_small_kind(&g.kind))
                        } else {
                            Ordering::Equal
                        }
                    })
                    == Ordering::Greater
            }
        };
        if better {
            best = Some((m, g));
        }
    }
    best.map(|(m, _)| m)
}

fn title_case(kind: &str) -> String {
    kind.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn level_label(level: &str) -> String {
    if level == "xhigh" {
        "XHigh".to_string()
    } else {
        title_case(level)
    }
}

fn gemini_description(alias: &GeminiAlias) -> String {
    let what = match &alias.kind {
        None => "newest Gemini (any kind)".to_string(),
        Some(k) => format!("newest Gemini {}", title_case(k)),
    };
    match alias.level {
        Some(l) => format!("{what} ({})", level_label(l)),
        None => what,
    }
}

// ------------------------------------------------- Claude, GPT-OSS (fixed)

#[derive(Clone, Copy, Debug)]
enum Family {
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
        aliases: &["opus", "claude-opus"],
        family: Family::Claude { kind: "opus" },
        prefer_suffix: Some("(thinking)"),
        description: "newest Claude Opus",
    },
    AliasRow {
        aliases: &["sonnet", "claude-sonnet"],
        family: Family::Claude { kind: "sonnet" },
        prefer_suffix: Some("(thinking)"),
        description: "newest Claude Sonnet",
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
/// numeric word ("claude opus 4.6 (thinking)" -> "4.6"), or "".
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
                Ordering::Greater => true,
                Ordering::Equal => preferred && !bp,
                Ordering::Less => false,
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

/// Gemini kinds `agy models` lists, newest version first (ties: listing
/// order), each with the levels of its newest version.
fn listed_gemini_kinds(models: &[Model]) -> Vec<(String, Vec<String>)> {
    let parsed: Vec<GeminiName> = models
        .iter()
        .filter_map(|m| parse_gemini(&m.name.to_lowercase()))
        .filter(|g| !g.kind.is_empty())
        .collect();
    let mut kinds: Vec<(String, String)> = Vec::new(); // (kind, newest version)
    for g in &parsed {
        match kinds.iter_mut().find(|(k, _)| *k == g.kind) {
            Some((_, v)) => {
                if agy::compare_versions(&g.version, v) == Ordering::Greater {
                    *v = g.version.clone();
                }
            }
            None => kinds.push((g.kind.clone(), g.version.clone())),
        }
    }
    // Stable sort: newest version first, listing order within a version.
    kinds.sort_by(|a, b| agy::compare_versions(&b.1, &a.1));
    kinds
        .into_iter()
        .map(|(kind, version)| {
            let mut levels: Vec<String> = Vec::new();
            for g in &parsed {
                if g.kind == kind
                    && g.version == version
                    && !g.level.is_empty()
                    && !levels.contains(&g.level)
                {
                    levels.push(g.level.clone());
                }
            }
            levels.sort_by_key(|l| level_order_rank(l));
            (kind, levels)
        })
        .collect()
}

/// Is `input` alias-shaped in a way that needs `agy models` to resolve (as
/// opposed to a model name to pass through unchecked)?
fn needs_listing(input: &str) -> bool {
    if alias_row(input).is_some() {
        return true;
    }
    parse_gemini_alias(&input.to_ascii_lowercase()).is_some_and(|a| match a.kind {
        None => true,
        Some(k) => WELL_KNOWN_KINDS.contains(&k.as_str()),
    })
}

/// Resolve against a model list (pure). Exact ids/names win over aliases.
pub(crate) fn resolve_from_list(models: &[Model], input: &str) -> Result<String, String> {
    if let Some(m) = exact_match(models, input) {
        return Ok(m.arg().to_string());
    }
    if let Some(row) = alias_row(input) {
        return newest_match(models, row)
            .map(|m| m.arg().to_string())
            .ok_or_else(|| {
                format!(
                    "error: alias '{input}' has no matching model in `agy models` (expected: {})",
                    row.description
                )
            });
    }
    if let Some(alias) = parse_gemini_alias(&input.to_ascii_lowercase()) {
        if let Some(m) = newest_gemini(models, &alias) {
            return Ok(m.arg().to_string());
        }
        // A kind we know of, or one listed at other levels: say what's missing.
        let known = match &alias.kind {
            None => true,
            Some(k) => {
                WELL_KNOWN_KINDS.contains(&k.as_str())
                    || listed_gemini_kinds(models).iter().any(|(lk, _)| lk == k)
            }
        };
        if known {
            return Err(format!(
                "error: alias '{input}' has no matching model in `agy models` (expected: {})",
                gemini_description(&alias)
            ));
        }
    }
    Err(format!(
        "error: unknown model '{input}' (not an alias and not listed by `agy models`)"
    ))
}

const LEVEL_HINT: &str = "Gemini aliases take a level suffix: -xhigh, -high (the default), -medium, -low\n(e.g. hydrogen-xhigh, flash-low, gemini-medium); `gemini` is the newest Gemini of any kind.\n";

/// Alias + model table for errors and `help`.
pub(crate) fn format_model_table(models: Option<&[Model]>) -> String {
    let listed = models.filter(|m| !m.is_empty());
    let mut rows: Vec<(String, String)> = Vec::new();
    match listed {
        None => {
            rows.push(("gemini".into(), "newest Gemini of any kind".into()));
            for k in WELL_KNOWN_KINDS {
                rows.push(((*k).into(), format!("newest Gemini {}", title_case(k))));
            }
            rows.push((
                "<kind>".into(),
                "newest Gemini of that kind, e.g. hydrogen".into(),
            ));
            for row in ALIASES {
                rows.push((row.aliases.join(", "), row.description.into()));
            }
        }
        Some(ms) => {
            let any = GeminiAlias {
                kind: None,
                level: None,
            };
            rows.push((
                "gemini".into(),
                newest_gemini(ms, &any)
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| format!("(not available: {})", gemini_description(&any))),
            ));
            let mut kinds = listed_gemini_kinds(ms);
            for k in WELL_KNOWN_KINDS {
                if !kinds.iter().any(|(lk, _)| lk == k) {
                    kinds.push(((*k).to_string(), Vec::new()));
                }
            }
            for (kind, levels) in kinds {
                let alias = GeminiAlias {
                    kind: Some(kind.clone()),
                    level: None,
                };
                let target = match newest_gemini(ms, &alias) {
                    Some(m) if levels.len() > 1 => {
                        format!("{}   [levels: {}]", m.name, levels.join(", "))
                    }
                    Some(m) => m.name.clone(),
                    None => format!("(not available: {})", gemini_description(&alias)),
                };
                rows.push((kind, target));
            }
            for row in ALIASES {
                let target = match newest_match(ms, row) {
                    Some(m) => m.name.clone(),
                    None => format!("(not available: {})", row.description),
                };
                rows.push((row.aliases.join(", "), target));
            }
        }
    }
    let mut out = String::new();
    out.push_str(if listed.is_some() {
        "Aliases (case-insensitive), resolved against `agy models`:\n"
    } else {
        "Aliases (case-insensitive). `agy models` could not be read, so they are shown unresolved:\n"
    });
    for (alias, target) in rows {
        out.push_str(&format!("  {alias:<28}  -> {target}\n"));
    }
    out.push_str(LEVEL_HINT);
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
        if needs_listing(input) {
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
        "error: --model requires a non-empty value (e.g. --model gemini)\n{}",
        format_model_table(models.as_deref()).trim_end()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `agy models` as agy 1.5.16 printed it on 2026-11-25, plus an older
    /// Flash and a non-thinking Sonnet to exercise the tie rules.
    fn listing() -> Vec<Model> {
        parse_model_list(
            "Fetching available models...\n\
             gemini-4-hydrogen-high\tGemini 4 Hydrogen (High)\n\
             gemini-4-hydrogen-xhigh\tGemini 4 Hydrogen (XHigh)\n\
             gemini-4-hydrogen-medium\tGemini 4 Hydrogen (Medium)\n\
             gemini-4-hydrogen-low\tGemini 4 Hydrogen (Low)\n\
             gemini-3.5-flash-high\tGemini 3.5 Flash (High)\n\
             gemini-3.8-flash-high\tGemini 3.8 Flash (High)\n\
             gemini-3.8-flash-medium\tGemini 3.8 Flash (Medium)\n\
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
        assert_eq!(m.len(), 12);
        assert_eq!(
            m[0],
            Model {
                id: "gemini-4-hydrogen-high".into(),
                name: "Gemini 4 Hydrogen (High)".into()
            }
        );
        assert_eq!(m[7].name, "Gemini 3.8 Flash (Low)", "CRLF stripped");
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
    fn gemini_names_parse() {
        let g = |s: &str| parse_gemini(s).map(|g| (g.version, g.kind, g.level));
        let t = |v: &str, k: &str, l: &str| Some((v.into(), k.into(), l.into()));
        assert_eq!(g("gemini 4 hydrogen (xhigh)"), t("4", "hydrogen", "xhigh"));
        assert_eq!(g("gemini 3.8 flash (high)"), t("3.8", "flash", "high"));
        assert_eq!(
            g("gemini 3.8 flash lite (low)"),
            t("3.8", "flash-lite", "low")
        );
        assert_eq!(g("gemini 4 carbon"), t("4", "carbon", ""));
        assert_eq!(g("gemini 4 (high)"), t("4", "", "high"));
        assert_eq!(g("gemini 4"), t("4", "", ""));
        assert_eq!(g("gemini flash (high)"), None, "no version");
        assert_eq!(g("gemini 3.8 flash (high) extra"), None);
        assert_eq!(g("gemini 3.8 flash (very high)"), None);
        assert_eq!(g("gemini 3.8 fl@sh (high)"), None);
        assert_eq!(g("gemini 3. flash (high)"), None, "strict version");
        assert_eq!(g("claude opus 4.6"), None);
    }

    #[test]
    fn gemini_aliases_parse() {
        let a = |s: &str| parse_gemini_alias(s);
        let k = |kind: Option<&str>, level: Option<&'static str>| {
            Some(GeminiAlias {
                kind: kind.map(str::to_string),
                level,
            })
        };
        assert_eq!(a("gemini"), k(None, None));
        assert_eq!(a("gemini-xhigh"), k(None, Some("xhigh")));
        assert_eq!(a("hydrogen"), k(Some("hydrogen"), None));
        assert_eq!(a("flash-med"), k(Some("flash"), Some("medium")));
        assert_eq!(a("flash-lite"), k(Some("flash-lite"), None));
        assert_eq!(a("flash-lite-low"), k(Some("flash-lite"), Some("low")));
        assert_eq!(a("high"), k(Some("high"), None), "a lone level is a kind");
        assert_eq!(a("-low"), None);
        assert_eq!(a("flash--low"), None);
        assert_eq!(a("gemini 4 hydrogen"), None);
    }

    #[test]
    fn aliases_pick_the_newest_in_family() {
        let m = listing();
        let r = |s: &str| resolve_from_list(&m, s).unwrap();
        assert_eq!(r("gemini"), "gemini-4-hydrogen-high");
        assert_eq!(r("gemini-low"), "gemini-4-hydrogen-low");
        assert_eq!(r("hydrogen"), "gemini-4-hydrogen-high");
        assert_eq!(r("Hydrogen-XHigh"), "gemini-4-hydrogen-xhigh");
        assert_eq!(r("hydrogen-med"), "gemini-4-hydrogen-medium");
        assert_eq!(r("flash"), "gemini-3.8-flash-high");
        assert_eq!(r("flash-high"), "gemini-3.8-flash-high");
        assert_eq!(r("flash-low"), "gemini-3.8-flash-low");
        assert_eq!(r("OPUS"), "claude-opus-4-6-thinking");
        assert_eq!(r("gpt-oss"), "gpt-oss-120b-medium");
    }

    #[test]
    fn a_new_kind_works_without_a_code_change() {
        let m = parse_model_list(
            "h\tGemini 4 Hydrogen (High)\nc\tGemini 4.1 Carbon (High)\ncl\tGemini 4.1 Carbon (Low)\n",
        );
        assert_eq!(resolve_from_list(&m, "carbon").unwrap(), "c");
        assert_eq!(resolve_from_list(&m, "carbon-low").unwrap(), "cl");
        assert_eq!(resolve_from_list(&m, "gemini").unwrap(), "c", "4.1 > 4");
    }

    #[test]
    fn gemini_ties() {
        // Same version: a big model beats Flash for `gemini`; listing order
        // decides between two big ones.
        let m = parse_model_list(
            "f\tGemini 4 Flash (High)\nh\tGemini 4 Hydrogen (High)\nc\tGemini 4 Carbon (High)\n",
        );
        assert_eq!(resolve_from_list(&m, "gemini").unwrap(), "h");
        // No level given: High beats XHigh and an unlabelled model.
        let m = parse_model_list(
            "x\tGemini 4 Neon (XHigh)\nn\tGemini 4 Neon\nh\tGemini 4 Neon (High)\n",
        );
        assert_eq!(resolve_from_list(&m, "neon").unwrap(), "h");
        let m = parse_model_list("x\tGemini 4 Neon (XHigh)\nl\tGemini 4 Neon (Low)\n");
        assert_eq!(resolve_from_list(&m, "neon").unwrap(), "x");
        // A newer version beats a preferred level.
        let m = parse_model_list("a\tGemini 3.8 Flash (High)\nb\tGemini 3.10 Flash (Low)\n");
        assert_eq!(resolve_from_list(&m, "flash").unwrap(), "b", "3.10 > 3.8");
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
        assert_eq!(
            resolve_from_list(&m, "gemini 4 hydrogen (xhigh)").unwrap(),
            "gemini-4-hydrogen-xhigh"
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
        let e = |s: &str| resolve_from_list(&m, s).unwrap_err();
        assert!(e("nope").contains("unknown model 'nope'"));
        assert!(e("carbon").contains("unknown model 'carbon'"));
        assert!(e("pro").contains("no matching model"));
        assert!(e("pro").contains("expected: newest Gemini Pro)"));
        assert!(e("pro-low").contains("expected: newest Gemini Pro (Low)"));
        assert!(e("flash-xhigh").contains("expected: newest Gemini Flash (XHigh)"));
        assert!(e("opus-low").contains("unknown model"));
        assert!(
            resolve_from_list(&[], "gemini")
                .unwrap_err()
                .contains("newest Gemini (any kind)")
        );
    }

    #[test]
    fn claude_and_gpt_family_matching_is_exact() {
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
        assert!(t.contains("  gemini                        -> Gemini 4 Hydrogen (High)\n"));
        assert!(t.contains(
            "  hydrogen                      -> Gemini 4 Hydrogen (High)   [levels: xhigh, high, medium, low]\n"
        ));
        assert!(t.contains(
            "  flash                         -> Gemini 3.8 Flash (High)   [levels: high, medium, low]\n"
        ));
        assert!(
            t.contains("  pro                           -> (not available: newest Gemini Pro)\n")
        );
        assert!(t.contains("  opus, claude-opus             -> Claude Opus 4.6 (Thinking)\n"));
        assert!(
            t.find("hydrogen   ").unwrap() < t.find("flash   ").unwrap(),
            "newest kind first"
        );
        assert!(t.contains("level suffix"));
        assert!(t.contains("  Gemini 3.8 Flash (Low)            gemini-3.8-flash-low\n"));
        let u = format_model_table(None);
        assert!(u.contains("could not be read"));
        assert!(u.contains("-> newest Claude Opus"));
        assert!(u.contains("<kind>"));
    }

    #[test]
    fn only_alias_shaped_inputs_need_the_listing() {
        assert!(needs_listing("gemini"));
        assert!(needs_listing("flash-low"));
        assert!(needs_listing("opus"));
        assert!(
            !needs_listing("hydrogen"),
            "may be a model name; agy decides"
        );
        assert!(!needs_listing("Some Future Model"));
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

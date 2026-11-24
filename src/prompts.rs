//! Prompt templates for /agy:review and /agy:adversarial-review
//! (port of lib/prompts.mjs, byte-identical output).
//!
//! Design A+: the diff and the full changed files live on disk in a staging
//! dir agy can read (passed via --add-dir); the prompt only POINTS at them.
//! That keeps the argv prompt tiny (no ENAMETOOLONG, no truncation) and gives
//! agy whole files for context, which kills "X not imported" / "missing
//! guard" false positives caused by seeing only a hunk.

use crate::cli::js_trim;

/// What is being reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Uncommitted changes against HEAD.
    WorkingTree,
    /// HEAD against the merge-base with `base`.
    Branch {
        base: String,
        merge_base: Option<String>,
    },
}

/// A changed file that was not staged for agy, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Omitted {
    pub path: String,
    pub reason: String,
}

const PREAMBLE_REVIEW: &str = "Review the following diff. Be specific and actionable. Skip nits unless they materially affect correctness or safety. Group findings by severity.";

const PREAMBLE_ADVERSARIAL: &str = "Pressure-test the diff below. Your job is to challenge the implementation, NOT to validate it. Question the chosen approach, surface hidden assumptions, name failure modes the author didn't address, and propose at least one alternative design with concrete tradeoffs vs the chosen one. If the diff is sound, say so explicitly — but make the reader work for that conclusion.";

const REVIEW_RULES: [&str; 5] = [
    "Group findings under: ## Correctness, ## Security, ## Edge cases, ## Style. Omit empty sections.",
    "For each finding cite the file path and (ideally) the line number from the diff.",
    "Prefer concrete code examples for suggested fixes over prose.",
    "Be explicit about confidence: HIGH for clear bugs, MEDIUM for likely issues, LOW for guesses.",
    "Do NOT propose unrelated refactors; review the diff as it stands.",
];

const ADVERSARIAL_RULES: [&str; 5] = [
    "Frame at least 3 questions the diff fails to answer.",
    "Identify the assumption you think most likely to break, and why.",
    "Propose one materially different design (not just 'rename this'); compare tradeoffs in 3-5 bullets.",
    "Call out the failure modes that aren't covered by tests in the diff.",
    "Conclude with a one-paragraph verdict: ship / change / rethink.",
];

fn join_rules(rules: &[&str]) -> String {
    rules
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}. {r}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Same reasons the JS regex `/too large|binary|symlink|outside/` matched:
/// files agy cannot see in full, so it must judge them from the diff alone.
fn is_big_reason(reason: &str) -> bool {
    ["too large", "binary", "symlink", "outside"]
        .iter()
        .any(|k| reason.contains(k))
}

fn staged_materials_block(stage_dir: &str, staged: &[String], omitted: &[Omitted]) -> String {
    // The prompt is read by agy on the same machine, so match the path
    // style of stage_dir itself (Windows temp dirs use backslashes).
    let sep = if stage_dir.contains('\\') { "\\" } else { "/" };
    let diff_path = format!("{stage_dir}{sep}diff.patch");
    let files_dir = format!("{stage_dir}{sep}files");
    let mut parts: Vec<String> = vec![
        String::new(),
        "## Material to review (read these files with your tools — they are on disk, not inline)"
            .to_string(),
        format!("- The unified diff (with expanded context) is at:\n  {diff_path}"),
    ];
    if !staged.is_empty() {
        parts.push(format!(
            "- The FULL current content of the changed files is under:\n  {files_dir}{sep}<same relative path>"
        ));
        parts.push("  Read the ones you need for context. Do NOT flag an issue (missing import, undefined name, missing guard) as a bug unless you've checked the full file and confirmed it — the definition is often outside the diff hunk.".to_string());
        parts.push("  Changed files staged for you:".to_string());
        parts.extend(staged.iter().map(|f| format!("    - {f}")));
    }
    let big: Vec<String> = omitted
        .iter()
        .filter(|o| is_big_reason(&o.reason))
        .map(|o| format!("{} [{}]", o.path, o.reason))
        .collect();
    if !big.is_empty() {
        parts.push(format!(
            "  (Not staged: {} — judge those from the diff alone, and mark cross-context concerns as [UNVERIFIED].)",
            big.join(", ")
        ));
    }
    parts.join("\n")
}

fn assemble(
    preamble: &str,
    rules: &[&str],
    scope_blurb: String,
    focus_block: String,
    stage_dir: &str,
    staged: &[String],
    omitted: &[Omitted],
) -> String {
    [
        preamble.to_string(),
        String::new(),
        "Rules:".to_string(),
        join_rules(rules),
        String::new(),
        scope_blurb,
        focus_block,
        staged_materials_block(stage_dir, staged, omitted),
        String::new(),
    ]
    .join("\n")
}

/// Prompt for `/agy:review`. `focus` empty means no focus block; a
/// whitespace-only focus still emits the (empty) block, as the JS did.
pub(crate) fn build_review_prompt(
    scope: &Scope,
    focus: &str,
    stage_dir: &str,
    staged: &[String],
    omitted: &[Omitted],
) -> String {
    let scope_blurb = match scope {
        Scope::Branch { base, merge_base } => format!(
            "Branch review: HEAD compared against merge-base of HEAD and `{base}` (resolved sha: {}).",
            merge_base.as_deref().unwrap_or("?")
        ),
        Scope::WorkingTree => {
            "Working-tree review: uncommitted changes against `HEAD`.".to_string()
        }
    };
    let focus_block = if focus.is_empty() {
        String::new()
    } else {
        format!("\n\nUser focus / extra steer:\n{}", js_trim(focus))
    };
    assemble(
        PREAMBLE_REVIEW,
        &REVIEW_RULES,
        scope_blurb,
        focus_block,
        stage_dir,
        staged,
        omitted,
    )
}

/// Prompt for `/agy:adversarial-review`: same staged materials, different
/// stance and rules.
pub(crate) fn build_adversarial_prompt(
    scope: &Scope,
    focus: &str,
    stage_dir: &str,
    staged: &[String],
    omitted: &[Omitted],
) -> String {
    let scope_blurb = match scope {
        Scope::Branch { base, .. } => {
            format!("Branch under challenge: HEAD vs merge-base of HEAD and `{base}`.")
        }
        Scope::WorkingTree => "Working-tree changes under challenge.".to_string(),
    };
    let focus_block = if focus.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nWhere the user wants the most pressure:\n{}",
            js_trim(focus)
        )
    };
    assemble(
        PREAMBLE_ADVERSARIAL,
        &ADVERSARIAL_RULES,
        scope_blurb,
        focus_block,
        stage_dir,
        staged,
        omitted,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected prompts below were generated by running the original
    // lib/prompts.mjs under Node (scratchpad gen_prompts.mjs), so these
    // tests pin byte-identity with the JS runtime.
    const JS_REVIEW_WT: &str = "Review the following diff. Be specific and actionable. Skip nits unless they materially affect correctness or safety. Group findings by severity.\n\nRules:\n1. Group findings under: ## Correctness, ## Security, ## Edge cases, ## Style. Omit empty sections.\n2. For each finding cite the file path and (ideally) the line number from the diff.\n3. Prefer concrete code examples for suggested fixes over prose.\n4. Be explicit about confidence: HIGH for clear bugs, MEDIUM for likely issues, LOW for guesses.\n5. Do NOT propose unrelated refactors; review the diff as it stands.\n\nWorking-tree review: uncommitted changes against `HEAD`.\n\n\n## Material to review (read these files with your tools \u{2014} they are on disk, not inline)\n- The unified diff (with expanded context) is at:\n  /tmp/agy-review-xyz/diff.patch\n- The FULL current content of the changed files is under:\n  /tmp/agy-review-xyz/files/<same relative path>\n  Read the ones you need for context. Do NOT flag an issue (missing import, undefined name, missing guard) as a bug unless you've checked the full file and confirmed it \u{2014} the definition is often outside the diff hunk.\n  Changed files staged for you:\n    - foo.js\n";

    const JS_REVIEW_BRANCH_OMITTED: &str = "Review the following diff. Be specific and actionable. Skip nits unless they materially affect correctness or safety. Group findings by severity.\n\nRules:\n1. Group findings under: ## Correctness, ## Security, ## Edge cases, ## Style. Omit empty sections.\n2. For each finding cite the file path and (ideally) the line number from the diff.\n3. Prefer concrete code examples for suggested fixes over prose.\n4. Be explicit about confidence: HIGH for clear bugs, MEDIUM for likely issues, LOW for guesses.\n5. Do NOT propose unrelated refactors; review the diff as it stands.\n\nBranch review: HEAD compared against merge-base of HEAD and `main` (resolved sha: fff0000).\n\n\nUser focus / extra steer:\nerror handling\n\n## Material to review (read these files with your tools \u{2014} they are on disk, not inline)\n- The unified diff (with expanded context) is at:\n  /tmp/agy-review-xyz/diff.patch\n- The FULL current content of the changed files is under:\n  /tmp/agy-review-xyz/files/<same relative path>\n  Read the ones you need for context. Do NOT flag an issue (missing import, undefined name, missing guard) as a bug unless you've checked the full file and confirmed it \u{2014} the definition is often outside the diff hunk.\n  Changed files staged for you:\n    - bar.ts\n    - bar.test.ts\n  (Not staged: huge.bin [binary], ln [symlink], big.json [too large (2 MB)], ../x [outside repo] \u{2014} judge those from the diff alone, and mark cross-context concerns as [UNVERIFIED].)\n";

    const JS_REVIEW_BRANCH_WS_FOCUS: &str = "Review the following diff. Be specific and actionable. Skip nits unless they materially affect correctness or safety. Group findings by severity.\n\nRules:\n1. Group findings under: ## Correctness, ## Security, ## Edge cases, ## Style. Omit empty sections.\n2. For each finding cite the file path and (ideally) the line number from the diff.\n3. Prefer concrete code examples for suggested fixes over prose.\n4. Be explicit about confidence: HIGH for clear bugs, MEDIUM for likely issues, LOW for guesses.\n5. Do NOT propose unrelated refactors; review the diff as it stands.\n\nBranch review: HEAD compared against merge-base of HEAD and `origin/dev` (resolved sha: ?).\n\n\nUser focus / extra steer:\n\n\n## Material to review (read these files with your tools \u{2014} they are on disk, not inline)\n- The unified diff (with expanded context) is at:\n  /tmp/s/diff.patch\n";

    const JS_REVIEW_WINDOWS: &str = "Review the following diff. Be specific and actionable. Skip nits unless they materially affect correctness or safety. Group findings by severity.\n\nRules:\n1. Group findings under: ## Correctness, ## Security, ## Edge cases, ## Style. Omit empty sections.\n2. For each finding cite the file path and (ideally) the line number from the diff.\n3. Prefer concrete code examples for suggested fixes over prose.\n4. Be explicit about confidence: HIGH for clear bugs, MEDIUM for likely issues, LOW for guesses.\n5. Do NOT propose unrelated refactors; review the diff as it stands.\n\nWorking-tree review: uncommitted changes against `HEAD`.\n\n\n## Material to review (read these files with your tools \u{2014} they are on disk, not inline)\n- The unified diff (with expanded context) is at:\n  C:\\Temp\\agy-review-xyz\\diff.patch\n- The FULL current content of the changed files is under:\n  C:\\Temp\\agy-review-xyz\\files\\<same relative path>\n  Read the ones you need for context. Do NOT flag an issue (missing import, undefined name, missing guard) as a bug unless you've checked the full file and confirmed it \u{2014} the definition is often outside the diff hunk.\n  Changed files staged for you:\n    - foo.js\n    - dir\\a.js\n  (Not staged: img.png [binary] \u{2014} judge those from the diff alone, and mark cross-context concerns as [UNVERIFIED].)\n";

    const JS_ADV_WT: &str = "Pressure-test the diff below. Your job is to challenge the implementation, NOT to validate it. Question the chosen approach, surface hidden assumptions, name failure modes the author didn't address, and propose at least one alternative design with concrete tradeoffs vs the chosen one. If the diff is sound, say so explicitly \u{2014} but make the reader work for that conclusion.\n\nRules:\n1. Frame at least 3 questions the diff fails to answer.\n2. Identify the assumption you think most likely to break, and why.\n3. Propose one materially different design (not just 'rename this'); compare tradeoffs in 3-5 bullets.\n4. Call out the failure modes that aren't covered by tests in the diff.\n5. Conclude with a one-paragraph verdict: ship / change / rethink.\n\nWorking-tree changes under challenge.\n\n\n## Material to review (read these files with your tools \u{2014} they are on disk, not inline)\n- The unified diff (with expanded context) is at:\n  /tmp/agy-review-xyz/diff.patch\n";

    const JS_ADV_BRANCH: &str = "Pressure-test the diff below. Your job is to challenge the implementation, NOT to validate it. Question the chosen approach, surface hidden assumptions, name failure modes the author didn't address, and propose at least one alternative design with concrete tradeoffs vs the chosen one. If the diff is sound, say so explicitly \u{2014} but make the reader work for that conclusion.\n\nRules:\n1. Frame at least 3 questions the diff fails to answer.\n2. Identify the assumption you think most likely to break, and why.\n3. Propose one materially different design (not just 'rename this'); compare tradeoffs in 3-5 bullets.\n4. Call out the failure modes that aren't covered by tests in the diff.\n5. Conclude with a one-paragraph verdict: ship / change / rethink.\n\nBranch under challenge: HEAD vs merge-base of HEAD and `main`.\n\n\nWhere the user wants the most pressure:\nthe retry/backoff design\n\n## Material to review (read these files with your tools \u{2014} they are on disk, not inline)\n- The unified diff (with expanded context) is at:\n  /tmp/agy-review-xyz/diff.patch\n- The FULL current content of the changed files is under:\n  /tmp/agy-review-xyz/files/<same relative path>\n  Read the ones you need for context. Do NOT flag an issue (missing import, undefined name, missing guard) as a bug unless you've checked the full file and confirmed it \u{2014} the definition is often outside the diff hunk.\n  Changed files staged for you:\n    - bar.ts\n    - bar.test.ts\n  (Not staged: huge.bin [binary] \u{2014} judge those from the diff alone, and mark cross-context concerns as [UNVERIFIED].)\n";

    const JS_ADV_WINDOWS_TRIM: &str = "Pressure-test the diff below. Your job is to challenge the implementation, NOT to validate it. Question the chosen approach, surface hidden assumptions, name failure modes the author didn't address, and propose at least one alternative design with concrete tradeoffs vs the chosen one. If the diff is sound, say so explicitly \u{2014} but make the reader work for that conclusion.\n\nRules:\n1. Frame at least 3 questions the diff fails to answer.\n2. Identify the assumption you think most likely to break, and why.\n3. Propose one materially different design (not just 'rename this'); compare tradeoffs in 3-5 bullets.\n4. Call out the failure modes that aren't covered by tests in the diff.\n5. Conclude with a one-paragraph verdict: ship / change / rethink.\n\nWorking-tree changes under challenge.\n\n\nWhere the user wants the most pressure:\nx\u{85}\n\n## Material to review (read these files with your tools \u{2014} they are on disk, not inline)\n- The unified diff (with expanded context) is at:\n  C:\\Temp\\s\\diff.patch\n- The FULL current content of the changed files is under:\n  C:\\Temp\\s\\files\\<same relative path>\n  Read the ones you need for context. Do NOT flag an issue (missing import, undefined name, missing guard) as a bug unless you've checked the full file and confirmed it \u{2014} the definition is often outside the diff hunk.\n  Changed files staged for you:\n    - a\n";

    const JS_REVIEW_UNICODE: &str = "Review the following diff. Be specific and actionable. Skip nits unless they materially affect correctness or safety. Group findings by severity.\n\nRules:\n1. Group findings under: ## Correctness, ## Security, ## Edge cases, ## Style. Omit empty sections.\n2. For each finding cite the file path and (ideally) the line number from the diff.\n3. Prefer concrete code examples for suggested fixes over prose.\n4. Be explicit about confidence: HIGH for clear bugs, MEDIUM for likely issues, LOW for guesses.\n5. Do NOT propose unrelated refactors; review the diff as it stands.\n\nBranch review: HEAD compared against merge-base of HEAD and `фича/ветка` (resolved sha: ?).\n\n\nUser focus / extra steer:\nмногострочный\nфокус 😀\n\n## Material to review (read these files with your tools \u{2014} they are on disk, not inline)\n- The unified diff (with expanded context) is at:\n  /tmp/путь с пробелом/diff.patch\n- The FULL current content of the changed files is under:\n  /tmp/путь с пробелом/files/<same relative path>\n  Read the ones you need for context. Do NOT flag an issue (missing import, undefined name, missing guard) as a bug unless you've checked the full file and confirmed it \u{2014} the definition is often outside the diff hunk.\n  Changed files staged for you:\n    - файл.rs\n    - 😀.md\n  (Not staged: x [binarysymlink] \u{2014} judge those from the diff alone, and mark cross-context concerns as [UNVERIFIED].)\n";

    const STAGE: &str = "/tmp/agy-review-xyz";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn om(path: &str, reason: &str) -> Omitted {
        Omitted {
            path: path.to_string(),
            reason: reason.to_string(),
        }
    }

    fn branch(base: &str, mb: Option<&str>) -> Scope {
        Scope::Branch {
            base: base.to_string(),
            merge_base: mb.map(str::to_string),
        }
    }

    // --- byte-identity with Node ---

    #[test]
    fn js_review_working_tree() {
        let p = build_review_prompt(&Scope::WorkingTree, "", STAGE, &s(&["foo.js"]), &[]);
        assert_eq!(p, JS_REVIEW_WT);
    }

    #[test]
    fn js_review_branch_with_omitted() {
        let omitted = [
            om("huge.bin", "binary"),
            om("gone.txt", "deleted"),
            om("ln", "symlink"),
            om("big.json", "too large (2 MB)"),
            om("../x", "outside repo"),
        ];
        let p = build_review_prompt(
            &branch("main", Some("fff0000")),
            "  error handling \n",
            STAGE,
            &s(&["bar.ts", "bar.test.ts"]),
            &omitted,
        );
        assert_eq!(p, JS_REVIEW_BRANCH_OMITTED);
    }

    #[test]
    fn js_review_branch_whitespace_focus_no_merge_base() {
        let p = build_review_prompt(&branch("origin/dev", None), "   ", "/tmp/s", &[], &[]);
        assert_eq!(p, JS_REVIEW_BRANCH_WS_FOCUS);
    }

    #[test]
    fn js_review_windows_paths() {
        let p = build_review_prompt(
            &Scope::WorkingTree,
            "",
            "C:\\Temp\\agy-review-xyz",
            &s(&["foo.js", "dir\\a.js"]),
            &[om("img.png", "binary")],
        );
        assert_eq!(p, JS_REVIEW_WINDOWS);
    }

    #[test]
    fn js_adversarial_working_tree() {
        let p = build_adversarial_prompt(&Scope::WorkingTree, "", STAGE, &[], &[]);
        assert_eq!(p, JS_ADV_WT);
    }

    #[test]
    fn js_adversarial_branch() {
        let p = build_adversarial_prompt(
            &branch("main", Some("fff0000")),
            "the retry/backoff design",
            STAGE,
            &s(&["bar.ts", "bar.test.ts"]),
            &[om("huge.bin", "binary")],
        );
        assert_eq!(p, JS_ADV_BRANCH);
    }

    #[test]
    fn js_adversarial_windows_and_js_trim() {
        // U+FEFF is trimmed by JS but not by str::trim; U+0085 the reverse.
        // The reason "Binary" does not match the case-sensitive JS regex.
        let p = build_adversarial_prompt(
            &Scope::WorkingTree,
            "\u{feff} x\u{85} \u{3000}",
            "C:\\Temp\\s",
            &s(&["a"]),
            &[om("q", "Binary")],
        );
        assert_eq!(p, JS_ADV_WINDOWS_TRIM);
    }

    #[test]
    fn js_review_unicode() {
        let p = build_review_prompt(
            &branch("фича/ветка", None),
            "многострочный\nфокус 😀",
            "/tmp/путь с пробелом",
            &s(&["файл.rs", "😀.md"]),
            &[om("x", "binarysymlink")],
        );
        assert_eq!(p, JS_REVIEW_UNICODE);
    }

    // --- ported from tests/lib.prompts.test.mjs ---

    #[test]
    fn includes_preamble_and_rules() {
        let p = build_review_prompt(&Scope::WorkingTree, "", STAGE, &s(&["foo.js"]), &[]);
        assert!(p.contains(&PREAMBLE_REVIEW[..30]));
        for r in REVIEW_RULES {
            assert!(p.contains(r), "{r}");
        }
    }

    #[test]
    fn scope_blurbs() {
        let wt = build_review_prompt(&Scope::WorkingTree, "", STAGE, &[], &[]);
        assert!(wt.contains("Working-tree review"));
        let b = build_review_prompt(
            &branch("main", Some("fff0000")),
            "",
            STAGE,
            &s(&["bar.ts", "bar.test.ts"]),
            &[],
        );
        assert!(b.contains("Branch review"));
        assert!(b.contains("`main`"));
        assert!(b.contains("fff0000"));
        let nb = build_review_prompt(&branch("main", None), "", STAGE, &[], &[]);
        assert!(nb.contains("(resolved sha: ?)"));
    }

    #[test]
    fn points_at_staged_materials() {
        let p = build_review_prompt(
            &branch("main", Some("fff0000")),
            "",
            STAGE,
            &s(&["bar.ts", "bar.test.ts"]),
            &[],
        );
        assert!(p.contains("/tmp/agy-review-xyz/diff.patch"));
        assert!(p.contains("/tmp/agy-review-xyz/files"));
        assert!(p.contains("- bar.ts"));
        assert!(p.contains("- bar.test.ts"));
        assert!(!p.contains("```diff"));
    }

    #[test]
    fn no_staged_files_omits_file_section() {
        let p = build_review_prompt(&Scope::WorkingTree, "", STAGE, &[], &[]);
        assert!(p.contains("diff.patch"));
        assert!(!p.contains("Changed files staged for you"));
        assert!(!p.contains("/files"));
    }

    #[test]
    fn omitted_files_marked_unverified() {
        let p = build_review_prompt(
            &branch("main", Some("fff0000")),
            "",
            STAGE,
            &s(&["bar.ts"]),
            &[om("huge.bin", "binary")],
        );
        assert!(p.contains("huge.bin"));
        assert!(p.contains("UNVERIFIED"));
    }

    #[test]
    fn omitted_with_other_reasons_not_listed() {
        let p = build_review_prompt(
            &Scope::WorkingTree,
            "",
            STAGE,
            &[],
            &[om("gone.txt", "deleted"), om("q", "BINARY")],
        );
        assert!(!p.contains("Not staged"));
        assert!(!p.contains("gone.txt"));
    }

    #[test]
    fn focus_block_only_when_non_empty() {
        let p = build_review_prompt(&Scope::WorkingTree, "error handling", STAGE, &[], &[]);
        assert!(p.contains("User focus / extra steer:\nerror handling"));
        let p = build_review_prompt(&Scope::WorkingTree, "", STAGE, &[], &[]);
        assert!(!p.contains("User focus"));
    }

    #[test]
    fn adversarial_uses_own_preamble_and_rules() {
        let p = build_adversarial_prompt(&Scope::WorkingTree, "", STAGE, &[], &[]);
        assert!(p.contains(&PREAMBLE_ADVERSARIAL[..30]));
        assert!(!p.contains(&PREAMBLE_REVIEW[..30]));
        for r in ADVERSARIAL_RULES {
            assert!(p.contains(r), "{r}");
        }
        assert!(!p.contains(REVIEW_RULES[0]));
    }

    #[test]
    fn adversarial_points_at_staged_and_frames_pressure() {
        let p = build_adversarial_prompt(
            &branch("main", Some("fff0000")),
            "the retry/backoff design",
            STAGE,
            &s(&["bar.ts", "bar.test.ts"]),
            &[],
        );
        assert!(p.contains("/tmp/agy-review-xyz/diff.patch"));
        assert!(p.to_lowercase().contains("pressure"));
        assert!(p.contains("the retry/backoff design"));
        assert!(p.contains("`main`"));
        // Adversarial blurb never shows the merge-base sha.
        assert!(!p.contains("fff0000"));
    }

    #[test]
    fn windows_separator() {
        let p = build_review_prompt(
            &Scope::WorkingTree,
            "",
            "C:\\Temp\\agy-review-xyz",
            &s(&["foo.js"]),
            &[],
        );
        assert!(p.contains("C:\\Temp\\agy-review-xyz\\diff.patch"));
        assert!(p.contains("C:\\Temp\\agy-review-xyz\\files\\<same relative path>"));
    }

    #[test]
    fn ends_with_single_newline() {
        for p in [
            build_review_prompt(&Scope::WorkingTree, "", STAGE, &[], &[]),
            build_adversarial_prompt(&Scope::WorkingTree, "x", STAGE, &s(&["a"]), &[]),
        ] {
            assert!(p.ends_with('\n') && !p.ends_with("\n\n"));
        }
    }

    #[test]
    fn hostile_inputs_are_inert() {
        // Format-string-looking and newline-laden inputs pass through verbatim.
        let p = build_review_prompt(
            &branch("{base}`\n## Security", Some("{}")),
            "{0} {focus}",
            "{stage_dir}",
            &s(&["{f}", ""]),
            &[om("{}", "too large {}")],
        );
        assert!(p.contains("`{base}`\n## Security` (resolved sha: {})"));
        assert!(p.contains("{0} {focus}"));
        assert!(p.contains("{stage_dir}/diff.patch"));
        assert!(p.contains("    - {f}\n    - \n"));
        assert!(p.contains("{} [too large {}]"));
    }
}

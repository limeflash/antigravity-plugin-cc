<!--
Thanks for the PR! Quick checklist below to keep the review tight.
Drop sections that aren't relevant; don't pad.
-->

## Summary

<!-- 1-3 sentences: what changes and why. -->

## Type of change

<!-- Pick one or more. -->
- [ ] Bug fix (`fix:`)
- [ ] New feature (`feat:`)
- [ ] Refactor / no behavior change (`refactor:`)
- [ ] Docs / examples (`docs:`)
- [ ] CI / tooling (`ci:` / `chore:`)
- [ ] Security guardrail or fix

## How was this tested?

<!--
- Unit tests in the module, or an end-to-end case in `tests/cli.rs`?
- `scripts/e2e-live.sh` against a real agy (if agy invocation changed)?
- End-to-end run inside Claude Code? (Paste the slash-command output if
  it's not too long.)
- Any platform you couldn't test on?
-->

## Compatibility

- [ ] No crate dependencies (std only — CONTRIBUTING.md).
- [ ] Platform code checked on macOS, Linux and Windows (or with
      `cargo clippy --target …`).
- [ ] `agy` CLI behavior assumed: <!-- pin a version if relying on
      specific flags -->

## Checklist

- [ ] CI is green (fmt, clippy, tests on 3 OSes, launcher tests).
- [ ] `CHANGELOG.md` entry under the unreleased section.
- [ ] Read-only commands still pin Default mode and never auto-approve.
- [ ] No new secret-scan false negatives introduced.

## Related issues

<!-- e.g. closes #42 -->

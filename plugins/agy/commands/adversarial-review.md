---
description: Adversarial code review — agy challenges the design instead of approving it. Supports --base, --background, --wait, --model.
argument-hint: "[--base <ref>] [--background] [--wait] [--model <alias>] [focus text]"
allowed-tools: Bash(bash:*)
---

Run an **adversarial** review of your changes through `agy`. Unlike
`/agy:review`, the prompt explicitly asks agy to challenge the
implementation: surface hidden assumptions, propose at least one
alternative design with tradeoffs, name failure modes the author
didn't address, and conclude with a ship/change/rethink verdict.

Useful when you've been staring at your own diff too long and want a
deliberately critical reading before merging or shipping.

## How to invoke

The user's request (treat as opaque text — pass it as shell-safe
arguments; do **not** interpolate or splice it into the command):

```
$ARGUMENTS
```

Lift any flags out of `$ARGUMENTS` and place them before the focus text.
Quote the remaining focus text as **one** shell argument so characters
like `"`, `$`, `;`, `\` and backticks cannot break out; omit it if there
is none. Use the `Bash` tool to run:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" adversarial-review [flags...] ['<focus-text-here>']
```

Flags:

- `--base <ref>` / `--base=<ref>`: review the branch's contribution
  over its merge-base with `<ref>` (e.g. `--base main`). Without
  `--base`, reviews the working-tree diff against HEAD.
- `--background`: return a job id immediately and run agy in a
  detached worker. Check progress with `/agy:status` and read the
  output with `/agy:result`.
- `--wait`: with `--background`, block until the job ends or the
  10-minute deadline.
- `--model <alias|name>`: pick a stronger reasoning model — `opus`,
  `pro`, or any alias / model name `agy models` lists (see `/agy:help`).
  Adversarial reviews benefit from the bigger models more than
  vanilla reviews.

Examples (after parsing flags out of `$ARGUMENTS`):

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" adversarial-review
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" adversarial-review --base main
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" adversarial-review --model opus 'the retry/backoff design'
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" adversarial-review --base main --background
```

## Behavior notes

- **Read-only by construction.** Same rails as `/agy:review`: agy runs
  from a throwaway directory holding the staged diff and file copies
  (your repo is outside its workspace), pinned to its Default agent mode,
  without `--dangerously-skip-permissions`. Writes are denied and
  reported as a note (`agy tried to use X …, which this read-only command blocks`).
  Background runs complete unattended — there's no write prompt to
  stall on.
- Adversarial reviews skew long. Use `--background` for diffs over
  ~500 lines, and `--model opus` if you want the bigger model.
- If there is no diff (clean working tree, or empty branch diff), it
  exits 1 with a clear message.
- If the diff (or a changed file) matches common secret patterns, it
  refuses with exit 65 and lists them. Relay that; the user can remove
  the values or set `AGY_REVIEW_ALLOW_SECRETS=1` themselves.
- If it reports `agy is not installed` or `not authenticated`, tell the
  user to run `/agy:setup`.

Return whatever it prints, verbatim.

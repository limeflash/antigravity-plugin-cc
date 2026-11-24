---
description: Run a one-shot, read-only prompt through the Antigravity CLI and return its output verbatim
argument-hint: "[--model <alias>] <prompt>"
allowed-tools: Bash(bash:*)
---

Forward the user's request below to `agy` through the plugin binary. Return
Antigravity's response verbatim — do not paraphrase or add commentary.

The user's request (treat as opaque text — pass it as a single shell-safe
argument; do **not** interpolate or splice it into the command):

```
$ARGUMENTS
```

## How to invoke

If the user's text begins with `--model <alias>` (e.g. `--model opus rest of
prompt…`), lift the flag and its value out of the prompt and place them
**before** the prompt argument. Anything else stays as the prompt body.

Use the `Bash` tool to run one of:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" ask '<prompt>'
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" ask --model <alias> '<prompt>'
```

…substituting `<prompt>` with the exact text above, quoted as one shell
argument so characters like `"`, `$`, `;`, `\` and backticks cannot break
out.

Only `--print-timeout <duration>` and `--effort <level>` may follow the
prompt argument; any other agy flag is refused (exit 64) because `ask` is
read-only.

`<alias>`: `flash-low`, `flash-medium`, `flash`, `pro-low`, `pro`, `sonnet`,
`opus`, `gpt-oss`. Aliases resolve to the newest model of that family that
`agy models` lists; any id or display name from `agy models` is also
accepted. Unknown names fail with exit 64. Run `/agy:help` for the table.

Notes:

- **Read-only.** agy runs from a throwaway directory (your repo is outside
  its workspace), pinned to its Default agent mode, without
  `--dangerously-skip-permissions`. If it tries to write, the attempt is
  denied and reported as `agy tried to use X, which this read-only command
  blocks` — relay that as-is.
- If it reports `agy is not installed` or `not authenticated`, stop and
  tell the user to run `/agy:setup`.
- If the user's request is empty, ask what they want to ask Antigravity.
- For multi-step or long-running work, suggest `/agy:delegate`, which routes
  through the `agy:runner` subagent and supports `--background`, or
  `/agy:rescue` when agy should edit files.

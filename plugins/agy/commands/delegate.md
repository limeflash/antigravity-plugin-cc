---
description: Delegate a task to the Antigravity (`agy`) runner subagent; supports background execution and model selection
argument-hint: "[--background] [--model <alias>] <task description>"
allowed-tools: Agent
---

Hand the user's task to the `agy:runner` subagent
(`subagent_type: "agy:runner"`). The runner uses the read-only `ask`
command, so agy can read and answer but not edit files — for a task where
agy should change code, use `/agy:rescue` instead.

Raw user request:
$ARGUMENTS

## Routing rules

- If the request contains `--background`, launch the subagent with
  `run_in_background: true`. Strip the flag from the forwarded task text.
- Otherwise run the subagent in the foreground.
- If the request contains `--model <alias>`, forward it to the subagent so
  it can pass `--model <alias>` **before** the prompt argument. Strip it
  from the task text.
- If no model is given, agy uses whatever model it is currently set to.

`<alias>`: `gemini` (newest Gemini of any kind), any Gemini kind
`agy models` lists (`hydrogen`, `flash`, `pro`, …) with an optional level
(`hydrogen-xhigh`, `flash-low`), `opus`, `sonnet`, `gpt-oss` — each resolves to the newest model of that family that
`agy models` lists. Any id or display name from `agy models` is also
accepted. Run `/agy:help` for the table.

## Response style

The subagent is a thin wrapper around `agy`. Return its output verbatim — no
extra commentary before or after.

If the user did not supply a task, ask what they would like Antigravity to do.

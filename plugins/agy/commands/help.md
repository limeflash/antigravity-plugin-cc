---
description: Show all /agy:* commands, supported --model aliases, and the models agy currently offers
allowed-tools: Bash(bash:*)
---

Run the plugin's help command, then print its stdout verbatim as a fenced
code block in your text response. Do not paraphrase, reorder, or summarize —
it is the authoritative source for the command list, model aliases, and the
models `agy models` currently lists. The output MUST appear as readable text
in the reply, not just as a collapsed tool result.

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" help
```

After the Bash call completes, copy the full stdout into your response inside
a ``` code block.

---
description: Convert a local document (PDF, DOCX, HTML, …) to Markdown, read-only
argument-hint: "[--model <alias>] <path>"
allowed-tools: Bash(bash:*)
---

Convert a local document to clean Markdown through the Antigravity CLI.
**Read-only and scoped:** the path is validated (allow-listed document
extensions only; a real regular file; 10 MB cap; **not** under a sensitive
location like `~/.ssh`, `~/.aws`, `~/.gemini` or `/etc`; symlinks resolved),
then copied under a fixed name into a throwaway directory so agy sees
**only that one file**. agy runs there pinned to its Default agent mode,
without `--dangerously-skip-permissions` — it cannot read the rest of your
filesystem and cannot write to your repo.

The user's input below is a single file path (optionally preceded by `--model
<alias>`). Pass it as one shell-safe argument; do **not** interpolate it:

```
$ARGUMENTS
```

## How to invoke

Lift a leading `--model <alias>` (if present) out and place it before the path.
Use the `Bash` tool to run one of:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" doc-to-md '<path>'
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" doc-to-md --model <alias> '<path>'
```

Return the Markdown verbatim. Offer to save it to a file if the user wants.

Notes:

- Allowed types: pdf, docx, doc, odt, rtf, html/htm/xhtml, xml, md/markdown,
  txt/text, rst, org, adoc, csv, tsv, pptx, ppt, epub. Anything else (`.env`,
  `.pem`, `.key`, an extension-less secret, …) is refused (exit 65) **by
  design** — relay the reason, don't bypass it.
- If it reports `agy is not installed` or `not authenticated`, tell the
  user to run `/agy:setup`.

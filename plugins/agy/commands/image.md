---
description: Generate an image with Antigravity CLI (uses agy's built-in generate_image tool)
argument-hint: "[--name <slug>] [--output <path>] <description>"
allowed-tools: Bash(bash:*)
---

Ask `agy` to generate an image with its native `generate_image` tool. The
plugin builds the prompt, locates the saved file and optionally copies it.

The user's request (treat as opaque — pass through as shell-safe
arguments):

```
$ARGUMENTS
```

## How to invoke

Parse the user's request into three parts:

- `--name <slug>` — optional. Filename slug `agy` should save the image
  under (no extension).
- `--output <path>` — optional. Local path to copy the generated file to
  after `agy` finishes.
- The remaining text — the description of what to generate.

Use the `Bash` tool to run:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" image [--name <slug>] [--output <path>] '<description>'
```

…with the description quoted as one shell argument so characters like `"`,
`$`, `;`, `\` and backticks cannot break out.

## Output

On success it prints:

```
[agy-plugin] generated: <path under ~/.gemini/antigravity-cli/>
[agy-plugin] copied to: <--output path>      (only with --output)
```

The image path is taken from a marker file agy writes and is accepted only
if it lies inside agy's artifacts directory
(`~/.gemini/antigravity-cli/{brain,scratch,cache}/`); anything else is
refused with exit 66 — relay that, don't work around it. If agy produced no
locatable image it exits 1.

## Notes

- `image` is write-capable: agy runs with auto-approval (sandboxed, in a
  throwaway directory) so its image tool can save the file.
- Return the output verbatim so the user can see the generated path.
- If the user did not provide any description, ask what they want to
  generate.
- If it reports `agy is not installed` or `not authenticated`, stop and
  tell the user to run `/agy:setup`.

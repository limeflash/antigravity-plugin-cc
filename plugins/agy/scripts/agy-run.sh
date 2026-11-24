#!/usr/bin/env bash
# agy-run.sh — Claude Code wrapper around Google Antigravity CLI (`agy`).
# Subcommands: check | ask | review | image | help.
# `--model` (on ask) overrides the model for one call via locked
# settings.json patching, restored on exit.

set -euo pipefail

# Directory this script lives in, so we can locate sibling lib/ helpers
# (lib/transcript.mjs powers the read-only issue-#76 output capture).
AGY_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGY_LIB_DIR="$AGY_SCRIPT_DIR/lib"

AGY_SETTINGS_FILE="${HOME}/.gemini/antigravity-cli/settings.json"
AGY_SETTINGS_LOCKDIR="${HOME}/.gemini/antigravity-cli/.agy-plugin.lock"
AGY_SETTINGS_BACKUP="${HOME}/.gemini/antigravity-cli/settings.json.agy-plugin.bak"
AGY_SETTINGS_SENTINEL="${HOME}/.gemini/antigravity-cli/.agy-plugin.patched"

find_agy() {
  if command -v agy >/dev/null 2>&1; then
    command -v agy
    return 0
  fi
  for candidate in \
      "$HOME/.local/bin/agy" \
      "/opt/antigravity/bin/agy" \
      "/usr/local/bin/agy"; do
    if [ -x "$candidate" ]; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

auth_status() {
  if [ -n "${ANTIGRAVITY_API_KEY:-}" ]; then
    echo "api-key"
  elif [ -d "$HOME/.config/antigravity" ] || [ -d "$HOME/.gemini/antigravity-cli" ]; then
    echo "oauth"
  else
    echo "missing"
  fi
}

j_esc() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  s="${s//$'\n'/\\n}"
  s="${s//$'\r'/\\r}"
  s="${s//$'\t'/\\t}"
  printf '%s' "$s"
}

# Resolve a path to absolute canonical form, falling back gracefully on
# platforms that lack `realpath` or GNU `readlink -f` (macOS /bin/bash,
# BusyBox). If no canonicalization tool is available, returns the input
# unchanged — callers must treat the result as untrusted in that case.
_canonicalize_path() {
  local p="$1"
  [ -n "$p" ] || return 1
  if command -v realpath >/dev/null 2>&1; then
    realpath "$p" 2>/dev/null && return 0
  fi
  if command -v readlink >/dev/null 2>&1; then
    readlink -f "$p" 2>/dev/null && return 0
  fi
  if command -v python3 >/dev/null 2>&1; then
    python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$p" 2>/dev/null && return 0
  fi
  printf '%s' "$p"
}

# Returns 0 iff $1 resolves to a path inside one of `agy`'s known
# artifacts directories. Used by cmd_image to refuse copying an
# attacker-controlled IMAGE_PATH (e.g. via prompt injection in the
# model's reply pointing at `/etc/passwd`).
_image_source_in_allowlist() {
  local src="$1"
  [ -n "$src" ] || return 1
  local canonical; canonical="$(_canonicalize_path "$src")" || return 1
  # The source is canonicalized, so the allowed dirs must be too: when $HOME
  # sits behind a symlink (macOS /var -> /private/var, a relocated home), a
  # canonical source never starts with the raw "$HOME/..." prefix and a
  # legitimate image was refused. Check both spellings of each dir.
  local dir prefix real_dir
  for dir in brain scratch cache; do
    prefix="$HOME/.gemini/antigravity-cli/$dir"
    case "$canonical" in
      "$prefix"/*) return 0 ;;
    esac
    if [ -d "$prefix" ]; then
      real_dir="$(_canonicalize_path "$prefix")" || continue
      case "$canonical" in
        "$real_dir"/*) return 0 ;;
      esac
    fi
  done
  return 1
}

# Best-effort scan for common secret patterns in added lines of a git
# diff. Returns 0 (silent) if nothing found, 1 if any pattern matched
# (writes one matched pattern label per line to stdout). This is a
# guardrail, not a substitute for a real secret scanner like gitleaks —
# patterns are deliberately conservative to keep false positives low.
#
# Patterns and labels are kept in parallel arrays because some patterns
# legitimately contain `|` (alternation), so a single-string
# "pattern|label" format is ambiguous.
# Scan RAW text against the secret patterns. Kept in sync with the Node
# companion's lib/secrets.mjs PATTERNS (tests exercise both so they don't
# drift). Prints matched labels and returns 1 if any hit; 0 if clean.
_scan_text_for_secrets() {
  local text="$1"
  [ -n "$text" ] || return 0
  local pats=(
    'AKIA[0-9A-Z]{16}'
    'ASIA[0-9A-Z]{16}'
    'gh[pousr]_[A-Za-z0-9]{36,}'
    'github_pat_[A-Za-z0-9_]{40,}'
    'xox[baprs]-[A-Za-z0-9-]{10,}'
    'sk-ant-[A-Za-z0-9_-]{20,}'
    'sk-proj-[A-Za-z0-9_-]{20,}'
    'sk-[A-Za-z0-9]{20,}'
    '-----BEGIN [A-Z ]*PRIVATE KEY-----'
    '(api[_-]?key|secret|token|password|access[_-]?key)[[:space:]]*[=:][[:space:]]*["'"'"']?[A-Za-z0-9_+/=\-]{16,}'
  )
  local labels=(
    'AWS access key'
    'AWS STS token'
    'GitHub personal access token'
    'GitHub fine-grained PAT'
    'Slack token'
    'Anthropic API key'
    'OpenAI project key'
    'OpenAI legacy API key'
    'PEM private key block'
    'inline credential assignment'
  )
  local hits=()
  local i
  for i in "${!pats[@]}"; do
    # `-e <pattern>` is required because some patterns start with `-`
    # (e.g. PEM headers), which grep would otherwise treat as an option.
    if printf '%s\n' "$text" | grep -aEi -e "${pats[$i]}" >/dev/null 2>&1; then
      hits+=("${labels[$i]}")
    fi
  done
  [ "${#hits[@]}" -gt 0 ] || return 0
  printf '%s\n' "${hits[@]}"
  return 1
}

# Scan only the ADDED lines of a unified diff (single `+`, not the `+++`
# header). Delegates the pattern matching to _scan_text_for_secrets.
_scan_diff_for_secrets() {
  local diff_text="$1"
  local added
  added="$(printf '%s\n' "$diff_text" | grep -E '^\+[^+]' || true)"
  _scan_text_for_secrets "$added"
}

cmd_check() {
  if ! path="$(find_agy | head -n1)"; then
    cat <<JSON
{ "installed": false, "path": "", "version": "", "auth": "unknown",
  "error": "agy binary not found; install with: curl -fsSL https://antigravity.google/cli/install.sh | bash" }
JSON
    return 0
  fi
  version="$("$path" --version 2>/dev/null | head -n1 || echo unknown)"
  auth="$(auth_status)"
  printf '{ "installed": true, "path": "%s", "version": "%s", "auth": "%s", "error": "" }\n' \
    "$(j_esc "$path")" "$(j_esc "$version")" "$(j_esc "$auth")"
}

require_ready() {
  if ! path="$(find_agy)"; then
    echo "error: agy is not installed." >&2
    echo "       install: curl -fsSL https://antigravity.google/cli/install.sh | bash" >&2
    exit 127
  fi
  if [ "$(auth_status)" = "missing" ]; then
    echo "error: agy is not authenticated." >&2
    echo "       run \`agy\` once interactively, or export ANTIGRAVITY_API_KEY" >&2
    exit 1
  fi
  echo "$path"
}

# ---------------------------------------------------------------------------
# Model selection (--model).
#
# Aliases resolve at call time against `agy models` (one "id<TAB>display
# name" row per model) to the NEWEST model in the alias's family, and the
# result goes to agy's own --model flag for that one call. Ids and display
# names listed by `agy models` are accepted verbatim, case-insensitive.
# Nothing touches settings.json any more (0.9.0, ported from upstream
# simplybychris/antigravity-plugin-cc 0.5.0; the old patch-and-restore flow
# flipped the model of a TUI open in parallel and went stale whenever Google
# renamed a model).
#
# Each row: comma-separated aliases | ERE matched against the lowercased
# display name | ERE that wins a version tie (may be empty) | description
# shown when no listing is available.
# ---------------------------------------------------------------------------
MODEL_ALIASES='flash-low|^gemini [0-9.]+ flash [(]low[)]$||newest Gemini Flash (Low)
flash-medium,flash-med|^gemini [0-9.]+ flash [(]medium[)]$||newest Gemini Flash (Medium)
flash,flash-high|^gemini [0-9.]+ flash [(]high[)]$||newest Gemini Flash (High)
pro-low|^gemini [0-9.]+ pro [(]low[)]$||newest Gemini Pro (Low)
pro,pro-high|^gemini [0-9.]+ pro [(]high[)]$||newest Gemini Pro (High)
sonnet,claude-sonnet|^claude sonnet [0-9.]+( [(].*[)])?$|[(]thinking[)]$|newest Claude Sonnet
opus,claude-opus|^claude opus [0-9.]+( [(].*[)])?$|[(]thinking[)]$|newest Claude Opus
gpt-oss,gpt-oss-120b|^gpt-oss 120b( [(].*[)])?$|[(]medium[)]$|GPT-OSS 120B'

# Older agy builds ignore --model in -p runs (fixed in 1.1.10) or hang in
# `agy models` on an open stdin.
MIN_AGY_MODEL_VERSION="1.2.0"

# agy 1.5.7 made Auto the default Agent Mode, headless -p runs included, and
# Auto approves file writes OUTSIDE the workspace (any file in a git repo,
# and any create/append elsewhere). See _readonly_mode_flags.
AGY_AUTO_DEFAULT_VERSION="1.5.7"

TAB="$(printf '\t')"

AWK_VERSION_NEWER='
function newer(a, b,    x, y, na, nb, n, i) {
  na = split(a, x, "."); nb = split(b, y, ".")
  n = na > nb ? na : nb
  for (i = 1; i <= n; i++) {
    if (x[i] + 0 > y[i] + 0) return 1
    if (x[i] + 0 < y[i] + 0) return 0
  }
  return 0
}'

# Prints agy's version ("1.5.16"), or nothing if it can't be read.
_agy_version() {
  "$1" --version </dev/null 2>/dev/null | grep -oE '[0-9]+[.][0-9]+([.][0-9]+)?' | head -n1 || true
}

# True when version $1 is >= version $2.
_version_at_least() {
  [ "$(awk -v a="$2" -v b="$1" "$AWK_VERSION_NEWER"'
      BEGIN { print newer(a, b) }')" = 0 ]
}

require_model_support() {
  local path="$1" version
  version="$(_agy_version "$path")"
  if [ -z "$version" ]; then
    echo "[wrapper] note: could not read the agy version; assuming --model works" >&2
    return 0
  fi
  if ! _version_at_least "$version" "$MIN_AGY_MODEL_VERSION"; then
    echo "error: agy $version is too old for --model (needs $MIN_AGY_MODEL_VERSION or newer)." >&2
    echo "       update it with \`agy update\`." >&2
    exit 1
  fi
}

# Prints "id<TAB>display name" per model. Some agy builds print names only,
# in which case the id column is empty.
list_models() {
  local path="$1" raw
  raw="$("$path" models </dev/null 2>/dev/null)" || return 1
  printf '%s\n' "$raw" | awk '
    { sub(/\r$/, "") }
    /^[[:space:]]*$/ || /:[[:space:]]*$/ || /^Fetching / { next }
    {
      tab = index($0, "\t")
      if (tab > 0) { id = substr($0, 1, tab - 1); name = substr($0, tab + 1) }
      else { id = ""; name = $0 }
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", id)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", name)
      if (name != "") print id "\t" name
    }'
}

# Line of $1 (list_models output) whose name matches ERE $2 and has the
# newest version. On a version tie, a name matching ERE $3 wins.
_newest_match() {
  printf '%s\n' "$1" | PATTERN="$2" PREFER="${3:-}" awk -F '\t' "$AWK_VERSION_NEWER"'
    NF >= 2 && tolower($2) ~ ENVIRON["PATTERN"] {
      version = ""
      count = split(tolower($2), word, " ")
      for (i = 1; i <= count; i++) if (word[i] ~ /^[0-9]+([.][0-9]+)*$/) { version = word[i]; break }
      preferred = ENVIRON["PREFER"] != "" && tolower($2) ~ ENVIRON["PREFER"]
      if (best == "" || newer(version, best_version) ||
          (!newer(best_version, version) && preferred && !best_preferred)) {
        best = $0; best_version = version; best_preferred = preferred
      }
    }
    END { if (best != "") print best }'
}

# Line of $1 whose id or name equals $2, ignoring case.
_exact_match() {
  printf '%s\n' "$1" | WANT="$2" awk -F '\t' '
    NF >= 2 && (tolower($1) == tolower(ENVIRON["WANT"]) || tolower($2) == tolower(ENVIRON["WANT"])) { print; exit }'
}

_model_name() {
  printf '%s' "${1#*"$TAB"}"
}

# The id when the listing has one, otherwise the display name.
_model_arg() {
  local id="${1%%"$TAB"*}"
  if [ -n "$id" ]; then
    printf '%s' "$id"
  else
    _model_name "$1"
  fi
}

# Prints "pattern|prefer|description" for alias $1 (already lowercased).
_alias_row() {
  local want="$1" aliases pattern prefer description
  case "$want" in "" | *,*) return 1 ;; esac
  while IFS='|' read -r aliases pattern prefer description; do
    case ",$aliases," in
      *",$want,"*)
        printf '%s|%s|%s' "$pattern" "$prefer" "$description"
        return 0
        ;;
    esac
  done <<<"$MODEL_ALIASES"
  return 1
}

# $1 = output fd, $2 = list_models output (may be empty).
print_model_table() {
  local fd="${1:-2}" models="${2:-}"
  {
    if [ -n "$models" ]; then
      echo "Aliases (case-insensitive), resolved against \`agy models\`:"
    else
      echo "Aliases (case-insensitive). \`agy models\` could not be read, so they are shown unresolved:"
    fi
    local aliases pattern prefer description match target
    while IFS='|' read -r aliases pattern prefer description; do
      target="$description"
      if [ -n "$models" ]; then
        match="$(_newest_match "$models" "$pattern" "$prefer")"
        if [ -n "$match" ]; then
          target="$(_model_name "$match")"
        else
          target="(not available: $description)"
        fi
      fi
      printf '  %-28s  -> %s\n' "$(printf '%s' "$aliases" | sed 's/,/, /g')" "$target"
    done <<<"$MODEL_ALIASES"
    if [ -n "$models" ]; then
      echo
      echo "Available models (names and ids are accepted verbatim, case-insensitive):"
      printf '%s\n' "$models" | awk -F '\t' '{ if ($1 != "") printf "  %-32s  %s\n", $2, $1; else printf "  %s\n", $2 }'
    fi
  } >&"$fd"
}

_current_default_model() {
  [ -f "$AGY_SETTINGS_FILE" ] || return 1
  grep -oE '"model"[[:space:]]*:[[:space:]]*"[^"]*"' "$AGY_SETTINGS_FILE" 2>/dev/null |
    sed -E 's/.*"model"[[:space:]]*:[[:space:]]*"([^"]*)".*/\1/' |
    head -n1
}

model_value_error() {
  echo "error: --model requires a non-empty value (e.g. --model pro)" >&2
  local path models=""
  if path="$(find_agy)"; then
    models="$(list_models "$path" || true)"
  fi
  print_model_table 2 "$models"
  local current
  current="$(_current_default_model 2>/dev/null || true)"
  if [ -n "$current" ]; then
    echo >&2
    echo "Tip: omit --model to use your current default (\"$current\")." >&2
  fi
  exit 64
}

# Prints the value for `agy --model`: the id when `agy models` lists one,
# otherwise the display name. Exits 64 for an unknown alias / model.
resolve_model() {
  local path="$1" input="$2"
  local lc
  lc="$(printf '%s' "$input" | tr '[:upper:]' '[:lower:]')"
  local row=""
  row="$(_alias_row "$lc" || true)"
  local models
  if ! models="$(list_models "$path")" || [ -z "$models" ]; then
    if [ -n "$row" ]; then
      echo "error: could not list models with \`agy models\` (run it directly to see why)." >&2
      echo "       aliases need that list; pass the full model name instead." >&2
      exit 1
    fi
    echo "[wrapper] note: could not list models; passing '$input' to agy unchecked" >&2
    printf '%s' "$input"
    return 0
  fi
  local match pattern prefer description
  match="$(_exact_match "$models" "$input")"
  if [ -z "$match" ] && [ -n "$row" ]; then
    IFS='|' read -r pattern prefer description <<<"$row"
    match="$(_newest_match "$models" "$pattern" "$prefer")"
    [ -n "$match" ] || echo "error: alias '$input' has no matching model in \`agy models\` (expected: $description)" >&2
  elif [ -z "$match" ]; then
    echo "error: unknown model '$input' (not an alias and not listed by \`agy models\`)" >&2
  fi
  if [ -n "$match" ]; then
    _model_arg "$match"
    return 0
  fi
  print_model_table 2 "$models"
  exit 64
}

# Prints the --model value for one call, or nothing when --model wasn't
# given. $1 = agy path, $2 = 1 if --model was seen, $3 = its value.
_model_for_call() {
  local path="$1" seen="$2" value="$3"
  [ "$seen" -eq 1 ] || return 0
  require_model_support "$path"
  resolve_model "$path" "$value"
}

# ---------------------------------------------------------------------------
# Read-only agent mode.
#
# Since agy 1.5.7 headless -p runs start in Auto mode, and Auto approves
# writes OUTSIDE the workspace without asking: any file inside a git /
# jj / hg repo (1.5.16) and any create or append elsewhere. So running agy
# from a throwaway temp dir no longer keeps it from writing: verified on
# agy 1.5.16, a plugin-style run with no --dangerously-skip-permissions
# overwrote a file in a git repo outside its workspace when the prompt asked
# it to. A prompt-injected diff could do the same to the user's repo, or
# append to ~/.zshrc.
#
# Default mode ("agentMode": "" in settings.json) makes a headless run deny
# what it may not do instead (reported as denied_actions). --settings
# applies it to this run only, never saved. Sets the global AGY_RO_FLAGS
# array. Fails closed: an agy that defaults to Auto but can't be pinned is
# refused rather than run.
# ---------------------------------------------------------------------------
_readonly_mode_flags() {
  local agy="$1" help version
  AGY_RO_FLAGS=()
  help="$("$agy" --help </dev/null 2>&1 || true)"
  case "$help" in
    *--settings*)
      AGY_RO_FLAGS=(--settings '{"agentMode":""}')
      return 0
      ;;
  esac
  version="$(_agy_version "$agy")"
  if [ -n "$version" ] && _version_at_least "$version" "$AGY_AUTO_DEFAULT_VERSION"; then
    echo "error: agy $version starts headless runs in Auto mode, which can write outside" >&2
    echo "       the workspace, and this build has no --settings flag to pin Default mode." >&2
    echo "       Refusing to run a read-only command. Update agy with \`agy update\`." >&2
    return 1
  fi
  return 0
}

# Recovers settings.json left patched by a killed run of plugin <= 0.8.x,
# which swapped the model in place instead of passing --model.
restore_orphaned_backup() {
  [ -f "$AGY_SETTINGS_SENTINEL" ] || {
    if [ -f "$AGY_SETTINGS_BACKUP" ]; then
      echo "[wrapper] note: stale backup with no sentinel; removing $AGY_SETTINGS_BACKUP" >&2
      rm -f "$AGY_SETTINGS_BACKUP"
    fi
    return 0
  }
  local pid
  pid="$(head -n1 "$AGY_SETTINGS_SENTINEL" 2>/dev/null || true)"
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    return 0
  fi
  if [ -f "$AGY_SETTINGS_BACKUP" ]; then
    mv "$AGY_SETTINGS_BACKUP" "$AGY_SETTINGS_FILE"
    echo "[wrapper] recovered orphaned settings backup from PID ${pid:-unknown}" >&2
  fi
  rm -f "$AGY_SETTINGS_SENTINEL"
  rm -rf "$AGY_SETTINGS_LOCKDIR"
}

# ---------------------------------------------------------------------------
# agy output capture (read-only).
#
# agy 1.0.15 fixed issue #76: earlier `agy --print`, when stdout was NOT a
# TTY (any time the plugin runs it from a subprocess / agent Bash tool),
# generated the response internally but flushed ZERO bytes to stdout — the
# "drip" typewriter only targeted a real terminal (`Drip stopped: length=N`
# in the log while stdout stayed empty, confirmed on agy 1.0.3). Separately,
# `agy --print` blocks forever on a non-TTY stdin that never reaches EOF, so
# we always close stdin (</dev/null).
#
# PRIMARY path (_agy_capture): run agy strictly READ-ONLY (--sandbox,
# --add-dir only a throwaway temp dir, --log-file) and read the answer from
# agy's STDOUT — the fast path on agy >= 1.0.15. No write_file, no
# --dangerously-skip-permissions.
#
# FALLBACK 1 (transcript): if stdout comes back empty (older agy still hit by
# #76), read the answer from agy's own on-disk conversation transcript — which
# it persists on every --print run with NO tool permission — via
# lib/transcript.mjs (the conversation id comes from the run's --log-file).
#
# FALLBACK 2 (_agy_capture_writefile): only for agy < 1.0.15 (stdout still
# empty) on a machine without `node` (no transcript parser). Reverts to
# instructing agy to write_file its answer under
# --dangerously-skip-permissions, scoped to a throwaway temp dir. On newer
# agy the stdout path needs no node, so this auto-approving path is never
# taken there (on agy >= 1.5.2 the flag also approves writes OUTSIDE the
# workspace, so it must stay out of reach of modern installs).
#
# Every run is pinned to Default agent mode (_readonly_mode_flags): agy
# >= 1.5.7 otherwise starts in Auto, which writes outside the workspace.
#
# Usage: _agy_capture <agy> <model-or-empty> <timeout> <prompt> [extra agy args...]
# <model> is the value for agy's own --model (see resolve_model).
# Prints the response on success (rc 0); prints an error and rc 1 otherwise.
# ---------------------------------------------------------------------------
_agy_capture() {
  local agy="$1" model="$2" timeout="$3" prompt="$4"
  shift 4

  # Without node there is no transcript fallback. That only matters for agy
  # < 1.0.15, whose non-TTY stdout is empty: use the write_file capture
  # there. Newer agy answers on stdout, which needs no node.
  if ! command -v node >/dev/null 2>&1; then
    local v
    v="$(_agy_version "$agy")"
    if [ -n "$v" ] && ! _version_at_least "$v" "1.0.15"; then
      _agy_capture_writefile "$agy" "$model" "$timeout" "$prompt" "$@"
      return $?
    fi
  fi

  _readonly_mode_flags "$agy" || return 1
  local -a model_flags=()
  [ -z "$model" ] || model_flags=(--model "$model")

  local outdir
  outdir="$(mktemp -d 2>/dev/null)" || { echo "error: could not create temp dir for agy output" >&2; return 1; }
  local logfile="$outdir/agy-run.log"
  # Optional: stage a single (caller-validated) input file into the temp dir
  # so a read-only command (e.g. /agy:doc-to-md) can hand agy a file to read
  # without exposing the rest of the filesystem. We stage via the Node helper
  # (lstat + copy in ONE step) rather than a shell `cp`: it rejects a symlink
  # swapped in after validation (TOCTOU) and copies to a FIXED name so the
  # untrusted original filename never reaches agy's prompt. agy runs from
  # $outdir with only $outdir in --add-dir, so it sees this copy and nothing
  # else.
  if [ -n "${AGY_CAPTURE_STAGE_FILE:-}" ]; then
    local stage_as="${AGY_CAPTURE_STAGE_AS:-document}"
    if ! node "$AGY_LIB_DIR/inputguard.mjs" stage "$AGY_CAPTURE_STAGE_FILE" "$outdir/$stage_as" >/dev/null 2>&1; then
      echo "error: could not stage the input file (it may have been swapped, removed, or is unreadable)." >&2
      rm -rf "$outdir"
      return 1
    fi
  fi
  # agy may be a Windows .exe invoked from Git Bash, which needs native
  # Windows paths for --add-dir / --log-file / cwd.
  #
  # CRITICAL: we run agy FROM the throwaway temp dir (cwd="$outdir"), NOT the
  # user's repo. `agy --print` still EXECUTES write tools even without
  # --dangerously-skip-permissions (a non-TTY prompt is auto-proceeded), so
  # "no auto-approve" alone does NOT make a command read-only. Two things
  # do, together: the repo is neither agy's cwd nor in --add-dir (so it is
  # outside the workspace), and the run is pinned to Default agent mode (so
  # writes outside the workspace are denied; agy >= 1.5.7's Auto mode would
  # approve them, absolute paths included). cwd_arg therefore points at the
  # temp dir (also the workspace key agy registers — used as the transcript
  # fallback).
  local target_dir logfile_arg cwd_arg
  if command -v cygpath >/dev/null 2>&1; then
    target_dir="$(cygpath -w "$outdir")"
    logfile_arg="$(cygpath -w "$logfile")"
    cwd_arg="$target_dir"
  else
    target_dir="$outdir"
    logfile_arg="$logfile"
    cwd_arg="$outdir"
  fi

  # agy --print takes the whole prompt as ONE argv entry, so it must stay
  # under the OS command-line limit (~32 KB on Windows) or the process
  # fails with ENAMETOOLONG. Cap by BYTES (multibyte-safe via wc -c/head -c).
  # No write_file suffix now, so the whole budget is the prompt body.
  local max_body="${AGY_PROMPT_MAX_BYTES:-30000}"
  local prompt_bytes; prompt_bytes="$(printf '%s' "$prompt" | wc -c)"
  if [ "$prompt_bytes" -gt "$max_body" ]; then
    prompt="$(printf '%s' "$prompt" | head -c "$max_body" || true)"$'\n\n[...content truncated to fit the OS command-line length limit; some context was omitted...]'
  fi

  # READ-ONLY run: execute agy FROM the throwaway temp dir (cwd="$outdir"),
  # with --sandbox + --add-dir <tmp> + --log-file, pinned to Default agent
  # mode, and NO --dangerously-skip-permissions / NO write_file. The temp-dir
  # cwd keeps the repo out of agy's workspace; Default mode makes the
  # headless run deny writes outside it (Auto would approve them).
  # AGY_RO_FLAGS goes AFTER the caller's extra args so a stray --settings in
  # them cannot override it (agy takes the last value of a flag).
  # `env -u …` strips the repo-location hints (CLAUDE_PROJECT_DIR / GIT_*) from
  # agy's environment as defense in depth. </dev/null dodges the non-TTY stdin
  # hang. We capture stdout to a file: agy >= 1.0.15 fixed the #76 bug that
  # swallowed non-TTY stdout, so stdout is now the fast path; the transcript
  # is the fallback for older agy where stdout still comes back empty.
  local stdout_file="$outdir/agy-stdout.txt"
  local -a agy_env=(env -u CLAUDE_PROJECT_DIR -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_COMMON_DIR)
  # ${arr[@]+"${arr[@]}"}: bash 3.2 (macOS /bin/bash) treats "${arr[@]}" of
  # an empty array as unbound under `set -u`.
  (cd "$outdir" && "${agy_env[@]}" "$agy" --sandbox \
    --add-dir "$target_dir" --log-file "$logfile_arg" --print-timeout "$timeout" \
    ${model_flags[@]+"${model_flags[@]}"} "$@" ${AGY_RO_FLAGS[@]+"${AGY_RO_FLAGS[@]}"} \
    --print "$prompt" \
    </dev/null >"$stdout_file" 2>/dev/null) || true

  # Prefer agy's direct stdout (the #76 fix). Fall back to reading agy's own
  # transcript for older agy / platforms where non-TTY stdout is still empty.
  # transcript.mjs prints the answer (exit 0) or exits non-zero; its stderr is
  # captured and surfaced only if BOTH paths come back empty.
  local rc=0 answer="" node_err="$outdir/node.err"
  answer="$(cat "$stdout_file" 2>/dev/null)"
  if [ -z "$answer" ] && command -v node >/dev/null 2>&1; then
    answer="$(node "$AGY_LIB_DIR/transcript.mjs" "$logfile_arg" "$cwd_arg" 2>"$node_err" || true)"
  fi
  if [ -n "$answer" ]; then
    printf '%s\n' "$answer"
  else
    rc=1
    {
      echo "error: agy returned no output."
      echo "       Neither agy's stdout nor its transcript produced an answer."
      echo "       Possible causes:"
      echo "       - the prompt timed out (raise the timeout), or"
      echo "       - agy was interrupted before it answered."
      if [ -s "$node_err" ]; then
        echo "       transcript.mjs stderr:"
        sed 's/^/         /' "$node_err"
      fi
      echo "       Run \`agy\` interactively to debug, or check ~/.gemini/antigravity-cli/log/."
    } >&2
  fi
  rm -rf "$outdir"
  return "$rc"
}

# ---------------------------------------------------------------------------
# Legacy fallback capture — used only when `node` is unavailable. Instructs
# agy to write_file its answer to a temp file under
# --dangerously-skip-permissions, scoped to a throwaway temp dir. See the
# _agy_capture header for why the transcript path is preferred.
# ---------------------------------------------------------------------------
_agy_capture_writefile() {
  local agy="$1" model="$2" timeout="$3" prompt="$4"
  shift 4
  local outdir
  outdir="$(mktemp -d 2>/dev/null)" || { echo "error: could not create temp dir for agy output" >&2; return 1; }
  local outfile="$outdir/agy-response.md"
  # agy may be a Windows .exe invoked from Git Bash, which needs a native
  # Windows path for both --add-dir and the write_file target.
  local prompt_path target_dir
  if command -v cygpath >/dev/null 2>&1; then
    prompt_path="$(cygpath -w "$outfile")"
    target_dir="$(cygpath -w "$outdir")"
  else
    prompt_path="$outfile"
    target_dir="$outdir"
  fi
  # agy --print takes the whole prompt as ONE argv entry, so it must
  # stay under the OS command-line limit (~32 KB on Windows) or the
  # process fails with "Argument list too long" / ENAMETOOLONG. Cap the
  # body (the write_file instruction is appended after, always intact).
  # Cap by BYTES, not characters: `${#prompt}` / `${prompt:0:N}` count
  # characters, so multibyte UTF-8 (CJK, emoji) could still blow past
  # the byte limit. `wc -c` / `head -c` are byte-accurate in any locale.
  local max_body="${AGY_PROMPT_MAX_BYTES:-26000}"
  local prompt_bytes; prompt_bytes="$(printf '%s' "$prompt" | wc -c)"
  if [ "$prompt_bytes" -gt "$max_body" ]; then
    prompt="$(printf '%s' "$prompt" | head -c "$max_body" || true)"$'\n\n[...content truncated to fit the OS command-line length limit; some diff/file context was omitted...]'
  fi
  local augmented
  augmented="$(printf '%s\n\n---\nOUTPUT INSTRUCTION (required): Use the write_file tool to write your COMPLETE response to this exact path:\n%s\nDo NOT print the answer to chat — that path is your only deliverable. After writing the file, stop.\n' "$prompt" "$prompt_path")"

  # Only agy < 1.0.15 reaches this path, and --model needs >= 1.2.0
  # (require_model_support), so a model value never gets here; pass it on
  # anyway rather than silently drop it.
  local -a model_flags=()
  [ -z "$model" ] || model_flags=(--model "$model")

  # Strip repo-location hints (CLAUDE_PROJECT_DIR / GIT_*) from agy's env as
  # defense in depth — same as _agy_capture. This fallback runs agy under
  # --dangerously-skip-permissions, so denying it a pointer to the host repo
  # matters even though the repo is still never its cwd nor in --add-dir.
  local -a agy_env=(env -u CLAUDE_PROJECT_DIR -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_COMMON_DIR)
  (cd "$outdir" && "${agy_env[@]}" "$agy" --dangerously-skip-permissions --sandbox \
    --add-dir "$target_dir" --print-timeout "$timeout" \
    ${model_flags[@]+"${model_flags[@]}"} "$@" --print "$augmented" \
    </dev/null >/dev/null 2>&1) || true

  local rc=0
  if [ -s "$outfile" ]; then
    cat "$outfile"
  else
    rc=1
    {
      echo "error: agy returned no output."
      echo "       This is agy issue #76 (empty stdout in non-TTY) combined with the"
      echo "       write_file workaround failing to produce a file. Possible causes:"
      echo "       - the prompt timed out (raise the timeout), or"
      echo "       - agy refused/limited the write_file tool."
      echo "       Run \`agy\` interactively to debug, or check ~/.gemini/antigravity-cli/log/."
    } >&2
  fi
  rm -rf "$outdir"
  return "$rc"
}

cmd_ask() {
  local model_alias=""
  local model_flag_seen=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --model)
        model_flag_seen=1
        # `shift 2` on a one-arg list would `set -e`-exit silently; handle
        # the empty case so model_value_error prints the real error.
        if [ $# -ge 2 ]; then
          model_alias="$2"; shift 2
        else
          shift
        fi ;;
      --model=*)
        model_flag_seen=1
        model_alias="${1#--model=}"
        shift ;;
      --)        shift; break ;;
      *)         break ;;
    esac
  done
  # Check the value before the prompt so `ask --model` reports the flag.
  if [ "$model_flag_seen" -eq 1 ] && [ -z "$model_alias" ]; then
    model_value_error
  fi
  local prompt="${1:-}"
  shift || true
  if [ -z "$prompt" ]; then
    echo "error: ask requires a prompt argument" >&2
    exit 64
  fi
  local path model
  path="$(require_ready)"
  model="$(_model_for_call "$path" "$model_flag_seen" "$model_alias")"
  # Read-only capture (stdout, transcript fallback). Default to an 8-minute
  # timeout; agy usually answers within seconds.
  _agy_capture "$path" "$model" "${AGY_ASK_TIMEOUT:-8m0s}" "$prompt" "$@"
}

# /agy:scrape <url> [--model <alias>] — fetch a web page read-only and return
# its main content as Markdown. The URL is validated by lib/inputguard.mjs
# (http/https only; localhost / private / link-local / metadata blocked) to
# prevent SSRF before agy ever touches it.
cmd_scrape() {
  local model_alias="" model_flag_seen=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --model)
        model_flag_seen=1
        if [ $# -ge 2 ]; then model_alias="$2"; shift 2; else shift; fi ;;
      --model=*) model_flag_seen=1; model_alias="${1#--model=}"; shift ;;
      --)        shift; break ;;
      *)         break ;;
    esac
  done
  if [ "$model_flag_seen" -eq 1 ] && [ -z "$model_alias" ]; then
    model_value_error
  fi
  local url="${1:-}"
  if [ -z "$url" ]; then
    echo "error: scrape requires a URL argument (http/https)" >&2
    exit 64
  fi
  if ! command -v node >/dev/null 2>&1; then
    echo "error: /agy:scrape needs Node.js (URL validation + output capture)." >&2
    exit 1
  fi
  local safe_url
  if ! safe_url="$(node "$AGY_LIB_DIR/inputguard.mjs" scrape "$url" 2>&1)"; then
    echo "error: refusing to scrape this URL — $safe_url" >&2
    exit 65
  fi
  local path model
  path="$(require_ready)"
  model="$(_model_for_call "$path" "$model_flag_seen" "$model_alias")"
  local prompt
  prompt="$(printf 'Fetch the web page at the URL below and return its MAIN readable content as clean Markdown — preserve headings, lists, links, tables, and code blocks; drop nav/ads/boilerplate. Output ONLY the Markdown, no preamble.\n\nURL: %s' "$safe_url")"
  _agy_capture "$path" "$model" "${AGY_SCRAPE_TIMEOUT:-5m0s}" "$prompt"
}

# /agy:doc-to-md <path> [--model <alias>] — convert a local document to
# Markdown read-only. The path is validated by lib/inputguard.mjs (allow-
# listed document extensions, real file, size cap, not under a sensitive
# dir, symlinks resolved), then staged into the throwaway temp dir so agy
# sees only that one file.
cmd_doc_to_md() {
  local model_alias="" model_flag_seen=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --model)
        model_flag_seen=1
        if [ $# -ge 2 ]; then model_alias="$2"; shift 2; else shift; fi ;;
      --model=*) model_flag_seen=1; model_alias="${1#--model=}"; shift ;;
      --)        shift; break ;;
      *)         break ;;
    esac
  done
  if [ "$model_flag_seen" -eq 1 ] && [ -z "$model_alias" ]; then
    model_value_error
  fi
  local input="${1:-}"
  if [ -z "$input" ]; then
    echo "error: doc-to-md requires a file path" >&2
    exit 64
  fi
  if ! command -v node >/dev/null 2>&1; then
    echo "error: /agy:doc-to-md needs Node.js (path validation + output capture)." >&2
    exit 1
  fi
  local real
  if ! real="$(node "$AGY_LIB_DIR/inputguard.mjs" doc "$input" "$PWD" 2>&1)"; then
    echo "error: refusing to convert this file — $real" >&2
    exit 65
  fi
  local path model
  path="$(require_ready)"
  model="$(_model_for_call "$path" "$model_flag_seen" "$model_alias")"
  # Stage under a FIXED name derived only from the validated extension — the
  # untrusted original filename is never interpolated into the prompt (prompt
  # injection guard).
  local ext; ext="$(printf '%s' "${real##*.}" | tr '[:upper:]' '[:lower:]')"
  local staged="document.${ext}"
  local prompt
  prompt="$(printf 'Read the file named "%s" in your workspace and convert it to clean, well-structured Markdown. Preserve headings, lists, tables, links, and code blocks. Output ONLY the Markdown — no preamble, no commentary.' "$staged")"
  AGY_CAPTURE_STAGE_FILE="$real" AGY_CAPTURE_STAGE_AS="$staged" \
    _agy_capture "$path" "$model" "${AGY_DOCTOMD_TIMEOUT:-8m0s}" "$prompt"
}

cmd_review() {
  local focus="${1:-Please review the following diff for correctness, edge cases, security issues, and style.}"
  local path
  path="$(require_ready)"
  local repo_dir="${CLAUDE_PROJECT_DIR:-$PWD}"
  # -U<N> gives agy more context around each hunk than git's default 3
  # lines, so it can see imports / early returns / nearby code and stop
  # flagging false positives for things just outside the window.
  local ctx="${AGY_REVIEW_CONTEXT:-25}"
  local diff
  local -a diff_range=("HEAD")
  diff="$(git -C "$repo_dir" diff "-U${ctx}" HEAD 2>/dev/null || true)"
  if [ -z "$diff" ]; then
    # Fall back to the unstaged diff — and sync the name-only file list
    # below to the same range (was always "HEAD", starving full-file
    # context of the fallback's files).
    diff_range=()
    diff="$(git -C "$repo_dir" diff "-U${ctx}" 2>/dev/null || true)"
  fi
  if [ -z "$diff" ]; then
    echo "error: no git diff found in $repo_dir. Stage or make changes first." >&2
    exit 1
  fi

  # Guardrail: refuse to ship a diff containing obvious secrets unless the
  # user opted in via AGY_REVIEW_ALLOW_SECRETS=1. Best-effort; false
  # negatives are possible.
  local secret_hits
  if ! secret_hits="$(_scan_diff_for_secrets "$diff")"; then
    {
      echo
      echo "[wrapper] WARNING: diff contains values matching common secret patterns:"
      printf '%s\n' "$secret_hits" | sed 's/^/  - /'
      echo
      echo "  Sending it through agy will forward those values to Google's Gemini API."
      if [ "${AGY_REVIEW_ALLOW_SECRETS:-0}" != "1" ]; then
        echo "  Aborting. Set AGY_REVIEW_ALLOW_SECRETS=1 to proceed anyway, or remove"
        echo "  the matching lines from your diff first."
        exit 65
      fi
      echo "  Proceeding because AGY_REVIEW_ALLOW_SECRETS=1."
    } >&2
  fi

  # Full content of small changed files, so agy sees whole-file
  # structure (imports, guards) not just hunks. Cap per-file lines and
  # total bytes so a big changeset can't blow up the prompt.
  # Defaults aligned with the Node companion (lib/git.mjs). Kept well
  # under AGY_PROMPT_MAX_BYTES so the files block + diff don't get
  # truncated mid-prompt.
  local max_lines="${AGY_REVIEW_FULLFILE_MAX_LINES:-250}"
  local budget="${AGY_REVIEW_FULLFILE_BUDGET_BYTES:-12288}"
  local files_block="" used=0
  local f
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    local abs="$repo_dir/$f"
    # Regular file only, and NOT a symlink (a symlinked path in the diff
    # could point at an arbitrary host file like /etc/passwd).
    [ -f "$abs" ] && [ ! -L "$abs" ] || continue
    # Directory-symlink defense: resolve the real path and require it to
    # stay inside the repo (catches `linked_dir/file` where linked_dir
    # points outside). Skip the file if it escapes.
    if command -v realpath >/dev/null 2>&1; then
      local rp rr
      rp="$(realpath "$abs" 2>/dev/null || echo)"
      rr="$(realpath "$repo_dir" 2>/dev/null || echo "$repo_dir")"
      # Case-insensitive prefix match: Windows/macOS filesystems are
      # case-insensitive, and realpath may return different casing than
      # $repo_dir, which would wrongly skip a legitimate in-repo file.
      local rp_l rr_l
      rp_l="$(printf '%s' "$rp" | tr '[:upper:]' '[:lower:]')"
      rr_l="$(printf '%s' "$rr" | tr '[:upper:]' '[:lower:]')"
      case "$rp_l" in "$rr_l"/*) : ;; *) continue ;; esac
    fi
    # skip binary (no NUL byte => text)
    if grep -qI . "$abs" 2>/dev/null; then :; else continue; fi
    local lc; lc="$(wc -l < "$abs" 2>/dev/null || echo 999999)"
    [ "$lc" -le "$max_lines" ] || continue
    local sz; sz="$(wc -c < "$abs" 2>/dev/null || echo 0)"
    # Only count toward the budget if it actually fits — otherwise a big
    # file would inflate `used` and starve every later (small) file.
    if [ "$((used + sz))" -le "$budget" ]; then
      used=$((used + sz))
    else
      continue
    fi
    local ext="${f##*.}"
    # Dynamic fence: one more backtick than the longest backtick run in
    # the file, so content containing ``` (or ````) can't close the
    # block early. min 3.
    local maxrun
    # `|| true`: grep exits 1 when the file contains no backticks; with
    # `set -o pipefail` that propagates and `set -e` would abort the whole
    # review on the first backtick-free file (e.g. package.json). awk still
    # prints the count (3 for no-backtick input), so swallow grep's status.
    maxrun="$(grep -oE '`+' "$abs" 2>/dev/null | awk '{ if (length>m) m=length } END { n=(m<2?2:m)+1; print n }' || true)"
    [ -n "$maxrun" ] || maxrun=3
    # Build the fence without `seq` (absent in some minimal shells).
    local fence="" _i
    for ((_i = 0; _i < maxrun; _i++)); do fence="${fence}\`"; done
    files_block="${files_block}"$'\n'"### ${f}"$'\n'"${fence}${ext}"$'\n'"$(cat "$abs")"$'\n'"${fence}"$'\n'
  done < <(git -C "$repo_dir" diff --name-only "${diff_range[@]}" 2>/dev/null)

  # Secret guard, pass 2: the diff scan above only sees added lines, but the
  # files block ships the FULL current content of changed files — a secret
  # on an unchanged line would slip through. Scan exactly what we ship.
  if [ -n "$files_block" ]; then
    local file_secret_hits
    if ! file_secret_hits="$(_scan_text_for_secrets "$files_block")"; then
      {
        echo
        echo "[wrapper] WARNING: full content of a changed file matches common secret patterns:"
        printf '%s\n' "$file_secret_hits" | sed 's/^/  - /'
        echo
        echo "  These values ship to Google's Gemini API as review context even though"
        echo "  they aren't in the diff."
        if [ "${AGY_REVIEW_ALLOW_SECRETS:-0}" != "1" ]; then
          echo "  Aborting. Set AGY_REVIEW_ALLOW_SECRETS=1 to proceed anyway, or remove"
          echo "  the matching values first."
          exit 65
        fi
        echo "  Proceeding because AGY_REVIEW_ALLOW_SECRETS=1."
      } >&2
    fi
  fi

  local full
  if [ -n "$files_block" ]; then
    full=$(printf '%s\n\nFull current content of the changed files (for context — do NOT flag issues already handled elsewhere in these files):\n%s\nDiff (with expanded context):\n```diff\n%s\n```\n' "$focus" "$files_block" "$diff")
  else
    full=$(printf '%s\n\nDiff (with expanded context):\n```diff\n%s\n```\n' "$focus" "$diff")
  fi
  # Route through the write_file workaround (agy issue #76). Reviews can
  # run long on big diffs; default to 10 minutes.
  _agy_capture "$path" "" "${AGY_REVIEW_TIMEOUT:-10m0s}" "$full"
}

cmd_image() {
  local description=""
  local name=""
  local output=""
  local positional=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --name)
        if [ $# -ge 2 ]; then
          name="$2"; shift 2
        else
          echo "error: --name requires a value (e.g. --name coffee_cup)" >&2
          exit 64
        fi ;;
      --name=*)
        name="${1#--name=}"
        if [ -z "$name" ]; then
          echo "error: --name= requires a non-empty value" >&2
          exit 64
        fi
        shift ;;
      --output)
        if [ $# -ge 2 ]; then
          output="$2"; shift 2
        else
          echo "error: --output requires a path (e.g. --output /tmp/out.png)" >&2
          exit 64
        fi ;;
      --output=*)
        output="${1#--output=}"
        if [ -z "$output" ]; then
          echo "error: --output= requires a non-empty path" >&2
          exit 64
        fi
        shift ;;
      --)        shift; positional+=("$@"); break ;;
      *)         positional+=("$1"); shift ;;
    esac
  done
  # `:-`: bash 3.2 (macOS /bin/bash) treats "${positional[*]}" of an empty
  # array as unbound under `set -u`, which aborted with "unbound variable"
  # instead of the usage error below (upstream fix d3b981f).
  description="${positional[*]:-}"
  if [ -z "$description" ]; then
    echo "error: image requires a description" >&2
    exit 64
  fi
  local agy_path
  agy_path="$(require_ready)"

  # agy issue #76 workaround. The old design asked agy to end its reply
  # with an `IMAGE_PATH:` line, but agy --print flushes nothing to a
  # non-TTY stdout (so the marker never arrived) and hangs on a non-TTY
  # stdin. Instead: agy generates the image, then write_file's the
  # saved image's absolute path into a marker file we read back.
  local outdir
  outdir="$(mktemp -d 2>/dev/null)" || { echo "error: could not create temp dir" >&2; return 1; }
  local marker="$outdir/image-path.txt"
  local marker_path target_dir
  if command -v cygpath >/dev/null 2>&1; then
    marker_path="$(cygpath -w "$marker")"
    target_dir="$(cygpath -w "$outdir")"
  else
    marker_path="$marker"
    target_dir="$outdir"
  fi
  local name_clause=""
  if [ -n "$name" ]; then
    name_clause=" Save the image with the name \"${name}\"."
  fi
  local prompt
  prompt="$(printf 'Use your built-in generate_image tool to create this image: %s.%s\n\nAfter the image is saved, use the write_file tool to write ONLY the absolute filesystem path of the saved image (a single line, no quotes, nothing else) to this exact path:\n%s\nDo not print anything to chat. The marker file is your only textual deliverable.' "$description" "$name_clause" "$marker_path")"

  # Run agy FROM the throwaway temp dir (cwd="$outdir") so its
  # auto-approved write tools land in the temp, not the user's repo; the
  # generated image is read back from the marker and copied to --output by
  # this script. stdin </dev/null avoids the non-TTY hang.
  ( cd "$outdir" && "$agy_path" --dangerously-skip-permissions --sandbox --add-dir "$target_dir" \
    --print-timeout "${AGY_IMAGE_TIMEOUT:-8m0s}" --print "$prompt" \
    </dev/null >/dev/null 2>&1 ) || true

  local src=""
  if [ -s "$marker" ]; then
    src="$(grep -m1 -v '^[[:space:]]*$' "$marker" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    # agy may report a native Windows path; translate so [ -f ] works.
    if command -v cygpath >/dev/null 2>&1 && printf '%s' "$src" | grep -q '^[A-Za-z]:[\\/]'; then
      src="$(cygpath -u "$src" 2>/dev/null || printf '%s' "$src")"
    fi
  fi
  rm -rf "$outdir"

  if [ -n "$src" ] && [ -f "$src" ]; then
    # Guardrail: refuse paths outside agy's artifacts dirs (prompt
    # injection could otherwise make us copy an arbitrary host file).
    if ! _image_source_in_allowlist "$src"; then
      {
        echo "[wrapper] error: refusing to use image source path outside agy's artifacts directory."
        echo "[wrapper]        path: $src"
        echo "[wrapper]        allowed prefixes: ~/.gemini/antigravity-cli/{brain,scratch,cache}/"
      } >&2
      return 66
    fi
    echo "[wrapper] generated: $src"
    if [ -n "$output" ]; then
      cp "$src" "$output"
      echo "[wrapper] copied to: $output"
    fi
    return 0
  fi

  {
    echo "[wrapper] error: agy did not produce a locatable image."
    echo "[wrapper]        The marker file was empty or the path it named does not exist."
    echo "[wrapper]        generate_image may have failed or write_file was declined; check"
    echo "[wrapper]        ~/.gemini/antigravity-cli/log/ or run \`agy\` interactively."
  } >&2
  return 1
}

cmd_help() {
  cat <<'HELP'
/agy:* commands (Claude Code plugin for the Antigravity CLI)

Slash commands
  /agy:setup                            Verify agy install + auth. Offers install if missing.
  /agy:ask [--model A] <prompt>         One-shot prompt; returns agy's response verbatim.
  /agy:delegate [--background] [--model A] <task>
                                        Hand a task to the agy:runner subagent.
  /agy:research [--background] [--model A] <topic>
                                        Deep-research investigation via agy:runner.
  /agy:review [focus]                   Send current `git diff` to agy for review.
  /agy:image [--name S] [--output P] <description>
                                        Generate an image via agy's built-in tool.
  /agy:scrape [--model A] <url>         Fetch a web page (read-only) -> Markdown. SSRF-guarded.
  /agy:doc-to-md [--model A] <path>     Convert a local doc (PDF/DOCX/HTML/...) -> Markdown,
                                        read-only + path-guarded (allow-listed types only).
  /agy:help                             This help.

Model selection (--model)
HELP
  local agy_path models=""
  if agy_path="$(find_agy)"; then
    models="$(list_models "$agy_path" || true)"
  fi
  print_model_table 1 "$models"
  cat <<'HELP'

How --model works
  Aliases resolve at call time against `agy models` to the newest model in
  their family, so a new Gemini/Claude release is picked up without a plugin
  update. Any id or display name that `agy models` lists is also accepted
  verbatim. The result goes to agy's own --model flag for that call only;
  settings.json is never touched, so a TUI open in parallel keeps its model.
  Needs agy 1.2.0 or newer. Unknown names fail with exit 64.

Read-only commands
  /agy:ask, /agy:review, /agy:scrape and /agy:doc-to-md run agy from a
  throwaway temp dir, pinned to agy's Default agent mode for that run, with
  no --dangerously-skip-permissions: agy can read, but writes outside its
  temp workspace are denied. (agy 1.5.7+ otherwise starts headless runs in
  Auto mode, which approves such writes.)

Underlying CLI
  Run `agy --help` for agy's own flags (--model, --effort, --mode,
  --settings, --output-format, --sandbox, --add-dir, --print-timeout, ...).
  Subcommands include models, agents, changelog, mcp, plugin, update.
HELP
}

main() {
  restore_orphaned_backup 2>/dev/null || true

  case "${1:-}" in
    check)                cmd_check ;;
    ask)       shift;     cmd_ask "$@" ;;
    review)    shift;     cmd_review "$@" ;;
    image)     shift;     cmd_image "$@" ;;
    scrape)    shift;     cmd_scrape "$@" ;;
    doc-to-md) shift;     cmd_doc_to_md "$@" ;;
    help|-h|--help|"")    cmd_help ;;
    *)                  echo "error: unknown subcommand '$1'" >&2; cmd_help >&2; exit 64 ;;
  esac
}

# Skip dispatch when sourced (lets unit tests call functions directly).
if [ "${BASH_SOURCE[0]:-}" = "${0:-}" ]; then
  main "$@"
fi

// agy.mjs — Antigravity CLI integration primitives.
//
// What lives here:
//   - findAgyBinary(): locate the `agy` executable on the user's
//     machine, matching the resolution order documented in
//     agy-run.sh (PATH first, then a small set of platform-specific
//     fallback paths).
//   - getAuthStatus(): coarse-grained "are we authed?" probe that
//     does NOT call out to `agy` (so it's cheap to call from
//     /agy:setup).
//
//   - resolveModel(): --model alias/name resolution against
//     `agy models`, same rules as agy-run.sh's resolve_model. The value
//     goes to agy's own --model flag; settings.json is never touched.
//   - readOnlyModeArgs(): the flags that pin a read-only run to agy's
//     Default agent mode (agy >= 1.5.7 otherwise starts headless runs in
//     Auto, which approves writes outside the workspace).

import os from "node:os";
import path from "node:path";
import process from "node:process";
import { binaryAvailable, runCaptured } from "./process.mjs";
import { pathExists } from "./fs.mjs";

/** agy release that made --model reliable in -p runs. */
export const MIN_AGY_MODEL_VERSION = "1.2.0";
/** agy release that made Auto the default Agent Mode, -p runs included. */
export const AGY_AUTO_DEFAULT_VERSION = "1.5.7";
/** Session-only settings that select agy's Default agent mode. */
export const READ_ONLY_SETTINGS = '{"agentMode":""}';

// Same table as agy-run.sh's MODEL_ALIASES: aliases, a regex matched against
// the lowercased display name, a regex that wins a version tie, and a
// description for when no listing is available. Aliases pick the newest
// version in their family.
export const MODEL_ALIASES = [
  { aliases: ["flash-low"], pattern: /^gemini [0-9.]+ flash [(]low[)]$/, prefer: null, description: "newest Gemini Flash (Low)" },
  { aliases: ["flash-medium", "flash-med"], pattern: /^gemini [0-9.]+ flash [(]medium[)]$/, prefer: null, description: "newest Gemini Flash (Medium)" },
  { aliases: ["flash", "flash-high"], pattern: /^gemini [0-9.]+ flash [(]high[)]$/, prefer: null, description: "newest Gemini Flash (High)" },
  { aliases: ["pro-low"], pattern: /^gemini [0-9.]+ pro [(]low[)]$/, prefer: null, description: "newest Gemini Pro (Low)" },
  { aliases: ["pro", "pro-high"], pattern: /^gemini [0-9.]+ pro [(]high[)]$/, prefer: null, description: "newest Gemini Pro (High)" },
  { aliases: ["sonnet", "claude-sonnet"], pattern: /^claude sonnet [0-9.]+( [(].*[)])?$/, prefer: /[(]thinking[)]$/, description: "newest Claude Sonnet" },
  { aliases: ["opus", "claude-opus"], pattern: /^claude opus [0-9.]+( [(].*[)])?$/, prefer: /[(]thinking[)]$/, description: "newest Claude Opus" },
  { aliases: ["gpt-oss", "gpt-oss-120b"], pattern: /^gpt-oss 120b( [(].*[)])?$/, prefer: /[(]medium[)]$/, description: "GPT-OSS 120B" },
];

/**
 * Return the resolved path to the `agy` binary, or null if it cannot
 * be found. Resolution order:
 *   1. AGY_BIN env var (if it exists on disk)
 *   2. `agy` on PATH (which/where)
 *   3. ${HOME}/.local/bin/agy           (macOS / Linux user install)
 *   4. /opt/antigravity/bin/agy         (Linux system install)
 *   5. /usr/local/bin/agy               (macOS Homebrew install)
 *   6. ${LOCALAPPDATA}/agy/bin/agy.exe  (Windows installer location)
 */
export async function findAgyBinary(envOverride) {
  const env = envOverride ?? process.env;
  if (env.AGY_BIN && await pathExists(env.AGY_BIN)) {
    return env.AGY_BIN;
  }
  if (await binaryAvailable("agy")) {
    return "agy"; // PATH lookup will resolve at spawn time
  }
  const home = env.HOME ?? os.homedir();
  const localAppData = env.LOCALAPPDATA ?? (home ? path.join(home, "AppData", "Local") : null);
  const candidates = [
    home && path.join(home, ".local", "bin", "agy"),
    "/opt/antigravity/bin/agy",
    "/usr/local/bin/agy",
    localAppData && path.join(localAppData, "agy", "bin", "agy.exe"),
  ].filter(Boolean);

  for (const candidate of candidates) {
    if (await pathExists(candidate)) return candidate;
  }
  return null;
}

/**
 * Coarse auth probe. Mirrors the bash wrapper's auth_status() —
 * if ANTIGRAVITY_API_KEY is set, we assume api-key auth; otherwise
 * we look for the OAuth state dir on disk. Returns one of:
 *   "api-key" | "oauth" | "missing"
 *
 * This is intentionally cheap (no subprocess). It cannot detect a
 * stale / expired token — only that *some* credential material is
 * in place. /agy:setup's real ping confirms it actually works.
 */
export async function getAuthStatus(envOverride) {
  const env = envOverride ?? process.env;
  if (env.ANTIGRAVITY_API_KEY && env.ANTIGRAVITY_API_KEY.length > 0) {
    return "api-key";
  }
  const home = env.HOME ?? os.homedir();
  if (!home) return "missing";
  const oauthDirs = [
    path.join(home, ".config", "antigravity"),
    path.join(home, ".gemini", "antigravity-cli"),
  ];
  for (const dir of oauthDirs) {
    if (await pathExists(dir)) return "oauth";
  }
  return "missing";
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/** Compare dotted versions numerically ("1.10" > "1.9"). Returns -1, 0, 1. */
export function compareVersions(a, b) {
  const x = String(a ?? "").split(".").filter((s) => s !== "");
  const y = String(b ?? "").split(".").filter((s) => s !== "");
  const n = Math.max(x.length, y.length);
  for (let i = 0; i < n; i++) {
    const xi = Number(x[i] ?? 0) || 0;
    const yi = Number(y[i] ?? 0) || 0;
    if (xi > yi) return 1;
    if (xi < yi) return -1;
  }
  return 0;
}

export function versionAtLeast(version, min) {
  return compareVersions(version, min) >= 0;
}

/** First dotted version in `text` ("agy 1.5.16" -> "1.5.16"), or null. */
export function parseVersion(text) {
  const m = String(text ?? "").match(/[0-9]+\.[0-9]+(\.[0-9]+)?/);
  return m ? m[0] : null;
}

/** agy's version, or null when it can't be read. */
export async function agyVersion(agyBin) {
  const r = await runCaptured(agyBin, ["--version"], { timeoutMs: 15_000 });
  return r.exitCode === 0 ? parseVersion(r.stdout) : null;
}

// ---------------------------------------------------------------------------
// --model resolution (mirrors agy-run.sh: list_models / resolve_model)
// ---------------------------------------------------------------------------

/**
 * Parse `agy models` output: one "id<TAB>display name" row per model (some
 * builds print names only, giving an empty id). Skips blank lines, the
 * "Fetching available models..." banner and "Header:" lines.
 */
export function parseModelList(text) {
  const models = [];
  for (const raw of String(text ?? "").split("\n")) {
    const line = raw.replace(/\r$/, "");
    if (!line.trim() || /:\s*$/.test(line) || line.startsWith("Fetching ")) continue;
    const tab = line.indexOf("\t");
    const id = (tab >= 0 ? line.slice(0, tab) : "").trim();
    const name = (tab >= 0 ? line.slice(tab + 1) : line).trim();
    if (name) models.push({ id, name });
  }
  return models;
}

// The family version inside a display name: its first purely numeric
// word ("gemini 3.8 flash (high)" -> "3.8"), or "" when there is none.
function familyVersion(lowerName) {
  return lowerName.split(" ").find((w) => /^[0-9]+(\.[0-9]+)*$/.test(w)) ?? "";
}

/**
 * The model whose lowercased name matches `pattern` with the newest family
 * version; on a version tie a name matching `prefer` wins. null if none.
 */
export function newestMatch(models, pattern, prefer = null) {
  let best = null;
  let bestVersion = "";
  let bestPreferred = false;
  for (const m of models) {
    const lower = m.name.toLowerCase();
    if (!pattern.test(lower)) continue;
    const version = familyVersion(lower);
    const preferred = Boolean(prefer && prefer.test(lower));
    if (
      best === null ||
      compareVersions(version, bestVersion) > 0 ||
      (compareVersions(version, bestVersion) === 0 && preferred && !bestPreferred)
    ) {
      best = m;
      bestVersion = version;
      bestPreferred = preferred;
    }
  }
  return best;
}

/** The model whose id or display name equals `input`, ignoring case. */
export function exactMatch(models, input) {
  const want = String(input).toLowerCase();
  return models.find((m) => m.id.toLowerCase() === want || m.name.toLowerCase() === want) ?? null;
}

/** The value for `agy --model`: the id when listed, else the display name. */
export function modelArg(model) {
  return model.id || model.name;
}

export function aliasRow(input) {
  const lc = String(input).toLowerCase();
  return MODEL_ALIASES.find((r) => r.aliases.includes(lc)) ?? null;
}

/** Human-readable alias + model table, for error messages and help. */
export function formatModelTable(models) {
  const lines = [];
  const listed = Array.isArray(models) && models.length > 0;
  lines.push(
    listed
      ? "Aliases (case-insensitive), resolved against `agy models`:"
      : "Aliases (case-insensitive). `agy models` could not be read, so they are shown unresolved:",
  );
  for (const row of MODEL_ALIASES) {
    let target = row.description;
    if (listed) {
      const m = newestMatch(models, row.pattern, row.prefer);
      target = m ? m.name : `(not available: ${row.description})`;
    }
    lines.push(`  ${row.aliases.join(", ").padEnd(28)}  -> ${target}`);
  }
  if (listed) {
    lines.push("", "Available models (names and ids are accepted verbatim, case-insensitive):");
    for (const m of models) lines.push(m.id ? `  ${m.name.padEnd(32)}  ${m.id}` : `  ${m.name}`);
  }
  return lines.join("\n");
}

/**
 * Resolve `input` against a parsed `agy models` list. Pure (no agy call).
 * Returns { ok: true, value } or { ok: false, error }.
 */
export function resolveModelFromList(models, input) {
  const row = aliasRow(input);
  let match = exactMatch(models, input);
  if (!match && row) {
    match = newestMatch(models, row.pattern, row.prefer);
    if (!match) {
      return {
        ok: false,
        error: `alias '${input}' has no matching model in \`agy models\` (expected: ${row.description})`,
      };
    }
  } else if (!match) {
    return { ok: false, error: `unknown model '${input}' (not an alias and not listed by \`agy models\`)` };
  }
  return { ok: true, value: modelArg(match) };
}

/** Parsed `agy models`, or null when it can't be listed. */
export async function listModels(agyBin) {
  const r = await runCaptured(agyBin, ["models"], { timeoutMs: 60_000 });
  if (r.exitCode !== 0) return null;
  return parseModelList(r.stdout);
}

/**
 * Resolve a --model value for agy's own --model flag.
 * Returns { ok: true, value, note? } or { ok: false, error, exitCode }
 * (exitCode 64 = bad value, 1 = environment problem), matching agy-run.sh.
 */
export async function resolveModel(agyBin, input) {
  if (typeof input !== "string" || !input.trim()) {
    return { ok: false, exitCode: 64, error: "--model requires a non-empty value (e.g. --model pro)" };
  }
  const version = await agyVersion(agyBin);
  if (version && !versionAtLeast(version, MIN_AGY_MODEL_VERSION)) {
    return {
      ok: false,
      exitCode: 1,
      error: `agy ${version} is too old for --model (needs ${MIN_AGY_MODEL_VERSION} or newer); update it with \`agy update\`.`,
    };
  }
  const models = await listModels(agyBin);
  if (!models || models.length === 0) {
    if (aliasRow(input)) {
      return {
        ok: false,
        exitCode: 1,
        error: "could not list models with `agy models` (run it directly to see why); aliases need that list, so pass the full model name instead.",
      };
    }
    return { ok: true, value: input, note: `could not list models; passing '${input}' to agy unchecked` };
  }
  const r = resolveModelFromList(models, input);
  if (!r.ok) return { ok: false, exitCode: 64, error: `${r.error}\n${formatModelTable(models)}` };
  return { ok: true, value: r.value };
}

// ---------------------------------------------------------------------------
// Read-only agent mode
// ---------------------------------------------------------------------------

/**
 * Flags that pin a read-only agy run to Default agent mode.
 *
 * agy 1.5.7 made Auto the default Agent Mode for headless -p runs, and Auto
 * approves writes OUTSIDE the workspace (any file in a git/jj/hg repo, any
 * create or append elsewhere). Verified on agy 1.5.16: a plugin-style run
 * with no --dangerously-skip-permissions overwrote a file in a git repo
 * outside its temp workspace. Default mode makes a headless run deny such
 * actions instead. `--settings` applies it to this run only, never saved.
 *
 * Returns { ok: true, args } (args may be empty on agy builds that predate
 * Auto mode and --settings) or { ok: false, error } when agy defaults to
 * Auto but can't be pinned (fail closed: the caller must not run it).
 */
export async function readOnlyModeArgs(agyBin) {
  const help = await runCaptured(agyBin, ["--help"], { timeoutMs: 15_000 });
  if (`${help.stdout}\n${help.stderr}`.includes("--settings")) {
    return { ok: true, args: ["--settings", READ_ONLY_SETTINGS] };
  }
  const version = await agyVersion(agyBin);
  if (version && versionAtLeast(version, AGY_AUTO_DEFAULT_VERSION)) {
    return {
      ok: false,
      error:
        `agy ${version} starts headless runs in Auto mode, which can write outside the ` +
        "workspace, and this build has no --settings flag to pin Default mode. " +
        "Refusing to run a read-only command; update agy with `agy update`.",
    };
  }
  return { ok: true, args: [] };
}

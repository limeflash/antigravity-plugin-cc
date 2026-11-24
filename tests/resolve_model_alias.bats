#!/usr/bin/env bats
# Tests for --model resolution (0.9.0: aliases resolve against `agy models`
# and go to agy's own --model flag) and for the read-only agent-mode pin.
# A stub `agy` answers --version / --help / models from env vars, so no real
# agy or network is needed.

load helpers

setup() {
  load_agy_run
  STUB="$BATS_TEST_TMPDIR/agy"
  cat >"$STUB" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  --version) printf '%s\n' "${STUB_VERSION:-1.5.16}"; exit 0 ;;
  --help)    printf '%s\n' "${STUB_HELP:-Usage of agy:
  --model    Model for the current CLI session
  --settings Settings for this session only}"; exit 0 ;;
  models)
    [ "${STUB_MODELS_FAIL:-0}" = 1 ] && exit 1
    printf 'Fetching available models...\n'
    printf '%s\n' "${STUB_MODELS:-}"
    exit 0 ;;
esac
exit 0
STUB
  chmod +x "$STUB"
  STUB_MODELS="$(printf '%s\n' \
    'gemini-3.5-flash-high	Gemini 3.5 Flash (High)' \
    'gemini-3.8-flash-high	Gemini 3.8 Flash (High)' \
    'gemini-3.8-flash-low	Gemini 3.8 Flash (Low)' \
    'claude-sonnet-4-6	Claude Sonnet 4.6' \
    'claude-sonnet-4-6-thinking	Claude Sonnet 4.6 (Thinking)' \
    'claude-opus-4-6-thinking	Claude Opus 4.6 (Thinking)' \
    'gpt-oss-120b-medium	GPT-OSS 120B (Medium)')"
  export STUB_MODELS
}

@test "alias 'flash' picks the newest Gemini Flash (High) by id" {
  run resolve_model "$STUB" "flash"
  [ "$status" -eq 0 ]
  [ "$output" = "gemini-3.8-flash-high" ]
}

@test "alias 'flash-low' resolves within its own family" {
  run resolve_model "$STUB" "flash-low"
  [ "$status" -eq 0 ]
  [ "$output" = "gemini-3.8-flash-low" ]
}

@test "alias 'sonnet' prefers the (Thinking) variant on a version tie" {
  run resolve_model "$STUB" "sonnet"
  [ "$status" -eq 0 ]
  [ "$output" = "claude-sonnet-4-6-thinking" ]
}

@test "alias is case-insensitive" {
  run resolve_model "$STUB" "OPUS"
  [ "$status" -eq 0 ]
  [ "$output" = "claude-opus-4-6-thinking" ]
}

@test "a display name from agy models is accepted verbatim and mapped to its id" {
  run resolve_model "$STUB" "claude opus 4.6 (thinking)"
  [ "$status" -eq 0 ]
  [ "$output" = "claude-opus-4-6-thinking" ]
}

@test "a model id from agy models passes through" {
  run resolve_model "$STUB" "gpt-oss-120b-medium"
  [ "$status" -eq 0 ]
  [ "$output" = "gpt-oss-120b-medium" ]
}

@test "unknown model exits 64 and lists what is available" {
  run resolve_model "$STUB" "definitely-not-a-real-model"
  [ "$status" -eq 64 ]
  [[ "$output" == *"unknown model"* ]]
  [[ "$output" == *"Gemini 3.8 Flash (High)"* ]]
}

@test "alias with no model in its family exits 64 (pro on an account without Pro)" {
  run resolve_model "$STUB" "pro"
  [ "$status" -eq 64 ]
  [[ "$output" == *"no matching model"* ]]
}

@test "empty --model value exits 64" {
  run model_value_error
  [ "$status" -eq 64 ]
  [[ "$output" == *"requires a non-empty value"* ]]
}

@test "unreadable agy models: an alias fails, a full name passes through unchecked" {
  export STUB_MODELS_FAIL=1
  run resolve_model "$STUB" "flash"
  [ "$status" -eq 1 ]
  run resolve_model "$STUB" "Some Future Model"
  [ "$status" -eq 0 ]
  [[ "$output" == *"Some Future Model" ]]
}

@test "--model needs agy 1.2.0 or newer" {
  export STUB_VERSION=1.1.5
  run require_model_support "$STUB"
  [ "$status" -eq 1 ]
  [[ "$output" == *"too old for --model"* ]]
  export STUB_VERSION=1.5.16
  run require_model_support "$STUB"
  [ "$status" -eq 0 ]
}

@test "_model_for_call prints nothing when --model was not given" {
  run _model_for_call "$STUB" 0 ""
  [ "$status" -eq 0 ]
  [ -z "$output" ]
}

@test "read-only runs are pinned to Default agent mode when agy has --settings" {
  _readonly_mode_flags "$STUB"
  [ "${#AGY_RO_FLAGS[@]}" -eq 2 ]
  [ "${AGY_RO_FLAGS[0]}" = "--settings" ]
  [ "${AGY_RO_FLAGS[1]}" = '{"agentMode":""}' ]
}

@test "agy older than Auto-by-default without --settings runs unpinned" {
  export STUB_VERSION=1.1.5 STUB_HELP="Usage of agy:"
  _readonly_mode_flags "$STUB"
  [ "${#AGY_RO_FLAGS[@]}" -eq 0 ]
}

@test "agy that defaults to Auto but cannot be pinned is refused (fail closed)" {
  export STUB_VERSION=1.5.7 STUB_HELP="Usage of agy:"
  run _readonly_mode_flags "$STUB"
  [ "$status" -eq 1 ]
  [[ "$output" == *"Refusing to run a read-only command"* ]]
}

@test "image without a description is a usage error under bash 3.2 too" {
  run bash "$AGY_RUN" image
  [ "$status" -eq 64 ]
  [[ "$output" == *"image requires a description"* ]]
}

#!/usr/bin/env bash
set -Eeuo pipefail

base_url=${OCTOROUTE_URL:-http://127.0.0.1:8081}
base_url=${base_url%/}
api_key=${OCTOROUTE_API_KEY:-}
local_model=${OCTOROUTE_LOCAL_MODEL:-worker}
provider_model=${OCTOROUTE_PROVIDER_MODEL:-}
timeout_seconds=${OCTOROUTE_CANARY_TIMEOUT_SECONDS:-300}
curl_bin=${CURL_BIN:-curl}

fail() {
  printf 'v3 canary failed: %s\n' "$*" >&2
  exit 1
}

case "$base_url" in
  http://* | https://*) ;;
  *) fail 'OCTOROUTE_URL must use http:// or https://' ;;
esac

[[ -n "$api_key" ]] || fail 'OCTOROUTE_API_KEY is required'
[[ "$api_key" != *$'\n'* && "$api_key" != *$'\r'* ]] \
  || fail 'OCTOROUTE_API_KEY must not contain a line break'
[[ "$timeout_seconds" =~ ^[1-9][0-9]*$ ]] \
  || fail 'OCTOROUTE_CANARY_TIMEOUT_SECONDS must be a positive integer'
[[ "$local_model" =~ ^[A-Za-z0-9._-]{1,128}$ ]] \
  || fail 'OCTOROUTE_LOCAL_MODEL is not a bounded model name'
if [[ -n "$provider_model" ]]; then
  [[ "$provider_model" =~ ^[A-Za-z0-9._-]{1,128}$ ]] \
    || fail 'OCTOROUTE_PROVIDER_MODEL is not a bounded model name'
fi
command -v "$curl_bin" >/dev/null 2>&1 || fail "curl executable not found: $curl_bin"
command -v awk >/dev/null 2>&1 || fail 'awk is required for SSE validation'

escaped_key=${api_key//\\/\\\\}
escaped_key=${escaped_key//\"/\\\"}

# Keep the bearer out of argv and off disk. curl reads this one-line config
# from stdin before starting the request; request payloads are supplied with
# --data-binary, so the config stream is never confused with a request body.
curl_auth() {
  printf 'header = "Authorization: Bearer %s"\n' "$escaped_key" \
    | "$curl_bin" --config - "$@"
}

common=(
  --silent
  --show-error
  --fail-with-body
  --connect-timeout 10
  --max-time "$timeout_seconds"
)

check_get() {
  local label=$1
  local path=$2
  local auth=$3
  local args=("${common[@]}" --output /dev/null)
  if [[ "$auth" == yes ]]; then
    curl_auth "${args[@]}" "$base_url$path" \
      || fail "$label"
  else
    "$curl_bin" "${args[@]}" "$base_url$path" \
      || fail "$label"
  fi
  printf 'ok: %s\n' "$label"
}

chat_payload() {
  local model=$1
  local stream=$2
  printf '{"model":"%s","messages":[{"role":"user","content":"Reply with exactly: octoroute-v3-canary"}],"stream":%s}' \
    "$model" "$stream"
}

check_chat() {
  local label=$1
  local model=$2
  local privacy=$3
  local payload
  payload=$(chat_payload "$model" false)
  local args=(
    "${common[@]}"
    --header 'Content-Type: application/json'
    --data-binary "$payload"
    --output /dev/null
  )
  if [[ "$privacy" == local-only ]]; then
    args+=(--header 'X-Octoroute-Privacy: local-only')
  fi
  curl_auth "${args[@]}" "$base_url/v1/chat/completions" \
    || fail "$label"
  printf 'ok: %s\n' "$label"
}

check_stream() {
  local label=$1
  local model=$2
  local payload
  payload=$(chat_payload "$model" true)
  curl_auth "${common[@]}" \
    --header 'Content-Type: application/json' \
    --header 'Accept: text/event-stream' \
    --header 'X-Octoroute-Privacy: local-only' \
    --data-binary "$payload" \
    "$base_url/v1/chat/completions" \
    | awk 'BEGIN { done = 0 } { sub(/\r$/, "") } $0 == "data: [DONE]" { done = 1 } END { exit(done ? 0 : 1) }' \
    || fail "$label did not complete with data: [DONE]"
  printf 'ok: %s\n' "$label"
}

check_get 'liveness' '/health/live' no
check_get 'cached active readiness' '/health/ready' no
check_get 'virtual model listing' '/v1/models' yes
check_chat 'local-only completion' "$local_model" local-only
check_stream 'local-only SSE completion' "$local_model"
if [[ -n "$provider_model" ]]; then
  check_chat 'explicit provider completion' "$provider_model" cloud
fi

printf 'v3 canary passed\n'

#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:3031}"
PROJECT="${PROJECT:-$(pwd)}"
RPI_BIN="${RPI_BIN:-rpi}"
RPI_WEB_BIN="${RPI_WEB_BIN:-rpi-web}"
ROOT="${TMPDIR:-/tmp}/rpi-web-smoke-$$"
REGISTRY="$ROOT/registry.json"
SESSION_DIR="$ROOT/sessions"
LOG="$ROOT/bridge.log"
mkdir -p "$SESSION_DIR"
bridge_pid=""
cleanup() {
  if [[ -n "$bridge_pid" ]]; then kill "$bridge_pid" 2>/dev/null || true; wait "$bridge_pid" 2>/dev/null || true; fi
  rm -rf "$ROOT"
}
trap cleanup EXIT

curl_json() {
  curl -fsS --retry 10 --retry-delay 0.2 -H 'x-rpi-web: 1' "$@"
}
assert_json() {
  python - "$1" <<'PY'
import json, sys
value = json.loads(sys.argv[1])
if not value:
    raise SystemExit("empty JSON response")
PY
}

"$RPI_WEB_BIN" --project "$PROJECT" --rpi-bin "$RPI_BIN" --bind 127.0.0.1 --port "${BASE_URL##*:}" --session-dir "$SESSION_DIR" --registry "$REGISTRY" >"$LOG" 2>&1 &
bridge_pid=$!

health=""
for _ in $(seq 1 40); do
  health=$(curl -sS "$BASE_URL/api/v1/health" 2>/dev/null || true)
  [[ "$health" == *'"ok":true'* ]] && break
  sleep 0.25
done
[[ "$health" == *'"ok":true'* ]] || { cat "$LOG"; exit 1; }

echo "PASS health"
caps=$(curl_json "$BASE_URL/api/v1/capabilities")
assert_json "$caps"
python - "$caps" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
required={'history','queue','queue_status','fork','clone','stats','history_mutation','provider_auth','oauth','session_metadata'}
actual=set(v.get('supportedActions', []))
missing=required-actual
if missing: raise SystemExit(f'missing capabilities: {sorted(missing)}')
PY
echo "PASS capabilities"

created=$(curl_json -X POST "$BASE_URL/api/v1/sessions")
session_id=$(python - "$created" <<'PY'
import json, sys
print(json.loads(sys.argv[1])['sessionId'])
PY
)
[[ -n "$session_id" ]]
echo "PASS create session $session_id"

snapshot=$(curl_json "$BASE_URL/api/v1/sessions/$session_id/snapshot")
python - "$snapshot" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
for key in ('messages','history','events','lastSeq'):
    assert key in v, key
PY
echo "PASS snapshot"

command() {
  curl_json -X POST "$BASE_URL/api/v1/sessions/$session_id/commands" \
    -H 'content-type: application/json' -d "$1"
}
history=$(command '{"type":"get_history"}')
stats=$(command '{"type":"get_stats"}')
queue=$(command '{"type":"get_queue"}')
auth=$(command '{"type":"get_auth_status"}')
for value in "$history" "$stats" "$queue" "$auth"; do assert_json "$value"; done
python - "$stats" "$auth" <<'PY'
import json, sys
stats, auth = (json.loads(value) for value in sys.argv[1:])
assert 'operationCount' in stats['data']
assert 'providers' in auth['data']
serialized=json.dumps(auth)
for forbidden in ('apiKey', 'accessToken', 'refreshToken', 'secret'):
    assert forbidden not in serialized
PY
echo "PASS history stats queue auth"

queued=$(command '{"type":"queue_message","message":"smoke follow-up","deliverAs":"followUp"}')
entry_id=$(python - "$queued" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
assert v['success'] is True
print(v['data']['entryId'])
PY
)
queue_after=$(command "{\"type\":\"get_queue\"}")
python - "$queue_after" "$entry_id" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
assert any(item['entryId']==sys.argv[2] for item in v['data']['items'])
PY
command '{"type":"clear_queue"}' >/dev/null
queue_history=$(command '{"type":"get_queue","includeCompleted":true}')
python - "$queue_history" "$entry_id" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
item=next(item for item in v['data']['items'] if item['entryId']==sys.argv[2])
assert item['status'] in {'cancelled', 'cleared'}
PY
echo "PASS queue enqueue/clear/status"

navigated=$(command '{"type":"navigate","targetId":null}')
python - "$navigated" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
assert v['success'] is True
assert v['data']['outcome'] in {'completed', 'declined'}
PY
echo "PASS navigate"

fork=$(curl_json -X POST "$BASE_URL/api/v1/sessions/$session_id/fork" \
  -H 'content-type: application/json' \
  -d '{"title":"Smoke Fork"}')
clone=$(curl_json -X POST "$BASE_URL/api/v1/sessions/$session_id/clone" \
  -H 'content-type: application/json' \
  -d '{"title":"Smoke Clone"}')
python - "$fork" "$clone" <<'PY'
import json, sys
for value, expected in zip(sys.argv[1:], ('Smoke Fork', 'Smoke Clone')):
    v=json.loads(value)
    assert v['sourceSessionId']
    assert v['sessionId'] != v['sourceSessionId']
    assert v['title'] == expected
PY
renamed=$(curl_json -X POST "$BASE_URL/api/v1/sessions/$session_id/rename" \
  -H 'content-type: application/json' -d '{"title":"Smoke Root"}')
python - "$renamed" <<'PY'
import json, sys
assert json.loads(sys.argv[1])['title'] == 'Smoke Root'
PY
listed=$(curl_json "$BASE_URL/api/v1/sessions")
python - "$listed" <<'PY'
import json, sys
sessions=json.loads(sys.argv[1])['sessions']
by_title={item.get('title'): item for item in sessions}
assert 'Smoke Root' in by_title
assert by_title['Smoke Fork']['relation'] in {'fork', 'clone'}
assert by_title['Smoke Clone']['relation'] == 'clone'
assert by_title['Smoke Fork']['parentSessionId'] == by_title['Smoke Root']['sessionId']
PY
echo "PASS fork/clone/rename"

if [[ "${SMOKE_PROMPT:-0}" == "1" ]]; then
  prompt=$(curl -sS -H 'x-rpi-web: 1' -X POST "$BASE_URL/api/v1/sessions/$session_id/commands" \
    -H 'content-type: application/json' -d '{"type":"prompt","message":"Reply with exactly: smoke ok"}' || true)
  if [[ "$prompt" == *"429"* || "$prompt" == *"rate limit"* || "$prompt" == *"quota"* ]]; then
    echo "SKIP provider prompt: provider quota/rate limit"
    exit 0
  fi
  [[ "$prompt" == *'"success":true'* ]] || { echo "$prompt"; exit 1; }
  echo "PASS provider prompt accepted"
  settled=""
  for _ in $(seq 1 120); do
    settled=$(curl -sS "$BASE_URL/api/v1/sessions/$session_id/snapshot" || true)
    if [[ "$settled" == *'"latestSettlement":{'* ]]; then break; fi
    sleep 1
  done
  python - "$settled" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
settlement=v.get('latestSettlement')
if not settlement:
    raise SystemExit('provider prompt did not settle within timeout')
assert settlement.get('outcome') in {'completed', 'aborted', 'failed', 'declined'}
PY
  echo "PASS provider settlement"
fi

echo "PASS dev-web smoke"

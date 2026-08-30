#!/usr/bin/env bash
set -euo pipefail

L1_LOCAL_RPC=${L1_LOCAL_RPC:-http://localhost:8899}
DEVNET_RPC=${DEVNET_RPC:-https://api.devnet.solana.com}
ER_RPC=${ER_RPC:-https://ephemeral.devnet.sonic.game}
SERVICE=${SERVICE:-northstar-validator}
RESTART_COMMAND=${RESTART_COMMAND:-}
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/solana/northstar}
DEVNET_SMOKE_BIN=${DEVNET_SMOKE_BIN:-}
DEPLOYER_KEYPAIR=${DEPLOYER_KEYPAIR:-/home/ubuntu/.config/solana/id.json}
ER_FEE_PAYER_KEYPAIR=${ER_FEE_PAYER_KEYPAIR:-/tmp/northstar-spl-e2e-fee-payer.json}
STATE_PATH=${STATE_PATH:-/tmp/northstar-spl-e2e-state.env}
REPORT_PATH=${REPORT_PATH:-/tmp/northstar-spl-e2e-report.txt}
TOKEN_BRIDGE_KEYPAIR=${TOKEN_BRIDGE_KEYPAIR:-/solana/northstar/token-bridge-keypair.json}
PORTAL_ADDRESS=${PORTAL_ADDRESS:-}
PHASE=initialize
STARTED_AT=$(date +%s)

write_failure_report() {
  local status=$?
  if [ "$status" -ne 0 ]; then
    cat > "$REPORT_PATH" <<EOF
:test_tube: Devnet SPL smoke status: *failure*
:triangular_flag_on_post: Failed phase: \`$PHASE\`
:stopwatch: Elapsed: $(( $(date +%s) - STARTED_AT ))s
EOF
  fi
  exit "$status"
}
trap write_failure_report EXIT

rpc_result() {
  local url=$1 method=$2 params=${3:-'[]'}
  curl -fsS --max-time 15 -X POST -H 'Content-Type: application/json' \
    --data "$(jq -cn --arg method "$method" --argjson params "$params" \
      '{jsonrpc:"2.0",id:1,method:$method,params:$params}')" \
    "$url" | jq -er 'if .error then error(.error.message) else .result end'
}

wait_for_l1_rpc() {
  for _ in $(seq 1 1080); do
    if [ -z "$RESTART_COMMAND" ] && sudo systemctl is-failed --quiet "$SERVICE"; then
      echo "Validator service failed" >&2
      return 1
    fi
    if { [ -n "$RESTART_COMMAND" ] || sudo systemctl is-active --quiet "$SERVICE"; } &&
      rpc_result "$L1_LOCAL_RPC" getVersion >/dev/null 2>&1; then
      return 0
    fi
    sleep 5
  done
  echo "Timed out waiting for L1 RPC" >&2
  return 1
}

restart_validator() {
  if [ -n "$RESTART_COMMAND" ]; then
    bash -lc "$RESTART_COMMAND"
  else
    sudo systemctl restart "$SERVICE"
  fi
}

wait_for_l1_catchup() {
  for _ in $(seq 1 360); do
    local local_slot public_slot gap
    local_slot=$(rpc_result "$L1_LOCAL_RPC" getSlot '[{"commitment":"confirmed"}]' 2>/dev/null || true)
    public_slot=$(rpc_result "$DEVNET_RPC" getSlot '[{"commitment":"confirmed"}]' 2>/dev/null || true)
    if [[ "$local_slot" =~ ^[0-9]+$ ]] && [[ "$public_slot" =~ ^[0-9]+$ ]]; then
      gap=$((public_slot - local_slot))
      if [ "$gap" -ge -50 ] && [ "$gap" -le 50 ]; then
        return 0
      fi
    fi
    sleep 5
  done
  echo "Timed out waiting for local L1 RPC to catch up with devnet" >&2
  return 1
}

wait_for_session() {
  local expected=$1
  for _ in $(seq 1 360); do
    local actual
    actual=$(rpc_result "$ER_RPC" getSessionPda 2>/dev/null || true)
    if [ "$actual" = "$expected" ]; then
      return 0
    fi
    sleep 5
  done
  echo "Timed out waiting for ER session $expected" >&2
  return 1
}

run_devnet_smoke() {
  if [ -n "$DEVNET_SMOKE_BIN" ]; then
    RUST_LOG=warn "$DEVNET_SMOKE_BIN" "$@"
  else
    RUST_LOG=warn cargo run --locked --quiet --release --target-dir "$CARGO_TARGET_DIR" \
      --package northstar-token-bridge --example devnet_smoke -- "$@"
  fi
}

PHASE=resolve_configuration
test -f "$DEPLOYER_KEYPAIR"
if [ -z "$PORTAL_ADDRESS" ]; then
  test -z "$RESTART_COMMAND"
  PORTAL_ADDRESS=$(systemctl cat "$SERVICE" | sed -n 's/.*--portal \([^ ]*\).*/\1/p' | tail -n 1)
fi
test -n "$PORTAL_ADDRESS"
if [ -z "${TOKEN_BRIDGE_ADDRESS:-}" ]; then
  test -f "$TOKEN_BRIDGE_KEYPAIR"
  TOKEN_BRIDGE_ADDRESS=$(solana-keygen pubkey "$TOKEN_BRIDGE_KEYPAIR")
fi
test -n "$TOKEN_BRIDGE_ADDRESS"
SESSION_PDA=$(rpc_result "$ER_RPC" getSessionPda)
test -n "$SESSION_PDA"
wait_for_l1_catchup

export DEVNET_RPC ER_RPC PORTAL_ADDRESS TOKEN_BRIDGE_ADDRESS DEPLOYER_KEYPAIR
export ER_FEE_PAYER_KEYPAIR STATE_PATH REPORT_PATH

PHASE=deposit_spl_on_l1
run_devnet_smoke prepare

PHASE=restart_with_unsettled_spl_state
restart_validator
wait_for_l1_rpc
wait_for_l1_catchup
wait_for_session "$SESSION_PDA"

PHASE=withdraw_spl_on_er_and_settle_l1
run_devnet_smoke withdraw

PHASE=complete
trap - EXIT
rm -f "$ER_FEE_PAYER_KEYPAIR" "$STATE_PATH"
cat "$REPORT_PATH"

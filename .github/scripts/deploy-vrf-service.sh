#!/usr/bin/env bash
# Sonic: Keep the oracle outside the checkout and GitHub's job process lifetime.
set -euo pipefail
umask 077

: "${GITHUB_EVENT_NAME:?}" "${GITHUB_REF:?}" "${GITHUB_WORKSPACE:?}" "${RUNNER_TEMP:?}"
: "${ORACLE_REVISION:?}" "${VRF_FUNDER_KEYPAIR:?}" "${VRF_MAX_FUNDING_LAMPORTS:?}" "${VRF_DEPLOYMENT_ID:?}"
[[ "$GITHUB_EVENT_NAME" == push && "$GITHUB_REF" == refs/heads/release ]]
[[ "$ORACLE_REVISION" =~ ^[a-f0-9]{40}$ ]]
test "$(git -C "$GITHUB_WORKSPACE/.vrf-ci/oracle" rev-parse HEAD)" = "$ORACLE_REVISION"
sudo -n true

PORTAL_ADDRESS=$(systemctl show --property=ExecStart --value northstar-validator.service | python3 -c '
import re,sys
matches=re.findall(r"(?:^|\s)--portal(?:=|\s+)([1-9A-HJ-NP-Za-km-z]{32,44})(?=\s|;|$)",sys.stdin.read())
if len(matches)!=1: raise SystemExit("Cannot resolve exactly one validator Portal")
print(matches[0])
')

BUILD=$(mktemp -d "$RUNNER_TEMP/northstar-vrf.XXXXXXXX")
mkdir "$BUILD/oracle"
git -C "$GITHUB_WORKSPACE/.vrf-ci/oracle" archive "$ORACLE_REVISION" | tar -xf - -C "$BUILD/oracle"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"
TOOLS=/solana/northstar/vrf-tools
if ! test -x "$TOOLS/bin/cargo-build-sbf" || ! "$TOOLS/bin/cargo-build-sbf" --version | grep -qx 'cargo-build-sbf 4.5.0'; then
  cargo install --locked --version 4.5.0 --root "$TOOLS" cargo-build-sbf --force
fi
export PATH="$TOOLS/bin:$PATH"
export CARGO_TARGET_DIR=/solana/northstar/vrf-build
(
  cd "$BUILD/oracle"
  cargo build --release --locked --bin vrf-oracle --example devnet-control
)
# SBF packages have their own target directories and platform toolchain.
unset CARGO_TARGET_DIR
python3 "$BUILD/oracle/scripts/devnet_service.py" \
  --directory /solana/northstar/vrf-service \
  --deployment-id "$VRF_DEPLOYMENT_ID" \
  --portal "$PORTAL_ADDRESS" \
  --funder "$VRF_FUNDER_KEYPAIR" \
  --max-funding-lamports "$VRF_MAX_FUNDING_LAMPORTS" \
  --coordinator-checkout "$GITHUB_WORKSPACE/.vrf-ci/coordinator" \
  --verifier-checkout "$GITHUB_WORKSPACE/.vrf-ci/verifier" \
  --binary /solana/northstar/vrf-build/release/vrf-oracle \
  --control /solana/northstar/vrf-build/release/examples/devnet-control \
  --oracle-revision "$ORACLE_REVISION" \
  --northstar-revision "$GITHUB_SHA"

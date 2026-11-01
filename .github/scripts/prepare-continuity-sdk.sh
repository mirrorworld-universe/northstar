#!/usr/bin/env bash
# Sonic: Use the reviewed public consumer bindings without adding deployment credentials.
set -euo pipefail
umask 077
if [[ $# != 1 || $1 != /* || -e "$1" || -L "$1" ]]; then
  echo "Usage: prepare-continuity-sdk.sh ABSOLUTE_NEW_CHECKOUT_DIRECTORY" >&2
  exit 2
fi
for tool in git node npm sha256sum; do command -v "$tool" >/dev/null; done
npm_version=$(npm --version)
if [[ ! $npm_version =~ ^11\. ]]; then
  echo "Pinned continuity SDK requires npm 11 (validated with Node 24.21.0); found $npm_version" >&2
  exit 2
fi
revision=24030ce9fc3b80aa21738db2bb9f5a5fbea030d8
lock=c9c3eab30574777558d737d043060eadfec8c156b41ee4b313bbab2d05910ded
checkout=$1
git init --quiet "$checkout"
git -C "$checkout" remote add origin https://github.com/mirrorworld-universe/NorthStarSDK.git
git -C "$checkout" fetch --quiet --no-tags --depth 1 origin "$revision"
git -C "$checkout" checkout --quiet --detach FETCH_HEAD
test "$(git -C "$checkout" rev-parse HEAD)" = "$revision"
printf '%s  %s\n' "$lock" "$checkout/package-lock.json" | sha256sum --check --status
(
  cd "$checkout"
  npm ci --include=dev --ignore-scripts --no-audit --no-fund
  npm run build
  git diff --quiet HEAD
  test -s dist/index.js
)
printf 'Pinned public continuity SDK ready: %s\n' "$revision"

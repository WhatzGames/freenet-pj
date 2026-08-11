#!/usr/bin/env bash
#
# Publishes dist/ to Freenet as a website, using fdev's built-in web container
# contract. Requires a running local node.
#
# The signing key (created on first run) determines the app's permanent address,
# so re-publishing under the same key keeps the URL stable — that is what makes
# `update` an upgrade rather than a second, unrelated app.
#
# Usage: scripts/publish.sh [key-name]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist"
KEY="${1:-freenet-pj}"
NODE_URL="${FREENET_NODE:-http://127.0.0.1:7509}"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

if [[ ! -f "$DIST/index.html" ]]; then
  echo "error: $DIST/index.html is missing — run scripts/build.sh first" >&2
  exit 1
fi

if ! curl -sf -o /dev/null --max-time 5 "$NODE_URL/"; then
  echo "error: no Freenet node answering at $NODE_URL" >&2
  echo "       start the Freenet app, or set FREENET_NODE to the right address" >&2
  exit 1
fi

# `fdev website list` is the only way to tell whether this key exists; init fails
# if it already does, and publish fails if it does not.
if fdev website list 2>/dev/null | grep -q "\b$KEY\b"; then
  echo "==> Updating the existing '$KEY' website (new version, same address)"
  fdev website update --key "$KEY" "$DIST"
else
  echo "==> Creating signing key '$KEY'"
  fdev website init "$KEY"
  echo "==> Publishing '$KEY' for the first time"
  fdev website publish --key "$KEY" "$DIST"
fi

echo
echo "Once published, open the address printed above under:"
echo "  $NODE_URL/v1/contract/web/<contract-id>/"

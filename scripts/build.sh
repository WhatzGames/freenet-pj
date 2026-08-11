#!/usr/bin/env bash
#
# Builds everything: the board contract, then the frontend that embeds it.
#
# Order matters. The frontend `include_bytes!`s the contract wasm so it can create
# new board instances, so the contract has to exist first.
#
# Output: dist/ — a self-contained directory ready for `scripts/publish.sh`.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist"

# fdev resolves its target dir from its own compile-time CARGO_MANIFEST_DIR, which
# lives in the cargo registry and has no [workspace] above it, so it panics with
# "Could not find workspace root" unless CARGO_TARGET_DIR is set. Setting it here
# also puts the frontend's artifacts in the same tree, since pj-web is its own
# workspace.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
WASM_DIR="$CARGO_TARGET_DIR/wasm32-unknown-unknown/release"

for tool in fdev wasm-bindgen cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is not on PATH" >&2
    echo "  fdev:         cargo install fdev" >&2
    echo "  wasm-bindgen: cargo install wasm-bindgen-cli" >&2
    exit 1
  fi
done

# Lints and formatting first, and they are fatal.
#
# Both workspaces, because pj-web is its own (see the root Cargo.toml) and a
# root-only check silently skips two thirds of the code. Both targets, because the
# contracts are compiled to wasm and a lint can fire on one target and not the
# other.
#
# `--all-targets` so tests are linted too. Test code is where the sloppy casts and
# unchecked indexing live, and it is also where a wrong assumption is most
# expensive: a test that passes for the wrong reason is worse than no test.
echo "==> Checking formatting"
cargo fmt --manifest-path "$ROOT/Cargo.toml" --all --check
(cd "$ROOT/crates/pj-web" && cargo fmt --check)

echo "==> Linting (deny-level; see the [lints] tables in Cargo.toml)"
cargo clippy --manifest-path "$ROOT/Cargo.toml" --workspace --all-targets --quiet
(cd "$ROOT/crates/pj-web" && cargo clippy --target wasm32-unknown-unknown --all-targets --quiet)

echo "==> Testing the domain model and contract"
cargo test --manifest-path "$ROOT/Cargo.toml" --quiet

echo "==> Building the board contract"
(cd "$ROOT/crates/pj-board-contract" && fdev build)

# One instance per task, so this is the contract the app PUTs most often by far.
echo "==> Building the task contract"
(cd "$ROOT/crates/pj-task-contract" && fdev build)

echo "==> Building the registry contract"
(cd "$ROOT/crates/pj-registry-contract" && fdev build)

echo "==> Building the organization contract"
(cd "$ROOT/crates/pj-org-contract" && fdev build)

echo "==> Building the user profile contract"
(cd "$ROOT/crates/pj-user-contract" && fdev build)

echo "==> Building the identity delegate"
(cd "$ROOT/crates/pj-identity-delegate" && fdev build --package-type delegate)

# Separate from the identity delegate on purpose. A delegate's secrets hang off
# hash(code + parameters), so folding preferences into that one would have moved
# its key and discarded every user's signing seed.
echo "==> Building the preferences delegate"
(cd "$ROOT/crates/pj-prefs-delegate" && fdev build --package-type delegate)

# The frontend embeds all three: it PUTs a board contract to create a board, PUTs
# the registry the first time anyone lists one, and registers the delegate with the
# node before asking it for a key. None of them needs a separate publish step.
echo "==> Staging the wasm components for the frontend to embed"
mkdir -p "$ROOT/crates/pj-web/contract"
for component in pj_board_contract pj_task_contract pj_registry_contract pj_org_contract \
                 pj_user_contract pj_identity_delegate pj_prefs_delegate; do
  cp "$WASM_DIR/$component.wasm" "$ROOT/crates/pj-web/contract/$component.wasm"
done

echo "==> Building the frontend"
(cd "$ROOT/crates/pj-web" && cargo build --release --target wasm32-unknown-unknown)

echo "==> Bundling into dist/"
rm -rf "$DIST"
mkdir -p "$DIST"
wasm-bindgen \
  --target web \
  --no-typescript \
  --out-dir "$DIST" \
  --out-name pj_web \
  "$WASM_DIR/pj_web.wasm"
cp "$ROOT/crates/pj-web/index.html" "$DIST/index.html"
cp "$ROOT/crates/pj-web/styles.css" "$DIST/styles.css"
cp "$ROOT/crates/pj-web/bridge.js" "$DIST/bridge.js"

echo
echo "dist/ contents:"
ls -lh "$DIST" | tail -n +2 | awk '{printf "  %-24s %s\n", $9, $5}'
echo
echo "Component code hashes:"
(cd "$ROOT/crates/pj-board-contract" && fdev inspect build/freenet/pj_board_contract code 2>/dev/null | sed 's/^/  board:    /')
(cd "$ROOT/crates/pj-task-contract" && fdev inspect build/freenet/pj_task_contract code 2>/dev/null | sed 's/^/  task:     /')
(cd "$ROOT/crates/pj-registry-contract" && fdev inspect build/freenet/pj_registry_contract code 2>/dev/null | sed 's/^/  registry: /')
(cd "$ROOT/crates/pj-org-contract" && fdev inspect build/freenet/pj_org_contract code 2>/dev/null | sed 's/^/  org:      /')
# Printed for the same reason as the others: a hash that moved without anyone
# meaning it to is everybody's device list and project index left behind.
(cd "$ROOT/crates/pj-user-contract" && fdev inspect build/freenet/pj_user_contract code 2>/dev/null | sed 's/^/  user:     /')
echo
echo "Next: scripts/publish.sh"

---
name: run-freenet-pj
description: Build, publish, run, screenshot and drive the freenet-pj web app against a local Freenet node. Use when asked to run, start, build, test, publish, screenshot, or visually verify this project's UI.
---

# Running freenet-pj

A Leptos/wasm kanban app whose state lives in Freenet contracts. There is no dev
server: you build a `dist/`, publish it into a local node as a web contract, and
open it through the node.

Paths are relative to the repo root. The node is assumed to be running on
`http://127.0.0.1:7509`.

**Drive it with `.claude/skills/run-freenet-pj/driver.mjs`** — screenshots and
in-page scripting over the DevTools Protocol. Do not reach for Chrome's
`--screenshot` flag or a DevTools extension; both hang on this app, for reasons
in Gotchas.

## Prerequisites

Verified on macOS (Apple silicon):

```
fdev 0.3.267           cargo install fdev
wasm-bindgen 0.2.126   cargo install wasm-bindgen-cli
cargo 1.97.1           rustup target add wasm32-unknown-unknown
node v25.9.0           needs >= 22 for the global WebSocket the driver uses
```

Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`;
override with `CHROME_PATH`.

## Build and publish

```bash
./scripts/build.sh     # fmt + clippy + tests, then 4 contracts + 2 delegates, then dist/
./scripts/publish.sh   # pushes dist/ to the node under the 'freenet-pj' key
```

**`build.sh` is the gate, not a convenience.** In order, and fatal at every step:
`cargo fmt --check` on both workspaces, `cargo clippy --all-targets` on both
workspaces and both targets, then the tests. A build that completed means the tree
is formatted, lint-clean at deny level, and 146 tests passed. Do not hand-roll a
shorter version of it — see "Test and lint" below for the four things a shortened
version misses.

It prints all four contract code hashes at the end — **read them**, and see the
`freenet-contracts` skill for why.

Note `build.sh` and `publish.sh` are separate steps: chaining them with `;` will
happily publish a stale `dist/` after a failed build. Use `&&`.

CSS-only change? Skip the two-minute rebuild:

```bash
cp crates/pj-web/styles.css dist/styles.css && ./scripts/publish.sh
```

The app is then at:

```
http://127.0.0.1:7509/v1/contract/web/6LpX8WFjTt2jad6TsvwM74XJ45W7oF3DFzF9JswKsTxS/
```

Routes are URL fragments: `#<boardId>`, `#<boardId>/<taskId>` (opens the board and
its task drawer), `#org/<orgId>`, `#me`, `#link/<publicKey>`.

`TaskRef::parse` in `pj-core` reads any of those back out of a pasted URL and
ignores everything before the last `#`, so a link copied on one node resolves on
another. That is what the task drawer's **Copy link** button hands out and what its
link form accepts — tested in `pj_core::link`.

## Run (agent path)

```bash
mkdir -p /tmp/pj-shots

# start page
node .claude/skills/run-freenet-pj/driver.mjs /tmp/pj-shots/start.png --port 9401

# a board, measured from inside the app
node .claude/skills/run-freenet-pj/driver.mjs /tmp/pj-shots/board.png \
  --hash '#2LxRm4z8RctqzNAadPs6QW6ALv2nvkquD4TNkqvvJK5X' --port 9402 --in-frame --js "
  const cols = Array.from(document.querySelectorAll('.column'));
  return JSON.stringify({
    columns: cols.map(c => c.querySelector('h3').innerText),
    overflow: document.documentElement.scrollWidth - innerWidth,
    sidebar: !!document.querySelector('.sidebar')
  });"
```

The second prints
`{"columns":["BACKLOG","TODO","IN PROGRESS","DONE"],"overflow":0,"sidebar":true}`
and writes the PNG.

**Then open the PNG and look at it.** Measurements pass on visibly broken
layouts — a grid bug that put the sidebar under the board and left the hint text
in its place reported `overflow: 0` throughout.

Options: `--hash`, `--w`/`--h` (under 600 enables mobile emulation), `--wait`
(ms; raise if the node is slow), `--scheme dark|light`, `--js`, `--in-frame`,
`--full`, `--port`, `--base`.

### `--js` versus `--js --in-frame`

- **Without** `--in-frame` the snippet runs in the shell frame and gets helpers
  (`doc()`, `all()`, `click()`, `clickText()`, `fill()`, `byText()`, `wait()`)
  that reach into the app, after waiting for it to appear.
- **With** `--in-frame` it runs *inside* the app via an auto-attached target, so
  plain `document` is the app's. **Use this in headless** — the sandbox puts the
  app on an opaque origin there and the shell frame cannot reach in.

Use a distinct `--port` per concurrent run; each gets its own profile.

### Checks worth running after a UI change

```bash
# phone width: neither number may be non-zero
node .claude/skills/run-freenet-pj/driver.mjs /tmp/pj-shots/mobile.png \
  --w 414 --h 850 --port 9403 --hash '#<boardId>' --in-frame --js "
  const tb = document.querySelector('.topbar');
  return JSON.stringify({
    pageOverflow: document.documentElement.scrollWidth - innerWidth,
    topbarOverflow: tb.scrollWidth - tb.clientWidth
  });"

# light theme
node .claude/skills/run-freenet-pj/driver.mjs /tmp/pj-shots/light.png --scheme light --port 9404
```

### Forcing the degraded paths

`bridge.js` exposes `window.__freenetDropSocket()`, which closes every proxied
socket and returns how many it closed. It exists because reconnection and
publish-confirmation are the kind of code that is either exercised or merely
hoped for. Call it in a loop from `--js` to hold the connection down:

```js
for (let i = 0; i < 40; i++) { window.__freenetDropSocket(); await sleep(250); }
```

Ten seconds of that outlasts `confirm_publish`, which is how the "not confirmed"
badge and its "Publish again" button get tested. A shorter outage recovers on its
own — correctly, and it is worth watching that happen too.

### Verifying node-side persistence

Preferences live in the node, not the browser, so a *fresh browser profile* is
the real test. Make the stored value disagree with the emulated OS value —
otherwise a pass proves nothing:

```bash
# store light while emulating a dark OS
node .claude/skills/run-freenet-pj/driver.mjs /tmp/pj-shots/p1.png --port 9413 --scheme dark --in-frame --js "
  const sleep = ms => new Promise(r=>setTimeout(r,ms));
  const btn = () => Array.from(document.querySelectorAll('.topbar button')).find(b=>/Switch to/.test(b.title));
  if (document.documentElement.dataset.theme !== 'light') { btn().click(); await sleep(1500); }
  return JSON.stringify({stored: document.documentElement.dataset.theme});"

# fresh browser, OS still dark — renders light only if the node remembered
node .claude/skills/run-freenet-pj/driver.mjs /tmp/pj-shots/p2.png --port 9414 --scheme dark --in-frame --js "
  return JSON.stringify({theme: document.documentElement.dataset.theme,
                         bodyBg: getComputedStyle(document.body).backgroundColor});"
```

Expect `{"theme":"light","bodyBg":"rgb(232, 241, 248)"}`. **Preferences are
per-node, so this changes the human's setting** — toggle it back when done.

## Test and lint

`./scripts/build.sh` already runs all of this and aborts on any of it, so a build
that completed is a clean tree. Run them directly while iterating:

```bash
# formatting — both workspaces
cargo fmt --all --check
(cd crates/pj-web && cargo fmt --check)

# lints — deny-level, from the [lints] tables in Cargo.toml, no flags needed
cargo clippy --workspace --all-targets
(cd crates/pj-web && cargo clippy --target wasm32-unknown-unknown --all-targets)

cargo test --workspace          # 146 tests
cargo doc --workspace --no-deps # broken intra-doc links are warnings, so: read them
```

### Four things this list gets wrong if you shorten it

- **`crates/pj-web` is its own workspace.** The root `--workspace` does not
  include it, and its clippy must run from inside that directory with the wasm
  target. Skipping it skips most of the code.
- **`--all-targets`, or tests go unlinted.** Test code is where the sloppy casts
  and the unchecked indexing live, and a test that passes for the wrong reason is
  worse than no test.
- **No `-D warnings` flag.** The levels live in `[workspace.lints]` in the root
  `Cargo.toml` and `[lints]` in `crates/pj-web/Cargo.toml`, so every invocation
  gets them — including yours, the editor's, and CI's. Passing flags by hand means
  the bar depends on remembering the flag.
- **`cargo doc` is part of the check.** Moving a module or renaming a type turns
  every `[`link`]` to it into a warning that nothing else reports.

### The bar, and the whole list of exceptions

`clippy::all` and `clippy::pedantic` at **deny**, plus `unsafe_code = "forbid"`,
`unreachable_pub`, `elided_lifetimes_in_paths` and `unused_qualifications`.

**There are no `#[allow]` or `#[expect]` attributes anywhere in the tree, and it
is worth keeping that true.** A lint turned off in one file is invisible; the
exceptions instead live in the two `Cargo.toml` lint tables where they can be
read as a list and argued with. There are six, each with its reason in a comment
next to it: four about documentation ceremony (`must_use_candidate`,
`return_self_not_must_use`, `missing_errors_doc`, `missing_panics_doc`) and two
that Leptos's shape makes unsatisfiable in `pj-web` (`unreachable_pub`, which
fires on `#[component]` expansion, and `large_types_passed_by_value`, which asks
for a `&Store` that a prop cannot be).

If you need a seventh, put it there and say why. Do not reach for an attribute.

## Run (human path)

Open the published URL in a normal browser. **Hard-reload (Cmd+Shift+R) after
every publish** — the node serves the same URLs and the browser caches the wasm
and CSS hard. Stale cache has cost more time on this project than any real bug:
measurements taken against a cached build look like code that doesn't work.

## Gotchas

- **A successful `PUT` produces no observable reply.** Measured against freenet
  0.2.105: the node logs `initial_state_installed`, `Added contract to locally
  hosted`, and the board is genuinely there — and nothing comes back down the
  socket. `ContractResponse::PutResponse` is handled in `node.rs` and simply never
  arrives. Anything that waits for an acknowledgement will report every single
  creation as failed. **Confirm by reading back** (`node::get`) instead: it does
  not depend on which reply the node chooses to send, and "the node can serve this"
  is the stronger claim anyway. `store.rs::confirm_publish` does exactly that.
- **A first read-back can beat the `PUT`.** It is processed asynchronously, so
  `NotFound` right after a publish means "not yet", not "lost". Retry a few times
  before believing it.
- **`resync()` cannot rescue an unpublished board.** It re-pushes state with an
  *update*, and an update to a contract that was never `PUT` has nothing to merge
  into. Only another `PUT` instantiates it — which is what "Publish again" sends.
- **`chrome --screenshot` hangs forever.** The node's shell page holds an
  EventSource open, so the page never goes quiescent and the flag waits for a
  load that never fires. `--virtual-time-budget` does not help. Hence CDP.
- **DevTools-extension screenshots time out** on anything from
  `127.0.0.1:7509` — `Script injection timed out after 5000ms`, same cause.
  Scripted *evaluation* still works; only capture fails.
- **The app is in a sandboxed iframe on an opaque origin.** Headless requires
  `Target.setAutoAttach` to reach it; `contentDocument` from the top frame is
  `null`. In an ordinary browser session it has been reachable from the top
  frame, so code that works interactively can fail headless.
- **`localStorage` throws** in that sandbox. Anything durable goes to the node
  via the preferences delegate — see `freenet-contracts`.
- **The identity delegate refuses non-owning origins.** Serving `dist/` yourself
  with `?node=127.0.0.1:7509` loads and connects, but identity fails with *"an
  identity is only served to the web app that owns it"*, so nothing can be
  created or edited. Always test through the node's URL.
- **`fdev` panics with "Could not find workspace root"** unless
  `CARGO_TARGET_DIR` is exported — it resolves its target dir from its own
  compile-time manifest path. `scripts/build.sh` sets it; you must too if you
  call `fdev` directly.
- **`publish.sh` intermittently fails** with `put timed out after 1 peer
  attempt(s)`. Run it again. And if you chain `build.sh; publish.sh` with `;`,
  **publish still runs when the build fails**, shipping the previous `dist/`.
- **A bare `>` inside a Leptos `view!` closes the tag.** `when=move || a > b`
  fails as `expected bool, found usize`. Write `b < a`.
- **A keyed `<For>` does not re-render surviving rows.** Pass a component only
  the id and derive the rest from the store, or edits render stale. Four
  separate bugs here; every such component carries a comment.
- **`var()` inside a custom property resolves where declared, not where used.**
  `--foo: hsl(var(--hue) …)` on `:root` bakes in the fallback, and every element
  inherits one colour. Put the `hsl()` at the point of use.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `no attached frame contained the app` | Node slow or down. Raise `--wait 15000`; check `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:7509/` |
| `Chrome never opened its debugging port` | Another run holds it. Use a different `--port`. |
| Screenshot shows the old UI | Driver profile cache — use a fresh `--port` (profiles are `/tmp/pj-driver-<port>`). In a real browser, hard-reload. |
| `couldn't read .../contract/pj_*.wasm` | Staged wasm missing. Run `./scripts/build.sh`. |
| `could not compile pj-core` after adding an enum variant | The folds match exhaustively. Add the arm. |
| Publish succeeded, page unchanged | The build failed and publish shipped the old `dist/`. Re-read the build output. |

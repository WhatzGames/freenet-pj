# freenet-pj

A project management and status tracking board that runs entirely on
[Freenet](https://freenet.org) — Ian Clarke's Rust implementation, not the
original Java network. There is no server: the board is a Freenet contract, the
UI is a WebAssembly app served by Freenet itself, and the rules about who may
change what are enforced by the contract on every peer that touches it.

Rust end to end — the contract, the shared domain model, and the browser UI.

## Status

Working and verified against a live node (freenet 0.2.105):

- boards with tasks (title, description, assignee), drag-and-drop between and
  within columns, and live updates via subscription
- **organizations** that own projects: create one, invite members, promote them to
  administrators, assign them to as many projects as you like, and leave
- **typed links between tasks**, across projects and organizations: related-to,
  causes/caused-by, parent-of/child-of, one kind per pair, visible from both ends
- **a page of your own**: your devices with an unlink action, and your
  organizations with their projects nested underneath
- a public directory of all projects and organizations, searchable by name
- membership with cryptographically signed writes
- **device linking** — one person, several keys, so a second browser or machine
  acts as you without any secret being transmitted
- **persistent identity** — your key is held by a Freenet delegate inside the
  node, so a reload keeps your ownership even though the sandbox denies the app
  all browser storage

## How it fits together

```
crates/pj-core               domain model + CRDTs + signatures  (no Freenet, no browser)
crates/pj-board-contract     board contract        → wasm       (thin shim over pj-core)
crates/pj-org-contract       organization contract → wasm       (thin shim over pj-core)
crates/pj-user-contract      one person's profile  → wasm       (thin shim over pj-core)
crates/pj-registry-contract  the public directory  → wasm       (thin shim over pj-core)
crates/pj-identity-proto     app ⇄ delegate protocol           (deliberately tiny)
crates/pj-identity-delegate  holds your signing key → wasm
crates/pj-web                Leptos UI             → wasm       (embeds all four)
```

The frontend carries all four wasm components because it has to instantiate them:
creating a board or an organization PUTs its contract, listing the first one PUTs
the registry, and the delegate is registered with the node before it will answer.
None needs a separate publish step.

`pj-core` deliberately depends on neither Freenet nor the browser, so the same
types compile into the contract, into the frontend, and into native test
binaries. All the interesting logic lives there and is tested with
`cargo test`, no wasm runtime or running node required.

### Why the state is an op log

Freenet requires `update_state` to be **commutative**: applying deltas in any
order must converge on the same state. So a board is not stored as "the current
tasks" but as a grow-only set of signed operations keyed by their content hash.
Merging is set union, which cannot depend on order and cannot depend on how many
times an op arrives — convergence is structural rather than something the merge
code has to get right.

The board a user sees is folded out of that op set on demand, with
last-writer-wins on individual fields. The fold walks ops in a total order
derived entirely from the ops themselves (`lamport`, then wall clock, then
content hash), so two peers holding the same ops always render the same board.

Ordering of cards within a column uses fractional indexing (`pj_core::rank`):
base-62 strings compared lexicographically, so there is always room to insert
between any two cards and dragging one card never has to renumber its
neighbours — which would fight with concurrent edits.

### Discovery

Freenet has no enumeration and no search: you can only fetch a contract whose
address you already know. So the directory isn't a query, it's a contract — one
instance at an address every build derives from the same constant parameters,
holding a grow-only set of owner-signed listings. Same CRDT shape as a board, so
merging is set union and convergence is free.

Creating a board publishes a listing; the start page subscribes, so new projects
appear live. Search and the 25-row cap are client-side filtering over the fetched
directory. Every board is listed publicly, by design.

### Task links

A link is stored as a single directed edge with a kind. The reverse direction is not
stored — it is *derived*, because every kind has an inverse. Recording "A is the
parent of B" therefore makes B a child of A with no second op to keep in step, and
no way for the two directions to disagree. `related to` is its own inverse.

One kind holds between a given pair, so re-linking replaces rather than accumulates,
resolved last-writer-wins like any other field.

Links may point at a task in another project, including one owned by a different
organization, so a reference carries an optional board id. Only the board storing the
edge can derive the far end, so a cross-project link shows on the side that stores it
and needs a matching link from the other project to be visible there too.

### Your own page

Device links and board membership live inside the op set of the board or organization
they were made on. That is right for authority — a contract enforcing a rule has to
see the evidence for it — but it leaves nowhere to answer *which devices are mine?*
and *what am I a member of?*, and Freenet has no reverse index to search.

So each person gets a contract addressed by their own public key: a personal index,
written only by them, holding their canonical device list, their memberships, and a
display name that finally survives a reload. It is authoritative for nothing except
their own view of things.

Membership is recorded as you open projects and organizations you belong to, and when
you create one.

### Linking a device from a phone

Pasting 44 characters of base58 into a form is miserable on a phone, so a device can
instead offer its public key as a link — `#link/<key>` — through the platform's own
share sheet. Opening that link on a browser that is already you presents the key for
confirmation.

A key arriving in a URL is a *request*, never authority: it is acted on only when
someone holding an already-trusted key accepts it. And nothing secret travels — a
public key is safe to send over any channel.

### Recovering from a dropped connection

Connections to the node do drop. Three things make that survivable rather than
lossy:

**Every request is queued, not sent directly.** `dispatch` appends to an outbox and
drains it whenever the socket is open, so an outage is a delay rather than a silent
loss. Before this, a write made while disconnected was applied locally, failed to
send, and disappeared on the next reload — the user saw their edit and then lost it.
The count of unsent writes is shown in the header.

**The socket reopens itself,** with exponential backoff from one second to a thirty
second cap, and a "Retry now" button for the impatient. A dropped socket surfaces
through `WebApi`'s error channel as a message containing "closed", which is where the
retry is hooked.

**On reconnect the client resyncs rather than reasoning.** Once a send has failed the
client cannot know what got through, so it does not try to work it out: it re-offers
the *entire* local state of everything open as an `UpdateData::State`, and re-fetches
each contract to pick up what it missed. Both halves are safe to repeat because every
state here is a grow-only set merged by union — pushing all of it is idempotent. That
is the payoff of the CRDT showing up somewhere unexpected: recovery needs no
bookkeeping, so there is no bookkeeping to get wrong.

Re-fetching also re-subscribes, which matters because a subscription belongs to the
socket that made it.

`bridge.js` exposes `__freenetDropSocket()` to close the connection on demand. It is
a diagnostic rather than a feature: recovery is the kind of code that is either
exercised or merely hoped for, and there is otherwise no way to make the socket drop
when you want it to.

### Organizations

An organization is its own contract, rooted at the founder's key in its immutable
parameters. Authority is two tiers deep, each computed as a closure over the op set:

- the founder's keys grant **Admin**,
- the founder's keys *and* admins grant **Member**.

Only the founder promotes admins. That is deliberate: if admins could promote
admins, demoting one would raise the question of what happens to everyone they
promoted, and a CRDT has no ordering with which to answer it. Two tiers keeps
revocation comprehensible.

Joining is by invitation, so the contract never has to accept a write from a key it
does not already know — no request queue and no spam surface. Leaving is
self-service: `Leave` is signed by the departing member, who is already authorised.

Membership grants nothing on the organization's projects by itself. Members are
assigned per project, and one member can be assigned to any number of them —
each assignment is just a grant on that project's own board.

Only the founder can *create* a project, and the reason is structural rather than a
policy choice: a board's root of trust is the owner key in its immutable parameters,
so for the organization to own the project that key has to be the founder's — and
only whoever holds it can sign the board's genesis ops. Creating a project seeds the
organization's current admins as board admins, so they can staff it without the
founder; admins appointed later are added by the "Sync admins" action.

### Permissions

Identity is a keypair, not an account. Every op carries its author's public key
and an ed25519 signature; the contract verifies both.

The root of trust is the contract's **parameters**, which contain the owner's
public key. Parameters are hashed with the code to form the contract instance
id, so they can never change — the owner of a board is fixed by the board's
address, and holds every right there is, including ones invented after the board.

Everyone else holds a **bitset**, conferred by a grant op signed by someone who
already holds `MAY_GRANT`. A grant confers at most `asked ∩ held`, so nobody can
give away more than they have; intersection is commutative, which is what keeps
the result the same on every peer. Removing someone is a grant of `NONE` — there
is no separate op — and a grant of `NONE` to *yourself* is always honoured,
because resigning exercises authority over nobody.

Authority is tiered by one bit. `MAY_APPOINT` is what lets you pass on
`MAY_GRANT`, and only the owner or founder holds it: an admin invites people, the
owner decides who else may. Without that bit `MAY_GRANT` would be transitive by
construction, since it is within any admin's own cap.

A person can be several keys: a link op, signed by a key that already belongs to
them, vouches for another. A device acts with exactly its person's rights, so the
owner's second browser runs the board.

Two answers are derived, not one. What you may do *now* is last-writer-wins; what
judges ops *already written* is monotone — the union of everything ever conferred.
Collapsing them would make removal impossible, since taking someone's rights away
would retroactively invalidate their work and so invalidate the removal itself.

Every op is also signed against a **scope**, the hash of its contract's
parameters. A grant made on one board is not a grant anywhere else, and cannot be
lifted into its author's own profile.

Linking never moves a secret: the new context shows its *public* key and an
existing one signs for it.

### Where your key lives

A node serves a web app inside an iframe sandboxed *without* `allow-same-origin`,
so the app runs on an opaque origin where `localStorage`, `sessionStorage`, and
IndexedDB all throw. Without help the app would mint a new identity on every
reload and silently lose access to boards you own.

So the key lives in a **delegate** — wasm running inside the node, with persistent
secret storage that the sandbox cannot touch. The app registers it, then asks for
the seed; secrets are keyed by the calling app's origin, so another web contract
that discovers the delegate cannot read your identity, and non-web-app callers are
refused outright.

A member's write is accepted by a peer that has never seen the invite, because
the client attaches its own membership proof to every delta. Re-sending it is
free: the op set dedupes by content hash.

### The shell bridge (the non-obvious part)

A Freenet node does not serve a web contract's page directly. It serves a shell
page that loads the app in an `<iframe sandbox="allow-scripts …">` — no
`allow-same-origin` — so the app runs on an opaque origin and **cannot open a
socket to the node itself**. The shell proxies the node's client API over
`postMessage`, injecting an auth token the app never sees. That is deliberate:
the app is untrusted code fetched from a P2P network, so it gets no ambient
authority over the node.

`crates/pj-web/bridge.js` adapts that proxy back into an object with a
`WebSocket` shape, which is what `freenet_stdlib::client_api::WebApi` expects.
Outside a frame (local development) it hands back a real `WebSocket` instead.

## Prerequisites

- Rust 1.85+ and the wasm target: `rustup target add wasm32-unknown-unknown`
- A running Freenet node — the [desktop app](https://freenet.org) is fine
- `cargo install fdev wasm-bindgen-cli`

`trunk` is deliberately **not** used: its current stable release (0.21.14) fails
to build on recent compilers, and driving `wasm-bindgen` directly gives exact
control over the directory that becomes the published site.

## Build and publish

```sh
./scripts/build.sh      # tests, builds the contract, then the UI → dist/
./scripts/publish.sh    # publishes dist/ to Freenet (creates a signing key once)
```

`publish.sh` prints the app's URL. The signing key it creates on first run fixes
the app's permanent address, so re-running it publishes a new version at the same
URL rather than a second, unrelated app. Back up
`~/Library/Application Support/freenet/website-keys/freenet-pj.toml` — losing it
means never being able to update the site again.

`FREENET_NODE` overrides the node address (default `http://127.0.0.1:7509`).

To share a board, send someone its id from the sidebar; to be added to one, send
the owner your public key from the "You" panel.

## Testing

```sh
cargo test              # domain model and contract, natively
```

The suite covers what would be expensive to discover on a live network:
CRDT commutativity and idempotence, that a stranger's op is refused, that a
member cannot invite others, that deletion beats a concurrent edit, that a
forged signature is rejected, and that fractional ranks survive hundreds of
insertions into the same gap.

### Visual verification

For UI changes, verify the built app rather than a partial source tree:

```sh
./scripts/build.sh
cd dist
python3 -m http.server 4173 --bind 127.0.0.1
```

Then render `http://127.0.0.1:4173/` in a browser at both a desktop viewport
and a narrow mobile viewport, at least `1440x1000` and `390x844`.

The reliable check is not "does a screenshot exist"; it is:

- set the viewport explicitly through the browser automation API, Chrome DevTools
  Protocol, or the Codex browser `viewport` capability;
- wait for the wasm app to boot;
- capture a screenshot for human inspection;
- assert that no visible element's bounding box extends outside
  `window.innerWidth`; and
- assert that `document.documentElement.clientWidth` and the page `scrollWidth`
  match at the mobile width.

Do not use Chrome's bare `--headless --screenshot --window-size=390,844` as the
only mobile check. On macOS it can produce a cropped image that looks like a
responsive layout failure even when the browser was not actually using the
intended viewport. Use DevTools viewport emulation, or the in-app browser's
`viewport` capability, when checking breakpoints.

When using Codex's in-app browser through the Node REPL, save screenshots and
metrics to files if normal REPL output is not visible. The browser API can still
work even when `nodeRepl.write(...)` output is suppressed. Also treat screenshot
bytes as opaque until identified; the in-app browser may return JPEG bytes even
when the destination filename says `.png`.

### Freenet visual development workflow

Freenet UI work should be developed in two stages. First iterate on the built
static site outside Freenet, then publish the same `dist/` output to a local node
as an integration smoke test.

```sh
./scripts/build.sh
python3 -m http.server 4173 --directory dist --bind 127.0.0.1
```

Use `http://127.0.0.1:4173/` for the main visual loop. This is the fastest and
most reliable way to check layout, styling, wasm boot, console errors, and
desktop/mobile screenshots. It matches the public Freenet app pattern: build a
normal web UI with local example data or a local API surface, then wrap the
finished artifact as a Freenet web contract.

After the static build passes visual checks, verify the Freenet wrapper:

```sh
./scripts/publish.sh
```

Open the printed `http://127.0.0.1:7509/v1/contract/web/.../` URL and check the
Freenet-specific behavior separately:

- the shell page loads and creates the sandbox iframe;
- the iframe navigates away from `about:blank` to the `?__sandbox=1` app URL;
- the app body contains real UI text, not only the shell title;
- `styles.css`, `pj_web.js`, and `pj_web_bg.wasm` return 200 with the expected
  CSS, JavaScript, and WebAssembly MIME types;
- browser console and network logs have no boot-blocking errors; and
- desktop and mobile screenshots still have no horizontal overflow.

If the direct static site works but the Freenet URL is blank, debug the shell,
sandbox iframe, CSP, injected bridge, and asset loading before changing CSS. A
blank Freenet page can mean the wrapper received the sandbox document but the
iframe never committed navigation, which is a Freenet integration issue rather
than a visual layout issue.

## Known limitations

These are deliberate v1 trade-offs, not oversights.

- **Removing a member does not cryptographically revoke them.** It hides them
  from the roster and the UI, but their key still satisfies the contract's
  "was this key ever invited" check. Real revocation needs an owner-bumped epoch
  that the contract checks ops against.
- **The delegate is a keystore, not a signer.** It hands the seed back and the app
  signs locally. The stronger design is for the delegate to hold the key and sign
  on request, so the app never touches the secret; that needs an async signing path
  in the client. Conveniently, `OpId` covers the payload and not the signature, so
  an op's identity is known before it is signed and optimistic rendering survives
  the change.
- **A delegate rebuild still loses identities.** A delegate's key is
  `hash(code + parameters)` and its secrets hang off that key, so changing its wasm
  files every seed under a new namespace and users return as strangers. Two
  mitigations are in place and one of them does not work yet:
  `crates/pj-identity-proto` exists so that ordinary `pj-core` changes no longer
  touch the delegate (this was observed losing identities twice before the split);
  and registration uses `RegisterDelegateWithPredecessors` with the old keys listed
  in `PREDECESSOR_DELEGATE_KEYS`, which *should* make the node copy old secrets
  forward but did not in testing. The likely cause is a race — the identity request
  goes out immediately after registration, and gating it on a `HostResponse::Ok`
  that never arrives stops identity loading altogether. Until that is resolved, the
  exported recovery key is the only reliable way across a delegate change. When
  changing the delegate, read its new key with
  `cargo test -p pj-identity-delegate -- --nocapture delegate_key` and append the
  previous one to that list.
- **Identity is node-local.** Delegate secrets live in the node, so a reload or a
  different browser on the same node keeps your identity, but a different node does
  not. Use device linking, or carry the recovery key. Note that on a *hosted* node
  over https the per-user token is stored in the shell's `localStorage`, which is
  per browser profile — so cross-browser sharing is a property of local mode, not a
  guarantee to build on.
- **Boards created before the envelope migration cannot be opened.** Their
  contracts speak a different state encoding entirely, and live at addresses this
  build no longer computes. The UI warns when the open board predates it. *New*
  op kinds no longer have this problem: a contract does not decode op bodies, so an
  op it has never heard of is stored and carried rather than rejected.
- **A cross-project link is one-sided until it is mirrored.** The board storing the
  edge can derive its own far end, but not one on another board — so a link to another
  project shows there only once someone links back from it. Mirroring automatically
  would mean writing to a board the user may have no right to write to.
- **Unlinking a device is best-effort across projects.** It leaves your canonical
  list at once and is revoked on the project you are viewing, but other projects keep
  their own link op until you open them. Cryptographic revocation has the same
  gap as member removal, above.
- **A queued write can still be lost if the send itself fails.** `WebApi::send`
  consumes the request, so a failure mid-flush cannot put it back on the queue. The
  resync is what covers this — the state is re-offered whole on the next reconnect —
  but the recovery is at state level, not per request.
- **The outbox is in memory.** Closing the tab during an outage loses whatever was
  queued, because the sandbox denies the app any durable storage. Surviving that
  would mean staging pending ops in the identity delegate.
- **An organization's founding key is a single point of failure.** Only it can
  promote admins or create projects, so losing it freezes the organization's
  administration — the members and projects remain, but nothing can be restructured.
  Succession would need an ownership-transfer op; see the design notes.
- **The registry only checks that a listing is signed by the owner it names**, not
  that the board really is that owner's contract. Verifying it would mean
  recomputing `hash(board_code + params)` in the registry, which needs the board
  contract's code hash as a registry parameter. It also grows without bound and has
  no spam pricing — proof-of-work on entries, or time-bucketed shards, before this
  carries real traffic.
- **Every board is public and unencrypted.** Anyone can list, find, and read any
  board. Real privacy needs encrypted state with a board key distributed to
  members.
- **State summaries list every op id**, so sync metadata grows linearly with
  board history. Exact and honest, but a board with a long history will want a
  digest instead, plus op-log compaction behind tombstones.
- **Only the owner can set display names**, since names live in membership ops.
  A member renaming themselves changes only their local label.
- **No board-level delete.** Freenet contracts are not deletable by design.
- **Changing the contract changes the address of *new* boards.** A board lives at
  `hash(contract code + parameters)`, so recompiling the contract differently —
  even something as incidental as bumping the Rust edition — means boards created
  afterwards get different addresses, while existing ones keep theirs and stay
  governed by the contract they were created with. The app addresses a board with
  the key the *node* reports rather than one derived from the embedded code, so
  older boards remain readable *and* writable; the sidebar shows which contract is
  actually governing the open board. This holds only while the op and state
  encoding is unchanged — altering `pj-core`'s types would strand old boards,
  because their contract could no longer decode new deltas.
- `fdev build` panics with "Could not find workspace root" unless
  `CARGO_TARGET_DIR` is set — it resolves its target directory from its own
  compile-time manifest path, which lives in the cargo registry. `build.sh` sets
  it. (Upstream bug, not a project setting.)
- A `put timed out after 1 peer attempt(s)` warning when creating a board does
  not necessarily mean failure — the contract is often stored and retrievable
  anyway. The warning reports the network operation's acknowledgement, not
  whether the state landed.

## Licence

AGPL-3.0-only. See [LICENSE](LICENSE).

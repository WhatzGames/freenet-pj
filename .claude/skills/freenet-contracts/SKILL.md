---
name: freenet-contracts
description: Rules for changing Freenet contracts and delegates in this repo — what belongs inside a contract, what must not, where to put stored settings, and how to avoid orphaning users' data. Use when editing pj-core, any pj-*-contract or pj-*-delegate crate, adding an op or a field, or adding anything that needs to persist.
---

# Working with contracts and delegates

**One rule, and everything follows from it.** A contract's address is
`hash(code + parameters)`. A delegate's secret namespace is
`hash(code + parameters)`. Change the compiled wasm by one byte and it is a
different object at a different address, holding none of the old data.

You cannot version a contract in place. So the goal is not "change it
compatibly" — that is impossible — the goal is **not needing to change it.**

## The envelope design — done, and why it is shaped this way

**Migration complete.** Boards, organizations and profiles all speak
[`EnvelopeState`](../../../crates/pj-core/src/envelope_state.rs). Published and
verified on 2026-07-26. Every address moved once; everything before it is gone,
which was the accepted trade.

One op type serves all three contracts:

```rust
Envelope { scope, author, lamport, wall_clock_ms, nonce, needs, kind, body }
```

A contract reads `scope`, `needs`, and three `kind`s — `GRANT`, `LINK_DEVICE`,
`UNLINK_DEVICE`. `body` is bytes it never decodes. That is the whole of its
policy, and the reason **adding an op kind now rebuilds nothing.**

Four things hold it together. Do not undo any of them without reading why.

### `scope` — one signature, one contract

`Scope::of(parameters)`. Without it, one envelope type across three contracts
means a signature says "this person wrote this", not "…here": an admin's grant of
`ADMIN` on one board replays onto every other board they administer, and onto
their own profile, where `ADMINISTER` is write access to everything. Checked in
`validate` before the signature, because a valid signature for somewhere else is
exactly what is being refused.

The old per-message domain separators prevented board↔org↔profile replay but not
board↔board. This closes both.

### Two-answer authority — `held` and `ever_held`

`held` is last-writer-wins: what you may do now. `ever_held` is monotone: what
judges ops already written.

Collapsing them makes removal impossible. Removing someone invalidates their past
work, which makes the removal op itself unacceptable forever — the state can never
be accepted again. There is a test; it was a real bug, found by tests failing.

### `MAY_APPOINT` — the second bit a contract interprets

A grant confers `asked ∩ held`, so `MAY_GRANT` would be transitive by
construction and every admin could mint admins. `MAY_APPOINT` is what keeps the
closure two levels deep: an admin invites people, the owner decides who else may.
It governs unmaking an admin too, since a removal is a grant of nothing and
nothing is within anybody's cap.

### Resigning needs no permission

A grant of `NONE` to *yourself* is always honoured. Otherwise leaving would be
something only an admin could do on your behalf. It is how `leave_org` works, and
there is no separate op for it.

### Two layers of authorization, and which is the real one

The contract checks the envelope's **declared** `needs` — which the author wrote,
so it proves only that they hold *something*. What an op actually requires is
`Op::needs()`, checked by the fold, which no author controls. An op can therefore
be stored and still not count.

That is deliberate. Dropping it at the contract would let a peer whose clock ran
ahead poison a board permanently; ignoring it in the fold costs nothing and
reverses itself if the grant that permits it arrives later.

### Unknown kinds are carried, not rejected

A `kind` this build has never heard of is skipped by the fold, counted in
`Board::unreadable_ops`, and re-encoded untouched on the next push. Compare the
old typed enum, where one new variant made the whole state fail to decode — a
board read as *unreadable*, not as missing one card.

One thing the contract does refuse: an authority op whose author has never held
any rights. It could never become meaningful, and accepting it would let any key
on the network grow any contract's state for free. `ever_`, so it stays monotone.

## Planned: a task is its own contract — designed, not built

Decided with the user, not yet implemented. Three requirements, in their words:

> "tasks should be a board independent resource."
> "But the contents of the task should only be loaded, when clicked on the task"
> "Cache the summary, but whenever a change is made to the task, that should be
> reflected on the board"

And, refining it:

> "we should have a set of board the ticket is listed on"
> "we consider all contents provided to be read-only in public."
> "members can only change the tickets status for boards they have access to"
> "anyone within the same org should have the right to modify them, whether they
> have now as seeding or when joining later. only the status change is restricted
> to board level rights."
> "Bidirectional linking Tasks can be done as long as both Tasks are within the
> same organization. Otherwise tasks will only be linked unidirectional."

Today a task lives inside its board's state, so it cannot exist on two boards, be
moved between them, or outlive one. Making it a first-class contract fixes that.
The objection to per-task contracts was fan-out: a fifty-card board would be
fifty fetches. Lazy loading dissolves it — a board render stays **one** fetch,
plus one when a card is opened.

### Data model

**Task contract** (`pj-task-contract`, new). Parameters
`TaskParameters { org: OrgId, org_owner: MemberId, creator: MemberId, created_ms: u64, nonce: [u8; 16] }`.
The nonce keeps two tasks made in the same millisecond by the same person at
different addresses; `org` and `org_owner` are load-bearing for permissions
(below) and cannot be added later without moving every address. State is
`EnvelopeState`, like everything else, so the two-answer authority fold and
unknown-kind tolerance come for free. Ops: title, notes, assignee, links,
`Attach { board }` / `Detach { board }`.

**Board contract** keeps columns, membership, organization, and gains
*placements* in place of tasks:

- `Place { task: TaskAddr, column, rank }`
- `Unplace { task }`
- `Summarize { task, title, assignee, seen_lamport }`

A placement is a reference plus a cached summary. The card renders from the
summary alone, before anything is fetched.

### The split: body on the task, status on the board

Status is **not** a task field. Which column a card sits in is a property of the
placement, so changing it needs `WRITE_TASKS` on *that board* and touches the task
contract not at all. The same task can legitimately be in `Doing` on one board and
`Done` on another — that is the point of a board-independent task, not a bug to
reconcile. A card's column and its status can never disagree because they are the
same fact.

Everything else — title, notes, assignee, links — lives on the task and is
governed by org membership.

### Body rights: org-scoped grants, a deliberate exception to scope binding

The requirement is that **anyone in the org may edit a task's body, including
people who join later**. That rules out seeding grants at creation or at attach
time: a snapshot cannot serve someone who was not yet a member, and there is no
reverse index to push new grants over.

A contract cannot read the org contract's state. What it *can* do is verify a
signature chain, which is what the authority fold already does. So: introduce a
grant whose scope is **the org**, not one contract instance. A task contract whose
parameters name that org accepts an org-scoped grant that chains from `org_owner`,
and treats its holder as a member.

This is a narrow, intentional hole in the rule that a signature is bound to one
contract instance — and that rule exists to stop cross-contract replay, so read
the exception carefully before widening it:

- An org-scoped grant is **meant** to be presentable at many contracts. It is a
  capability certificate, so replay is the feature.
- Only contracts whose parameters name that org will accept it. It buys nothing
  anywhere else.
- Instance-scoped grants keep working exactly as they do now, and board rights
  stay instance-scoped — which is what keeps "status only on boards you have
  access to" true.
- The certificate reaches the task by the joiner copying the org's own grant
  envelopes into the task's state on their first edit, signature intact. Validity
  is cryptographic, not positional, so self-service is fine. Cost is a little
  state per (task, member), once.

### All contract state is public

A design invariant, decided explicitly: everything the app puts on Freenet is
read-only public data. Nothing may be built that treats an address as a secret —
no private boards, no confidential fields, no visibility flags. This is why a task
freely lists the boards it is on, and why that disclosure needed no mitigation.

### The summary is kept in sync, and `seen_lamport` is what makes that safe

`Summarize` carries the task's own lamport as read at the moment it was written,
and the fold keeps only the highest one per task. That single field is what lets
*anyone* write a summary at *any* time without a stale writer clobbering a fresh
one — reconciliation is convergent rather than racy. Do not drop it.

Three parts to the rule:

1. **Write-through.** Editing a task emits the op to the task contract, plus a
   `Summarize` to every board in the task's `boards` set that the author holds
   `WRITE_TASKS` on — not just the board in front of them.
2. **Reconcile on open.** Because the body is only fetched on click, a client
   only ever *learns* a summary is stale at the moment a user opens that task.
   So: on open, if the fetched task disagrees with the board's cached summary and
   the client holds `WRITE_TASKS` on the board, emit the missing `Summarize`.
   Self-healing, and bounded to at most one write per open rather than a sweep
   over every card.
3. **Accept staleness where you have no rights.** A client that can write the
   task but not the board cannot fix the card — there is no writing to a contract
   you hold nothing on. The card stays stale until someone with board rights
   opens it. Same shape as device unlinking across projects today; say so in the
   UI rather than pretending otherwise.

Note what part 1 depends on: the author has to *know* which boards to write to.
That is what the `boards` set below is for. Without it this rule degrades to "fix
the board in front of you and hope", which is the version of this design that was
almost shipped.

### The task holds the set of boards it is on

`Attach`/`Detach` maintain a `boards: BTreeSet<BoardId>` on the task, LWW per
board like `held` in the authority fold. This is a **client-maintained reverse
index**, and it is what makes the rest of the design work rather than merely
limp:

- The route can be the bare address, `#task/<44 chars of base58>`, with no board
  in it. The task's `ContractInstanceId` *is* its address and a GET needs only the
  instance id, so a link is self-describing. `TaskRef` collapses from
  `{ board: Option<BoardId>, task: TaskId }` to one address, and `link.rs::parse`
  keeps its shape — whole URL, bare route, or lone id — over one component.
- A cold-opened task link can still offer "← Board", for every board it is on.
- Reconciliation becomes *complete* rather than best-effort: opening a task tells
  you every board whose summary needs checking, not just the one in front of you.

Treat the set as a **hint verified on use**: it is written by clients, so a failed
or missing `Attach`/`Detach` makes it lie. On following it to a board that has no
placement for the task, drop it from the display rather than showing a dead link.

Placing a task therefore writes two envelopes — the board's `Place` and the task's
`Attach` — and neither is a precondition for the other rendering correctly.

### Links between tasks: bidirectional in-org, unidirectional across

Linking A→B always writes the forward link on A. The back-link on B is written
**only when both tasks name the same org**, because that is when the author holds
an org-scoped certificate for B and can write to it at all. Across orgs the link
stays one-way.

This is a consequence, not a policy: same-org bidirectionality is exactly the set
of cases where the write is possible. If the back-link write fails anyway, fall
back to unidirectional rather than refusing the forward link.

### Lifecycle: create, remove, migrate

**Creating** a task PUTs a contract, and the placement is written only after the
PUT is confirmed by read-back (the existing 1.5 s × 4 dance). The card appears a
beat later than it would otherwise, and in exchange a board never holds a
reference to a contract that is not there. That failure mode is the dead-board-URL
bug already fixed once in this codebase; do not reintroduce it by placing
optimistically.

**Removing** a card unplaces only. The task contract survives, its link keeps
resolving, and it can be placed on another board. A task on no board is reachable
only by someone holding its link — accepted, since orphaning is the price of
board-independence and nothing on the network is ever really deleted anyway.

**Migrating** the tasks that exist inside board state today: on opening a legacy
board, the client PUTs a task contract per card and writes `Place` + `Attach` +
`Summarize` for each. Roughly one PUT and three envelopes per card. The path is
dead code once every board the user owns has been converted — delete it then,
rather than leaving it to rot.

### Order of work, and what it breaks

**Every existing task and link is rewritten, not preserved in place** — the
migration above is what carries them over, and anything it misses is gone. Third
address break of this line of work.

1. ~~`pj-core`: org-scoped grants in `envelope_state.rs`~~ — **done.** `Org`,
   `Trust`, `admits`, `authority_in`/`validate_in`/`accept_in`, one shared
   `fold_authority`, seven tests pinning the hole to exactly one kind and one org.
2. ~~`pj-core/src/task.rs`~~ — **done.** `TaskParameters` (creator + optional
   `TaskOrg` + created_ms + nonce), `TaskOp` (title/description/assignee, link,
   attach/detach — no status, no task id), `Task` fold, `TaskSummary` with
   `seen_lamport`, `summary_is_stale` deciding on content so an untouched open
   writes nothing. `TaskAddr` added to `ids.rs`.
3. ~~`crates/pj-task-contract` + `build.sh`~~ — **done.** Mirrors the board
   contract; the only difference is `validate_in`/`accept_in` against
   `params.trust()`. Needs its own `freenet.toml` (`fdev build` fails without one).
4. ~~Board placements~~ — **done.** `link.rs` retargeted to bare `TaskAddr`
   (`parse_task`/`task_route`, route `#task/<addr>`); board gained
   `Place`/`Unplace`/`Summarize` and lost all seven task ops; `Task` became
   `Placement`; `from_state_with_id` collapsed back to `from_state`, since links no
   longer name a board and a board has no reason to know where it is.
5. ~~`pj-web/src/store.rs`~~ — **done.** `Kind::Task`, `fetch_task` on open,
   `task_emit` carrying org certificates, `reconcile_summary`, confirm-then-place
   via `pending_placement`, and the legacy conversion.
6. ~~UI~~ — **done.** Cards render from the cached summary; drawer and page share
   `TaskBody` and read the fetched task; the task lists the boards it is on.

All of it is green: 167 tests, `cargo fmt --check` clean, clippy clean at deny
level on both workspaces, zero in-code suppressions, `./scripts/build.sh` passing.

Published and driven end to end on 2026-07-26. Five bugs only a running node
found, each worth remembering:

- **Writing the route re-navigated.** `select_task` puts `#task/<addr>` in the
  URL; the browser's own hashchange read it straight back and opened the task's
  *page*, so every click on a card left the board. Any handler that both writes a
  route and reads one needs the "am I already here?" guard.
- **Removing enum variants renumbered the rest.** Dropping seven task variants
  from `Op` shifted `SetOrganization` and `SetMemberName` down, so old boards lost
  their member names and org link as well as their cards — and reported them as
  "written by a newer version", which was exactly backwards. `legacy` now decodes
  and rewrites those too. The rule in "Adding an op" is not only about appending.
- **Counting cards where ops were meant.** Subtracting recovered *cards* from
  unreadable *ops* left phantom entries, because one card is several ops.
- **Conversion could run twice.** Nothing is ever deleted, so the old ops stayed
  and every visit re-offered them, duplicating cards. Fixed by writing a tombstone
  *in the old encoding* — `recover_tasks` already honours those, so it converges
  for every client rather than being remembered locally.
- **Three buttons ate the column name.** `.column-tools` took 91px of a 176px
  header and clipped every title to "Tod". Out of flow now.

### Reading boards written before the split

`pj_core::legacy` decodes the old task ops out of a board's existing op set — the
board contract never understood them, it stored bytes, so they are still there and
still readable. `recover_tasks` folds them the way the old board did;
`Store::migrate_legacy` writes each as a task contract and puts a card back.

Offered as a button rather than run on open: it is one PUT per card, and two tabs
on the same board would otherwise both start converting it.

Delete the module once the boards that matter have been converted. It is a second
definition of what an op is, kept only for as long as it is earning its place.

Step 1 is the one to get right before any of the others are written. It changes an
invariant the three existing contracts already rely on, so it needs its own tests
proving that an instance-scoped grant is still refused everywhere it was refused
before, and that an org-scoped grant is refused by a contract naming a different
org.

Do not start this in the same session as a design change to it. A rewrite this
wide, stopped halfway, leaves a tree that does not build.

Rejected alternative, for the record: **one shared task store per owner** — two
fetches per board and no denormalisation, but a state-size ceiling and every edit
re-encoding a store holding every task in the org. Only lazy loading made
per-task contracts the better trade; if lazy loading is ever abandoned, this
becomes the right answer again.

## Before you touch anything: record the hashes

```bash
export CARGO_TARGET_DIR="$PWD/target"
for c in pj-board-contract pj-task-contract pj-user-contract pj-org-contract pj-registry-contract; do
  n=$(echo $c | tr '-' '_')
  printf "%-22s %s\n" "$c" "$( (cd "$PWD/crates/$c" && fdev inspect build/freenet/$n code 2>/dev/null | head -1) )"
done
```

Last **published** values, 2026-07-26 — what users' data currently hangs off:

```
pj-board-contract      B6F4YpJv4WWNHtjcGe5vPmknKQXmh6pbU8LwbE37KhwM
pj-user-contract       5s1YRZEbxNDCiMyvHiRWhVxSRs5SmrkdoSESR9vF5XAo
pj-org-contract        DD22v8Lnp8ke9PETne9VX7wgu3gfhJz1TENNjhUyhwie
pj-registry-contract   BfmbUKaqY2UbExxnCuUc1VfaKeVAS8i5A2c9sTe4XFzt
```

Built but **not yet published**, after the task split:

```
pj-board-contract      DYsfpVJ2S5RMPBJwC5b9ycSMapefDocPk7svaKzzdHqU
pj-task-contract       7R27RadcL7VbPmAaw5X4ceq5r1QZUciEABZV5VGSgiH8   (new)
pj-user-contract       82od2phm7nGSVhkkj7xjjct3aVUjK7eHVkfRNadptjVF
pj-org-contract        Fk6v6o1kT1izorVDmXai456J7b9wDt7tpDt6bgKWJ2ai
pj-registry-contract   21313Pg8jrjJ8cFDgjAiAkTGUvtuRrhYgRrq9otEh9te
```

Measured, not assumed: **all five moved, including the three that gained
nothing.** Org, user and registry never needed org-scoped grants or placements —
they simply link the crate that changed, and `Trust`/`Option<Org>` in `validate`
did not constant-fold away. Holding them still would take a second verbatim copy
of the authority fold, refused on the grounds that two copies of the authorization
core is how a divergence bug gets in.

So publishing resets boards, orgs, profiles and the directory. Identity keypairs
live in the delegate and survive. **Do this in one publish**, not two: every
address that was going to move has now moved, and a second reset later buys
nothing that could not have been batched into this one.

**Re-record these whenever you publish.** An earlier set had drifted until all
four had moved with no way left to attribute it. A stale baseline cannot tell you
a hash moved by accident, which is the only thing it is for.

### A hash moves for reasons that have nothing to do with meaning

Worth internalising, because it is counter-intuitive. Board, org and user all
moved during a pass whose entire purpose was *lint cleanup* — no behaviour
changed. `(lo + hi) / 2` became `lo.midpoint(hi)`, a `pub fn` became
`pub(crate) fn`, `cargo fmt` reordered some imports. Different bytes, different
wasm, different address.

The registry did not move, because nothing it compiles was touched. That is the
rule in practice: **the blast radius is whichever contracts compile the `pj-core`
modules you edited**, and semantic neutrality buys you nothing.

So: run the hash check after *any* change to `pj-core`, including one you would
describe as cosmetic, and batch cosmetic work with whatever else is breaking
anyway.

Rebuild, run it again, diff. `scripts/build.sh` prints three of them at the end
of every build for this reason. **A hash that moved unintentionally is a data
loss event, not a detail.**

### Blast radius is per-module — measured, not assumed

Adding one variant to `UserOp` in `pj-core/src/user.rs` and rebuilding gave:

```
pj-user-contract      AQUxDL25mHH7… → 8wBvNP36mKUN…   MOVED
pj-board-contract     unchanged
pj-org-contract       unchanged
pj-registry-contract  unchanged
```

Dead-code elimination confines a `pj-core` change to the contracts that use the
changed module. A board-only change cannot orphan profiles. Do not extend that
trust to your own change without measuring it.

### What "orphaned" actually costs

The client **computes** these addresses rather than remembering them
(`contract::user_key(&params)`, `store.rs`). New hash, new address, and the old
object is still out there, unreachable. For the user contract that is everyone's
device list and their index of projects and organizations. Boards survive: they
are separate contracts with their own membership records.

## What belongs inside a contract

Only what answers: **may this author write this?** In practice, only what is
already there:

- Grants, because rights have to come from somewhere
- Device links, since a device inherits a person's authority
- Scope and signature verification
- The merge itself

Everything else is an op body it never decodes. Nothing new belongs here.

## What does not

- **Display names** — presentation.
- **Preferences, themes, layout** — not shared state at all.
- **Anything the client derives.** `Board`, `Organization` and `UserProfile` are
  folds over ops, never stored. Adding a field to one is free and moves no hash.
- **Anything order-dependent.** `update_state` must be commutative; peers merge
  in arbitrary order.

The fold is the real authority — see "two layers of authorization" above. The
contract's checks are a first line against garbage, not the last word. Moving a
rule out of the contract into the fold is rarely the weakening it looks like.

## Adding an op

This is now cheap, which is the entire point of the envelope. It moves no hash and
orphans nothing.

1. **Append the variant at the end** of `Op` / `OrgOp` / `UserOp`. `bincode` enum
   tags are positional; inserting one silently reinterprets every stored op of
   every later variant.
2. Give it a `needs()` — the rights it actually requires, not the ones you expect
   the author to have. If the rule depends on *who* the op names (renaming
   somebody else, say), `needs()` cannot express it; check that in the fold, as
   `SetMemberName` does.
3. Add the arm to the fold. It matches exhaustively; the compiler finds it.
4. Rebuild and diff the hashes anyway. They should not move. If one did, find out
   why before publishing.

An older client meeting your new variant fails to decode *that op* and carries its
bytes intact. It does not fail to decode the state, which is what the old typed
enum did.

## Adding a right

Cheaper still: a `Rights` bit is a client-side change, because the contract's check
is arithmetic. Add it in `rights.rs`, use it in the relevant `needs()`, done.

The exceptions are `MAY_GRANT` and `MAY_APPOINT`, which the authority fold reads
by name and which therefore live in every contract's wasm. Those two are fixed.

## Anything that must persist goes to the node

The app runs in a sandboxed iframe on an opaque origin where `localStorage`,
`sessionStorage` and IndexedDB all throw. **The node is the only local storage.**

Use the existing preferences delegate:

```rust
store.set_preference("density", "compact");
let value = store.preference("density");   // None until the delegate answers
```

`pj-prefs-delegate` stores an **opaque blob**; the schema lives in
`pj-prefs-proto` as a `BTreeMap<String, String>`. So **adding a preference
changes no wasm and moves no key** — that is the entire point of the delegate not
understanding what it stores. The map also means a build that has never heard of
a key does not delete it by saving; `pj-prefs-proto` has a test for that.

Scope: **per node.** Two browsers on one node share a setting; the same account
on another node does not. Correct for a theme, which describes the screen you are
at. Wrong for anything that should follow a person — that belongs in the user
contract, at the cost above.

### Never extend an existing delegate

Adding preferences to `pj-identity-delegate` would have moved its key and taken
every user's signing seed with it. **A new capability means a new delegate**; a
new one has nothing to lose. Both register independently in `store.boot()`, and
`HostResponse::DelegateResponse` routes on `key`, since two delegates now answer
on one channel with different payload encodings.

### `RegisterDelegateWithPredecessors` does not work — settled, do not retry

Tested to destruction on 2026-07-25 against freenet 0.2.105. Do not spend a day
rediscovering this.

The node **does** implement copy-forward, synchronously inside the registration
handler (`freenet/src/contract/executor/runtime/delegates.rs`). It then refuses:

```text
delegate secret copy-forward: predecessor has no recorded origin (NoProvenance);
refusing (legacy data migrates via the app-side path)
… copy-forward completed predecessors=4 copied=0
```

It will not copy from a predecessor with no recorded *origin*, and ours never
have one. **This is not a legacy-data problem.** Generation 3 was registered by
this app, on this node, minutes before generation 4 asked to inherit from it —
and was refused identically. Registration requests apparently do not carry the
web-app origin that application messages do, so provenance is never recorded and
every predecessor is refused. Nothing the client sends changes that.

A genuine defect was found and fixed en route, and is worth knowing separately:
registration acknowledges with `DelegateResponse { values: [] }`, which this
app's `find_map` over `values` silently discarded — so the seed was requested
before any copy could land. `store.rs` now waits for it, with a 3s fallback. (An
earlier attempt waited on `HostResponse::Ok`, which the node never sends for a
registration, and identity stopped loading entirely.) Worth fixing; not the
blocker.

**Therefore: a delegate rebuild is destructive.** Consequences:

- The recovery key is the migration path for identity today. Export it *before*
  touching a delegate; restoring is one paste into the account page and works.
- Preferences have no recovery path, so rebuilding `pj-prefs-delegate` loses
  every node's settings. It stores opaque bytes precisely so it never has to be
  rebuilt — keep it that way.
- The real fix the node's message points at is an **app-side migration**: embed
  the previous generation's wasm, read the seed from it, write it into the new
  one with `IdentityRequest::Replace`. Entirely within our control; not yet built.

Diagnose any future attempt from the node's own log, not from the app:

```bash
grep -h "copy-forward" ~/Library/Logs/freenet/*.log | tail -5
```

To read a delegate's key: `cargo test -p pj-identity-delegate -- --nocapture delegate_key`.
Note the recorded predecessor keys drifted from the real ones — an edition bump
moved the wasm without anyone bumping `GENERATION`. Read the key; never assume it.

## Client-side rules that follow

- **Never recompute an open board's key from embedded code.** Use the
  node-reported `ContractKey` in `store.contract_key`. A board created under an
  older contract lives at the older address, and recomputing aims updates at a
  contract that does not exist — the writes vanish with no error. Only `create`
  may use the locally derived key, because it is minting the address.
- `pj-identity-proto` and `pj-prefs-proto` exist so a `pj-core` change cannot
  move a delegate's key. **Keep their dependencies minimal**; a bump there is a
  migration for every user.

## If you are designing a change that would move an address anyway

Ask first whether it can be an op kind or a rights bit instead — both are free,
by construction, and that is what the envelope design bought. See the top of this
file. If it genuinely cannot, batch it with every other such change so the data
is orphaned once rather than three times.

## Checklist

- [ ] Hashes recorded before the change
- [ ] New enum variants appended, never inserted
- [ ] Nothing presentational added to a contract
- [ ] `update_state` still commutative
- [ ] `./scripts/build.sh` run, not a shortened version of it — it gates on
      `cargo fmt --check` and deny-level clippy across **both** workspaces and
      **both** targets, and on the tests. See the `run-freenet-pj` skill.
- [ ] No `#[allow]` or `#[expect]` added. There are none in the tree; exceptions
      belong in the `[lints]` tables with a reason, where they can be read as a list
- [ ] Hashes diffed after the build; every move intended — *including* after
      changes you would call cosmetic, which move them just the same
- [ ] Settings went to the preferences delegate, not a contract
- [ ] No existing delegate's wasm was modified

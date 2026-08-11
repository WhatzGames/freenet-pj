//! The materialised board: what the op set means, folded into something a UI
//! can render.
//!
//! The fold is a pure function of the op set. It walks ops in a total order
//! derived entirely from their own contents ([`SignedEnvelope::order_key`]) and
//! resolves competing writes to the same field by taking the later one, so two
//! peers holding equal op sets always render an identical board — which is the
//! property that makes "eventually consistent" mean something to a user.
//!
//! # Two layers, and which one is the real authority
//!
//! The contract enforces the coarse gate: an op is stored only if its author holds
//! the rights the *envelope declares*. But the author writes that declaration, so
//! it proves only that they hold *something*. What an op actually requires is
//! [`Op::needs`], which this fold checks and no author controls.
//!
//! So an op can be in the state and still not count. That is deliberate: dropping
//! it at the contract would mean a peer whose clock ran ahead could permanently
//! poison a board, while ignoring it here costs nothing and reverses itself if the
//! grant that permits it turns up later.
//!
//! # Ops from the future
//!
//! An envelope whose `kind` this build has never heard of, or whose body it cannot
//! decode, is skipped and kept. The board renders as though it were not there, the
//! bytes survive the next push, and a client that does understand it sees the whole
//! picture. Compare the alternative: one unknown variant, and the entire state
//! fails to decode.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::envelope::{SignedEnvelope, kind};
use crate::envelope_state::{Authority, EnvelopeState};
use crate::ids::{ColumnId, MemberId, OrgId, TaskAddr};
use crate::op::Op;
use crate::params::BoardParameters;
use crate::rank::Rank;
use crate::rights::{Rights, Role};
use crate::task::TaskSummary;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: MemberId,
    pub name: String,
    /// What they may do now. The authority; `role` is a reading of it.
    pub rights: Rights,
    pub role: Role,
    /// False once their rights have been taken away. Kept in the map rather than
    /// deleted so their name still renders on tasks they touched.
    pub active: bool,
}

/// The organization owning a project, as recorded on the board itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardOrganization {
    pub org: OrgId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub id: ColumnId,
    pub title: String,
    pub rank: Rank,
}

/// A task's position on this board, plus enough of it to draw a card.
///
/// The board holds no task bodies. What it holds is a reference, a position, and a
/// cached [`TaskSummary`] — so a board renders in one fetch and a task's contents
/// are fetched only when someone opens it. See [`crate::task`] for the other half.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub task: TaskAddr,
    pub column: ColumnId,
    pub rank: Rank,
    /// What the card shows before anything is fetched. May lag the task; see
    /// [`TaskSummary`] for what stops it lagging permanently.
    pub summary: TaskSummary,
    pub placed_by: MemberId,
    pub placed_ms: u64,
}

impl Placement {
    /// What to show on the card. Falls back to the address rather than to an empty
    /// card, because a task whose summary never arrived is still something a person
    /// has to be able to click on.
    pub fn title(&self) -> String {
        if self.summary.title.is_empty() {
            self.task.short()
        } else {
            self.summary.title.clone()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    pub members: BTreeMap<MemberId, Member>,
    /// Sorted by rank, then id to break rank ties.
    pub columns: Vec<Column>,
    /// Cards on this board, by the address of the task each one points at.
    pub tasks: BTreeMap<TaskAddr, Placement>,
    /// Each linked device key mapped to the member it belongs to, so a person
    /// acting from a second browser still renders as one person.
    pub devices: BTreeMap<MemberId, MemberId>,
    /// The organization this project belongs to, if any. Display and navigation
    /// only — authority comes from the owner key in the parameters.
    pub organization: Option<BoardOrganization>,
    /// One past the highest lamport in the op set — the clock value a client
    /// should stamp its next op with.
    pub next_lamport: u64,
    /// Ops in the state that this build could not interpret. Not an error: they
    /// are carried intact. Surfaced so a UI can say "some of this board was
    /// written by a newer version" rather than silently showing less.
    pub unreadable_ops: usize,
}

/// An application op that survived both the decode and the rights check, paired
/// with the envelope it arrived in so the fold can still see who and when.
struct Applied<'a> {
    envelope: &'a SignedEnvelope,
    op: Op,
}

impl Applied<'_> {
    fn author(&self) -> MemberId {
        self.envelope.author()
    }

    fn at(&self) -> u64 {
        self.envelope.payload.wall_clock_ms
    }
}

impl Board {
    /// Folds an op set into a board.
    ///
    /// This used to need to know the board's own id, so that a link written as
    /// "board X, task T" from board X could be recognised as local. Links live on
    /// the task now and name nothing but an address, so a board no longer has any
    /// reason to know where it is.
    pub fn from_state(state: &EnvelopeState, params: &BoardParameters) -> Self {
        let authority = state.authority(params.owner);

        let next_lamport = state
            .ops
            .values()
            .map(|op| op.payload.lamport)
            .max()
            .map_or(1, |max| max.saturating_add(1));

        let mut unreadable_ops = 0;
        let mut applied: Vec<Applied<'_>> = Vec::new();
        for envelope in state.application_ops() {
            // Anything beyond the one application kind this build knows is data
            // from a newer client. Carried by the state, invisible here.
            if envelope.payload.kind != kind::FIRST_APPLICATION_KIND {
                unreadable_ops += 1;
                continue;
            }
            let Ok(op) = Op::decode(&envelope.payload.body) else {
                unreadable_ops += 1;
                continue;
            };
            // `ever`, not `held`: work already done survives its author's removal.
            if !authority
                .ever_rights_of(&envelope.author())
                .contains(op.needs())
            {
                continue;
            }
            applied.push(Applied { envelope, op });
        }

        let members = fold_members(&applied, &authority, params);
        // `person_of` is already resolved to a person at link time, so there is no
        // chain left to walk here.
        let devices = authority.person_of.clone();

        let (columns, removed_columns) = fold_columns(&applied);
        let tasks = fold_tasks(&applied);
        let organization = fold_organization(&applied);

        let mut board = Board {
            members,
            columns,
            tasks,
            devices,
            organization,
            next_lamport,
            unreadable_ops,
        };
        board.drop_removed_columns(&removed_columns);
        board
    }

    /// Cards in a column, in display order.
    pub fn tasks_in(&self, column: &ColumnId) -> Vec<&Placement> {
        let mut tasks: Vec<&Placement> = self
            .tasks
            .values()
            .filter(|task| &task.column == column)
            .collect();
        tasks.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.task.cmp(&b.task)));
        tasks
    }

    pub fn active_members(&self) -> Vec<&Member> {
        self.members.values().filter(|m| m.active).collect()
    }

    /// The member a key belongs to, following a device link if that is what it is.
    pub fn person_of(&self, id: &MemberId) -> MemberId {
        if self.members.contains_key(id) {
            return *id;
        }
        self.devices.get(id).copied().unwrap_or(*id)
    }

    /// What a key may do now, following a device link to its owner.
    pub fn rights_of(&self, id: &MemberId) -> Rights {
        self.members
            .get(&self.person_of(id))
            .filter(|member| member.active)
            .map_or(Rights::NONE, |member| member.rights)
    }

    /// Whether a key may do a thing. The question every disabled button asks.
    pub fn may(&self, id: &MemberId, rights: Rights) -> bool {
        self.rights_of(id).contains(rights)
    }

    /// Resolves a key to a display name, so work done from a linked device is
    /// still attributed to the person who did it.
    pub fn member_name(&self, id: &MemberId) -> String {
        let person = self.person_of(id);
        self.members
            .get(&person)
            .map_or_else(|| id.short(), |m| m.name.clone())
    }

    /// The extra keys a member has linked to themselves.
    pub fn devices_of(&self, person: &MemberId) -> Vec<MemberId> {
        self.devices
            .iter()
            .filter(|(_, owner)| *owner == person)
            .map(|(device, _)| *device)
            .collect()
    }

    /// The rank a card dropped at `index` in `column` should take.
    ///
    /// `exclude` is the task being dragged. Leaving it in the list would shift
    /// the neighbours by one when reordering within a column, so a card dropped
    /// one slot down would land back exactly where it started.
    pub fn rank_for_drop(
        &self,
        column: &ColumnId,
        index: usize,
        exclude: Option<TaskAddr>,
    ) -> Rank {
        let tasks: Vec<&Placement> = self
            .tasks_in(column)
            .into_iter()
            .filter(|placement| Some(placement.task) != exclude)
            .collect();
        let lo = index
            .checked_sub(1)
            .and_then(|i| tasks.get(i))
            .map(|t| &t.rank);
        let hi = tasks.get(index).map(|t| &t.rank);
        Rank::between(lo, hi)
    }

    /// Removing a column would otherwise make its tasks invisible without
    /// deleting them. Move them to the first surviving column instead, so no
    /// work silently disappears.
    fn drop_removed_columns(&mut self, removed: &BTreeSet<ColumnId>) {
        if removed.is_empty() {
            return;
        }
        self.columns.retain(|column| !removed.contains(&column.id));
        let Some(fallback) = self.columns.first().map(|c| c.id) else {
            // Every column is gone; nothing sensible to reassign to.
            return;
        };
        for task in self.tasks.values_mut() {
            if removed.contains(&task.column) {
                task.column = fallback;
            }
        }
    }
}

/// Which organization owns this project. Last writer wins.
///
/// No rights check here: `Op::SetOrganization` needs `ADMINISTER`, and anything
/// that reached `applied` already holds what it needs.
fn fold_organization(applied: &[Applied<'_>]) -> Option<BoardOrganization> {
    let mut organization = None;
    for entry in applied {
        if let Op::SetOrganization { org, name } = &entry.op {
            organization = Some(BoardOrganization {
                org: *org,
                name: name.clone(),
            });
        }
    }
    organization
}

/// Membership comes from the authority ops, which the contract also reads; names
/// come from ordinary ops, which it does not.
///
/// Splitting them this way is the point of the whole design: renaming yourself
/// travels the same path as moving a card, and only *granting* travels the path
/// the contract polices.
fn fold_members(
    applied: &[Applied<'_>],
    authority: &Authority,
    params: &BoardParameters,
) -> BTreeMap<MemberId, Member> {
    let mut names: BTreeMap<MemberId, String> = BTreeMap::new();
    for entry in applied {
        let Op::SetMemberName { member, name } = &entry.op else {
            continue;
        };
        // Naming yourself needs only `SET_NAME`, which every member holds.
        // Naming somebody else is an administrative act. This is the one rule
        // `Op::needs` cannot express, because it depends on who is named.
        let author = authority.ever_person(&entry.author());
        let renaming_self = *member == author;
        if !renaming_self
            && !authority
                .ever_rights_of(&author)
                .contains(Rights::ADMINISTER)
        {
            continue;
        }
        names.insert(*member, name.clone());
    }

    let mut members = BTreeMap::new();
    // The owner is a member by definition — their authority is in the parameters,
    // so no op can confer or remove it.
    for id in authority.ever_members().into_iter().chain([params.owner]) {
        let rights = authority.held.get(&id).copied().unwrap_or(Rights::NONE);
        members.insert(
            id,
            Member {
                id,
                name: names.get(&id).cloned().unwrap_or_else(|| id.short()),
                rights,
                role: Role::of(rights),
                active: !rights.is_empty(),
            },
        );
    }

    members
}

fn fold_columns(applied: &[Applied<'_>]) -> (Vec<Column>, BTreeSet<ColumnId>) {
    let mut columns: BTreeMap<ColumnId, Column> = BTreeMap::new();
    let mut removed: BTreeSet<ColumnId> = BTreeSet::new();

    for entry in applied {
        match &entry.op {
            Op::SetColumn {
                column,
                title,
                rank,
            } => {
                columns.insert(
                    *column,
                    Column {
                        id: *column,
                        title: title.clone(),
                        rank: rank.clone(),
                    },
                );
            }
            Op::RemoveColumn { column } => {
                removed.insert(*column);
            }
            _ => {}
        }
    }

    let mut columns: Vec<Column> = columns.into_values().collect();
    columns.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.id.cmp(&b.id)));
    (columns, removed)
}

/// Folds the placements: which tasks are on this board, where, and what their
/// cards say.
///
/// Much smaller than the fold it replaces, because most of what a task *is* now
/// lives at the task's own address. What is left is position and a cached summary.
fn fold_tasks(applied: &[Applied<'_>]) -> BTreeMap<TaskAddr, Placement> {
    let mut tasks: BTreeMap<TaskAddr, Placement> = BTreeMap::new();
    // Kept separately from the map so that an `Unplace` cannot be undone by a
    // `Summarize` that happens to sort after it. Removal has to be the last word,
    // or it would not converge.
    let mut unplaced: BTreeSet<TaskAddr> = BTreeSet::new();
    // Summaries can arrive before the placement they describe — they are written to
    // whichever boards a task says it is on, by someone who may not be looking at
    // this one. Held aside and applied after, so the result does not depend on the
    // order they happened to be written in.
    let mut summaries: BTreeMap<TaskAddr, TaskSummary> = BTreeMap::new();

    for entry in applied {
        match &entry.op {
            Op::Place { task, column, rank } => {
                unplaced.remove(task);
                let placement = tasks.entry(*task).or_insert_with(|| Placement {
                    task: *task,
                    column: *column,
                    rank: rank.clone(),
                    summary: TaskSummary::default(),
                    placed_by: entry.author(),
                    placed_ms: entry.at(),
                });
                // A later `Place` is a move: total assignment, last writer wins.
                placement.column = *column;
                placement.rank = rank.clone();
            }
            Op::Unplace { task } => {
                tasks.remove(task);
                unplaced.insert(*task);
            }
            Op::Summarize { task, summary } => {
                let known = summaries.entry(*task).or_default();
                // Highest `seen_lamport` wins, which is what lets any client repair
                // any summary at any time without a stale write clobbering a fresh
                // one. `>=` rather than `>` so that two summaries read at the same
                // task clock still resolve by op order rather than sticking on
                // whichever the map happened to see first.
                if summary.seen_lamport >= known.seen_lamport {
                    *known = summary.clone();
                }
            }
            _ => {}
        }
    }

    for (task, summary) in summaries {
        if let Some(placement) = tasks.get_mut(&task) {
            placement.summary = summary;
        }
    }

    tasks
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;
    use crate::envelope::{Envelope, Scope, Stamp};

    /// The contract these tests stand in for. The fold does not look at it — only
    /// `validate` does, and cross-contract replay has its own tests in `envelope`.
    const HERE: Scope = Scope([3; 32]);

    struct Peer {
        key: SigningKey,
        id: MemberId,
        lamport: u64,
        nonce: u8,
    }

    impl Peer {
        fn new(seed: u8) -> Self {
            let key = SigningKey::from_bytes(&[seed; 32]);
            let id = MemberId(key.verifying_key().to_bytes());
            Self {
                key,
                id,
                lamport: 0,
                nonce: 0,
            }
        }

        fn stamp(&mut self, lamport: u64) -> Stamp {
            self.nonce = self.nonce.wrapping_add(1);
            Stamp::new(
                HERE,
                self.id,
                lamport,
                1_700_000_000_000 + lamport,
                [self.nonce; 16],
            )
        }

        fn sign(&mut self, op: Op) -> SignedEnvelope {
            self.lamport += 1;
            let stamp = self.stamp(self.lamport);
            op.envelope(stamp).sign(&self.key)
        }

        /// Signs at an explicit lamport, for building concurrent-edit scenarios.
        fn sign_at(&mut self, op: Op, lamport: u64) -> SignedEnvelope {
            let stamp = self.stamp(lamport);
            op.envelope(stamp).sign(&self.key)
        }

        fn grant(&mut self, member: MemberId, rights: Rights) -> SignedEnvelope {
            self.lamport += 1;
            self.grant_at(member, rights, self.lamport)
        }

        fn link(&mut self, device: MemberId) -> SignedEnvelope {
            self.lamport += 1;
            self.link_at(device, self.lamport)
        }

        fn unlink(&mut self, device: MemberId) -> SignedEnvelope {
            self.lamport += 1;
            self.unlink_at(device, self.lamport)
        }

        // Each peer keeps its own counter, so a second peer's first op sorts
        // *before* the grant that admitted it unless the test says otherwise.
        // Real clients read `next_lamport` off the board; tests say otherwise.

        fn grant_at(&mut self, member: MemberId, rights: Rights, lamport: u64) -> SignedEnvelope {
            let stamp = self.stamp(lamport);
            Envelope::grant(stamp, member, rights).sign(&self.key)
        }

        fn link_at(&mut self, device: MemberId, lamport: u64) -> SignedEnvelope {
            let stamp = self.stamp(lamport);
            Envelope::link_device(stamp, device).sign(&self.key)
        }

        fn unlink_at(&mut self, device: MemberId, lamport: u64) -> SignedEnvelope {
            let stamp = self.stamp(lamport);
            Envelope::unlink_device(stamp, device).sign(&self.key)
        }
    }

    const TODO: ColumnId = ColumnId([1; 16]);
    const DONE: ColumnId = ColumnId([2; 16]);
    const TASK: TaskAddr = TaskAddr([9; 32]);

    fn fold(ops: Vec<SignedEnvelope>, params: &BoardParameters) -> Board {
        Board::from_state(&EnvelopeState::from_ops(ops), params)
    }

    fn seeded_board(owner: &mut Peer) -> Vec<SignedEnvelope> {
        let todo_rank = Rank::middle();
        let done_rank = Rank::after(&todo_rank);
        vec![
            owner.sign(Op::SetColumn {
                column: TODO,
                title: "Todo".to_owned(),
                rank: todo_rank,
            }),
            owner.sign(Op::SetColumn {
                column: DONE,
                title: "Done".to_owned(),
                rank: done_rank,
            }),
        ]
    }

    fn create(rank: Rank) -> Op {
        Op::Place {
            task: TASK,
            column: TODO,
            rank,
        }
    }

    /// A card's text arrives separately from its position, so most fixtures need
    /// both.
    fn titled(task: TaskAddr, title: &str, seen_lamport: u64) -> Op {
        Op::Summarize {
            task,
            summary: TaskSummary {
                title: title.to_owned(),
                assignee: None,
                seen_lamport,
            },
        }
    }

    #[test]
    fn folds_columns_and_tasks_into_display_order() {
        let mut owner = Peer::new(1);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.sign(create(Rank::middle())));
        ops.push(owner.sign(titled(TASK, "Ship it", 1)));

        let board = fold(ops, &params);

        assert_eq!(
            board
                .columns
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Todo", "Done"]
        );
        assert_eq!(board.tasks_in(&TODO).len(), 1);
        assert_eq!(board.tasks_in(&DONE).len(), 0);
        assert_eq!(board.tasks[&TASK].title(), "Ship it");
        assert!(board.next_lamport > 3);
        assert_eq!(board.unreadable_ops, 0);
    }

    /// The convergence property, at the level a user would notice: two peers
    /// that received the same edits in opposite orders see the same board.
    #[test]
    fn peers_receiving_ops_in_opposite_orders_render_the_same_board() {
        let mut owner = Peer::new(1);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.sign(create(Rank::middle())));
        ops.push(owner.sign(titled(TASK, "Ship it properly", 2)));
        ops.push(owner.sign(Op::Place {
            task: TASK,
            column: DONE,
            rank: Rank::middle(),
        }));

        let forwards = fold(ops.clone(), &params);
        ops.reverse();
        let backwards = fold(ops, &params);

        assert_eq!(forwards, backwards);
        assert_eq!(forwards.tasks[&TASK].title(), "Ship it properly");
        assert_eq!(forwards.tasks[&TASK].column, DONE);
    }

    /// Two members moving one card concurrently: the higher lamport wins, and both
    /// peers agree on which that is.
    #[test]
    fn concurrent_edits_to_one_field_resolve_the_same_way_everywhere() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.grant(member.id, Rights::MEMBER));
        ops.push(owner.sign(create(Rank::middle())));

        ops.push(owner.sign_at(
            Op::Place {
                task: TASK,
                column: TODO,
                rank: Rank::middle(),
            },
            50,
        ));
        ops.push(member.sign_at(
            Op::Place {
                task: TASK,
                column: DONE,
                rank: Rank::middle(),
            },
            51,
        ));

        let a = fold(ops.clone(), &params);
        ops.reverse();
        let b = fold(ops, &params);

        assert_eq!(a, b);
        assert_eq!(a.tasks[&TASK].column, DONE, "higher lamport wins");
    }

    /// A summary read from a fresher task beats one read from a staler task,
    /// whichever order the two writes arrive in and whoever wrote them. This is
    /// what lets any client repair any card without coordinating.
    #[test]
    fn the_freshest_read_of_a_task_wins_the_summary() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.grant(member.id, Rights::MEMBER));
        ops.push(owner.sign(create(Rank::middle())));

        // The member writes later by the board's clock, but from an older read of
        // the task itself — so the owner's summary is the true one.
        ops.push(owner.sign_at(titled(TASK, "current", 9), 50));
        ops.push(member.sign_at(titled(TASK, "from a stale read", 4), 51));

        let a = fold(ops.clone(), &params);
        ops.reverse();
        let b = fold(ops, &params);

        assert_eq!(a, b);
        assert_eq!(a.tasks[&TASK].title(), "current");
    }

    /// A summary can be written by someone looking at another board, so it can
    /// arrive before the placement it describes — and must not be lost when it
    /// does.
    #[test]
    fn a_summary_that_arrives_before_its_placement_is_not_lost() {
        let mut owner = Peer::new(1);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.sign_at(titled(TASK, "described first", 3), 10));
        ops.push(owner.sign_at(create(Rank::middle()), 20));

        assert_eq!(fold(ops, &params).tasks[&TASK].title(), "described first");
    }

    /// A card with no summary yet is still something a person has to be able to
    /// click on.
    #[test]
    fn a_placement_without_a_summary_still_renders() {
        let mut owner = Peer::new(1);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.sign(create(Rank::middle())));

        let board = fold(ops, &params);
        assert_eq!(board.tasks[&TASK].title(), TASK.short());
    }

    /// Unplacing takes the card off the board for good: a concurrent summary with
    /// a higher lamport must not bring it back, or removal would not converge.
    #[test]
    fn unplacing_beats_a_concurrent_edit() {
        let mut owner = Peer::new(1);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.sign(create(Rank::middle())));
        ops.push(owner.sign_at(Op::Unplace { task: TASK }, 40));
        ops.push(owner.sign_at(titled(TASK, "back from the dead", 99), 99));

        let board = fold(ops, &params);
        assert!(!board.tasks.contains_key(&TASK));
    }

    /// …but unplacing is not deletion. Placing it again is an ordinary thing to
    /// do, and it comes back.
    #[test]
    fn a_task_can_be_placed_again_after_being_unplaced() {
        let mut owner = Peer::new(1);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.sign_at(create(Rank::middle()), 10));
        ops.push(owner.sign_at(Op::Unplace { task: TASK }, 20));
        ops.push(owner.sign_at(create(Rank::middle()), 30));

        assert!(fold(ops, &params).tasks.contains_key(&TASK));
    }

    #[test]
    fn ops_from_unauthorized_keys_are_ignored_by_the_fold() {
        let mut owner = Peer::new(1);
        let mut stranger = Peer::new(3);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(stranger.sign(create(Rank::middle())));

        let board = fold(ops, &params);
        assert!(
            board.tasks.is_empty(),
            "a validly signed op from a non-member must not render"
        );
    }

    /// The gap the contract cannot close: `needs` is written by the author. An op
    /// that understates what it requires is stored and then ignored.
    #[test]
    fn understating_what_an_op_needs_does_not_smuggle_it_past_the_fold() {
        let mut owner = Peer::new(1);
        let mut linker = Peer::new(4);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        // Allowed to link tasks, and nothing else.
        ops.push(owner.grant(linker.id, Rights::LINK_TASKS));

        // A hand-built envelope claiming to need only what they hold, carrying a
        // body that in fact needs WRITE_TASKS.
        let stamp = linker.stamp(40);
        let smuggled = Envelope::stamped(
            stamp,
            Rights::LINK_TASKS,
            kind::FIRST_APPLICATION_KIND,
            create(Rank::middle()).encode(),
        )
        .sign(&linker.key);

        // The contract would take it: the declared need is genuinely held.
        let state = EnvelopeState::from_ops({
            let mut all = ops.clone();
            all.push(smuggled);
            all
        });
        assert!(
            state.validate(HERE, params.owner).is_ok(),
            "stored, as designed"
        );

        // The fold is the layer that knows better.
        let board = Board::from_state(&state, &params);
        assert!(board.tasks.is_empty(), "and ignored, as designed");
    }

    #[test]
    fn removing_a_member_deactivates_them_without_hiding_their_work() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.grant(member.id, Rights::MEMBER));
        ops.push(owner.sign(Op::SetMemberName {
            member: member.id,
            name: "Sam".to_owned(),
        }));
        ops.push(member.sign(create(Rank::middle())));
        // Removal is a grant of nothing — no separate op kind for it.
        ops.push(owner.grant(member.id, Rights::NONE));

        let board = fold(ops, &params);

        assert!(!board.members[&member.id].active);
        assert_eq!(board.member_name(&member.id), "Sam", "name still resolves");
        assert_eq!(
            board.tasks.len(),
            1,
            "their task must survive their removal"
        );
        assert_eq!(board.active_members().len(), 1, "just the owner");
        assert!(!board.may(&member.id, Rights::WRITE_TASKS), "no longer");
    }

    /// A grant confers at most what the granter holds, so an admin cannot mint
    /// an admin.
    #[test]
    fn a_grant_is_capped_by_the_granter() {
        let mut owner = Peer::new(1);
        let mut admin = Peer::new(2);
        let hopeful = Peer::new(3);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        // An admin without ADMINISTER: may bring people in, may not rename the
        // board's organization.
        ops.push(owner.grant(admin.id, Rights::MEMBER.union(Rights::MAY_GRANT)));
        ops.push(admin.grant_at(hopeful.id, Rights::ALL, 20));

        let board = fold(ops, &params);

        let got = board.members[&hopeful.id].rights;
        assert!(got.contains(Rights::WRITE_TASKS));
        assert!(
            !got.contains(Rights::ADMINISTER),
            "cannot confer what you do not hold"
        );
        assert_eq!(board.members[&admin.id].role, Role::Admin);
        assert_eq!(
            board.members[&hopeful.id].role,
            Role::Member,
            "an admin invites people; only the owner decides who else may"
        );
    }

    /// You can always take yourself off a board, whatever you hold — otherwise
    /// leaving would be something only an admin could do on your behalf.
    #[test]
    fn a_member_may_resign_without_anybody_granting_them_that() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.grant(member.id, Rights::MEMBER));
        ops.push(member.sign_at(create(Rank::middle()), 20));
        ops.push(member.grant_at(member.id, Rights::NONE, 21));

        let board = fold(ops, &params);

        assert!(!board.members[&member.id].active, "gone");
        assert_eq!(board.tasks.len(), 1, "but their work stays");
    }

    /// The other half of the rule: renouncing is only ever about *yourself*.
    #[test]
    fn resigning_on_somebody_elses_behalf_is_not_a_thing() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);
        let other = Peer::new(3);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.grant(member.id, Rights::MEMBER));
        ops.push(owner.grant(other.id, Rights::MEMBER));
        ops.push(member.grant_at(other.id, Rights::NONE, 30));

        let board = fold(ops, &params);
        assert!(board.members[&other.id].active, "not their call");
    }

    #[test]
    fn a_member_may_rename_themselves_but_not_anybody_else() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);
        let other = Peer::new(3);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.grant(member.id, Rights::MEMBER));
        ops.push(owner.grant(other.id, Rights::MEMBER));
        ops.push(member.sign(Op::SetMemberName {
            member: member.id,
            name: "Sam".to_owned(),
        }));
        ops.push(member.sign(Op::SetMemberName {
            member: other.id,
            name: "Impostor".to_owned(),
        }));
        // The owner holds ADMINISTER, so their rename lands.
        ops.push(owner.sign(Op::SetMemberName {
            member: other.id,
            name: "Kim".to_owned(),
        }));

        let board = fold(ops, &params);
        assert_eq!(board.members[&member.id].name, "Sam");
        assert_eq!(board.members[&other.id].name, "Kim");
    }

    /// Work done from a linked device is attributed to the person, not to an
    /// anonymous second key.
    #[test]
    fn a_linked_device_renders_as_the_person_who_linked_it() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);
        let mut laptop = Peer::new(9);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.grant(member.id, Rights::MEMBER));
        ops.push(owner.sign(Op::SetMemberName {
            member: member.id,
            name: "Sam".to_owned(),
        }));
        ops.push(member.link_at(laptop.id, 20));
        // The task is created from the laptop, not from Sam's original key.
        ops.push(laptop.sign_at(create(Rank::middle()), 21));

        let board = fold(ops, &params);

        assert_eq!(board.tasks.len(), 1, "the device's write must render");
        assert_eq!(board.person_of(&laptop.id), member.id);
        assert_eq!(
            board.member_name(&laptop.id),
            "Sam",
            "a device is attributed to its person"
        );
        assert_eq!(board.devices_of(&member.id), vec![laptop.id]);
        assert_eq!(
            board.active_members().len(),
            2,
            "a device is not a separate member"
        );
        assert!(board.may(&laptop.id, Rights::WRITE_TASKS), "acts as Sam");
    }

    /// The owner's second browser must be able to run the board, including
    /// inviting people.
    #[test]
    fn an_owner_device_has_the_owners_authority() {
        let mut owner = Peer::new(1);
        let mut laptop = Peer::new(9);
        let invitee = Peer::new(2);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.link(laptop.id));
        ops.push(laptop.grant_at(invitee.id, Rights::MEMBER, 20));

        let board = fold(ops, &params);

        assert!(
            board.members[&invitee.id].active,
            "an invite from the owner's device must count"
        );
        assert_eq!(board.person_of(&laptop.id), owner.id);
    }

    /// Two peers that received the linking ops in opposite orders must agree.
    #[test]
    fn device_resolution_is_order_independent() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);
        let mut laptop = Peer::new(9);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.grant(member.id, Rights::MEMBER));
        ops.push(member.link(laptop.id));
        ops.push(laptop.sign(create(Rank::middle())));

        let forwards = fold(ops.clone(), &params);
        ops.reverse();
        let backwards = fold(ops, &params);
        assert_eq!(forwards, backwards);
    }

    /// A device can detach itself, and its voucher can detach it; nobody else can.
    #[test]
    fn unlinking_a_device_takes_the_voucher_or_the_device_itself() {
        let mut owner = Peer::new(1);
        let mut laptop = Peer::new(9);
        let mut member = Peer::new(2);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        // An outsider's attempt changes nothing.
        let mut ops = seeded_board(&mut owner);
        ops.push(owner.grant(member.id, Rights::MEMBER));
        ops.push(owner.link(laptop.id));
        ops.push(member.unlink_at(laptop.id, 20));
        let board = fold(ops, &params);
        assert_eq!(board.person_of(&laptop.id), owner.id, "not their call");

        // The device detaching itself works.
        let mut ops = seeded_board(&mut owner);
        ops.push(owner.link(laptop.id));
        ops.push(laptop.unlink_at(laptop.id, 30));
        let board = fold(ops, &params);
        assert!(board.devices_of(&owner.id).is_empty());

        // And so does the voucher revoking it.
        let mut ops = seeded_board(&mut owner);
        ops.push(owner.link(laptop.id));
        ops.push(owner.unlink(laptop.id));
        let board = fold(ops, &params);
        assert!(board.devices_of(&owner.id).is_empty());
    }

    #[test]
    fn removing_a_column_rehomes_its_tasks_instead_of_losing_them() {
        let mut owner = Peer::new(1);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.sign(create(Rank::middle())));
        ops.push(owner.sign(Op::RemoveColumn { column: TODO }));

        let board = fold(ops, &params);

        assert_eq!(board.columns.len(), 1);
        assert_eq!(board.tasks[&TASK].column, DONE, "rehomed, not dropped");
    }

    #[test]
    fn rank_for_drop_places_a_card_between_its_new_neighbours() {
        let mut owner = Peer::new(1);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        let first = Rank::middle();
        let second = Rank::after(&first);
        ops.push(owner.sign(Op::Place {
            task: TaskAddr([1; 32]),
            column: TODO,
            rank: first.clone(),
        }));
        ops.push(owner.sign(Op::Place {
            task: TaskAddr([2; 32]),
            column: TODO,
            rank: second.clone(),
        }));

        let board = fold(ops, &params);

        let between = board.rank_for_drop(&TODO, 1, None);
        assert!(first < between && between < second);
        assert!(
            board.rank_for_drop(&TODO, 0, None) < first,
            "drop at the top"
        );
        assert!(
            board.rank_for_drop(&TODO, 2, None) > second,
            "drop at the bottom"
        );
    }

    /// Reordering within a column: dragging the first card into second place has
    /// to actually move it, which only works if it is excluded from the
    /// neighbour lookup.
    #[test]
    fn reordering_within_a_column_excludes_the_dragged_card() {
        let mut owner = Peer::new(1);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        let first_rank = Rank::middle();
        let second_rank = Rank::after(&first_rank);
        let dragged = TaskAddr([1; 32]);
        ops.push(owner.sign(Op::Place {
            task: dragged,
            column: TODO,
            rank: first_rank.clone(),
        }));
        ops.push(owner.sign(Op::Place {
            task: TaskAddr([2; 32]),
            column: TODO,
            rank: second_rank.clone(),
        }));

        let board = fold(ops, &params);

        // Drop the first card below the second: index 1 among the *other* cards.
        let moved = board.rank_for_drop(&TODO, 1, Some(dragged));
        assert!(
            moved > second_rank,
            "dragged card must land after the card it was dropped past"
        );

        // Without excluding it, the same request would put it back where it was.
        let stuck = board.rank_for_drop(&TODO, 1, None);
        assert!(stuck < second_rank, "demonstrates why exclude matters");
    }

    /// The whole reason for the opaque body: a board written by a newer client
    /// still opens, still renders what this build understands, and still carries
    /// what it does not.
    #[test]
    fn an_op_from_a_newer_client_is_counted_and_carried_but_not_rendered() {
        let mut owner = Peer::new(1);
        let params = BoardParameters::new(owner.id, "board", [0; 16]);

        let mut ops = seeded_board(&mut owner);
        ops.push(owner.sign(create(Rank::middle())));

        // Some kind this build has never heard of.
        let stamp = owner.stamp(70);
        let future = Envelope::stamped(
            stamp,
            Rights::WRITE_TASKS,
            kind::FIRST_APPLICATION_KIND + 9,
            b"whatever comes next".to_vec(),
        )
        .sign(&owner.key);
        ops.push(future.clone());

        let state = EnvelopeState::from_ops(ops);
        let board = Board::from_state(&state, &params);

        assert_eq!(board.tasks.len(), 1, "the readable part still renders");
        assert_eq!(board.unreadable_ops, 1, "and the rest is counted, not lost");
        assert!(
            state.ops.contains_key(&future.id()),
            "the bytes survive to be pushed on"
        );
        assert!(state.validate(HERE, params.owner).is_ok());
    }
}

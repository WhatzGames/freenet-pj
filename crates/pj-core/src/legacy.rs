//! Reading tasks written before they had contracts of their own.
//!
//! A board used to hold its tasks: `CreateTask`, `SetTaskTitle` and the rest were
//! ops in the board's own state. They are gone from [`crate::op::Op`], so a board
//! written by the old build now decodes as columns and members plus a pile of ops
//! this build cannot read — the cards, invisible.
//!
//! They are not lost, though. Nothing is ever deleted from an op set, and the
//! board contract never understood these ops in the first place; it stored bytes.
//! So the old variants can be decoded here, from exactly the same bytes, and
//! turned into real tasks.
//!
//! # Why this is a decoder and not a second `Op`
//!
//! Only the variants needed to reconstruct a card are here, in their original
//! positions — `bincode` encodes a variant as its index, so the order is the
//! format and nothing may be inserted or removed. Everything after `DeleteTask` is
//! `Other`, because this needs to recognise the task ops and nothing else.
//!
//! This module is temporary by design. Once the boards that matter have been
//! converted it is dead weight, and leaving it in place would mean carrying a
//! second definition of what an op is for no reason. Delete it then.

use serde::{Deserialize, Serialize};

use crate::envelope::kind;
use crate::envelope_state::EnvelopeState;
use crate::ids::{BoardId, ColumnId, MemberId, OrgId, TaskId};
use crate::link::LinkKind;
use crate::rank::Rank;

/// The task-carrying variants of the old op enum, in their original order.
///
/// `Other` swallows every variant from `LinkTask` on. Links are not migrated: they
/// named `(board, task)` pairs whose task ids no longer address anything, and a
/// link that cannot be resolved is worse than one that is simply absent.
#[derive(Clone, Debug, Serialize, Deserialize)]
enum LegacyOp {
    SetColumn {
        column: ColumnId,
        title: String,
        rank: Rank,
    },
    RemoveColumn {
        column: ColumnId,
    },
    CreateTask {
        task: TaskId,
        column: ColumnId,
        title: String,
        rank: Rank,
    },
    SetTaskTitle {
        task: TaskId,
        title: String,
    },
    SetTaskDescription {
        task: TaskId,
        description: String,
    },
    MoveTask {
        task: TaskId,
        column: ColumnId,
        rank: Rank,
    },
    SetTaskAssignee {
        task: TaskId,
        assignee: Option<MemberId>,
    },
    DeleteTask {
        task: TaskId,
    },
    /// Declared faithfully rather than skipped, because a variant's *index* is its
    /// identity: getting the shape wrong here would silently shift everything
    /// after it. Links themselves are not migrated — they named task ids that no
    /// longer address anything.
    LinkTask {
        from: TaskId,
        to: LegacyTaskRef,
        kind: LinkKind,
    },
    UnlinkTask {
        from: TaskId,
        to: LegacyTaskRef,
    },
    /// These two are the reason the board's *members* went missing after the
    /// split, not just its cards: removing seven variants from the middle of the
    /// enum renumbered them, so an old board's names and organization stopped
    /// decoding along with everything else.
    SetOrganization {
        org: OrgId,
        name: String,
    },
    SetMemberName {
        member: MemberId,
        name: String,
    },
    #[serde(other)]
    Other,
}

/// The old cross-board task reference. Only here to give `LinkTask` its shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegacyTaskRef {
    board: Option<BoardId>,
    task: TaskId,
}

/// A card recovered from a board written by the old build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyTask {
    pub id: TaskId,
    pub title: String,
    pub description: String,
    pub column: ColumnId,
    pub rank: Rank,
    pub assignee: Option<MemberId>,
}

/// Recovers every card still readable in a board's op set.
///
/// Folded the way the old board folded them — creations first, then edits in
/// order, then deletions — so what comes out is what the board last showed rather
/// than a pile of half-applied history.
///
/// Deliberately no rights check. The ops were validated when they were written,
/// and re-judging them now against a membership list that has moved on would drop
/// work whose author has since left. The caller is migrating a board they can
/// already read.
pub fn recover_tasks(state: &EnvelopeState) -> Vec<LegacyTask> {
    let mut tasks: Vec<LegacyTask> = Vec::new();
    let mut deleted: Vec<TaskId> = Vec::new();

    let ops: Vec<LegacyOp> = state
        .ordered()
        .into_iter()
        .filter(|envelope| envelope.payload.kind == kind::FIRST_APPLICATION_KIND)
        .filter_map(|envelope| bincode::deserialize(&envelope.payload.body).ok())
        .collect();

    for op in &ops {
        if let LegacyOp::CreateTask {
            task,
            column,
            title,
            rank,
        } = op
            && !tasks.iter().any(|existing| existing.id == *task)
        {
            tasks.push(LegacyTask {
                id: *task,
                title: title.clone(),
                description: String::new(),
                column: *column,
                rank: rank.clone(),
                assignee: None,
            });
        }
    }

    for op in &ops {
        let find = |tasks: &mut Vec<LegacyTask>, id: &TaskId| -> Option<usize> {
            tasks.iter().position(|task| task.id == *id)
        };
        match op {
            LegacyOp::SetTaskTitle { task, title } => {
                if let Some(at) = find(&mut tasks, task) {
                    tasks[at].title.clone_from(title);
                }
            }
            LegacyOp::SetTaskDescription { task, description } => {
                if let Some(at) = find(&mut tasks, task) {
                    tasks[at].description.clone_from(description);
                }
            }
            LegacyOp::MoveTask { task, column, rank } => {
                if let Some(at) = find(&mut tasks, task) {
                    tasks[at].column = *column;
                    tasks[at].rank = rank.clone();
                }
            }
            LegacyOp::SetTaskAssignee { task, assignee } => {
                if let Some(at) = find(&mut tasks, task) {
                    tasks[at].assignee = *assignee;
                }
            }
            LegacyOp::DeleteTask { task } => deleted.push(*task),
            _ => {}
        }
    }

    tasks.retain(|task| !deleted.contains(&task.id));
    tasks
}

/// Member names written by the old build, which no longer decode on their own.
///
/// Recovered so that converting a board restores who everybody is, rather than
/// leaving a project of key prefixes.
pub fn recover_names(state: &EnvelopeState) -> Vec<(MemberId, String)> {
    let mut names: Vec<(MemberId, String)> = Vec::new();
    for op in decode_all(state) {
        if let LegacyOp::SetMemberName { member, name } = op {
            // Later wins, as the old fold had it.
            match names.iter_mut().find(|(id, _)| *id == member) {
                Some(entry) => entry.1 = name,
                None => names.push((member, name)),
            }
        }
    }
    names
}

/// The organization an old board belonged to.
pub fn recover_organization(state: &EnvelopeState) -> Option<(OrgId, String)> {
    decode_all(state).into_iter().rev().find_map(|op| match op {
        LegacyOp::SetOrganization { org, name } => Some((org, name)),
        _ => None,
    })
}

/// The body of an op that retires an old card, in the encoding the old build
/// used.
///
/// Written when a card is converted, and the reason a board can be converted only
/// once: [`recover_tasks`] honours these tombstones, so after conversion the old
/// card is gone from the recovery just as it would have been from the old board.
/// Without it the ops stay in the state — nothing is ever deleted — and every
/// visit would offer to convert the same cards again, duplicating them.
///
/// A tombstone rather than client-side bookkeeping because it has to hold for
/// *every* client, including one opening the board for the first time.
pub fn tombstone(task: TaskId) -> Vec<u8> {
    bincode::serialize(&LegacyOp::DeleteTask { task }).expect("LegacyOp is always serializable")
}

fn decode_all(state: &EnvelopeState) -> Vec<LegacyOp> {
    state
        .ordered()
        .into_iter()
        .filter(|envelope| envelope.payload.kind == kind::FIRST_APPLICATION_KIND)
        .filter_map(|envelope| bincode::deserialize(&envelope.payload.body).ok())
        .collect()
}

/// How many ops in this state are old task ops.
///
/// Distinct from `recover_tasks(..).len()`, and the distinction matters: one card
/// is several ops — a creation, a retitle, a move — so a caller that subtracts a
/// count of *cards* from a count of unreadable *ops* is comparing different
/// things. That mistake showed up as a board reporting phantom entries "written by
/// a newer version" which were in fact its own old cards.
pub fn legacy_op_count(state: &EnvelopeState) -> usize {
    state
        .ordered()
        .into_iter()
        .filter(|envelope| envelope.payload.kind == kind::FIRST_APPLICATION_KIND)
        .filter(|envelope| {
            bincode::deserialize::<LegacyOp>(&envelope.payload.body).is_ok_and(|op| {
                // Only the *task* variants. `SetColumn` and `RemoveColumn` sit at
                // the same indices with the same shape in the current enum, so
                // counting them would report every ordinary column op on every
                // board as legacy.
                matches!(
                    op,
                    LegacyOp::CreateTask { .. }
                        | LegacyOp::SetTaskTitle { .. }
                        | LegacyOp::SetTaskDescription { .. }
                        | LegacyOp::MoveTask { .. }
                        | LegacyOp::SetTaskAssignee { .. }
                        | LegacyOp::DeleteTask { .. }
                        | LegacyOp::LinkTask { .. }
                        | LegacyOp::UnlinkTask { .. }
                        // Renumbered by the split, so these no longer decode as
                        // themselves either — and a board reporting them as
                        // "written by a newer version" was exactly backwards.
                        | LegacyOp::SetOrganization { .. }
                        | LegacyOp::SetMemberName { .. }
                )
            })
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, Scope, SignedEnvelope, Stamp};
    use crate::ids::MemberId;
    use crate::rights::Rights;
    use ed25519_dalek::SigningKey;

    const HERE: Scope = Scope([1; 32]);
    const TODO: ColumnId = ColumnId([1; 16]);
    const DONE: ColumnId = ColumnId([2; 16]);
    const TASK: TaskId = TaskId([9; 16]);

    /// Writes an op the way the old build did: the same envelope, the same kind,
    /// the same body encoding.
    fn old(key: &SigningKey, author: MemberId, lamport: u64, op: &LegacyOp) -> SignedEnvelope {
        // Truncation is the point: the nonce only has to distinguish ops within
        // one test, and every test's lamports are small.
        let nonce = lamport.to_le_bytes()[0];
        let stamp = Stamp::new(HERE, author, lamport, 0, [nonce; 16]);
        Envelope::stamped(
            stamp,
            Rights::WRITE_TASKS,
            kind::FIRST_APPLICATION_KIND,
            bincode::serialize(op).expect("the fixture serializes"),
        )
        .sign(key)
    }

    fn peer() -> (SigningKey, MemberId) {
        let key = SigningKey::from_bytes(&[1; 32]);
        let id = MemberId(key.verifying_key().to_bytes());
        (key, id)
    }

    #[test]
    fn a_card_is_recovered_with_its_last_title_column_and_assignee() {
        let (key, me) = peer();
        let state = EnvelopeState::from_ops(vec![
            old(
                &key,
                me,
                1,
                &LegacyOp::CreateTask {
                    task: TASK,
                    column: TODO,
                    title: "Ship it".to_owned(),
                    rank: Rank::middle(),
                },
            ),
            old(
                &key,
                me,
                2,
                &LegacyOp::SetTaskDescription {
                    task: TASK,
                    description: "with notes".to_owned(),
                },
            ),
            old(
                &key,
                me,
                3,
                &LegacyOp::MoveTask {
                    task: TASK,
                    column: DONE,
                    rank: Rank::middle(),
                },
            ),
            old(
                &key,
                me,
                4,
                &LegacyOp::SetTaskAssignee {
                    task: TASK,
                    assignee: Some(me),
                },
            ),
        ]);

        let recovered = recover_tasks(&state);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].title, "Ship it");
        assert_eq!(recovered[0].description, "with notes");
        assert_eq!(recovered[0].column, DONE);
        assert_eq!(recovered[0].assignee, Some(me));
    }

    #[test]
    fn a_deleted_card_stays_deleted() {
        let (key, me) = peer();
        let state = EnvelopeState::from_ops(vec![
            old(
                &key,
                me,
                1,
                &LegacyOp::CreateTask {
                    task: TASK,
                    column: TODO,
                    title: "Ship it".to_owned(),
                    rank: Rank::middle(),
                },
            ),
            old(&key, me, 2, &LegacyOp::DeleteTask { task: TASK }),
        ]);
        assert!(recover_tasks(&state).is_empty());
    }

    /// A board with nothing old in it must migrate to nothing, or opening an
    /// ordinary board would offer to convert it.
    #[test]
    fn a_board_with_no_legacy_tasks_recovers_nothing() {
        let (key, me) = peer();
        let state = EnvelopeState::from_ops(vec![old(
            &key,
            me,
            1,
            &LegacyOp::SetColumn {
                column: TODO,
                title: "Todo".to_owned(),
                rank: Rank::middle(),
            },
        )]);
        assert!(recover_tasks(&state).is_empty());
        assert_eq!(
            legacy_op_count(&state),
            0,
            "a column op is not a task op, and shares an index with the current one"
        );
    }

    /// A converted card must stop being offered for conversion — to every client,
    /// not just the one that converted it. Without this a second visit would
    /// duplicate every card on the board.
    #[test]
    fn a_tombstone_retires_a_converted_card_for_everyone() {
        let (key, me) = peer();
        let create = old(
            &key,
            me,
            1,
            &LegacyOp::CreateTask {
                task: TASK,
                column: TODO,
                title: "converted already".to_owned(),
                rank: Rank::middle(),
            },
        );
        let state = EnvelopeState::from_ops(vec![create.clone()]);
        assert_eq!(recover_tasks(&state).len(), 1, "offered before conversion");

        // The tombstone as `Store::retire_legacy` writes it: an ordinary envelope
        // carrying the old encoding.
        let stamp = Stamp::new(HERE, me, 2, 0, [2; 16]);
        let retired = Envelope::stamped(
            stamp,
            Rights::WRITE_TASKS,
            kind::FIRST_APPLICATION_KIND,
            tombstone(TASK),
        )
        .sign(&key);

        let state = EnvelopeState::from_ops(vec![create, retired]);
        assert!(
            recover_tasks(&state).is_empty(),
            "a converted card must not be offered again"
        );
    }

    /// Member names and the organization were renumbered out of readability by the
    /// same change that hid the cards, so conversion has to carry them too.
    #[test]
    fn names_and_the_organization_are_recoverable() {
        let (key, me) = peer();
        let state = EnvelopeState::from_ops(vec![
            old(
                &key,
                me,
                1,
                &LegacyOp::SetMemberName {
                    member: me,
                    name: "Sam".to_owned(),
                },
            ),
            old(
                &key,
                me,
                2,
                &LegacyOp::SetMemberName {
                    member: me,
                    name: "Sam again".to_owned(),
                },
            ),
            old(
                &key,
                me,
                3,
                &LegacyOp::SetOrganization {
                    org: OrgId([6; 32]),
                    name: "Acme".to_owned(),
                },
            ),
        ]);

        assert_eq!(
            recover_names(&state),
            vec![(me, "Sam again".to_owned())],
            "later wins, as the old fold had it"
        );
        assert_eq!(
            recover_organization(&state),
            Some((OrgId([6; 32]), "Acme".to_owned()))
        );
        // And they count as old ops, so the board does not report them as coming
        // from the future.
        assert_eq!(legacy_op_count(&state), 3);
    }

    /// Cards and ops are counted separately because they are different things:
    /// one card here is three ops, and conflating them made a board claim it held
    /// entries from the future that were really its own past.
    #[test]
    fn ops_are_counted_separately_from_the_cards_they_make() {
        let (key, me) = peer();
        let state = EnvelopeState::from_ops(vec![
            old(
                &key,
                me,
                1,
                &LegacyOp::CreateTask {
                    task: TASK,
                    column: TODO,
                    title: "one card".to_owned(),
                    rank: Rank::middle(),
                },
            ),
            old(
                &key,
                me,
                2,
                &LegacyOp::SetTaskDescription {
                    task: TASK,
                    description: "…written over three ops".to_owned(),
                },
            ),
            old(
                &key,
                me,
                3,
                &LegacyOp::MoveTask {
                    task: TASK,
                    column: DONE,
                    rank: Rank::middle(),
                },
            ),
        ]);

        assert_eq!(recover_tasks(&state).len(), 1);
        assert_eq!(legacy_op_count(&state), 3);
    }

    /// The new ops must not be mistaken for old ones. `Place` sits where
    /// `CreateTask` used to, and a `TaskAddr` is twice the width of a `TaskId`, so
    /// a decode that succeeded anyway would produce nonsense cards.
    #[test]
    fn ops_from_the_current_build_are_not_read_as_legacy_tasks() {
        use crate::ids::TaskAddr;
        use crate::op::Op;

        let (key, me) = peer();
        let state = EnvelopeState::from_ops(vec![{
            let stamp = Stamp::new(HERE, me, 1, 0, [1; 16]);
            Op::Place {
                task: TaskAddr([7; 32]),
                column: TODO,
                rank: Rank::middle(),
            }
            .envelope(stamp)
            .sign(&key)
        }]);

        assert!(recover_tasks(&state).is_empty());
    }
}

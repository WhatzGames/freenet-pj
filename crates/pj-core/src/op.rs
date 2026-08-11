//! What a board op *says*, as distinct from what a contract checks.
//!
//! Every one of these travels inside an [`Envelope`] as
//! an opaque body under [`kind::FIRST_APPLICATION_KIND`]. The contract never
//! decodes them; it checks that the author holds the rights the envelope declares
//! and stores the bytes. So **adding a variant here rebuilds no contract and moves
//! no address** — the whole reason the enum lives on this side of the line.
//!
//! Membership is not here. Granting rights, and linking or unlinking a device, are
//! *authority* ops with their own kinds, because they are the only things a
//! contract has to understand in order to police the rest. Keeping them out of this
//! enum is what stops a rename from travelling down the authority path.
//!
//! # Ordering still matters, for a smaller reason than before
//!
//! `bincode` encodes a variant as a positional index, so inserting one in the
//! middle silently reinterprets stored ops. Append. The blast radius is now a board
//! that renders some ops as unknown rather than a board that fails to decode, but
//! it is still wrong.

use serde::{Deserialize, Serialize};

use crate::envelope::{Envelope, Stamp, kind};
use crate::error::{Error, Result};
use crate::ids::{ColumnId, MemberId, OrgId, TaskAddr};
use crate::rank::Rank;
use crate::rights::Rights;
use crate::task::TaskSummary;

/// A single change to a board.
///
/// Every variant is an idempotent assignment rather than a relative change
/// ("set the title to X", never "append X to the title"), so applying the same
/// op twice is a no-op and applying two ops in either order lands on the same
/// place once last-writer-wins picks between them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    /// Create or rename/reorder a column. One op covers both because the
    /// assignment is total.
    SetColumn {
        column: ColumnId,
        title: String,
        rank: Rank,
    },
    RemoveColumn {
        column: ColumnId,
    },

    /// Puts a task on this board, in a column, at a rank.
    ///
    /// The task itself lives at its own address and is not created by this — see
    /// [`crate::task`]. A placement is a reference plus a position, and one op
    /// covers both creating and moving it because the assignment is total.
    ///
    /// **Status lives here, not on the task.** The same task can be in `Doing` on
    /// one board and `Done` on another, and both are true. It also means moving a
    /// card takes rights on this board and nothing else, which is what makes "you
    /// may only change status on boards you have access to" a rule a contract can
    /// actually enforce — a board contract cannot read a task, and a task contract
    /// cannot read a board.
    Place {
        task: TaskAddr,
        column: ColumnId,
        rank: Rank,
    },

    /// Takes a card off this board.
    ///
    /// The task survives, keeps its address, and can be placed elsewhere. There is
    /// no delete: nothing on the network is ever really removed, and a task on no
    /// board is still reachable by anyone holding its link.
    Unplace {
        task: TaskAddr,
    },

    /// Refreshes the cached summary a card renders from.
    ///
    /// Denormalised on purpose: a board holds no task bodies, so without this a
    /// card could show nothing until it was opened. [`TaskSummary::seen_lamport`]
    /// is what keeps the copy honest — the fold keeps the highest per task, so a
    /// summary written from a stale read loses to a fresher one however they
    /// arrive, and any client may repair any summary at any time.
    Summarize {
        task: TaskAddr,
        summary: TaskSummary,
    },

    /// Records which organization owns this project.
    ///
    /// Display and navigation only — authority still comes from the owner key in
    /// the parameters, which for an org-owned board *is* the organization's founder
    /// key. Keeping this an op rather than a parameter is what lets an existing
    /// board join an organization without changing its address.
    SetOrganization {
        org: OrgId,
        name: String,
    },

    /// What to call somebody on this board.
    ///
    /// A name is presentation, so it is an ordinary op rather than part of a grant.
    /// That separation is the point: renaming yourself is not an exercise of
    /// authority and must not need the right to hand out authority.
    SetMemberName {
        member: MemberId,
        name: String,
    },
}

impl Op {
    /// The rights an author must hold for this op to count.
    ///
    /// The contract checks the envelope's *declared* `needs`, which an author
    /// controls; the fold checks this, which they do not. So an author holding only
    /// [`Rights::LINK_TASKS`] cannot smuggle a `CreateTask` through by declaring a
    /// modest `needs` — the envelope would be stored and then ignored.
    pub fn needs(&self) -> Rights {
        match self {
            Op::SetColumn { .. } | Op::RemoveColumn { .. } => Rights::WRITE_COLUMNS,

            // Placing, moving and unplacing are the whole of "status" on a board.
            //
            // `Summarize` shares the right deliberately, and is not given a lesser
            // one: a summary is displayed as the card's title, so writing one is
            // deciding what a card says.
            Op::Place { .. } | Op::Unplace { .. } | Op::Summarize { .. } => Rights::WRITE_TASKS,

            Op::SetOrganization { .. } => Rights::ADMINISTER,

            // Enough to name *yourself*. Naming anyone else additionally takes
            // `ADMINISTER`, which the fold checks — it cannot be expressed here,
            // because it depends on who is being named.
            Op::SetMemberName { .. } => Rights::SET_NAME,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("Op is always serializable")
    }

    /// Wraps this op in the envelope that will carry it, declaring exactly the
    /// rights it needs. Sign the result.
    ///
    /// Takes `self`: from here on the op exists as bytes in the envelope, and a
    /// caller still holding the typed value would have two things that have to
    /// agree. Clone first in the rare case you want both.
    pub fn envelope(self, stamp: Stamp) -> Envelope {
        Envelope::stamped(
            stamp,
            self.needs(),
            kind::FIRST_APPLICATION_KIND,
            self.encode(),
        )
    }

    /// Decodes an op body.
    ///
    /// Fails for a variant this build has never heard of — which is not a
    /// problem, because the envelope carrying it is kept either way. A newer op
    /// reads as one this client cannot render, not as a board it cannot open.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("board op", e))
    }
}

/// A write waiting for a clock.
///
/// Genesis is a mixture: naming people and laying out columns are ordinary ops,
/// while seeding an organization's admins is a grant, and all of them have to be
/// stamped with consecutive lamports and land in one state. This is the smallest
/// thing that lets a caller build that list before knowing the numbers.
pub enum Draft {
    Op(Op),
    Grant { member: MemberId, rights: Rights },
}

impl Draft {
    pub fn envelope(self, stamp: Stamp) -> Envelope {
        match self {
            Draft::Op(op) => op.envelope(stamp),
            Draft::Grant { member, rights } => Envelope::grant(stamp, member, rights),
        }
    }
}

impl From<Op> for Draft {
    fn from(op: Op) -> Self {
        Draft::Op(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_declares_something_it_needs() {
        let ops = [
            Op::SetColumn {
                column: ColumnId([1; 16]),
                title: "Todo".to_owned(),
                rank: Rank::middle(),
            },
            Op::Unplace {
                task: TaskAddr([1; 32]),
            },
            Op::Summarize {
                task: TaskAddr([1; 32]),
                summary: TaskSummary::default(),
            },
            Op::SetOrganization {
                org: OrgId([1; 32]),
                name: "Acme".to_owned(),
            },
            Op::SetMemberName {
                member: MemberId([1; 32]),
                name: "Sam".to_owned(),
            },
        ];
        for op in ops {
            assert!(!op.needs().is_empty(), "{op:?} must require something");
            // An ordinary member can do all of these to themselves or to shared
            // structure; only the organization is an administrator's call.
            let expected_of_a_member = !matches!(op, Op::SetOrganization { .. });
            assert_eq!(
                Rights::MEMBER.contains(op.needs()),
                expected_of_a_member,
                "{op:?}"
            );
        }
    }

    #[test]
    fn an_op_round_trips_through_its_body_encoding() {
        let op = Op::Place {
            task: TaskAddr([7; 32]),
            column: ColumnId([2; 16]),
            rank: Rank::middle(),
        };
        assert_eq!(
            Op::decode(&op.encode()).expect("must decode: this test produced the bytes"),
            op
        );
    }

    /// The property that makes an unknown op survivable: it fails to *decode*
    /// here, and that is a local failure, not a failure of the state around it.
    #[test]
    fn a_body_this_build_cannot_read_is_an_error_and_not_a_panic() {
        assert!(Op::decode(&[250, 250, 250]).is_err());
        assert!(Op::decode(&[]).is_err());
    }
}

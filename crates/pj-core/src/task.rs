//! A task, as a contract of its own.
//!
//! # Why a task is not part of a board
//!
//! It used to be: a task was a handful of ops inside its board's state. That made
//! a task something a board *contained*, so it could not sit on two boards, could
//! not move between them, and could not outlive one.
//!
//! Giving it its own address fixes all three, at the cost that a board no longer
//! holds the thing it displays. The cost is paid by fetching a task's body only
//! when someone opens it: a board render is one fetch, plus one per opened card.
//! Without that, a fifty-card board would be fifty-one fetches and this design
//! would not be worth having.
//!
//! # The split, and where each field lives
//!
//! What the task owns: title, description, assignee, links, and the set of boards
//! it is on.
//!
//! What it deliberately does *not* own: **status**. Which column a card sits in is
//! a property of the placement on a board, not of the task, so the same task can be
//! in `Doing` on one board and `Done` on another — both true. It also means moving
//! a card needs rights on that board and touches this contract not at all, which is
//! what keeps "you may only change status on boards you have access to" enforceable
//! by a contract that cannot read the board.
//!
//! The board additionally caches a [`TaskSummary`] on each placement so a card can
//! render before anything is fetched. That is denormalised on purpose, and
//! [`TaskSummary::seen_lamport`] is what keeps it honest — see its docs.
//!
//! # Who may write here
//!
//! Body edits are open to the whole organization, including people who join after
//! the task was made. No snapshot of membership can do that, so authority comes
//! from org-scoped certificates instead — see
//! [`crate::envelope_state::Org`]. A task on a personal board has no org, and then
//! only its creator and whoever they grant may write.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::envelope::{Envelope, Scope, Stamp, kind};
use crate::envelope_state::{Authority, EnvelopeState, Org, Trust};
use crate::error::{Error, Result};
use crate::ids::{BoardId, MemberId, OrgId, TaskAddr};
use crate::link::LinkKind;
use crate::rights::Rights;

/// The organization a task belongs to, as its parameters record it.
///
/// All three fields are needed and none is derivable from the others: `scope` is
/// what a certificate is signed against, `founder` is the root of the chain, and
/// `id` is the address — the only one of the three you can navigate to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskOrg {
    pub id: OrgId,
    pub scope: Scope,
    pub founder: MemberId,
}

/// The parameters a task contract instance is created with.
///
/// Hashed into the address along with the code, so none of it can change. In
/// particular the organization cannot: a task cannot be moved between orgs, only
/// copied. That is the price of having membership be checkable without reading
/// another contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskParameters {
    /// Who made it. Holds [`Rights::ALL`] here, as an owner does anywhere.
    pub creator: MemberId,
    /// Whose members may edit the body, if any.
    pub org: Option<TaskOrg>,
    /// Creation time. Present so that two tasks made by one person in one
    /// millisecond still differ — together with `nonce`, which is what actually
    /// guarantees it.
    pub created_ms: u64,
    pub nonce: [u8; 16],
}

impl TaskParameters {
    pub fn new(creator: MemberId, org: Option<TaskOrg>, created_ms: u64, nonce: [u8; 16]) -> Self {
        Self {
            creator,
            org,
            created_ms,
            nonce,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("TaskParameters is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("TaskParameters", e))
    }

    /// What every op written to this task is signed against.
    pub fn scope(&self) -> Scope {
        Scope::of(&self.encode())
    }

    /// The two roots this contract folds against: its creator, and its org if it
    /// has one.
    pub fn trust(&self) -> Trust {
        match self.org {
            Some(org) => Trust::under(
                self.scope(),
                self.creator,
                Org {
                    scope: org.scope,
                    owner: org.founder,
                },
            ),
            None => Trust::instance(self.scope(), self.creator),
        }
    }
}

/// A single change to a task.
///
/// No task id in any variant: the contract *is* the task. That is the one piece of
/// bookkeeping this design removes rather than adds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskOp {
    SetTitle {
        title: String,
    },
    SetDescription {
        description: String,
    },
    SetAssignee {
        assignee: Option<MemberId>,
    },

    /// Links this task to another. Only one kind holds between a pair, so
    /// re-linking replaces it. The inverse direction is derived, not stored.
    Link {
        to: TaskAddr,
        kind: LinkKind,
    },
    Unlink {
        to: TaskAddr,
    },

    /// Records that this task is placed on a board.
    ///
    /// The board holds the placement; this is the task's own copy, and the pair are
    /// written together. Keeping it here is what gives the system a reverse index it
    /// otherwise has none of: without it, an edit could only refresh the cached
    /// summary on the board the editor happens to be looking at, and every other
    /// board would drift.
    Attach {
        board: BoardId,
    },
    Detach {
        board: BoardId,
    },
}

impl TaskOp {
    /// The rights an author must hold for this op to count.
    ///
    /// As with [`crate::op::Op::needs`], the contract checks the envelope's
    /// *declared* rights, which the author chooses, and the fold checks this, which
    /// they do not.
    pub fn needs(&self) -> Rights {
        match self {
            TaskOp::SetTitle { .. }
            | TaskOp::SetDescription { .. }
            | TaskOp::SetAssignee { .. }
            | TaskOp::Attach { .. }
            | TaskOp::Detach { .. } => Rights::WRITE_TASKS,

            TaskOp::Link { .. } | TaskOp::Unlink { .. } => Rights::LINK_TASKS,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("TaskOp is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("task op", e))
    }

    /// Wraps this op in the envelope that will carry it. Sign the result.
    pub fn envelope(self, stamp: Stamp) -> Envelope {
        Envelope::stamped(
            stamp,
            self.needs(),
            kind::FIRST_APPLICATION_KIND,
            self.encode(),
        )
    }
}

/// What a board caches so a card can render before the task is fetched.
///
/// # Why this is safe to denormalise
///
/// Two copies of a title is normally an invitation to drift. What stops it is
/// [`Self::seen_lamport`]: it records the task's own clock as the summary's author
/// read it, and a board keeps only the highest per task. So a summary written from
/// a stale read loses to a fresher one no matter who wrote which first, or in what
/// order they arrive — reconciliation converges instead of racing.
///
/// Anyone may therefore repair any summary at any time. In practice a client only
/// learns that one is stale when a user opens the task, because that is the only
/// time it holds the body, which bounds repair to one write per open rather than a
/// sweep across every card.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub title: String,
    pub assignee: Option<MemberId>,
    /// The task's highest lamport as of the read this summary came from.
    pub seen_lamport: u64,
}

/// A task, folded out of its op set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Task {
    pub title: String,
    pub description: String,
    pub assignee: Option<MemberId>,
    /// What this task is to other tasks. The inverse is derived by the reader.
    pub links: BTreeMap<TaskAddr, LinkKind>,
    /// Every board this task is placed on, as far as any client has recorded.
    ///
    /// A hint, not a fact: clients write it, so a failed or missing
    /// [`TaskOp::Attach`] makes it incomplete and a missed [`TaskOp::Detach`]
    /// makes it stale. Verify on use — a board with no placement for this task
    /// should be dropped from the display rather than shown as a dead link.
    pub boards: BTreeSet<BoardId>,
    /// Highest lamport of any op in the state, applied or not.
    ///
    /// What a [`TaskSummary`] records, and deliberately not "highest lamport of an
    /// op that changed the title": it has to be monotone across everything the
    /// state holds, or two clients reading the same state could disagree about
    /// which summary is fresher.
    pub lamport: u64,
    pub next_lamport: u64,
    /// Ops carried but not rendered: from a newer client, or from an author who
    /// lacked the rights. Surfaced so a stale build can admit there is more here
    /// than it can show.
    pub unreadable_ops: usize,
    pub authority: Authority,
}

impl Task {
    pub fn from_state(state: &EnvelopeState, params: &TaskParameters) -> Self {
        let trust = params.trust();
        let authority = state.authority_in(&trust);

        let lamport = state
            .ops
            .values()
            .map(|op| op.payload.lamport)
            .max()
            .unwrap_or(0);

        let mut task = Task {
            lamport,
            next_lamport: lamport.saturating_add(1),
            authority,
            ..Task::default()
        };

        for envelope in state.application_ops() {
            if envelope.payload.kind != kind::FIRST_APPLICATION_KIND {
                task.unreadable_ops += 1;
                continue;
            }
            let Ok(op) = TaskOp::decode(&envelope.payload.body) else {
                task.unreadable_ops += 1;
                continue;
            };
            // `ever`, not `held`: work already done survives its author's removal.
            if !task
                .authority
                .ever_rights_of(&envelope.author())
                .contains(op.needs())
            {
                task.unreadable_ops += 1;
                continue;
            }
            // Ops arrive in one total order, so plain assignment *is*
            // last-writer-wins and needs no per-field clock.
            match op {
                TaskOp::SetTitle { title } => task.title = title,
                TaskOp::SetDescription { description } => task.description = description,
                TaskOp::SetAssignee { assignee } => task.assignee = assignee,
                TaskOp::Link { to, kind } => {
                    task.links.insert(to, kind);
                }
                TaskOp::Unlink { to } => {
                    task.links.remove(&to);
                }
                TaskOp::Attach { board } => {
                    task.boards.insert(board);
                }
                TaskOp::Detach { board } => {
                    task.boards.remove(&board);
                }
            }
        }

        task
    }

    /// The summary a board should cache for this task.
    pub fn summary(&self) -> TaskSummary {
        TaskSummary {
            title: self.title.clone(),
            assignee: self.assignee,
            seen_lamport: self.lamport,
        }
    }

    /// Whether `cached` is out of date, and therefore whether opening this task
    /// should write a fresh summary back to the board holding it.
    ///
    /// Compares content and not just the clock: a summary that happens to carry a
    /// lower lamport but the right title needs no write, and writing one anyway
    /// would mean every open of an untouched task grew the board.
    pub fn summary_is_stale(&self, cached: &TaskSummary) -> bool {
        cached.title != self.title || cached.assignee != self.assignee
    }

    pub fn may(&self, member: &MemberId, rights: Rights) -> bool {
        self.authority.rights_of(member).contains(rights)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    const ORG_SCOPE: Scope = Scope([3; 32]);

    fn peer(seed: u8) -> (SigningKey, MemberId) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        (key.clone(), MemberId(key.verifying_key().to_bytes()))
    }

    fn params(creator: MemberId, founder: Option<MemberId>) -> TaskParameters {
        TaskParameters::new(
            creator,
            founder.map(|founder| TaskOrg {
                id: OrgId([1; 32]),
                scope: ORG_SCOPE,
                founder,
            }),
            1_700_000_000_000,
            [5; 16],
        )
    }

    /// Signs a task op for this task.
    fn write(
        key: &SigningKey,
        author: MemberId,
        params: &TaskParameters,
        lamport: u64,
        op: TaskOp,
    ) -> crate::envelope::SignedEnvelope {
        // Truncation is the point: the nonce only has to distinguish ops within one
        // test, and every test's lamports are small.
        let nonce = lamport.to_le_bytes()[0];
        let stamp = Stamp::new(params.scope(), author, lamport, 0, [nonce; 16]);
        op.envelope(stamp).sign(key)
    }

    #[test]
    fn a_creator_can_write_and_the_fold_is_last_writer_wins() {
        let (key, creator) = peer(1);
        let params = params(creator, None);

        let state = EnvelopeState::from_ops(vec![
            write(
                &key,
                creator,
                &params,
                1,
                TaskOp::SetTitle {
                    title: "First".to_owned(),
                },
            ),
            write(
                &key,
                creator,
                &params,
                2,
                TaskOp::SetTitle {
                    title: "Second".to_owned(),
                },
            ),
        ]);
        state
            .validate_in(&params.trust())
            .expect("must validate: the creator wrote both");

        let task = Task::from_state(&state, &params);
        assert_eq!(task.title, "Second");
        assert_eq!(task.lamport, 2);
        assert_eq!(task.unreadable_ops, 0);
    }

    /// The requirement this whole design serves: someone who was not granted
    /// anything on the task, and who may have joined the org after it was created,
    /// can still edit its body.
    #[test]
    fn an_org_member_may_edit_without_ever_being_granted_on_the_task() {
        let (_, creator) = peer(1);
        let (founder_key, founder) = peer(2);
        let (joiner_key, joiner) = peer(3);
        let params = params(creator, Some(founder));

        // The org's own grant, signed for the org and copied in here.
        let certificate = {
            let stamp = Stamp::new(ORG_SCOPE, founder, 1, 0, [1; 16]);
            Envelope::grant(stamp, joiner, Rights::MEMBER).sign(&founder_key)
        };
        let edit = write(
            &joiner_key,
            joiner,
            &params,
            2,
            TaskOp::SetTitle {
                title: "Edited by a newcomer".to_owned(),
            },
        );

        let mut state = EnvelopeState::new();
        state
            .accept_in(vec![certificate, edit], &params.trust())
            .expect("must be accepted: the certificate is from the named org");
        assert_eq!(
            Task::from_state(&state, &params).title,
            "Edited by a newcomer"
        );
    }

    /// A task on a personal board has no org, so there is no certificate to
    /// present and a stranger is simply refused.
    #[test]
    fn without_an_org_a_stranger_cannot_write() {
        let (_, creator) = peer(1);
        let (stranger_key, stranger) = peer(9);
        let params = params(creator, None);

        let edit = write(
            &stranger_key,
            stranger,
            &params,
            1,
            TaskOp::SetTitle {
                title: "Not mine".to_owned(),
            },
        );
        assert!(
            EnvelopeState::new()
                .accept_in(vec![edit], &params.trust())
                .is_err()
        );
    }

    #[test]
    fn attach_and_detach_maintain_the_board_set() {
        let (key, creator) = peer(1);
        let params = params(creator, None);
        let (a, b) = (BoardId([1; 32]), BoardId([2; 32]));

        let state = EnvelopeState::from_ops(vec![
            write(&key, creator, &params, 1, TaskOp::Attach { board: a }),
            write(&key, creator, &params, 2, TaskOp::Attach { board: b }),
            write(&key, creator, &params, 3, TaskOp::Detach { board: a }),
        ]);

        let task = Task::from_state(&state, &params);
        assert_eq!(task.boards, BTreeSet::from([b]));
    }

    /// Linking replaces rather than accumulates: one kind holds between a pair.
    #[test]
    fn relinking_replaces_the_kind() {
        let (key, creator) = peer(1);
        let params = params(creator, None);
        let other = TaskAddr([7; 32]);

        let state = EnvelopeState::from_ops(vec![
            write(
                &key,
                creator,
                &params,
                1,
                TaskOp::Link {
                    to: other,
                    kind: LinkKind::RelatedTo,
                },
            ),
            write(
                &key,
                creator,
                &params,
                2,
                TaskOp::Link {
                    to: other,
                    kind: LinkKind::ParentOf,
                },
            ),
        ]);
        assert_eq!(
            Task::from_state(&state, &params).links,
            BTreeMap::from([(other, LinkKind::ParentOf)])
        );
    }

    /// The summary carries the clock, and staleness is decided on content so that
    /// opening an unchanged task writes nothing.
    #[test]
    fn a_summary_is_stale_only_when_the_content_differs() {
        let (key, creator) = peer(1);
        let params = params(creator, None);
        let state = EnvelopeState::from_ops(vec![write(
            &key,
            creator,
            &params,
            4,
            TaskOp::SetTitle {
                title: "Ship it".to_owned(),
            },
        )]);
        let task = Task::from_state(&state, &params);

        let fresh = task.summary();
        assert_eq!(fresh.title, "Ship it");
        assert_eq!(fresh.seen_lamport, 4);
        assert!(!task.summary_is_stale(&fresh));

        // An older read of the same title needs no repair…
        assert!(!task.summary_is_stale(&TaskSummary {
            title: "Ship it".to_owned(),
            assignee: None,
            seen_lamport: 1,
        }));
        // …but a different title does, whatever clock it claims.
        assert!(task.summary_is_stale(&TaskSummary {
            title: "Stale".to_owned(),
            assignee: None,
            seen_lamport: 99,
        }));
    }

    /// An op an author was not entitled to write is carried and counted, never
    /// rendered — the same treatment a board gives one.
    #[test]
    fn an_unauthorised_op_is_counted_rather_than_applied() {
        let (_, creator) = peer(1);
        let (founder_key, founder) = peer(2);
        let (linker_key, linker) = peer(3);
        let params = params(creator, Some(founder));

        // Enough to link, not to retitle.
        let certificate = {
            let stamp = Stamp::new(ORG_SCOPE, founder, 1, 0, [1; 16]);
            Envelope::grant(stamp, linker, Rights::LINK_TASKS).sign(&founder_key)
        };
        let overreach = write(
            &linker_key,
            linker,
            &params,
            2,
            TaskOp::SetTitle {
                title: "Not allowed".to_owned(),
            },
        );

        // Declared `needs` is what the contract checks, and this op declares
        // honestly, so the contract refuses it outright.
        assert!(
            EnvelopeState::new()
                .accept_in(
                    vec![certificate.clone(), overreach.clone()],
                    &params.trust()
                )
                .is_err()
        );

        // Understating `needs` gets it stored — and the fold ignores it anyway.
        let smuggled = {
            let stamp = Stamp::new(params.scope(), linker, 2, 0, [2; 16]);
            Envelope::stamped(
                stamp,
                Rights::LINK_TASKS,
                kind::FIRST_APPLICATION_KIND,
                TaskOp::SetTitle {
                    title: "Not allowed".to_owned(),
                }
                .encode(),
            )
            .sign(&linker_key)
        };
        let mut state = EnvelopeState::new();
        state
            .accept_in(vec![certificate, smuggled], &params.trust())
            .expect("stored: it declares only what its author holds");

        let task = Task::from_state(&state, &params);
        assert_eq!(task.title, "", "the fold checks what the op really needs");
        assert_eq!(task.unreadable_ops, 1);
    }
}

//! End-to-end properties of the board/task split.
//!
//! The unit tests in each module check one rule at a time. These check the things
//! that only go wrong when the two contracts are used together: that a board and
//! the tasks on it converge independently, that a summary written from any peer in
//! any order lands on the same answer, and that the permission split actually
//! splits — board rights moving cards, org rights editing bodies, neither standing
//! in for the other.

use ed25519_dalek::SigningKey;
use pj_core::envelope::{Envelope, GrantBody, SignedEnvelope, Stamp, kind};
use pj_core::task::{Task, TaskOp, TaskOrg, TaskParameters, TaskSummary};
use pj_core::{
    Board, BoardId, BoardParameters, ColumnId, EnvelopeState, MemberId, Op, OrgId, Rank, Rights,
    Scope, TaskAddr,
};

const TODO: ColumnId = ColumnId([1; 16]);
const DONE: ColumnId = ColumnId([2; 16]);
const ORG_SCOPE: Scope = Scope([77; 32]);
const TASK: TaskAddr = TaskAddr([9; 32]);

struct Peer {
    key: SigningKey,
    id: MemberId,
}

impl Peer {
    fn new(seed: u8) -> Self {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let id = MemberId(key.verifying_key().to_bytes());
        Self { key, id }
    }

    /// A board op at an explicit lamport, because every test here turns on which
    /// of two concurrent writes is the later one.
    fn board_op(&self, params: &BoardParameters, lamport: u64, op: Op) -> SignedEnvelope {
        let stamp = Stamp::new(
            params.scope(),
            self.id,
            lamport,
            lamport,
            [lamport.to_le_bytes()[0]; 16],
        );
        op.envelope(stamp).sign(&self.key)
    }

    fn task_op(&self, params: &TaskParameters, lamport: u64, op: TaskOp) -> SignedEnvelope {
        let stamp = Stamp::new(
            params.scope(),
            self.id,
            lamport,
            lamport,
            [lamport.to_le_bytes()[0]; 16],
        );
        op.envelope(stamp).sign(&self.key)
    }

    fn board_grant(
        &self,
        params: &BoardParameters,
        lamport: u64,
        to: MemberId,
        rights: Rights,
    ) -> SignedEnvelope {
        let stamp = Stamp::new(
            params.scope(),
            self.id,
            lamport,
            lamport,
            [lamport.to_le_bytes()[0]; 16],
        );
        Envelope::grant(stamp, to, rights).sign(&self.key)
    }

    /// A grant signed for the *organization*, which is what travels as a
    /// certificate.
    fn certificate(&self, lamport: u64, to: MemberId, rights: Rights) -> SignedEnvelope {
        let stamp = Stamp::new(
            ORG_SCOPE,
            self.id,
            lamport,
            lamport,
            [lamport.to_le_bytes()[0]; 16],
        );
        Envelope::grant(stamp, to, rights).sign(&self.key)
    }
}

fn board_params(owner: MemberId) -> BoardParameters {
    BoardParameters::new(owner, "project", [0; 16])
}

fn task_params(creator: MemberId, founder: MemberId) -> TaskParameters {
    TaskParameters::new(
        creator,
        Some(TaskOrg {
            id: OrgId([1; 32]),
            scope: ORG_SCOPE,
            founder,
        }),
        1_700_000_000_000,
        [5; 16],
    )
}

fn summarize(task: TaskAddr, title: &str, seen_lamport: u64) -> Op {
    Op::Summarize {
        task,
        summary: TaskSummary {
            title: title.to_owned(),
            assignee: None,
            seen_lamport,
        },
    }
}

fn columns(owner: &Peer, params: &BoardParameters) -> Vec<SignedEnvelope> {
    let todo = Rank::middle();
    let done = Rank::after(&todo);
    vec![
        owner.board_op(
            params,
            1,
            Op::SetColumn {
                column: TODO,
                title: "Todo".to_owned(),
                rank: todo,
            },
        ),
        owner.board_op(
            params,
            2,
            Op::SetColumn {
                column: DONE,
                title: "Done".to_owned(),
                rank: done,
            },
        ),
    ]
}

/// Every ordering of the same ops folds to the same board.
///
/// The CRDT claim, checked exhaustively rather than on the one or two orderings a
/// hand-written test happens to pick. Six ops is 720 permutations, which is cheap
/// and covers every interleaving of place, move, summarize and unplace.
#[test]
fn a_board_folds_identically_under_every_ordering() {
    let owner = Peer::new(1);
    let params = board_params(owner.id);

    let mut ops = columns(&owner, &params);
    ops.push(owner.board_op(
        &params,
        3,
        Op::Place {
            task: TASK,
            column: TODO,
            rank: Rank::middle(),
        },
    ));
    ops.push(owner.board_op(&params, 4, summarize(TASK, "first read", 2)));
    ops.push(owner.board_op(
        &params,
        5,
        Op::Place {
            task: TASK,
            column: DONE,
            rank: Rank::middle(),
        },
    ));
    ops.push(owner.board_op(&params, 6, summarize(TASK, "later read", 7)));

    let expected = Board::from_state(&EnvelopeState::from_ops(ops.clone()), &params);
    assert_eq!(expected.tasks[&TASK].column, DONE);
    assert_eq!(expected.tasks[&TASK].title(), "later read");

    for permutation in permutations(&ops) {
        let folded = Board::from_state(&EnvelopeState::from_ops(permutation), &params);
        assert_eq!(
            folded, expected,
            "the fold must not depend on arrival order"
        );
    }
}

#[test]
fn a_task_folds_identically_under_every_ordering() {
    let founder = Peer::new(1);
    let params = task_params(founder.id, founder.id);
    let other = TaskAddr([3; 32]);

    let ops = vec![
        founder.task_op(
            &params,
            1,
            TaskOp::SetTitle {
                title: "one".to_owned(),
            },
        ),
        founder.task_op(
            &params,
            2,
            TaskOp::SetTitle {
                title: "two".to_owned(),
            },
        ),
        founder.task_op(
            &params,
            3,
            TaskOp::Attach {
                board: BoardId([8; 32]),
            },
        ),
        founder.task_op(
            &params,
            4,
            TaskOp::Link {
                to: other,
                kind: pj_core::LinkKind::RelatedTo,
            },
        ),
        founder.task_op(&params, 5, TaskOp::Unlink { to: other }),
    ];

    let expected = Task::from_state(&EnvelopeState::from_ops(ops.clone()), &params);
    assert_eq!(expected.title, "two");
    assert!(expected.links.is_empty());
    assert_eq!(expected.boards.len(), 1);

    for permutation in permutations(&ops) {
        let folded = Task::from_state(&EnvelopeState::from_ops(permutation), &params);
        assert_eq!(folded.title, expected.title);
        assert_eq!(folded.links, expected.links);
        assert_eq!(folded.boards, expected.boards);
    }
}

/// The property the whole cache-honesty design rests on: whoever writes a summary,
/// whenever they write it, the board ends up showing the one read from the
/// freshest task.
///
/// Checked across every ordering *and* with the board-clock order deliberately
/// opposed to the task-clock order, which is the case a naive last-writer-wins
/// would get wrong.
#[test]
fn the_freshest_read_wins_however_the_writes_are_ordered() {
    let owner = Peer::new(1);
    let member = Peer::new(2);
    let params = board_params(owner.id);

    let mut ops = columns(&owner, &params);
    ops.push(owner.board_grant(&params, 3, member.id, Rights::MEMBER));
    ops.push(owner.board_op(
        &params,
        4,
        Op::Place {
            task: TASK,
            column: TODO,
            rank: Rank::middle(),
        },
    ));
    // The member writes *later* by the board's clock but from an *older* read of
    // the task. The stale one must lose despite winning on the board's clock.
    ops.push(owner.board_op(&params, 5, summarize(TASK, "current", 20)));
    ops.push(member.board_op(&params, 6, summarize(TASK, "stale", 3)));

    for permutation in permutations(&ops) {
        let folded = Board::from_state(&EnvelopeState::from_ops(permutation), &params);
        assert_eq!(
            folded.tasks[&TASK].title(),
            "current",
            "a summary read from an older task must never win"
        );
    }
}

/// Two peers who each saw only half the writes, then merged, agree with a peer who
/// saw everything. Union of a content-addressed set, so this should hold — but it
/// is the property a user actually experiences as "the board is the same on both
/// laptops".
#[test]
fn peers_that_merge_partial_views_agree_with_one_that_saw_everything() {
    let owner = Peer::new(1);
    let member = Peer::new(2);
    let params = board_params(owner.id);

    let mut all = columns(&owner, &params);
    all.push(owner.board_grant(&params, 3, member.id, Rights::MEMBER));
    all.push(owner.board_op(
        &params,
        4,
        Op::Place {
            task: TASK,
            column: TODO,
            rank: Rank::middle(),
        },
    ));
    all.push(member.board_op(&params, 5, summarize(TASK, "member's read", 4)));
    all.push(owner.board_op(
        &params,
        6,
        Op::Place {
            task: TASK,
            column: DONE,
            rank: Rank::middle(),
        },
    ));

    let complete = Board::from_state(&EnvelopeState::from_ops(all.clone()), &params);

    // Split down the middle, fold each half, then merge the halves both ways.
    let (left, right) = all.split_at(3);
    let mut a = EnvelopeState::from_ops(left.to_vec());
    let mut b = EnvelopeState::from_ops(right.to_vec());
    a.merge(b.clone());
    b.merge(EnvelopeState::from_ops(left.to_vec()));

    assert_eq!(Board::from_state(&a, &params), complete);
    assert_eq!(Board::from_state(&b, &params), complete);
}

/// The permission split, stated as one test: board rights move a card, org rights
/// edit the body, and neither substitutes for the other.
#[test]
fn board_rights_move_a_card_and_org_rights_edit_it_and_neither_does_both() {
    let founder = Peer::new(1);
    // On the board but not in the org: can arrange cards, cannot rename them.
    let arranger = Peer::new(2);
    // In the org but not on the board: can rename, cannot arrange.
    let editor = Peer::new(3);

    let board = board_params(founder.id);
    let task = task_params(founder.id, founder.id);

    // --- the board half
    let mut board_ops = columns(&founder, &board);
    board_ops.push(founder.board_grant(&board, 3, arranger.id, Rights::MEMBER));
    board_ops.push(founder.board_op(
        &board,
        4,
        Op::Place {
            task: TASK,
            column: TODO,
            rank: Rank::middle(),
        },
    ));
    // The arranger moves it: allowed, and touches no task contract.
    board_ops.push(arranger.board_op(
        &board,
        5,
        Op::Place {
            task: TASK,
            column: DONE,
            rank: Rank::middle(),
        },
    ));
    // The editor tries to move it: they hold nothing here.
    board_ops.push(editor.board_op(
        &board,
        6,
        Op::Place {
            task: TASK,
            column: TODO,
            rank: Rank::middle(),
        },
    ));

    let folded = Board::from_state(&EnvelopeState::from_ops(board_ops), &board);
    assert_eq!(
        folded.tasks[&TASK].column, DONE,
        "the arranger's move stands and the outsider's is ignored"
    );

    // --- the task half
    let mut state = EnvelopeState::new();
    state
        .accept_in(
            vec![
                founder.certificate(1, editor.id, Rights::MEMBER),
                editor.task_op(
                    &task,
                    2,
                    TaskOp::SetTitle {
                        title: "renamed by the org".to_owned(),
                    },
                ),
            ],
            &task.trust(),
        )
        .expect("an org member may edit the body");
    assert_eq!(
        Task::from_state(&state, &task).title,
        "renamed by the org",
        "org membership is what grants the body, not board membership"
    );

    // And the arranger, who holds MEMBER on the *board*, is refused here — the
    // grant they hold was signed for the board and does not travel.
    let refused = state.clone().accept_in(
        vec![arranger.board_op(
            &board,
            7,
            Op::Place {
                task: TASK,
                column: TODO,
                rank: Rank::middle(),
            },
        )],
        &task.trust(),
    );
    assert!(
        refused.is_err(),
        "a board's op must not be accepted by a task, whoever signed it"
    );
}

/// A stale summary is detected and a fresh one is not, which is what bounds
/// repair to one write per open instead of one per render.
#[test]
fn opening_an_unchanged_task_finds_nothing_to_repair() {
    let founder = Peer::new(1);
    let board = board_params(founder.id);
    let task = task_params(founder.id, founder.id);

    let state = EnvelopeState::from_ops(vec![founder.task_op(
        &task,
        1,
        TaskOp::SetTitle {
            title: "Ship it".to_owned(),
        },
    )]);
    let folded = Task::from_state(&state, &task);

    // The board caches exactly what the task says.
    let mut ops = columns(&founder, &board);
    ops.push(founder.board_op(
        &board,
        3,
        Op::Place {
            task: TASK,
            column: TODO,
            rank: Rank::middle(),
        },
    ));
    ops.push(founder.board_op(
        &board,
        4,
        Op::Summarize {
            task: TASK,
            summary: folded.summary(),
        },
    ));
    let cached = Board::from_state(&EnvelopeState::from_ops(ops), &board).tasks[&TASK]
        .summary
        .clone();

    assert!(
        !folded.summary_is_stale(&cached),
        "an untouched task must not cause a write on every open"
    );

    // Now the task moves on, and the same board copy is stale.
    let mut state = state;
    state.merge(EnvelopeState::from_ops(vec![founder.task_op(
        &task,
        2,
        TaskOp::SetTitle {
            title: "Shipped".to_owned(),
        },
    )]));
    assert!(Task::from_state(&state, &task).summary_is_stale(&cached));
}

/// A certificate keeps working after the org admin who issued it is demoted.
///
/// `ever_held`, not `held`: judging past work by present rights would mean
/// removing an admin silently invalidated everything they had ever authorised.
#[test]
fn work_authorised_by_a_since_demoted_admin_survives() {
    let founder = Peer::new(1);
    let admin = Peer::new(2);
    let member = Peer::new(3);
    let params = task_params(founder.id, founder.id);

    let mut state = EnvelopeState::new();
    state
        .accept_in(
            vec![
                founder.certificate(1, admin.id, Rights::ADMIN),
                admin.certificate(2, member.id, Rights::MEMBER),
                member.task_op(
                    &params,
                    3,
                    TaskOp::SetTitle {
                        title: "written under the old regime".to_owned(),
                    },
                ),
                // …and now the admin is removed.
                founder.certificate(4, admin.id, Rights::NONE),
            ],
            &params.trust(),
        )
        .expect("removing an admin must not make the state unacceptable");

    let task = Task::from_state(&state, &params);
    assert_eq!(task.title, "written under the old regime");
    assert!(
        !task.may(&admin.id, Rights::WRITE_TASKS),
        "the demotion still takes effect for anything new"
    );
}

/// Cards written by the old build are recovered with their fields intact, and
/// nothing written by the current build is mistaken for one.
#[test]
fn legacy_cards_survive_the_split() {
    // The old bytes, built as bytes rather than by re-declaring the old enum.
    //
    // `bincode` writes an enum as a u32 variant index followed by the fields in
    // order, and a tuple as its fields in order — so this is byte-identical to
    // what the old `Op::CreateTask` (index 2, with a *16*-byte task id) produced.
    // Spelling it out is the point: what is under test is the wire format, and a
    // re-declared enum would only be testing that two copies of a definition
    // agree with each other.
    const CREATE_TASK: u32 = 2;

    let owner = Peer::new(1);
    let params = board_params(owner.id);
    let stamp = Stamp::new(params.scope(), owner.id, 1, 1, [1; 16]);
    let body = bincode::serialize(&(
        CREATE_TASK,
        [4u8; 16],
        TODO,
        "an old card".to_owned(),
        Rank::middle(),
    ))
    .expect("the fixture serializes");
    let old = Envelope::stamped(
        stamp,
        Rights::WRITE_TASKS,
        kind::FIRST_APPLICATION_KIND,
        body,
    )
    .sign(&owner.key);

    let mut ops = columns(&owner, &params);
    ops.push(old);
    // A card from the current build, alongside it.
    ops.push(owner.board_op(
        &params,
        4,
        Op::Place {
            task: TASK,
            column: TODO,
            rank: Rank::middle(),
        },
    ));
    let state = EnvelopeState::from_ops(ops);

    let recovered = pj_core::legacy::recover_tasks(&state);
    assert_eq!(
        recovered.len(),
        1,
        "exactly the old card, and not the new one"
    );
    assert_eq!(recovered[0].title, "an old card");
    assert_eq!(recovered[0].column, TODO);

    // The board itself shows the new card and counts the old one as unreadable
    // rather than dropping it.
    let board = Board::from_state(&state, &params);
    assert!(board.tasks.contains_key(&TASK));
    assert_eq!(board.tasks.len(), 1);
    assert_eq!(board.unreadable_ops, 1);
}

/// A grant naming a member is what a certificate is; check the body really does
/// survive the trip, because everything above depends on it decoding on the far
/// side.
#[test]
fn a_certificate_carries_its_grant_intact() {
    let founder = Peer::new(1);
    let member = Peer::new(2);
    let certificate = founder.certificate(1, member.id, Rights::MEMBER);

    assert_eq!(certificate.kind(), kind::GRANT);
    assert_eq!(certificate.payload.scope, ORG_SCOPE);
    let body = GrantBody::decode(&certificate.payload.body).expect("a grant body decodes");
    assert_eq!(body.member, member.id);
    assert_eq!(body.rights, Rights::MEMBER);
    assert!(certificate.verify().is_ok());
}

/// Every ordering of a slice, for the convergence tests.
fn permutations(ops: &[SignedEnvelope]) -> Vec<Vec<SignedEnvelope>> {
    if ops.len() <= 1 {
        return vec![ops.to_vec()];
    }
    let mut out = Vec::new();
    for (at, op) in ops.iter().enumerate() {
        let mut rest = ops.to_vec();
        rest.remove(at);
        for mut tail in permutations(&rest) {
            tail.insert(0, op.clone());
            out.push(tail);
        }
    }
    out
}

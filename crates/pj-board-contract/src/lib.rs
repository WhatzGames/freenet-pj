//! The Freenet contract governing a project board.
//!
//! Deliberately thin: it translates between Freenet's byte-oriented interface and
//! [`pj_core`], which holds the rules. Keeping the logic out of here means the
//! interesting behaviour is testable without a wasm runtime or a running node.
//!
//! # What this contract knows, and what it refuses to know
//!
//! A Freenet contract is the only thing between shared state and any peer who
//! feels like rewriting it. There is no server; every peer runs this. So it has to
//! answer exactly one question: **may this author write this?**
//!
//! It answers with three ops and one piece of arithmetic:
//!
//! - `GRANT`, `LINK_DEVICE` and `UNLINK_DEVICE` decide who holds which rights.
//! - Everything else is an opaque body carrying a declared `needs`, accepted when
//!   `held & needs == needs`.
//!
//! It does not know what a task is, or a column, or a name. That is the point.
//! A contract's address is `hash(code + parameters)`, so every rule it learns is a
//! future migration: teaching a task-aware contract about due dates would move
//! every board to a new address and orphan the old one. This contract never has to
//! learn that, because it never knew what a task was.
//!
//! The corollary matters as much. An op of a kind this build has never heard of is
//! *carried*, not rejected — older peers keep newer data intact instead of failing
//! to decode the state around it.

use freenet_stdlib::prelude::*;
use pj_core::{BoardParameters, EnvelopeDelta, EnvelopeState, EnvelopeSummary, SignedEnvelope};

pub struct BoardContract;

#[contract]
impl ContractInterface for BoardContract {
    /// Accepts a state only if every op is signed by its stated author and
    /// permitted by the rights that author holds.
    ///
    /// A malformed state is a hard error; a well-formed state that breaks the
    /// rules is `Invalid`. That is the distinction the node uses to tell garbage
    /// from something a peer merely isn't allowed to say.
    fn validate_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        let params = decode_params(&parameters)?;
        let state = EnvelopeState::decode(state.as_ref()).map_err(invalid_state)?;

        Ok(match state.validate(params.scope(), params.owner) {
            Ok(()) => ValidateResult::Valid,
            Err(_) => ValidateResult::Invalid,
        })
    }

    /// Merges incoming ops into the current state.
    ///
    /// Commutativity — Freenet's central requirement — comes from the state being
    /// a set keyed by content hash: merging is a union, so arrival order cannot
    /// change the result and a duplicate update is a no-op.
    fn update_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        data: Vec<UpdateData<'static>>,
    ) -> Result<UpdateModification<'static>, ContractError> {
        let params = decode_params(&parameters)?;
        let mut current = EnvelopeState::decode(state.as_ref()).map_err(invalid_state)?;

        for update in data {
            match update {
                UpdateData::State(incoming) => {
                    let incoming =
                        EnvelopeState::decode(incoming.as_ref()).map_err(invalid_state)?;
                    merge(&mut current, incoming.ops.into_values().collect(), &params)?;
                }
                UpdateData::Delta(delta) => {
                    let delta = EnvelopeDelta::decode(delta.as_ref()).map_err(invalid_delta)?;
                    merge(&mut current, delta.ops, &params)?;
                }
                UpdateData::StateAndDelta { state, delta } => {
                    let incoming = EnvelopeState::decode(state.as_ref()).map_err(invalid_state)?;
                    merge(&mut current, incoming.ops.into_values().collect(), &params)?;
                    let delta = EnvelopeDelta::decode(delta.as_ref()).map_err(invalid_delta)?;
                    merge(&mut current, delta.ops, &params)?;
                }
                // A board's state is self-contained; it never defers to another
                // contract, so there is nothing sensible to do with these.
                _ => {
                    return Err(ContractError::InvalidUpdateWithInfo {
                        reason: "board contracts do not take related-contract updates".to_owned(),
                    });
                }
            }
        }

        Ok(UpdateModification::valid(State::from(current.encode())))
    }

    /// Summarises the state as the set of op ids it holds, which is what lets a
    /// peer ask for precisely the ops it is missing.
    fn summarize_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
    ) -> Result<StateSummary<'static>, ContractError> {
        let state = EnvelopeState::decode(state.as_ref()).map_err(invalid_state)?;
        Ok(StateSummary::from(state.summary().encode()))
    }

    /// The ops this peer holds that the summarising peer does not.
    fn get_state_delta(
        _parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> Result<StateDelta<'static>, ContractError> {
        let state = EnvelopeState::decode(state.as_ref()).map_err(invalid_state)?;
        let summary = EnvelopeSummary::decode(summary.as_ref())
            .map_err(|e| ContractError::Deser(e.to_string()))?;
        Ok(StateDelta::from(state.delta_since(&summary).encode()))
    }
}

fn decode_params(parameters: &Parameters<'static>) -> Result<BoardParameters, ContractError> {
    BoardParameters::decode(parameters.as_ref()).map_err(|e| ContractError::Deser(e.to_string()))
}

/// Rejects the whole update if any op in it is unsigned, forged, or beyond what
/// its author is permitted.
///
/// All-or-nothing rather than dropping the bad ops: silently accepting part of an
/// update would leave the sender believing something landed when it did not, and
/// would leave two peers holding different states from the same message.
fn merge(
    current: &mut EnvelopeState,
    ops: Vec<SignedEnvelope>,
    params: &BoardParameters,
) -> Result<(), ContractError> {
    current
        .accept(ops, params.scope(), params.owner)
        .map(|_| ())
        .map_err(|e| ContractError::InvalidUpdateWithInfo {
            reason: e.to_string(),
        })
}

fn invalid_state(_: pj_core::Error) -> ContractError {
    ContractError::InvalidState
}

fn invalid_delta(_: pj_core::Error) -> ContractError {
    ContractError::InvalidDelta
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use pj_core::envelope::{DeviceBody, Envelope, GrantBody, kind};
    use pj_core::{MemberId, Rights, Stamp};

    struct Peer {
        key: SigningKey,
        id: MemberId,
        lamport: u64,
    }

    impl Peer {
        fn new(seed: u8) -> Self {
            let key = SigningKey::from_bytes(&[seed; 32]);
            let id = MemberId(key.verifying_key().to_bytes());
            Self {
                key,
                id,
                lamport: 0,
            }
        }

        fn sign(&mut self, needs: Rights, kind: u16, body: Vec<u8>) -> SignedEnvelope {
            self.lamport += 1;
            let stamp = Stamp::new(
                the_board().scope(),
                self.id,
                self.lamport,
                0,
                // Truncation is the point: the nonce only has to distinguish this
                // peer's ops within one test, and every test's lamports are small.
                [self.lamport.to_le_bytes()[0]; 16],
            );
            Envelope::stamped(stamp, needs, kind, body).sign(&self.key)
        }

        fn grant(&mut self, to: MemberId, rights: Rights) -> SignedEnvelope {
            self.sign(
                Rights::MAY_GRANT,
                kind::GRANT,
                GrantBody { member: to, rights }.encode(),
            )
        }

        fn task(&mut self, what: &[u8]) -> SignedEnvelope {
            self.sign(
                Rights::WRITE_TASKS,
                kind::FIRST_APPLICATION_KIND,
                what.to_vec(),
            )
        }
    }

    fn params(owner: &Peer) -> BoardParameters {
        BoardParameters::new(owner.id, "board", [0; 16])
    }

    /// The one board every test here is about — peer 1's. Peers sign against its
    /// scope, because an op signed for anywhere else is refused, which is the
    /// point of the scope and has its own tests in `pj_core::envelope`.
    fn the_board() -> BoardParameters {
        params(&Peer::new(1))
    }

    fn as_params(p: &BoardParameters) -> Parameters<'static> {
        Parameters::from(p.encode())
    }

    fn state_of(ops: Vec<SignedEnvelope>) -> State<'static> {
        State::from(EnvelopeState::from_ops(ops).encode())
    }

    #[test]
    fn a_properly_signed_state_is_valid() {
        let mut owner = Peer::new(1);
        let p = params(&owner);

        let result = BoardContract::validate_state(
            as_params(&p),
            state_of(vec![owner.task(b"first")]),
            RelatedContracts::default(),
        )
        .expect("the contract accepts this fixture");
        assert!(matches!(result, ValidateResult::Valid));
    }

    #[test]
    fn a_state_containing_a_strangers_op_is_invalid() {
        let owner = Peer::new(1);
        let mut stranger = Peer::new(9);
        let p = params(&owner);

        let result = BoardContract::validate_state(
            as_params(&p),
            state_of(vec![stranger.task(b"intrusion")]),
            RelatedContracts::default(),
        )
        .expect("the contract accepts this fixture");
        assert!(matches!(result, ValidateResult::Invalid));
    }

    #[test]
    fn a_granted_member_may_write_and_a_stranger_still_may_not() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);
        let mut never = Peer::new(3);
        let p = params(&owner);

        let grant = owner.grant(member.id, Rights::MEMBER);
        let work = member.task(b"work");

        let valid = BoardContract::validate_state(
            as_params(&p),
            state_of(vec![grant.clone(), work.clone()]),
            RelatedContracts::default(),
        )
        .expect("the contract accepts this fixture");
        assert!(matches!(valid, ValidateResult::Valid));

        let invalid = BoardContract::validate_state(
            as_params(&p),
            state_of(vec![grant, work, never.task(b"nope")]),
            RelatedContracts::default(),
        )
        .expect("the contract accepts this fixture");
        assert!(matches!(invalid, ValidateResult::Invalid));
    }

    /// An op is signed against the board it was written for, so an admin's grant
    /// cannot be lifted onto a second board they also administer.
    #[test]
    fn an_op_signed_for_another_board_is_refused() {
        let mut owner = Peer::new(1);
        let member = Peer::new(2);

        // Written for the board every `Peer` here signs against…
        let grant = owner.grant(member.id, Rights::ADMIN);

        // …and replayed at a board that differs only in its salt.
        let elsewhere = BoardParameters::new(owner.id, "board", [1; 16]);
        let mut state = EnvelopeState::new();

        assert!(
            state
                .accept(vec![grant], elsewhere.scope(), elsewhere.owner)
                .is_err(),
            "a signature valid on one board must not be valid on another"
        );
        assert!(state.is_empty());
    }

    /// `MAY_GRANT` is the one bit the contract will not take on trust, which is
    /// what stops an op declaring `needs: NONE` and appointing its own author.
    #[test]
    fn a_grant_from_someone_without_may_grant_confers_nothing() {
        let owner = Peer::new(1);
        let mut stranger = Peer::new(9);
        let p = params(&owner);

        let sneaky = stranger.sign(
            Rights::NONE,
            kind::GRANT,
            GrantBody {
                member: stranger.id,
                rights: Rights::ALL,
            }
            .encode(),
        );

        let state = EnvelopeState::from_ops(vec![sneaky]);
        assert_eq!(
            state.authority(p.owner).rights_of(&stranger.id),
            Rights::NONE
        );
    }

    #[test]
    fn a_device_may_write_for_its_owner() {
        let mut owner = Peer::new(1);
        let mut laptop = Peer::new(5);
        let p = params(&owner);

        let link = owner.sign(
            Rights::NONE,
            kind::LINK_DEVICE,
            DeviceBody { device: laptop.id }.encode(),
        );

        let result = BoardContract::validate_state(
            as_params(&p),
            state_of(vec![link, laptop.task(b"from the laptop")]),
            RelatedContracts::default(),
        )
        .expect("the contract accepts this fixture");
        assert!(matches!(result, ValidateResult::Valid));
    }

    /// The property the redesign exists for: this contract will never be rebuilt
    /// to learn a new op kind, because it does not read them.
    #[test]
    fn an_op_of_an_unknown_kind_is_accepted_and_preserved() {
        let mut owner = Peer::new(1);
        let p = params(&owner);

        let exotic = owner.sign(Rights::WRITE_TASKS, 4095, b"invented later".to_vec());
        let updated = BoardContract::update_state(
            as_params(&p),
            State::from(EnvelopeState::new().encode()),
            vec![UpdateData::State(State::from(
                EnvelopeState::from_ops(vec![exotic.clone()]).encode(),
            ))],
        )
        .expect("the contract accepts this fixture");

        let after = EnvelopeState::decode(updated.unwrap_valid().as_ref())
            .expect("must decode: this test produced the bytes");
        assert_eq!(after.len(), 1);
        assert_eq!(after.ops.get(&exotic.id()), Some(&exotic));
    }

    #[test]
    fn update_state_is_commutative_and_idempotent() {
        let mut owner = Peer::new(1);
        let p = params(&owner);
        let a = owner.task(b"a");
        let b = owner.task(b"b");

        let apply = |batches: Vec<Vec<SignedEnvelope>>| {
            let mut state = State::from(EnvelopeState::new().encode());
            for batch in batches {
                state = BoardContract::update_state(
                    as_params(&p),
                    state,
                    vec![UpdateData::State(State::from(
                        EnvelopeState::from_ops(batch).encode(),
                    ))],
                )
                .expect("the contract accepts this fixture")
                .unwrap_valid();
            }
            EnvelopeState::decode(state.as_ref())
                .expect("must decode: this test produced the bytes")
        };

        let forwards = apply(vec![vec![a.clone()], vec![b.clone()]]);
        let backwards = apply(vec![vec![b.clone()], vec![a.clone()]]);
        assert_eq!(forwards, backwards);

        let twice = apply(vec![vec![a.clone(), b.clone()], vec![a, b]]);
        assert_eq!(twice, forwards);
    }

    #[test]
    fn an_update_containing_one_bad_op_is_rejected_whole() {
        let mut owner = Peer::new(1);
        let mut stranger = Peer::new(9);
        let p = params(&owner);

        let result = BoardContract::update_state(
            as_params(&p),
            State::from(EnvelopeState::new().encode()),
            vec![UpdateData::State(State::from(
                EnvelopeState::from_ops(vec![owner.task(b"good"), stranger.task(b"bad")]).encode(),
            ))],
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_delta_carries_exactly_what_the_summary_lacks() {
        let mut owner = Peer::new(1);
        let p = params(&owner);
        let a = owner.task(b"a");
        let b = owner.task(b"b");

        let theirs = EnvelopeState::from_ops(vec![a.clone()]);
        let delta = BoardContract::get_state_delta(
            as_params(&p),
            state_of(vec![a, b.clone()]),
            StateSummary::from(theirs.summary().encode()),
        )
        .expect("the contract accepts this fixture");

        assert_eq!(
            EnvelopeDelta::decode(delta.as_ref())
                .expect("must decode: this test produced the bytes")
                .ops,
            vec![b]
        );
    }

    #[test]
    fn an_unreadable_state_is_an_error_not_a_verdict() {
        let owner = Peer::new(1);
        let p = params(&owner);
        let result = BoardContract::validate_state(
            as_params(&p),
            State::from(vec![0xff; 8]),
            RelatedContracts::default(),
        );
        assert!(result.is_err());
    }
}

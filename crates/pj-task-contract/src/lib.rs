//! The Freenet contract governing a single task.
//!
//! Structurally the board contract with one difference, and the difference is the
//! whole reason this crate exists: a task folds against **two** roots of trust
//! rather than one.
//!
//! # Why a second root
//!
//! Its own creator is the first, as an owner is anywhere. The second is the
//! organization named in its parameters, and it is there to answer a question no
//! contract can answer for itself: *is this person in my organization?*
//!
//! Contracts are isolated — this one cannot read the org's state, and it refuses
//! `RelatedContracts` like every other contract here. So membership has to arrive
//! as evidence rather than as a lookup: an org-scoped grant, signed by a chain
//! rooted at the org's founder, copied into this task's own state by whoever wants
//! to use it. Validity is cryptographic, so self-service is fine.
//!
//! The alternative was seeding each task's grants from its board at creation time.
//! It cannot meet the requirement, which is that *anyone in the org may edit,
//! including people who join later* — a snapshot cannot enfranchise someone who did
//! not yet exist, and Freenet has no reverse index to push new grants over.
//!
//! What the second root deliberately does **not** cover: status. Which column a
//! card sits in belongs to the board's placement, not here, so moving a card needs
//! rights on that board and never touches this contract. See [`pj_core::task`].

use freenet_stdlib::prelude::*;
use pj_core::{EnvelopeDelta, EnvelopeState, EnvelopeSummary, SignedEnvelope, TaskParameters};

pub struct TaskContract;

#[contract]
impl ContractInterface for TaskContract {
    fn validate_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        let params = decode_params(&parameters)?;
        let state = EnvelopeState::decode(state.as_ref()).map_err(invalid_state)?;

        Ok(match state.validate_in(&params.trust()) {
            Ok(()) => ValidateResult::Valid,
            Err(_) => ValidateResult::Invalid,
        })
    }

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
                // Note this is *not* how the org is consulted. Membership arrives
                // as a certificate inside the state, precisely so that a task never
                // has to depend on another contract being fetched alongside it.
                _ => {
                    return Err(ContractError::InvalidUpdateWithInfo {
                        reason: "task contracts do not take related-contract updates".to_owned(),
                    });
                }
            }
        }

        Ok(UpdateModification::valid(State::from(current.encode())))
    }

    fn summarize_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
    ) -> Result<StateSummary<'static>, ContractError> {
        let state = EnvelopeState::decode(state.as_ref()).map_err(invalid_state)?;
        Ok(StateSummary::from(state.summary().encode()))
    }

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

fn decode_params(parameters: &Parameters<'static>) -> Result<TaskParameters, ContractError> {
    TaskParameters::decode(parameters.as_ref()).map_err(|e| ContractError::Deser(e.to_string()))
}

fn merge(
    current: &mut EnvelopeState,
    ops: Vec<SignedEnvelope>,
    params: &TaskParameters,
) -> Result<(), ContractError> {
    current
        .accept_in(ops, &params.trust())
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
    use pj_core::envelope::{Envelope, Stamp};
    use pj_core::task::{TaskOp, TaskOrg};
    use pj_core::{MemberId, OrgId, Rights, Scope};

    /// The organization's own contract, whose grants travel as certificates.
    const ORG_SCOPE: Scope = Scope([3; 32]);

    fn peer(seed: u8) -> (SigningKey, MemberId) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let id = MemberId(key.verifying_key().to_bytes());
        (key, id)
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

    fn retitle(
        key: &SigningKey,
        author: MemberId,
        params: &TaskParameters,
        lamport: u64,
        title: &str,
    ) -> SignedEnvelope {
        let nonce = lamport.to_le_bytes()[0];
        let stamp = Stamp::new(params.scope(), author, lamport, 0, [nonce; 16]);
        TaskOp::SetTitle {
            title: title.to_owned(),
        }
        .envelope(stamp)
        .sign(key)
    }

    /// A certificate as the org contract would have stored it: signed for the org,
    /// presented here.
    fn certificate(
        key: &SigningKey,
        founder: MemberId,
        member: MemberId,
        rights: Rights,
    ) -> SignedEnvelope {
        let stamp = Stamp::new(ORG_SCOPE, founder, 1, 0, [1; 16]);
        Envelope::grant(stamp, member, rights).sign(key)
    }

    fn state_of(ops: Vec<SignedEnvelope>) -> State<'static> {
        State::from(EnvelopeState::from_ops(ops).encode())
    }

    fn validate(params: &TaskParameters, ops: Vec<SignedEnvelope>) -> ValidateResult {
        TaskContract::validate_state(
            Parameters::from(params.encode()),
            state_of(ops),
            RelatedContracts::default(),
        )
        .expect("the fixture encodes cleanly")
    }

    #[test]
    fn a_creator_may_write_their_own_task() {
        let (key, creator) = peer(1);
        let params = params(creator, None);
        assert!(matches!(
            validate(&params, vec![retitle(&key, creator, &params, 1, "Mine")]),
            ValidateResult::Valid
        ));
    }

    #[test]
    fn a_stranger_is_refused() {
        let (_, creator) = peer(1);
        let (key, stranger) = peer(9);
        let params = params(creator, None);
        assert!(matches!(
            validate(
                &params,
                vec![retitle(&key, stranger, &params, 1, "Not mine")]
            ),
            ValidateResult::Invalid
        ));
    }

    /// The certificate path, end to end through the contract interface.
    #[test]
    fn an_org_certificate_admits_a_member_the_task_never_granted() {
        let (_, creator) = peer(1);
        let (founder_key, founder) = peer(2);
        let (member_key, member) = peer(3);
        let params = params(creator, Some(founder));

        assert!(matches!(
            validate(
                &params,
                vec![
                    certificate(&founder_key, founder, member, Rights::MEMBER),
                    retitle(&member_key, member, &params, 2, "Edited by the org"),
                ]
            ),
            ValidateResult::Valid
        ));
    }

    /// The hole is exactly one org wide.
    #[test]
    fn a_certificate_from_a_different_org_is_refused() {
        let (_, creator) = peer(1);
        let (founder_key, founder) = peer(2);
        let (member_key, member) = peer(3);

        // The task names an org whose scope is ORG_SCOPE; the certificate was
        // signed for some other org entirely.
        let params = params(creator, Some(founder));
        let elsewhere = {
            let stamp = Stamp::new(Scope([4; 32]), founder, 1, 0, [1; 16]);
            Envelope::grant(stamp, member, Rights::MEMBER).sign(&founder_key)
        };

        assert!(matches!(
            validate(
                &params,
                vec![
                    elsewhere,
                    retitle(&member_key, member, &params, 2, "Should not land"),
                ]
            ),
            ValidateResult::Invalid
        ));
    }

    /// A task with no org has no second root, so a certificate proves nothing.
    #[test]
    fn a_personal_task_honours_no_certificates() {
        let (_, creator) = peer(1);
        let (founder_key, founder) = peer(2);
        let (member_key, member) = peer(3);
        let params = params(creator, None);

        assert!(matches!(
            validate(
                &params,
                vec![
                    certificate(&founder_key, founder, member, Rights::MEMBER),
                    retitle(&member_key, member, &params, 2, "Should not land"),
                ]
            ),
            ValidateResult::Invalid
        ));
    }

    /// Merging is a union of a content-addressed set, so the same update applied
    /// twice leaves the state where it was.
    #[test]
    fn an_update_is_idempotent() {
        let (key, creator) = peer(1);
        let params = params(creator, None);
        let op = retitle(&key, creator, &params, 1, "Once");
        let delta = EnvelopeDelta {
            ops: vec![op.clone()],
        };

        let apply = |state: State<'static>| {
            TaskContract::update_state(
                Parameters::from(params.encode()),
                state,
                vec![UpdateData::Delta(StateDelta::from(delta.encode()))],
            )
            .expect("the fixture is authorised")
            .unwrap_valid()
            .into_bytes()
        };

        let once = apply(state_of(Vec::new()));
        let twice = apply(State::from(once.clone()));
        assert_eq!(once, twice);
    }

    #[test]
    fn related_contract_updates_are_refused() {
        let (_, creator) = peer(1);
        let params = params(creator, None);
        assert!(
            TaskContract::update_state(
                Parameters::from(params.encode()),
                state_of(Vec::new()),
                vec![UpdateData::RelatedState {
                    related_to: ContractInstanceId::new([0; 32]),
                    state: State::from(Vec::new()),
                }],
            )
            .is_err()
        );
    }
}

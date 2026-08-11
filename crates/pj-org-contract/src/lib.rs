//! The Freenet contract governing an organization.
//!
//! Thin, like the board contract: it translates between Freenet's byte-oriented
//! interface and [`pj_core::org`], which holds the rules. Every peer runs this
//! independently, so it is what actually stops an arbitrary key from adding itself
//! to somebody else's organization.
//!
//! Two rules, both rooted in the immutable contract parameters:
//!
//! 1. every op must carry a valid signature from the key that claims to have
//!    authored it, and
//! 2. that key must be one the organization accepts — the founder, an admin the
//!    founder appointed, a member an admin invited, or a linked device of any of
//!    them.
//!
//! Anything passing both is merged by set union, which is what makes `update_state`
//! commutative as Freenet requires.

use freenet_stdlib::prelude::*;
use pj_core::org::OrgParameters;
use pj_core::{EnvelopeDelta, EnvelopeState, EnvelopeSummary, SignedEnvelope};

pub struct OrgContract;

#[contract]
impl ContractInterface for OrgContract {
    fn validate_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        let params = decode_params(&parameters)?;
        let state =
            EnvelopeState::decode(state.as_ref()).map_err(|_| ContractError::InvalidState)?;

        Ok(match state.validate(params.scope(), params.founder) {
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
        let mut current =
            EnvelopeState::decode(state.as_ref()).map_err(|_| ContractError::InvalidState)?;

        for update in data {
            match update {
                UpdateData::State(incoming) => {
                    let incoming = EnvelopeState::decode(incoming.as_ref())
                        .map_err(|_| ContractError::InvalidState)?;
                    merge(&mut current, incoming.ops.into_values().collect(), &params)?;
                }
                UpdateData::Delta(delta) => {
                    let delta = EnvelopeDelta::decode(delta.as_ref())
                        .map_err(|_| ContractError::InvalidDelta)?;
                    merge(&mut current, delta.ops, &params)?;
                }
                UpdateData::StateAndDelta { state, delta } => {
                    let incoming = EnvelopeState::decode(state.as_ref())
                        .map_err(|_| ContractError::InvalidState)?;
                    merge(&mut current, incoming.ops.into_values().collect(), &params)?;
                    let delta = EnvelopeDelta::decode(delta.as_ref())
                        .map_err(|_| ContractError::InvalidDelta)?;
                    merge(&mut current, delta.ops, &params)?;
                }
                _ => {
                    return Err(ContractError::InvalidUpdateWithInfo {
                        reason: "organizations do not take related-contract updates".to_owned(),
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
        let state =
            EnvelopeState::decode(state.as_ref()).map_err(|_| ContractError::InvalidState)?;
        Ok(StateSummary::from(state.summary().encode()))
    }

    fn get_state_delta(
        _parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> Result<StateDelta<'static>, ContractError> {
        let state =
            EnvelopeState::decode(state.as_ref()).map_err(|_| ContractError::InvalidState)?;
        let summary = EnvelopeSummary::decode(summary.as_ref())
            .map_err(|e| ContractError::Deser(e.to_string()))?;
        Ok(StateDelta::from(state.delta_since(&summary).encode()))
    }
}

fn decode_params(parameters: &Parameters<'static>) -> Result<OrgParameters, ContractError> {
    OrgParameters::decode(parameters.as_ref()).map_err(|e| ContractError::Deser(e.to_string()))
}

/// All-or-nothing: silently accepting part of an update would leave the sender
/// believing something landed when it did not.
fn merge(
    current: &mut EnvelopeState,
    ops: Vec<SignedEnvelope>,
    params: &OrgParameters,
) -> Result<(), ContractError> {
    current
        .accept(ops, params.scope(), params.founder)
        .map(|_| ())
        .map_err(|e| ContractError::InvalidUpdateWithInfo {
            reason: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use pj_core::{Envelope, MemberId, Rights, Stamp};

    use super::*;

    struct Client {
        key: ed25519_dalek::SigningKey,
        id: MemberId,
        lamport: u64,
        nonce: u8,
    }

    impl Client {
        fn new(seed: u8) -> Self {
            let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
            let id = MemberId(key.verifying_key().to_bytes());
            Self {
                key,
                id,
                lamport: 0,
                nonce: 0,
            }
        }

        /// Every client here writes to client 1's organization — the only scope
        /// these signatures are valid for.
        fn stamp(&mut self) -> Stamp {
            self.lamport += 1;
            self.nonce = self.nonce.wrapping_add(1);
            Stamp::new(
                the_org().scope(),
                self.id,
                self.lamport,
                1_700_000_000_000,
                [self.nonce; 16],
            )
        }

        /// Invites somebody with the given rights. Membership is a grant, which is
        /// one of the three op kinds the contract itself reads.
        fn invite(&mut self, member: MemberId, rights: Rights) -> SignedEnvelope {
            let stamp = self.stamp();
            Envelope::grant(stamp, member, rights).sign(&self.key)
        }
    }

    fn the_org() -> OrgParameters {
        OrgParameters::new(Client::new(1).id, "Acme", [0; 16])
    }

    fn params_of(founder: &Client) -> Parameters<'static> {
        Parameters::from(OrgParameters::new(founder.id, "Acme", [0; 16]).encode())
    }

    fn delta_of(ops: Vec<SignedEnvelope>) -> UpdateData<'static> {
        UpdateData::Delta(StateDelta::from(EnvelopeDelta::new(ops).encode()))
    }

    #[test]
    fn an_empty_organization_is_valid() {
        let founder = Client::new(1);
        let result = OrgContract::validate_state(
            params_of(&founder),
            State::from(Vec::new()),
            RelatedContracts::default(),
        )
        .expect("the contract accepts this fixture");
        assert_eq!(result, ValidateResult::Valid);
    }

    #[test]
    fn the_founder_can_write() {
        let mut founder = Client::new(1);
        let updated = OrgContract::update_state(
            params_of(&founder),
            State::from(Vec::new()),
            vec![delta_of(vec![
                founder.invite(MemberId([2; 32]), Rights::ADMIN),
            ])],
        )
        .expect("the contract accepts this fixture");

        let state = EnvelopeState::decode(
            updated
                .new_state
                .expect("a valid update returns a new state")
                .as_ref(),
        )
        .expect("must decode: this test produced the bytes");
        assert_eq!(state.len(), 1);
    }

    #[test]
    fn a_stranger_is_refused() {
        let founder = Client::new(1);
        let mut stranger = Client::new(9);

        let refused = OrgContract::update_state(
            params_of(&founder),
            State::from(Vec::new()),
            vec![delta_of(vec![
                stranger.invite(MemberId([3; 32]), Rights::MEMBER),
            ])],
        );

        assert!(matches!(
            refused,
            Err(ContractError::InvalidUpdateWithInfo { .. })
        ));
    }

    /// An admin's invite has to be accepted by the contract, or admins could not
    /// run the organization.
    #[test]
    fn an_admin_invite_is_accepted_when_the_appointment_travels_with_it() {
        let mut founder = Client::new(1);
        let mut admin = Client::new(2);

        let appoint = founder.invite(admin.id, Rights::ADMIN);
        let invited = admin.invite(MemberId([3; 32]), Rights::MEMBER);

        let updated = OrgContract::update_state(
            params_of(&founder),
            State::from(Vec::new()),
            vec![delta_of(vec![appoint, invited])],
        )
        .expect("the contract accepts this fixture");

        let state = EnvelopeState::decode(
            updated
                .new_state
                .expect("a valid update returns a new state")
                .as_ref(),
        )
        .expect("must decode: this test produced the bytes");
        assert_eq!(state.len(), 2);
    }

    #[test]
    fn a_forged_signature_is_rejected() {
        let founder = Client::new(1);
        let mut impostor = Client::new(9);

        let mut op = impostor.invite(MemberId([3; 32]), Rights::MEMBER);
        op.payload.author = founder.id;

        assert!(matches!(
            OrgContract::update_state(
                params_of(&founder),
                State::from(Vec::new()),
                vec![delta_of(vec![op])]
            ),
            Err(ContractError::InvalidUpdateWithInfo { .. })
        ));
    }

    /// The property Freenet demands of `update_state`.
    #[test]
    fn update_state_is_commutative_and_idempotent() {
        let mut founder = Client::new(1);
        let a = founder.invite(MemberId([2; 32]), Rights::MEMBER);
        let b = founder.invite(MemberId([3; 32]), Rights::MEMBER);

        let apply = |updates: Vec<UpdateData<'static>>| -> Vec<u8> {
            let mut state = State::from(Vec::new());
            for update in updates {
                state = OrgContract::update_state(params_of(&founder), state, vec![update])
                    .expect("the contract accepts this fixture")
                    .new_state
                    .expect("the contract accepts this fixture");
            }
            state.as_ref().to_vec()
        };

        let forwards = apply(vec![delta_of(vec![a.clone()]), delta_of(vec![b.clone()])]);
        let backwards = apply(vec![delta_of(vec![b.clone()]), delta_of(vec![a.clone()])]);
        assert_eq!(forwards, backwards);

        let replayed = apply(vec![
            delta_of(vec![a.clone()]),
            delta_of(vec![b.clone()]),
            delta_of(vec![a, b]),
        ]);
        assert_eq!(replayed, forwards);
    }

    #[test]
    fn summary_and_delta_bring_a_lagging_peer_up_to_date() {
        let mut founder = Client::new(1);
        let shared = founder.invite(MemberId([2; 32]), Rights::MEMBER);
        let extra = founder.invite(MemberId([3; 32]), Rights::MEMBER);

        let behind = State::from(EnvelopeState::from_ops([shared.clone()]).encode());
        let ahead = State::from(EnvelopeState::from_ops([shared, extra]).encode());

        let summary = OrgContract::summarize_state(params_of(&founder), behind.clone())
            .expect("the contract accepts this fixture");
        let delta = OrgContract::get_state_delta(params_of(&founder), ahead.clone(), summary)
            .expect("the contract can diff this fixture");

        let caught_up =
            OrgContract::update_state(params_of(&founder), behind, vec![UpdateData::Delta(delta)])
                .expect("the contract accepts this fixture")
                .new_state
                .expect("the contract accepts this fixture");

        assert_eq!(
            EnvelopeState::decode(caught_up.as_ref())
                .expect("must decode: this test produced the bytes"),
            EnvelopeState::decode(ahead.as_ref())
                .expect("must decode: this test produced the bytes")
        );
    }
}

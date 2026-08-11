//! The Freenet contract behind one person's profile.
//!
//! Its address is derived from that person's own public key, so a client can find
//! its owner's profile without being told where it is — which matters, because
//! Freenet has no way to search for it.
//!
//! One rule, rooted in the immutable parameters: an op must be signed by the profile
//! owner, or by a key that owner has linked. Nobody else can write to your profile,
//! which is the whole point of it being yours.
//!
//! What it is *not*: a source of authority anywhere else. Boards and organizations
//! decide who may write to them from their own op sets. This is an index its owner
//! keeps for their own benefit.

use freenet_stdlib::prelude::*;
use pj_core::user::UserParameters;
use pj_core::{EnvelopeDelta, EnvelopeState, EnvelopeSummary, SignedEnvelope};

pub struct UserContract;

#[contract]
impl ContractInterface for UserContract {
    fn validate_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        let params = decode_params(&parameters)?;
        let state =
            EnvelopeState::decode(state.as_ref()).map_err(|_| ContractError::InvalidState)?;

        Ok(match state.validate(params.scope(), params.owner) {
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
                        reason: "user profiles do not take related-contract updates".to_owned(),
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

fn decode_params(parameters: &Parameters<'static>) -> Result<UserParameters, ContractError> {
    UserParameters::decode(parameters.as_ref()).map_err(|e| ContractError::Deser(e.to_string()))
}

fn merge(
    current: &mut EnvelopeState,
    ops: Vec<SignedEnvelope>,
    params: &UserParameters,
) -> Result<(), ContractError> {
    current
        .accept(ops, params.scope(), params.owner)
        .map(|_| ())
        .map_err(|e| ContractError::InvalidUpdateWithInfo {
            reason: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use pj_core::user::UserOp;
    use pj_core::{Envelope, MemberId, Stamp};

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

        fn stamp(&mut self) -> Stamp {
            self.lamport += 1;
            self.nonce = self.nonce.wrapping_add(1);
            // Every client in a test writes to peer 1's profile, which is the only
            // scope any of these signatures is valid for.
            let owner = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
            let scope = UserParameters::new(MemberId(owner.verifying_key().to_bytes())).scope();
            Stamp::new(
                scope,
                self.id,
                self.lamport,
                1_700_000_000_000,
                [self.nonce; 16],
            )
        }

        fn sign(&mut self, op: UserOp) -> SignedEnvelope {
            let stamp = self.stamp();
            op.envelope(stamp).sign(&self.key)
        }

        fn link(&mut self, device: MemberId) -> SignedEnvelope {
            let stamp = self.stamp();
            Envelope::link_device(stamp, device).sign(&self.key)
        }
    }

    fn params_of(owner: &Client) -> Parameters<'static> {
        Parameters::from(UserParameters::new(owner.id).encode())
    }

    fn delta_of(ops: Vec<SignedEnvelope>) -> UpdateData<'static> {
        UpdateData::Delta(StateDelta::from(EnvelopeDelta::new(ops).encode()))
    }

    #[test]
    fn an_empty_profile_is_valid() {
        let me = Client::new(1);
        assert_eq!(
            UserContract::validate_state(
                params_of(&me),
                State::from(Vec::new()),
                RelatedContracts::default()
            )
            .expect("the contract accepts this fixture"),
            ValidateResult::Valid
        );
    }

    #[test]
    fn the_owner_can_write_their_own_profile() {
        let mut me = Client::new(1);
        let updated = UserContract::update_state(
            params_of(&me),
            State::from(Vec::new()),
            vec![delta_of(vec![me.sign(UserOp::SetName {
                name: "Ada".to_owned(),
            })])],
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
    fn nobody_else_can() {
        let me = Client::new(1);
        let mut stranger = Client::new(9);

        let refused = UserContract::update_state(
            params_of(&me),
            State::from(Vec::new()),
            vec![delta_of(vec![stranger.sign(UserOp::SetName {
                name: "hijacked".to_owned(),
            })])],
        );

        assert!(matches!(
            refused,
            Err(ContractError::InvalidUpdateWithInfo { .. })
        ));
    }

    #[test]
    fn a_linked_device_can_write_when_the_link_travels_with_it() {
        let mut me = Client::new(1);
        let mut phone = Client::new(2);

        let link = me.link(phone.id);
        let write = phone.sign(UserOp::SetName {
            name: "Ada".to_owned(),
        });

        let updated = UserContract::update_state(
            params_of(&me),
            State::from(Vec::new()),
            vec![delta_of(vec![link, write])],
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
    fn update_state_is_commutative_and_idempotent() {
        let mut me = Client::new(1);
        let a = me.sign(UserOp::SetName {
            name: "A".to_owned(),
        });
        let b = me.sign(UserOp::SetName {
            name: "B".to_owned(),
        });

        let apply = |updates: Vec<UpdateData<'static>>| -> Vec<u8> {
            let mut state = State::from(Vec::new());
            for update in updates {
                state = UserContract::update_state(params_of(&me), state, vec![update])
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
        let mut me = Client::new(1);
        let shared = me.sign(UserOp::SetName {
            name: "A".to_owned(),
        });
        let extra = me.link(MemberId([5; 32]));

        let behind = State::from(EnvelopeState::from_ops([shared.clone()]).encode());
        let ahead = State::from(EnvelopeState::from_ops([shared, extra]).encode());

        let summary = UserContract::summarize_state(params_of(&me), behind.clone())
            .expect("the contract accepts this fixture");
        let delta = UserContract::get_state_delta(params_of(&me), ahead.clone(), summary)
            .expect("the contract can diff this fixture");

        let caught_up =
            UserContract::update_state(params_of(&me), behind, vec![UpdateData::Delta(delta)])
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

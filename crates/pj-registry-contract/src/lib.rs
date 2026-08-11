//! The Freenet contract behind the public board directory.
//!
//! Freenet can only fetch a contract whose address you already know, so a
//! "browse all projects" view has to be backed by something that *is* a known
//! address. This is it: one instance, at a location every build of the app derives
//! from the same constant parameters, whose state is the list of boards.
//!
//! It is deliberately permissive. A listing has to be signed by the key it names
//! as the board's owner, and that is the only rule — the directory is public by
//! design, so anyone may advertise a board they own.

use freenet_stdlib::prelude::*;
use pj_core::registry::{RegistryDelta, RegistryState, RegistrySummary, SignedListing};

pub struct RegistryContract;

#[contract]
impl ContractInterface for RegistryContract {
    fn validate_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        let state =
            RegistryState::decode(state.as_ref()).map_err(|_| ContractError::InvalidState)?;

        Ok(match state.validate() {
            Ok(()) => ValidateResult::Valid,
            Err(_) => ValidateResult::Invalid,
        })
    }

    /// Merges incoming listings. Union of a set keyed by content hash, so the
    /// order updates arrive in cannot change the outcome — which is what Freenet
    /// requires of this function.
    fn update_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
        data: Vec<UpdateData<'static>>,
    ) -> Result<UpdateModification<'static>, ContractError> {
        let mut current =
            RegistryState::decode(state.as_ref()).map_err(|_| ContractError::InvalidState)?;

        for update in data {
            match update {
                UpdateData::State(incoming) => {
                    let incoming = RegistryState::decode(incoming.as_ref())
                        .map_err(|_| ContractError::InvalidState)?;
                    merge(&mut current, incoming.listings.into_values().collect())?;
                }
                UpdateData::Delta(delta) => {
                    let delta = RegistryDelta::decode(delta.as_ref())
                        .map_err(|_| ContractError::InvalidDelta)?;
                    merge(&mut current, delta.listings)?;
                }
                UpdateData::StateAndDelta { state, delta } => {
                    let incoming = RegistryState::decode(state.as_ref())
                        .map_err(|_| ContractError::InvalidState)?;
                    merge(&mut current, incoming.listings.into_values().collect())?;
                    let delta = RegistryDelta::decode(delta.as_ref())
                        .map_err(|_| ContractError::InvalidDelta)?;
                    merge(&mut current, delta.listings)?;
                }
                _ => {
                    return Err(ContractError::InvalidUpdateWithInfo {
                        reason: "the registry does not take related-contract updates".to_owned(),
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
            RegistryState::decode(state.as_ref()).map_err(|_| ContractError::InvalidState)?;
        Ok(StateSummary::from(state.summary().encode()))
    }

    fn get_state_delta(
        _parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> Result<StateDelta<'static>, ContractError> {
        let state =
            RegistryState::decode(state.as_ref()).map_err(|_| ContractError::InvalidState)?;
        let summary = RegistrySummary::decode(summary.as_ref())
            .map_err(|e| ContractError::Deser(e.to_string()))?;
        Ok(StateDelta::from(state.delta_since(&summary).encode()))
    }
}

fn merge(current: &mut RegistryState, listings: Vec<SignedListing>) -> Result<(), ContractError> {
    current
        .accept(listings)
        .map(|_| ())
        .map_err(|e| ContractError::InvalidUpdateWithInfo {
            reason: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use pj_core::registry::{Listing, ListingTarget};
    use pj_core::{BoardId, MemberId};

    use super::*;

    fn listing(seed: u8, board: u8, name: &str) -> SignedListing {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let owner = MemberId(key.verifying_key().to_bytes());
        Listing {
            target: ListingTarget::Board(BoardId([board; 32])),
            name: name.to_owned(),
            owner,
            created_ms: 1_700_000_000_000,
        }
        .sign(&key)
    }

    fn delta_of(listings: Vec<SignedListing>) -> UpdateData<'static> {
        UpdateData::Delta(StateDelta::from(RegistryDelta::new(listings).encode()))
    }

    #[test]
    fn an_empty_registry_is_valid() {
        let result = RegistryContract::validate_state(
            Parameters::from(Vec::new()),
            State::from(Vec::new()),
            RelatedContracts::default(),
        )
        .expect("the contract accepts this fixture");
        assert_eq!(result, ValidateResult::Valid);
    }

    #[test]
    fn anyone_may_list_a_board_they_own() {
        let updated = RegistryContract::update_state(
            Parameters::from(Vec::new()),
            State::from(Vec::new()),
            vec![delta_of(vec![
                listing(1, 1, "Mine"),
                listing(2, 2, "Theirs"),
            ])],
        )
        .expect("the contract accepts this fixture");

        let state = RegistryState::decode(
            updated
                .new_state
                .expect("a valid update returns a new state")
                .as_ref(),
        )
        .expect("must decode: this test produced the bytes");
        assert_eq!(state.len(), 2, "the directory is public, not gated");
    }

    #[test]
    fn a_forged_listing_is_refused() {
        let mut forged = listing(1, 1, "Mine");
        forged.listing.name = "Hijacked".to_owned();

        let refused = RegistryContract::update_state(
            Parameters::from(Vec::new()),
            State::from(Vec::new()),
            vec![delta_of(vec![forged])],
        );

        assert!(matches!(
            refused,
            Err(ContractError::InvalidUpdateWithInfo { .. })
        ));
    }

    #[test]
    fn update_state_is_commutative_and_idempotent() {
        let a = listing(1, 1, "One");
        let b = listing(2, 2, "Two");

        let apply = |updates: Vec<UpdateData<'static>>| -> Vec<u8> {
            let mut state = State::from(Vec::new());
            for update in updates {
                state = RegistryContract::update_state(
                    Parameters::from(Vec::new()),
                    state,
                    vec![update],
                )
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
        let shared = listing(1, 1, "Shared");
        let extra = listing(1, 2, "Extra");

        let behind = State::from(RegistryState::from_listings([shared.clone()]).encode());
        let ahead = State::from(RegistryState::from_listings([shared, extra]).encode());

        let summary =
            RegistryContract::summarize_state(Parameters::from(Vec::new()), behind.clone())
                .expect("the contract accepts this fixture");
        let delta =
            RegistryContract::get_state_delta(Parameters::from(Vec::new()), ahead.clone(), summary)
                .expect("the contract accepts this fixture");

        let caught_up = RegistryContract::update_state(
            Parameters::from(Vec::new()),
            behind,
            vec![UpdateData::Delta(delta)],
        )
        .expect("the contract accepts this fixture")
        .new_state
        .expect("the contract accepts this fixture");

        assert_eq!(
            RegistryState::decode(caught_up.as_ref())
                .expect("must decode: this test produced the bytes"),
            RegistryState::decode(ahead.as_ref())
                .expect("must decode: this test produced the bytes")
        );
    }
}

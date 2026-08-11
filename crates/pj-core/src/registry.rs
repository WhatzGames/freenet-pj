//! The public project registry: how a board becomes findable.
//!
//! Freenet has no enumeration and no search — you can only fetch a contract whose
//! address you already know. So a directory is not something you query, it is
//! something the network stores: one well-known contract whose state is a list of
//! boards, appended to by whoever creates one.
//!
//! Structurally it is the same design as [`crate::envelope_state::EnvelopeState`] — a grow-only
//! set of signed entries keyed by content hash, so merging is set union and
//! convergence is free. Only the payload and the authorisation rule differ.

use std::collections::{BTreeMap, BTreeSet};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ids::{BoardId, ListingId, MemberId, OrgId};

/// Domain separator, distinct from the one board ops use so a signature can never
/// be replayed from one context into the other.
const SIGNING_DOMAIN: &[u8] = b"freenet-pj:listing:v1\0";

/// A listing timestamped further than this into the future is ignored when
/// displaying, since `created_ms` comes from whoever wrote it and nothing stops
/// them claiming the next century to pin themselves to the top of the list.
const MAX_CLOCK_SKEW_MS: u64 = 24 * 60 * 60 * 1000;

/// Parameters of the registry contract.
///
/// Constant, so every build derives the same instance id and therefore finds the
/// same registry. `epoch` is the escape hatch: bumping it starts a fresh registry
/// without having to change the contract code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryParameters {
    pub app: String,
    pub epoch: u32,
}

impl RegistryParameters {
    /// The one registry this version of the app uses.
    pub fn current() -> Self {
        Self {
            app: "freenet-pj".to_owned(),
            epoch: 1,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("RegistryParameters is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("RegistryParameters", e))
    }
}

/// What a listing points at.
///
/// An enum rather than a bare id plus a `kind` field, so it is impossible to read
/// an organization's id as a board's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ListingTarget {
    Board(BoardId),
    Organization(OrgId),
}

/// One board or organization, as advertised to everyone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub target: ListingTarget,
    pub name: String,
    pub owner: MemberId,
    pub created_ms: u64,
}

impl Listing {
    pub fn board(&self) -> Option<BoardId> {
        match self.target {
            ListingTarget::Board(board) => Some(board),
            ListingTarget::Organization(_) => None,
        }
    }

    pub fn organization(&self) -> Option<OrgId> {
        match self.target {
            ListingTarget::Organization(org) => Some(org),
            ListingTarget::Board(_) => None,
        }
    }

    pub fn is_organization(&self) -> bool {
        matches!(self.target, ListingTarget::Organization(_))
    }

    /// The base58 id, whichever kind of thing this points at.
    pub fn encoded_id(&self) -> String {
        match self.target {
            ListingTarget::Board(board) => board.to_base58(),
            ListingTarget::Organization(org) => org.to_base58(),
        }
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).expect("Listing is always serializable")
    }

    pub fn id(&self) -> ListingId {
        ListingId(*blake3::hash(&self.canonical_bytes()).as_bytes())
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(SIGNING_DOMAIN.len() + 128);
        buf.extend_from_slice(SIGNING_DOMAIN);
        buf.extend_from_slice(&self.canonical_bytes());
        buf
    }

    /// Signs this listing. Only the board's owner can produce a valid one, which
    /// is what stops anyone advertising someone else's board.
    pub fn sign(self, key: &SigningKey) -> SignedListing {
        let signature: Signature = key.sign(&self.signing_bytes());
        SignedListing {
            listing: self,
            signature: signature.to_bytes(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedListing {
    pub listing: Listing,
    #[serde(with = "crate::serde_bytes64")]
    pub signature: [u8; 64],
}

impl SignedListing {
    pub fn id(&self) -> ListingId {
        self.listing.id()
    }

    /// Verifies the listing was signed by the owner it names.
    ///
    /// Note what this does *not* prove: that `board` really is a contract owned by
    /// `owner`. Establishing that would mean recomputing
    /// `hash(board_code + params)` inside the contract, which needs the board
    /// contract's code hash as a registry parameter. Worth doing before this is
    /// load-bearing; see the README.
    pub fn verify(&self) -> Result<()> {
        let key = VerifyingKey::from_bytes(&self.listing.owner.0)
            .map_err(|_| Error::BadKey(self.listing.owner))?;
        let signature = Signature::from_bytes(&self.signature);
        key.verify_strict(&self.listing.signing_bytes(), &signature)
            .map_err(|_| Error::BadSignature {
                id: crate::ids::OpId(self.id().0),
                author: self.listing.owner,
            })
    }

    /// Total order used to pick a winner between duplicate listings of one board.
    fn order_key(&self) -> (u64, ListingId) {
        (self.listing.created_ms, self.id())
    }
}

/// The registry's whole state: a grow-only set of listings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryState {
    pub listings: BTreeMap<ListingId, SignedListing>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryDelta {
    pub listings: Vec<SignedListing>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrySummary {
    pub ids: Vec<ListingId>,
}

impl RegistryState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_listings(listings: impl IntoIterator<Item = SignedListing>) -> Self {
        let mut state = Self::new();
        for listing in listings {
            state.listings.insert(listing.id(), listing);
        }
        state
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("RegistryState is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::new());
        }
        bincode::deserialize(bytes).map_err(|e| Error::decode("RegistryState", e))
    }

    pub fn len(&self) -> usize {
        self.listings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.listings.is_empty()
    }

    /// Set union — commutative, associative, idempotent.
    pub fn merge(&mut self, other: RegistryState) -> usize {
        let before = self.listings.len();
        self.listings.extend(other.listings);
        self.listings.len() - before
    }

    /// Every listing must be signed by the owner it names. That is the only rule:
    /// the registry is public by design, so anyone may add a board they own.
    pub fn validate(&self) -> Result<()> {
        for listing in self.listings.values() {
            listing.verify()?;
        }
        Ok(())
    }

    /// Merges incoming listings, rejecting the batch if any is unsigned or forged.
    pub fn accept(&mut self, incoming: Vec<SignedListing>) -> Result<usize> {
        for listing in &incoming {
            listing.verify()?;
        }
        let before = self.listings.len();
        for listing in incoming {
            self.listings.insert(listing.id(), listing);
        }
        Ok(self.listings.len() - before)
    }

    pub fn summary(&self) -> RegistrySummary {
        RegistrySummary {
            ids: self.listings.keys().copied().collect(),
        }
    }

    pub fn delta_since(&self, summary: &RegistrySummary) -> RegistryDelta {
        let known: BTreeSet<ListingId> = summary.ids.iter().copied().collect();
        RegistryDelta {
            listings: self
                .listings
                .iter()
                .filter(|(id, _)| !known.contains(id))
                .map(|(_, listing)| listing.clone())
                .collect(),
        }
    }

    /// One listing per target, newest first, optionally filtered by a name query
    /// and restricted to boards or organizations.
    ///
    /// A target listed more than once collapses to its earliest claim, so re-listing
    /// it with a flattering timestamp cannot displace the original. `now_ms` is used
    /// only to drop listings claiming to be from the future — a display-side guard,
    /// so it never affects what the contract stores or whether peers converge.
    pub fn browse(
        &self,
        query: &str,
        organizations: bool,
        limit: usize,
        now_ms: u64,
    ) -> Vec<Listing> {
        let horizon = now_ms.saturating_add(MAX_CLOCK_SKEW_MS);
        let needle = query.trim().to_lowercase();

        let mut best: BTreeMap<ListingTarget, &SignedListing> = BTreeMap::new();
        for signed in self.listings.values() {
            if signed.listing.is_organization() != organizations {
                continue;
            }
            if signed.listing.created_ms > horizon {
                continue;
            }
            if !needle.is_empty() && !signed.listing.name.to_lowercase().contains(&needle) {
                continue;
            }
            best.entry(signed.listing.target)
                .and_modify(|existing| {
                    if signed.order_key() < existing.order_key() {
                        *existing = signed;
                    }
                })
                .or_insert(signed);
        }

        let mut listings: Vec<Listing> = best
            .into_values()
            .map(|signed| signed.listing.clone())
            .collect();
        // Newest first, content hash breaking ties so the order is total.
        listings.sort_by(|a, b| {
            b.created_ms
                .cmp(&a.created_ms)
                .then_with(|| a.id().cmp(&b.id()))
        });
        listings.truncate(limit);
        listings
    }
}

impl RegistryDelta {
    pub fn new(listings: Vec<SignedListing>) -> Self {
        Self { listings }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("RegistryDelta is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        bincode::deserialize(bytes).map_err(|e| Error::decode("RegistryDelta", e))
    }

    pub fn is_empty(&self) -> bool {
        self.listings.is_empty()
    }
}

impl RegistrySummary {
    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("RegistrySummary is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        bincode::deserialize(bytes).map_err(|e| Error::decode("RegistrySummary", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000_000;

    fn signer(seed: u8) -> (SigningKey, MemberId) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let id = MemberId(key.verifying_key().to_bytes());
        (key, id)
    }

    fn listing(seed: u8, board: u8, name: &str, created_ms: u64) -> SignedListing {
        let (key, owner) = signer(seed);
        Listing {
            target: ListingTarget::Board(BoardId([board; 32])),
            name: name.to_owned(),
            owner,
            created_ms,
        }
        .sign(&key)
    }

    #[test]
    fn a_signed_listing_verifies_and_round_trips() {
        let signed = listing(1, 1, "Roadmap", NOW);
        assert!(signed.verify().is_ok());

        let state = RegistryState::from_listings([signed]);
        let decoded = RegistryState::decode(&state.encode())
            .expect("must decode: this test produced the bytes");
        assert_eq!(decoded, state);
        assert!(decoded.validate().is_ok());
    }

    #[test]
    fn a_tampered_listing_is_rejected() {
        let mut signed = listing(1, 1, "Roadmap", NOW);
        signed.listing.name = "Someone else's board".to_owned();

        assert!(signed.verify().is_err());
        let mut state = RegistryState::new();
        assert!(state.accept(vec![signed]).is_err());
        assert!(state.is_empty(), "a rejected batch must not be applied");
    }

    #[test]
    fn merging_is_commutative_and_idempotent() {
        let a = listing(1, 1, "One", NOW);
        let b = listing(2, 2, "Two", NOW + 1);

        let mut left = RegistryState::from_listings([a.clone()]);
        left.merge(RegistryState::from_listings([b.clone()]));
        let mut right = RegistryState::from_listings([b.clone()]);
        right.merge(RegistryState::from_listings([a.clone()]));
        assert_eq!(left, right);

        let snapshot = left.clone();
        assert_eq!(left.merge(RegistryState::from_listings([a, b])), 0);
        assert_eq!(left, snapshot);
    }

    #[test]
    fn browse_returns_newest_first_and_respects_the_limit() {
        let state = RegistryState::from_listings([
            listing(1, 1, "Oldest", NOW - 2000),
            listing(1, 2, "Middle", NOW - 1000),
            listing(1, 3, "Newest", NOW),
        ]);

        let names: Vec<String> = state
            .browse("", false, 25, NOW)
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, vec!["Newest", "Middle", "Oldest"]);

        assert_eq!(state.browse("", false, 2, NOW).len(), 2, "limit is applied");
    }

    #[test]
    fn browse_filters_by_name_case_insensitively() {
        let state = RegistryState::from_listings([
            listing(1, 1, "Freenet Roadmap", NOW),
            listing(1, 2, "Kitchen Renovation", NOW),
        ]);

        let hits: Vec<String> = state
            .browse("roadmap", false, 25, NOW)
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(hits, vec!["Freenet Roadmap"]);
        assert!(state.browse("nothing here", false, 25, NOW).is_empty());
    }

    /// Re-listing a board must not create a second row, and must not let a later
    /// claim with a nicer timestamp displace the original.
    #[test]
    fn duplicate_listings_of_one_board_collapse_to_the_earliest() {
        let state = RegistryState::from_listings([
            listing(1, 7, "Original", NOW),
            listing(1, 7, "Renamed later", NOW + 5000),
        ]);

        let found = state.browse("", false, 25, NOW + 10_000);
        assert_eq!(found.len(), 1, "one row per board");
        assert_eq!(found[0].name, "Original");
    }

    /// `created_ms` is written by whoever made the listing, so a claim from the
    /// future must not be allowed to sit at the top of the list forever.
    #[test]
    fn listings_claiming_the_future_are_not_displayed() {
        let state = RegistryState::from_listings([
            listing(1, 1, "Honest", NOW),
            listing(2, 2, "Year 3000", NOW + 1_000_000_000_000),
        ]);

        let names: Vec<String> = state
            .browse("", false, 25, NOW)
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, vec!["Honest"]);
        assert_eq!(
            state.len(),
            2,
            "the entry is still stored — this is a display guard, not a contract rule"
        );
    }

    #[test]
    fn summary_and_delta_close_the_gap() {
        let shared = listing(1, 1, "Shared", NOW);
        let extra = listing(1, 2, "Extra", NOW + 1);

        let behind = RegistryState::from_listings([shared.clone()]);
        let ahead = RegistryState::from_listings([shared, extra.clone()]);

        let delta = ahead.delta_since(&behind.summary());
        assert_eq!(delta.listings, vec![extra]);

        let mut caught_up = behind;
        caught_up.merge(RegistryState::from_listings(delta.listings));
        assert_eq!(caught_up, ahead);
    }
}

//! A signed op whose body the contract never reads.
//!
//! # The problem this solves
//!
//! Today the contract decodes every op into a typed enum, so every new op kind is
//! a new variant, a new wasm, a new `hash(code + parameters)`, and a new address
//! with none of the old data. Adding a field is a migration for everyone.
//!
//! # The split
//!
//! Sign an envelope the contract understands, carrying a body it does not:
//!
//! - [`Envelope::needs`] — the rights this op claims to require. The contract
//!   checks the author holds them, by bit arithmetic, without knowing what they
//!   mean.
//! - [`Envelope::kind`] — a number. The client dispatches on it; the contract
//!   only special-cases the handful of *authority* kinds that decide who may
//!   write at all.
//! - [`Envelope::body`] — opaque bytes. The client encodes and decodes them.
//!
//! Adding an op kind then touches this crate and the folds, and no contract.
//!
//! # The property that matters more than the address
//!
//! An unknown `kind` is not an error. A client that has never heard of kind 12
//! keeps the envelope, folds around it, and re-encodes it untouched on the next
//! push. With a typed enum the whole state failed to decode — one new op made a
//! board unreadable rather than incomplete, and an old client that did manage to
//! write would have dropped what it could not parse.
//!
//! So the body is `Vec<u8>` rather than a generic parameter on purpose: the type
//! has to be able to hold data this build cannot interpret.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ids::{MemberId, OpId};
use crate::rights::Rights;

/// Separated from the v1 op domain so a signature can never be replayed across
/// the two encodings.
const SIGNING_DOMAIN: &[u8] = b"freenet-pj:envelope:v1\0";

/// Which contract instance an op was written for.
///
/// # Why a signature is not enough on its own
///
/// One envelope type now serves boards, organizations and profiles. Without this,
/// a signature would say only "this person wrote this", not "…here" — so an op
/// could be lifted out of the state it was written for and dropped into any other
/// contract where its author also holds rights. An admin's grant of `ADMIN` on one
/// board could be replayed onto every other board they administer, and onto the
/// author's own profile, where `ADMINISTER` is write access to everything.
///
/// The previous design separated the three *types* by signing domain but let one
/// board's ops replay onto another. Binding to the parameters closes both, because
/// the parameters are what make a contract instance itself: they include the owner
/// and the salt, and they are hashed into its address.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Scope(pub [u8; 32]);

impl Scope {
    /// Derived from a contract's encoded parameters — the same bytes the network
    /// hashes into its address.
    pub fn of(parameters: &[u8]) -> Self {
        Scope(*blake3::hash(parameters).as_bytes())
    }
}

/// Kinds the *contract* understands, because they decide who may write.
/// Everything else is opaque to it.
pub mod kind {
    /// Confer rights on a member. A grant of `Rights::NONE` is a removal.
    pub const GRANT: u16 = 0;
    /// Record another key as acting for the author.
    pub const LINK_DEVICE: u16 = 1;
    /// Revoke such a key.
    pub const UNLINK_DEVICE: u16 = 2;

    /// Kinds at or above this are payload, never authority. The contract does
    /// not look at them beyond checking `needs`.
    pub const FIRST_APPLICATION_KIND: u16 = 16;
}

/// Body of [`kind::GRANT`]. The only op whose *contents* the contract reads.
///
/// A display name is deliberately absent: that is presentation, it lives in an
/// application op, and putting it here would mean a rename went through the
/// authority path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantBody {
    pub member: MemberId,
    /// What to confer. `Rights::NONE` is a removal.
    pub rights: Rights,
}

/// Body of [`kind::LINK_DEVICE`] and [`kind::UNLINK_DEVICE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceBody {
    pub device: MemberId,
}

impl GrantBody {
    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("GrantBody is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("grant body", e))
    }
}

impl DeviceBody {
    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("DeviceBody is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("device body", e))
    }
}

/// Who is writing, and when.
///
/// Grouped because every envelope needs all four and no caller ever varies one
/// without the others — passing them separately turned every construction site
/// into four positional arguments of the same two types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// The contract this op is being written to.
    pub scope: Scope,
    pub author: MemberId,
    /// Logical clock: one more than the highest the author has seen here.
    pub lamport: u64,
    pub wall_clock_ms: u64,
    /// What makes two otherwise-identical ops (two "move to Done" in the same
    /// millisecond) distinct entries rather than one.
    pub nonce: [u8; 16],
}

impl Stamp {
    pub fn new(
        scope: Scope,
        author: MemberId,
        lamport: u64,
        wall_clock_ms: u64,
        nonce: [u8; 16],
    ) -> Self {
        Self {
            scope,
            author,
            lamport,
            wall_clock_ms,
            nonce,
        }
    }
}

/// The signed part of an op.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// The contract this op was written for. Signed, and checked on arrival, so an
    /// op cannot be lifted into a different contract. See [`Scope`].
    pub scope: Scope,
    pub author: MemberId,
    /// Logical clock, the primary key for last-writer-wins.
    pub lamport: u64,
    /// Wall clock, for display and as a tiebreak. Never trusted for ordering.
    pub wall_clock_ms: u64,
    pub nonce: [u8; 16],
    /// Rights this op claims to require. The contract refuses it unless the
    /// author holds all of them.
    pub needs: Rights,
    pub kind: u16,
    /// Opaque to the contract; decoded only by a client that knows `kind`.
    pub body: Vec<u8>,
}

impl Envelope {
    /// Builds an envelope from a stamp and an already-encoded body.
    pub fn stamped(stamp: Stamp, needs: Rights, kind: u16, body: Vec<u8>) -> Self {
        Self {
            scope: stamp.scope,
            author: stamp.author,
            lamport: stamp.lamport,
            wall_clock_ms: stamp.wall_clock_ms,
            nonce: stamp.nonce,
            needs,
            kind,
            body,
        }
    }

    /// Confer rights on a member. `Rights::NONE` is a removal.
    pub fn grant(stamp: Stamp, member: MemberId, rights: Rights) -> Self {
        Self::stamped(
            stamp,
            Rights::MAY_GRANT,
            kind::GRANT,
            GrantBody { member, rights }.encode(),
        )
    }

    /// Vouch for another key as acting for the author.
    pub fn link_device(stamp: Stamp, device: MemberId) -> Self {
        Self::stamped(
            stamp,
            Rights::NONE,
            kind::LINK_DEVICE,
            DeviceBody { device }.encode(),
        )
    }

    /// Revoke such a key. Only its voucher or the device itself is honoured.
    pub fn unlink_device(stamp: Stamp, device: MemberId) -> Self {
        Self::stamped(
            stamp,
            Rights::NONE,
            kind::UNLINK_DEVICE,
            DeviceBody { device }.encode(),
        )
    }

    /// Whether this is one of the kinds the contract itself interprets.
    pub fn is_authority(&self) -> bool {
        self.kind < kind::FIRST_APPLICATION_KIND
    }

    /// The bytes the id and the signature are both computed over.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).expect("Envelope is always serializable")
    }

    /// Content hash. Two peers receiving the same op derive the same id, which
    /// is what makes the op set dedupe correctly.
    pub fn id(&self) -> OpId {
        OpId(*blake3::hash(&self.canonical_bytes()).as_bytes())
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(SIGNING_DOMAIN.len() + 128);
        buf.extend_from_slice(SIGNING_DOMAIN);
        buf.extend_from_slice(&self.canonical_bytes());
        buf
    }

    /// Signs this envelope. The caller is responsible for `author` matching
    /// `key`; [`SignedEnvelope::verify`] is what catches a mismatch.
    pub fn sign(self, key: &SigningKey) -> SignedEnvelope {
        let signature: Signature = key.sign(&self.signing_bytes());
        SignedEnvelope {
            payload: self,
            signature: signature.to_bytes(),
        }
    }
}

/// An envelope plus proof that its stated author produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEnvelope {
    pub payload: Envelope,
    #[serde(with = "crate::serde_bytes64")]
    pub signature: [u8; 64],
}

impl SignedEnvelope {
    pub fn id(&self) -> OpId {
        self.payload.id()
    }

    pub fn author(&self) -> MemberId {
        self.payload.author
    }

    pub fn kind(&self) -> u16 {
        self.payload.kind
    }

    pub fn needs(&self) -> Rights {
        self.payload.needs
    }

    /// Verifies the signature against the author key embedded in the payload.
    ///
    /// `verify_strict` rejects small-order and non-canonical public keys. Not
    /// exploitable here, but it means every peer agrees on exactly which
    /// signatures are valid — and disagreement about validity breaks
    /// convergence.
    pub fn verify(&self) -> Result<()> {
        let key = VerifyingKey::from_bytes(&self.payload.author.0)
            .map_err(|_| Error::BadKey(self.payload.author))?;
        let signature = Signature::from_bytes(&self.signature);
        key.verify_strict(&self.payload.signing_bytes(), &signature)
            .map_err(|_| Error::BadSignature {
                id: self.id(),
                author: self.payload.author,
            })
    }

    /// Total order for last-writer-wins and for a deterministic fold. Lamport
    /// first so causality is respected, wall clock as a human-meaningful
    /// tiebreak, content hash last so the order is total.
    pub fn order_key(&self) -> (u64, u64, OpId) {
        (self.payload.lamport, self.payload.wall_clock_ms, self.id())
    }
}

/// What the contract checks, and all it checks, for a non-authority op.
///
/// Deliberately a free function taking the held rights rather than a method on
/// anything: the contract derives `held` from the authority ops it *does*
/// understand, and this is the whole of the rest of its policy.
pub fn permitted(held: Rights, envelope: &Envelope) -> bool {
    held.contains(envelope.needs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn peer(seed: u8) -> (SigningKey, MemberId) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let id = MemberId(key.verifying_key().to_bytes());
        (key, id)
    }

    const HERE: Scope = Scope([7; 32]);

    fn envelope(author: MemberId, kind: u16, body: Vec<u8>) -> Envelope {
        let stamp = Stamp::new(HERE, author, 1, 0, [0; 16]);
        Envelope::stamped(stamp, Rights::WRITE_TASKS, kind, body)
    }

    #[test]
    fn a_signed_envelope_verifies_and_a_tampered_one_does_not() {
        let (key, id) = peer(1);
        let signed = envelope(id, 16, b"hello".to_vec()).sign(&key);
        assert!(signed.verify().is_ok());

        let mut tampered = signed.clone();
        tampered.payload.body = b"goodbye".to_vec();
        assert!(tampered.verify().is_err());

        // The rights an op claims are signed too, so they cannot be widened in
        // flight to smuggle an op past a check.
        let mut widened = signed;
        widened.payload.needs = Rights::NONE;
        assert!(widened.verify().is_err());
    }

    #[test]
    fn signing_someone_elses_name_fails() {
        let (key, _) = peer(1);
        let (_, other) = peer(2);
        let signed = envelope(other, 16, b"x".to_vec()).sign(&key);
        assert!(signed.verify().is_err());
    }

    #[test]
    fn the_id_is_the_content_hash_so_duplicates_collapse() {
        let (key, id) = peer(1);
        let a = envelope(id, 16, b"x".to_vec()).sign(&key);
        let b = envelope(id, 16, b"x".to_vec()).sign(&key);
        assert_eq!(a.id(), b.id());

        let c = envelope(id, 16, b"y".to_vec()).sign(&key);
        assert_ne!(a.id(), c.id());
    }

    #[test]
    fn permission_is_a_bit_subset_check() {
        let (key, id) = peer(1);
        let mut op = envelope(id, 16, Vec::new());
        op.needs = Rights::WRITE_TASKS;
        let signed = op.sign(&key);

        assert!(permitted(Rights::MEMBER, &signed.payload));
        assert!(permitted(Rights::ALL, &signed.payload));
        assert!(!permitted(Rights::NONE, &signed.payload));
        assert!(!permitted(Rights::MAY_GRANT, &signed.payload));
    }

    /// The reason the body is bytes: this build must be able to hold, order and
    /// re-emit an op it cannot interpret.
    #[test]
    fn an_unknown_kind_survives_a_decode_and_re_encode() {
        let (key, id) = peer(1);
        let from_the_future = envelope(id, 4095, vec![9, 9, 9]).sign(&key);

        let bytes = bincode::serialize(&from_the_future).expect("bincode round-trips this type");
        let read_back: SignedEnvelope =
            bincode::deserialize(&bytes).expect("bincode round-trips this type");

        assert_eq!(read_back, from_the_future);
        assert!(read_back.verify().is_ok(), "still provably authentic");
        assert_eq!(
            bincode::serialize(&read_back).expect("bincode round-trips this type"),
            bytes
        );
    }

    #[test]
    fn authority_kinds_are_separated_from_application_kinds() {
        let (_, id) = peer(1);
        assert!(envelope(id, kind::GRANT, Vec::new()).is_authority());
        assert!(envelope(id, kind::UNLINK_DEVICE, Vec::new()).is_authority());
        assert!(!envelope(id, kind::FIRST_APPLICATION_KIND, Vec::new()).is_authority());
        assert!(!envelope(id, 4095, Vec::new()).is_authority());
    }

    /// A signature from the old op encoding must not verify here, or the domain
    /// separation is doing nothing.
    #[test]
    fn the_domain_separator_differs_from_the_op_domain() {
        assert_ne!(SIGNING_DOMAIN, b"freenet-pj:op:v1\0");
    }

    /// The scope is signed, so an op cannot be re-aimed at another contract even
    /// by whoever wrote it.
    #[test]
    fn moving_an_op_to_another_contract_breaks_its_signature() {
        let (key, id) = peer(1);
        let signed = envelope(
            id,
            kind::GRANT,
            GrantBody {
                member: id,
                rights: Rights::ALL,
            }
            .encode(),
        )
        .sign(&key);
        assert!(signed.verify().is_ok());

        let mut elsewhere = signed;
        elsewhere.payload.scope = Scope([8; 32]);
        assert!(
            elsewhere.verify().is_err(),
            "a grant made on one board must not be replayable onto another"
        );
    }

    /// Two contracts differ if their parameters differ at all — which includes the
    /// salt, so two same-named boards from one owner are still distinct.
    #[test]
    fn a_scope_follows_the_parameters_it_was_derived_from() {
        assert_eq!(Scope::of(b"params"), Scope::of(b"params"));
        assert_ne!(Scope::of(b"params"), Scope::of(b"other params"));
    }
}

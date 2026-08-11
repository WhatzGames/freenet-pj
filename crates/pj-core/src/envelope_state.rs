//! A grow-only set of signed envelopes, and the authority derived from it.
//!
//! This is what a contract stores and merges. It is deliberately the *only* thing
//! in the system that a contract needs to understand, and it understands three op
//! kinds: grant, link device, unlink device. Everything else is bytes it carries.
//!
//! # Convergence
//!
//! The set is keyed by content hash, so merging is a union and is therefore
//! commutative, associative and idempotent — `update_state` gets its required
//! commutativity from the data structure rather than from care.
//!
//! Authority is derived by folding the authority ops in one total order
//! (lamport, wall clock, content hash), so every peer holding the same set
//! derives the same rights. Grants are capped by the granter's own rights via
//! intersection, which is itself order-independent.
//!
//! # Two passes, on purpose
//!
//! Authority is computed over *all* authority ops before any application op is
//! judged. Judging each op against the authority as of its own lamport would make
//! acceptance depend on the order ops happened to be written in, so an op that
//! arrived before the grant that permits it would be rejected forever — even
//! though both are in the set. Two passes means the set alone determines the
//! answer.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::envelope::{DeviceBody, GrantBody, Scope, SignedEnvelope, kind};
use crate::error::{Error, Result};
use crate::ids::{MemberId, OpId};
use crate::rights::Rights;

/// Everything anybody has ever validly written, keyed by content hash.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeState {
    pub ops: BTreeMap<OpId, SignedEnvelope>,
}

/// An organization whose grants a contract honours in addition to its own.
///
/// # Why the scope rule has a hole in it, and why this is the shape of the hole
///
/// [`Scope`] exists to stop a signature written for one contract being replayed
/// into another. That is exactly right for a board: an admin's grant there must
/// not appear anywhere else.
///
/// It is exactly wrong for the question "is this person in my organization?". A
/// contract cannot read another contract's state, so the only way it can answer is
/// from a signature chain carried in its own state — and a certificate is only
/// useful if it can be *presented*, at contracts nobody had in mind when it was
/// signed. Replay is the feature.
///
/// So an org-scoped grant crosses, and nothing else does:
///
/// - Only [`kind::GRANT`] crosses. Application ops, device links and everything
///   else stay bound to one instance.
/// - Only into a contract naming *this* org. A certificate from another org is
///   [`Error::MisdirectedOp`], the same as any other misdirected op.
/// - Instance-scoped grants are untouched, so board rights stay board rights —
///   which is what keeps "you may only change status on boards you have access
///   to" true.
///
/// The alternative was seeding a task's grants from its board's membership at
/// creation. It cannot work: a snapshot cannot enfranchise someone who joins
/// later, and there is no reverse index to push new grants over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Org {
    /// Scope of the organization's own contract — what its grants are signed for.
    pub scope: Scope,
    /// The founder, from the org contract's parameters: the root of the chain.
    pub owner: MemberId,
}

/// Where a contract roots trust, and whose grants it will honour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trust {
    /// This contract instance. Ops must be written for it…
    pub scope: Scope,
    /// …and this key holds [`Rights::ALL`] here, from the parameters.
    pub owner: MemberId,
    /// …unless they are certificates from this org. See [`Org`].
    pub org: Option<Org>,
}

impl Trust {
    /// A contract that honours only its own ops — every contract but a task.
    pub fn instance(scope: Scope, owner: MemberId) -> Self {
        Self {
            scope,
            owner,
            org: None,
        }
    }

    /// …and also certificates from `org`.
    pub fn under(scope: Scope, owner: MemberId, org: Org) -> Self {
        Self {
            scope,
            owner,
            org: Some(org),
        }
    }

    /// Whether an op written for some scope may appear in this contract's state at
    /// all. Checked before the signature, because a perfectly valid signature for
    /// somewhere else is precisely what is being refused.
    fn admits(&self, op: &SignedEnvelope) -> bool {
        if op.payload.scope == self.scope {
            return true;
        }
        op.payload.kind == kind::GRANT && self.org.is_some_and(|org| op.payload.scope == org.scope)
    }
}

/// Who may do what, as derived from the authority ops.
///
/// # Two answers, because there are two questions
///
/// *What may this person do now?* is last-writer-wins: a removal is a grant of
/// nothing and it takes effect. That is [`Authority::held`].
///
/// *Was this op allowed when it was written?* has to be monotone. Ops are never
/// deleted, so if losing a right retroactively invalidated everything that right
/// had authorised, then removing a member would make the state permanently
/// unacceptable — the removal op itself could not be applied, because applying it
/// invalidates the history sitting beside it. That is [`Authority::ever_held`],
/// the union of everything ever conferred.
///
/// Both are derived from the same ops in the same order, so both converge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Authority {
    /// Rights held right now by each *person* (a primary key, never a device).
    pub held: BTreeMap<MemberId, Rights>,
    /// Rights ever held. Used to judge existing ops, never to permit new action.
    pub ever_held: BTreeMap<MemberId, Rights>,
    /// Device key to the person it currently acts for.
    pub person_of: BTreeMap<MemberId, MemberId>,
    /// Device key to the person it ever acted for, for the same reason as
    /// `ever_held`: revoking a device must not invalidate what it already wrote.
    pub ever_person_of: BTreeMap<MemberId, MemberId>,
}

impl Authority {
    /// The person behind a key: itself, unless it is a linked device.
    pub fn person(&self, key: &MemberId) -> MemberId {
        self.person_of.get(key).copied().unwrap_or(*key)
    }

    /// What a key may do now, following a device link to its owner.
    pub fn rights_of(&self, key: &MemberId) -> Rights {
        self.held
            .get(&self.person(key))
            .copied()
            .unwrap_or(Rights::NONE)
    }

    /// The person behind a key, counting links since revoked. Only for judging
    /// ops already written — a device losing its link must not retroactively
    /// orphan the work it did.
    pub fn ever_person(&self, key: &MemberId) -> MemberId {
        self.ever_person_of.get(key).copied().unwrap_or(*key)
    }

    /// What a key was ever entitled to. Only for judging ops already written.
    pub fn ever_rights_of(&self, key: &MemberId) -> Rights {
        self.ever_held
            .get(&self.ever_person(key))
            .copied()
            .unwrap_or(Rights::NONE)
    }

    /// Everyone who has ever been on the board, including those since removed.
    ///
    /// The membership list is drawn from this rather than from [`Self::held`], so
    /// a removed person's name still renders on the work they did instead of
    /// collapsing to a hex prefix.
    pub fn ever_members(&self) -> BTreeSet<MemberId> {
        self.ever_held.keys().copied().collect()
    }

    pub fn members(&self) -> BTreeSet<MemberId> {
        self.held
            .iter()
            .filter(|(_, rights)| !rights.is_empty())
            .map(|(member, _)| *member)
            .collect()
    }
}

impl EnvelopeState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_ops(ops: impl IntoIterator<Item = SignedEnvelope>) -> Self {
        let mut state = Self::new();
        for op in ops {
            state.ops.insert(op.id(), op);
        }
        state
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("EnvelopeState is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::new());
        }
        bincode::deserialize(bytes).map_err(|e| Error::decode("envelope state", e))
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Unions `other` in, returning how many ops were new.
    pub fn merge(&mut self, other: EnvelopeState) -> usize {
        let before = self.ops.len();
        for (id, op) in other.ops {
            self.ops.entry(id).or_insert(op);
        }
        self.ops.len() - before
    }

    /// Every op in the one total order the whole system agrees on.
    pub fn ordered(&self) -> Vec<&SignedEnvelope> {
        let mut ops: Vec<&SignedEnvelope> = self.ops.values().collect();
        ops.sort_by_key(|op| op.order_key());
        ops
    }

    /// Application ops in order, for a client fold. Authority ops are excluded
    /// because they are not the application's business.
    pub fn application_ops(&self) -> Vec<&SignedEnvelope> {
        self.ordered()
            .into_iter()
            .filter(|op| !op.payload.is_authority())
            .collect()
    }

    /// Derives who may do what.
    ///
    /// `owner` comes from the contract's immutable parameters and is the root of
    /// trust: it holds [`Rights::ALL`], including bits invented after the board
    /// was created, so an owner is never short of a permission that did not exist
    /// when they started.
    pub fn authority(&self, owner: MemberId) -> Authority {
        self.fold_authority(owner, None)
    }

    /// Derives who may do what, honouring any org certificates `trust` admits.
    pub fn authority_in(&self, trust: &Trust) -> Authority {
        self.fold_authority(trust.owner, trust.org)
    }

    /// The one fold both of the above are.
    ///
    /// It takes the roots rather than the whole [`Trust`] because it has no use for
    /// a scope: [`Self::validate_in`] has already established that every op in the
    /// set was addressed somewhere this contract accepts, so a certificate that
    /// reached here is one to be counted. Passing a `Trust` would mean handing it a
    /// scope it must be trusted to ignore.
    ///
    /// All an org adds is a second root — its founder holds [`Rights::ALL`], so a
    /// chain starting from them means something.
    fn fold_authority(&self, owner: MemberId, org: Option<Org>) -> Authority {
        let mut authority = Authority::default();
        authority.held.insert(owner, Rights::ALL);
        authority.ever_held.insert(owner, Rights::ALL);
        if let Some(org) = org {
            // Not `insert` on top of the owner: a task created by the founder has
            // the same key in both roles, and clobbering would be harmless only by
            // accident.
            authority.held.entry(org.owner).or_insert(Rights::ALL);
            authority.ever_held.entry(org.owner).or_insert(Rights::ALL);
        }

        for op in self.ordered() {
            if !op.payload.is_authority() {
                continue;
            }
            let author = op.author();
            // A device acts with exactly its owner's rights.
            let acting_as = authority.person(&author);
            let author_rights = authority
                .held
                .get(&acting_as)
                .copied()
                .unwrap_or(Rights::NONE);

            match op.payload.kind {
                kind::GRANT => {
                    let Ok(body) = GrantBody::decode(&op.payload.body) else {
                        continue;
                    };
                    // Never a root of trust: the parameters say who those are, and
                    // a grant must not be able to demote one. Both roots are
                    // protected, or a task-scoped grant could strip the org's
                    // founder of the standing the org contract gave them.
                    if body.member == owner || org.is_some_and(|org| body.member == org.owner) {
                        continue;
                    }

                    // Renouncing your own rights is not an exercise of authority
                    // over anybody, so it needs none. Without this, leaving would
                    // be something only an admin could do for you.
                    let resigning = body.member == acting_as && body.rights.is_empty();

                    let conferred = if resigning {
                        Rights::NONE
                    } else {
                        if !author_rights.contains(Rights::MAY_GRANT) {
                            continue;
                        }
                        // Unmaking an administrator is the same power as making
                        // one. Intersection alone cannot express this, because a
                        // removal is a grant of nothing and nothing is within
                        // anybody's cap — so without this an admin could purge
                        // their peers.
                        let target_is_admin = authority
                            .held
                            .get(&body.member)
                            .copied()
                            .unwrap_or(Rights::NONE)
                            .contains(Rights::MAY_GRANT);
                        if target_is_admin && !author_rights.contains(Rights::MAY_APPOINT) {
                            continue;
                        }
                        // Capped at what the granter holds. Intersection is
                        // order-independent, which is what keeps this convergent.
                        let asked = body.rights.intersect(author_rights);
                        // …and an admin cannot hand on the power to appoint
                        // admins unless they were given it. See `MAY_APPOINT`.
                        if author_rights.contains(Rights::MAY_APPOINT) {
                            asked
                        } else {
                            asked
                                .without(Rights::MAY_GRANT)
                                .without(Rights::MAY_APPOINT)
                        }
                    };

                    authority.held.insert(body.member, conferred);
                    let ever = authority
                        .ever_held
                        .get(&body.member)
                        .copied()
                        .unwrap_or(Rights::NONE);
                    authority
                        .ever_held
                        .insert(body.member, ever.union(conferred));
                }

                kind::LINK_DEVICE => {
                    if author_rights.is_empty() {
                        continue;
                    }
                    let Ok(body) = DeviceBody::decode(&op.payload.body) else {
                        continue;
                    };
                    // A device cannot itself be a person with rights, and cannot
                    // be re-pointed at someone else once claimed.
                    if body.device != acting_as && !authority.person_of.contains_key(&body.device) {
                        authority.person_of.insert(body.device, acting_as);
                        authority
                            .ever_person_of
                            .entry(body.device)
                            .or_insert(acting_as);
                    }
                }

                kind::UNLINK_DEVICE => {
                    let Ok(body) = DeviceBody::decode(&op.payload.body) else {
                        continue;
                    };
                    // Only the person who vouched for the device, or the device
                    // itself. Checkable without consulting authority, which
                    // avoids a device revoking the link that grants it standing.
                    let voucher = authority.person_of.get(&body.device).copied();
                    if voucher == Some(acting_as) || author == body.device {
                        authority.person_of.remove(&body.device);
                    }
                }

                _ => {}
            }
        }

        authority
    }

    /// Whether every op in the set was written for this contract, is signed by its
    /// stated author, and is permitted.
    ///
    /// This is the whole of a contract's policy.
    pub fn validate(&self, scope: Scope, owner: MemberId) -> Result<()> {
        self.validate_in(&Trust::instance(scope, owner))
    }

    /// As [`Self::validate`], for a contract that also honours org certificates.
    pub fn validate_in(&self, trust: &Trust) -> Result<()> {
        for op in self.ops.values() {
            // Before the signature, because a valid signature for somewhere else is
            // exactly the thing being refused.
            if !trust.admits(op) {
                return Err(Error::MisdirectedOp { id: op.id() });
            }
            op.verify()?;
        }
        let authority = self.authority_in(trust);
        for op in self.ops.values() {
            if op.payload.is_authority() {
                // What an authority op *confers* is decided by the fold above,
                // which ignores one its author could not make — keeping it means an
                // op that becomes meaningful later is not lost.
                //
                // But an author who has never held anything can never become
                // meaningful, and accepting those would let any key on the network
                // grow any contract's state for free. `ever_`, so this is monotone:
                // more ops can only ever make an author qualify, never disqualify
                // one, and a state a peer accepted cannot later be rejected.
                if authority.ever_rights_of(&op.author()).is_empty() {
                    return Err(Error::Unauthorized {
                        id: op.id(),
                        author: op.author(),
                    });
                }
                continue;
            }
            // `ever`, not `held`: see the note on `Authority`. Judging history by
            // present rights would make removing anyone impossible.
            if !authority.ever_rights_of(&op.author()).contains(op.needs()) {
                return Err(Error::Unauthorized {
                    id: op.id(),
                    author: op.author(),
                });
            }
        }
        Ok(())
    }

    /// Merges `incoming` if all of it is acceptable, and rejects all of it
    /// otherwise.
    ///
    /// All-or-nothing so a peer cannot half-apply an update and diverge from one
    /// that rejected it.
    pub fn accept(
        &mut self,
        incoming: Vec<SignedEnvelope>,
        scope: Scope,
        owner: MemberId,
    ) -> Result<usize> {
        self.accept_in(incoming, &Trust::instance(scope, owner))
    }

    /// As [`Self::accept`], for a contract that also honours org certificates.
    pub fn accept_in(&mut self, incoming: Vec<SignedEnvelope>, trust: &Trust) -> Result<usize> {
        let mut candidate = self.clone();
        candidate.merge(EnvelopeState::from_ops(incoming));
        candidate.validate_in(trust)?;
        let added = candidate.ops.len() - self.ops.len();
        *self = candidate;
        Ok(added)
    }
}

/// Ops one peer has that another does not.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeDelta {
    pub ops: Vec<SignedEnvelope>,
}

/// The ids a peer holds, so it can be sent precisely what it is missing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeSummary {
    pub ids: BTreeSet<OpId>,
}

impl EnvelopeState {
    pub fn summary(&self) -> EnvelopeSummary {
        EnvelopeSummary {
            ids: self.ops.keys().copied().collect(),
        }
    }

    pub fn delta_since(&self, summary: &EnvelopeSummary) -> EnvelopeDelta {
        EnvelopeDelta {
            ops: self
                .ops
                .iter()
                .filter(|(id, _)| !summary.ids.contains(*id))
                .map(|(_, op)| op.clone())
                .collect(),
        }
    }
}

impl EnvelopeDelta {
    pub fn new(ops: Vec<SignedEnvelope>) -> Self {
        Self { ops }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("EnvelopeDelta is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        bincode::deserialize(bytes).map_err(|e| Error::decode("envelope delta", e))
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

impl EnvelopeSummary {
    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("EnvelopeSummary is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        bincode::deserialize(bytes).map_err(|e| Error::decode("envelope summary", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, Stamp};

    /// The contract these tests are pretending to be. Any fixed value does; what
    /// matters is that every op in one test agrees on it.
    const HERE: Scope = Scope([42; 32]);
    use ed25519_dalek::SigningKey;

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
            let lamport = self.lamport;
            self.sign_at(lamport, needs, kind, body)
        }

        /// Signs at an explicit lamport.
        ///
        /// Needed wherever a test depends on the order of two peers' ops: with
        /// per-peer counters they collide, the tie falls to the content hash, and
        /// the test passes or fails on the value of a signature.
        fn sign_at(
            &mut self,
            lamport: u64,
            needs: Rights,
            kind: u16,
            body: Vec<u8>,
        ) -> SignedEnvelope {
            self.lamport = self.lamport.max(lamport);
            // Truncation is the point: the nonce only has to distinguish ops of
            // one peer within a test, and every test's lamports are small.
            let nonce = lamport.to_le_bytes()[0];
            let stamp = Stamp::new(HERE, self.id, lamport, 0, [nonce; 16]);
            Envelope::stamped(stamp, needs, kind, body).sign(&self.key)
        }

        fn grant(&mut self, to: MemberId, rights: Rights) -> SignedEnvelope {
            let body = GrantBody { member: to, rights }.encode();
            self.sign(Rights::MAY_GRANT, kind::GRANT, body)
        }

        fn grant_at(&mut self, lamport: u64, to: MemberId, rights: Rights) -> SignedEnvelope {
            let body = GrantBody { member: to, rights }.encode();
            self.sign_at(lamport, Rights::MAY_GRANT, kind::GRANT, body)
        }

        fn task(&mut self) -> SignedEnvelope {
            self.sign(
                Rights::WRITE_TASKS,
                kind::FIRST_APPLICATION_KIND,
                b"a task".to_vec(),
            )
        }

        /// Signs for somewhere other than [`HERE`]: an org certificate when
        /// `scope` is an org's, and a misdirected op when it is anything else.
        fn sign_for(
            &mut self,
            scope: Scope,
            needs: Rights,
            kind: u16,
            body: Vec<u8>,
        ) -> SignedEnvelope {
            self.lamport += 1;
            let nonce = self.lamport.to_le_bytes()[0];
            let stamp = Stamp::new(scope, self.id, self.lamport, 0, [nonce; 16]);
            Envelope::stamped(stamp, needs, kind, body).sign(&self.key)
        }

        fn grant_for(&mut self, scope: Scope, to: MemberId, rights: Rights) -> SignedEnvelope {
            let body = GrantBody { member: to, rights }.encode();
            self.sign_for(scope, Rights::MAY_GRANT, kind::GRANT, body)
        }
    }

    /// An organization's own contract, whose grants travel as certificates.
    const ORG: Scope = Scope([7; 32]);
    /// Somebody else's organization.
    const OTHER_ORG: Scope = Scope([8; 32]);
    /// Another board — a scope nothing here should ever admit.
    const ELSEWHERE: Scope = Scope([9; 32]);

    /// The whole point of the exception: someone the task itself never granted
    /// anything, and who may not even have existed when it was created, can write —
    /// on the strength of a certificate copied in from the org.
    #[test]
    fn an_org_certificate_enfranchises_someone_the_task_never_granted() {
        let owner = Peer::new(1);
        let mut founder = Peer::new(2);
        let mut member = Peer::new(3);

        let certificate = founder.grant_for(ORG, member.id, Rights::MEMBER);
        let work = member.task();

        let org = Org {
            scope: ORG,
            owner: founder.id,
        };
        let mut state = EnvelopeState::new();
        state
            .accept_in(
                vec![certificate.clone(), work.clone()],
                &Trust::under(HERE, owner.id, org),
            )
            .expect("must be accepted: the certificate is from the named org");

        assert_eq!(
            state
                .authority_in(&Trust::under(HERE, owner.id, org))
                .rights_of(&member.id),
            Rights::MEMBER
        );

        // And without the org in the trust, the same two ops are refused — the
        // certificate is not admitted, so it cannot enfranchise anybody.
        assert!(
            EnvelopeState::new()
                .accept_in(vec![certificate, work], &Trust::instance(HERE, owner.id))
                .is_err(),
            "a contract that names no org must not honour certificates"
        );
    }

    #[test]
    fn a_certificate_from_another_org_is_misdirected() {
        let owner = Peer::new(1);
        let mut founder = Peer::new(2);
        let member = Peer::new(3);

        let certificate = founder.grant_for(OTHER_ORG, member.id, Rights::MEMBER);
        let err = EnvelopeState::new()
            .accept_in(
                vec![certificate],
                &Trust::under(
                    HERE,
                    owner.id,
                    Org {
                        scope: ORG,
                        owner: founder.id,
                    },
                ),
            )
            .expect_err("a certificate for another org must not be admitted");
        assert!(matches!(err, Error::MisdirectedOp { .. }), "got {err:?}");
    }

    /// The hole is one kind wide. An application op scoped to the org is still an
    /// op written for somewhere else.
    #[test]
    fn only_grants_cross_between_scopes() {
        let owner = Peer::new(1);
        let mut founder = Peer::new(2);

        let work = founder.sign_for(
            ORG,
            Rights::WRITE_TASKS,
            kind::FIRST_APPLICATION_KIND,
            b"work".to_vec(),
        );
        let link = founder.sign_for(
            ORG,
            Rights::NONE,
            kind::LINK_DEVICE,
            DeviceBody { device: owner.id }.encode(),
        );

        let trust = Trust::under(
            HERE,
            owner.id,
            Org {
                scope: ORG,
                owner: founder.id,
            },
        );
        for op in [work, link] {
            let kind = op.kind();
            assert!(
                matches!(
                    EnvelopeState::new().accept_in(vec![op], &trust),
                    Err(Error::MisdirectedOp { .. })
                ),
                "kind {kind} must not cross scopes"
            );
        }
    }

    /// Widening the rule for org certificates must not have widened it for
    /// anything else: another board's grants are refused exactly as before.
    #[test]
    fn admitting_certificates_does_not_admit_other_contracts_grants() {
        let owner = Peer::new(1);
        let mut founder = Peer::new(2);
        let member = Peer::new(3);

        let from_another_board = founder.grant_for(ELSEWHERE, member.id, Rights::ALL);
        assert!(
            matches!(
                EnvelopeState::new().accept_in(
                    vec![from_another_board],
                    &Trust::under(
                        HERE,
                        owner.id,
                        Org {
                            scope: ORG,
                            owner: founder.id,
                        },
                    ),
                ),
                Err(Error::MisdirectedOp { .. })
            ),
            "a grant from another board must not be replayable into a task"
        );
    }

    /// A certificate is an ordinary grant that happens to travel, so every cap on
    /// an ordinary grant still applies to it.
    #[test]
    fn a_certificate_chain_is_capped_like_any_other_grant() {
        let owner = Peer::new(1);
        let mut founder = Peer::new(2);
        let mut admin = Peer::new(3);
        let member = Peer::new(4);

        let org = Org {
            scope: ORG,
            owner: founder.id,
        };
        let trust = Trust::under(HERE, owner.id, org);

        let appoint = founder.grant_for(ORG, admin.id, Rights::ADMIN);
        let onward = admin.grant_for(ORG, member.id, Rights::ADMIN);

        let mut state = EnvelopeState::new();
        state
            .accept_in(vec![appoint, onward], &trust)
            .expect("must be accepted: both authors are authorised");

        let authority = state.authority_in(&trust);
        assert_eq!(authority.rights_of(&admin.id), Rights::ADMIN);
        assert!(
            !authority.rights_of(&member.id).contains(Rights::MAY_GRANT),
            "an org admin without MAY_APPOINT cannot pass on the power to grant, \
             here as anywhere else"
        );
    }

    /// Two roots, both protected. Without this a grant written on the task could
    /// strip the org's founder of the standing the org contract gave them.
    #[test]
    fn a_grant_cannot_demote_either_root_of_trust() {
        let owner = Peer::new(1);
        let mut founder = Peer::new(2);
        let mut admin = Peer::new(3);

        let org = Org {
            scope: ORG,
            owner: founder.id,
        };
        let trust = Trust::under(HERE, owner.id, org);

        let appoint = founder.grant_for(ORG, admin.id, Rights::ALL);
        let demote_owner = admin.grant_for(ORG, owner.id, Rights::NONE);
        let demote_founder = admin.grant_for(ORG, founder.id, Rights::NONE);

        let mut state = EnvelopeState::new();
        state
            .accept_in(vec![appoint, demote_owner, demote_founder], &trust)
            .expect("kept, and ignored: an op that confers nothing is not invalid");

        let authority = state.authority_in(&trust);
        assert_eq!(authority.rights_of(&owner.id), Rights::ALL);
        assert_eq!(authority.rights_of(&founder.id), Rights::ALL);
    }

    /// A task created by the founder has one key in both roles. It must still hold
    /// everything, and the two seedings must not fight.
    #[test]
    fn one_key_can_be_both_roots_at_once() {
        let founder = Peer::new(1);
        let trust = Trust::under(
            HERE,
            founder.id,
            Org {
                scope: ORG,
                owner: founder.id,
            },
        );
        assert_eq!(
            EnvelopeState::new()
                .authority_in(&trust)
                .rights_of(&founder.id),
            Rights::ALL
        );
    }

    #[test]
    fn the_owner_holds_everything_and_a_stranger_holds_nothing() {
        let owner = Peer::new(1);
        let stranger = Peer::new(9);
        let authority = EnvelopeState::new().authority(owner.id);

        assert_eq!(authority.rights_of(&owner.id), Rights::ALL);
        assert_eq!(authority.rights_of(&stranger.id), Rights::NONE);
    }

    #[test]
    fn a_stranger_cannot_write_but_a_granted_member_can() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);

        let mut state = EnvelopeState::new();
        assert!(state.accept(vec![member.task()], HERE, owner.id).is_err());

        let grant = owner.grant(member.id, Rights::MEMBER);
        state
            .accept(vec![grant], HERE, owner.id)
            .expect("must be accepted: the fixture is authorised");
        state
            .accept(vec![member.task()], HERE, owner.id)
            .expect("must be accepted: the fixture is authorised");
    }

    #[test]
    fn a_grant_confers_no_more_than_the_granter_holds() {
        let mut owner = Peer::new(1);
        let mut middle = Peer::new(2);
        let outsider = Peer::new(3);

        // Explicit lamports: this test is entirely about the order of two peers'
        // ops, and per-peer counters would collide and decide it by hash.
        let mut state = EnvelopeState::new();
        // A plain member may write but not grant.
        state
            .accept(
                vec![owner.grant_at(1, middle.id, Rights::MEMBER)],
                HERE,
                owner.id,
            )
            .expect("must be accepted: the fixture is authorised");
        // So their attempt to appoint anyone is ignored by the fold.
        state
            .accept(
                vec![middle.grant_at(2, outsider.id, Rights::ALL)],
                HERE,
                owner.id,
            )
            .expect("must be accepted: the fixture is authorised");
        assert_eq!(
            state.authority(owner.id).rights_of(&outsider.id),
            Rights::NONE
        );

        // An admin may invite, but cannot exceed themselves, and cannot pass on
        // the power to invite — that takes `MAY_APPOINT`, which only the owner has.
        state
            .accept(
                vec![owner.grant_at(3, middle.id, Rights::ADMIN)],
                HERE,
                owner.id,
            )
            .expect("must be accepted: the fixture is authorised");
        state
            .accept(
                vec![middle.grant_at(4, outsider.id, Rights::ALL)],
                HERE,
                owner.id,
            )
            .expect("must be accepted: the fixture is authorised");
        let granted = state.authority(owner.id).rights_of(&outsider.id);
        assert_eq!(granted, Rights::ADMIN.without(Rights::MAY_GRANT));
        assert!(
            !granted.contains(Rights(1 << 42)),
            "cannot exceed the granter"
        );

        // Given `MAY_APPOINT`, the same admin can now make another one.
        state
            .accept(
                vec![owner.grant_at(5, middle.id, Rights::ADMIN.union(Rights::MAY_APPOINT))],
                HERE,
                owner.id,
            )
            .expect("must be accepted: the fixture is authorised");
        state
            .accept(
                vec![middle.grant_at(6, outsider.id, Rights::ADMIN)],
                HERE,
                owner.id,
            )
            .expect("must be accepted: the fixture is authorised");
        assert!(
            state
                .authority(owner.id)
                .rights_of(&outsider.id)
                .contains(Rights::MAY_GRANT)
        );
    }

    /// Anyone may give up their own rights, whatever they hold.
    #[test]
    fn resigning_needs_no_permission_but_only_works_on_yourself() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);
        let victim = Peer::new(3);

        let mut state = EnvelopeState::from_ops([
            owner.grant_at(1, member.id, Rights::MEMBER),
            owner.grant_at(2, victim.id, Rights::MEMBER),
        ]);

        // A plain member has no MAY_GRANT, and still gets to leave.
        state
            .accept(
                vec![member.grant_at(3, member.id, Rights::NONE)],
                HERE,
                owner.id,
            )
            .expect("must be accepted: the fixture is authorised");
        assert_eq!(
            state.authority(owner.id).rights_of(&member.id),
            Rights::NONE
        );

        // What they cannot do is throw somebody else out.
        state
            .accept(
                vec![member.grant_at(4, victim.id, Rights::NONE)],
                HERE,
                owner.id,
            )
            .expect("must be accepted: the fixture is authorised");
        assert_eq!(
            state.authority(owner.id).rights_of(&victim.id),
            Rights::MEMBER
        );
    }

    #[test]
    fn removal_is_a_grant_of_nothing() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);

        let mut state = EnvelopeState::from_ops([owner.grant(member.id, Rights::MEMBER)]);
        let work = member.task();
        state
            .accept(vec![work.clone()], HERE, owner.id)
            .expect("must be accepted: the fixture is authorised");

        state
            .accept(vec![owner.grant(member.id, Rights::NONE)], HERE, owner.id)
            .expect("must be accepted: the fixture is authorised");
        assert_eq!(
            state.authority(owner.id).rights_of(&member.id),
            Rights::NONE
        );

        // The removal had to be acceptable *while their work sat beside it*.
        // Judging that work by their present rights would have made removing
        // anyone impossible — the removal invalidates the history it arrives
        // with, so no peer could ever apply it.
        assert!(state.validate(HERE, owner.id).is_ok());
        assert!(state.ops.contains_key(&work.id()), "history is kept");

        // But they may not write again.
        let after = member.task();
        assert!(
            state
                .authority(owner.id)
                .rights_of(&member.id)
                .contains(after.needs())
                .eq(&false)
        );
    }

    #[test]
    fn the_owner_cannot_be_demoted() {
        let mut owner = Peer::new(1);
        let mut admin = Peer::new(2);

        let mut state = EnvelopeState::from_ops([owner.grant(admin.id, Rights::ADMIN)]);
        state
            .accept(vec![admin.grant(owner.id, Rights::NONE)], HERE, owner.id)
            .expect("must be accepted: the fixture is authorised");

        assert_eq!(state.authority(owner.id).rights_of(&owner.id), Rights::ALL);
    }

    #[test]
    fn a_device_acts_as_its_owner_and_can_be_revoked() {
        let mut owner = Peer::new(1);
        let mut laptop = Peer::new(5);

        let link = owner.sign(
            Rights::NONE,
            kind::LINK_DEVICE,
            DeviceBody { device: laptop.id }.encode(),
        );
        let mut state = EnvelopeState::from_ops([link]);
        assert_eq!(state.authority(owner.id).rights_of(&laptop.id), Rights::ALL);

        // The device may therefore write.
        state
            .accept(vec![laptop.task()], HERE, owner.id)
            .expect("must be accepted: the fixture is authorised");

        let unlink = owner.sign(
            Rights::NONE,
            kind::UNLINK_DEVICE,
            DeviceBody { device: laptop.id }.encode(),
        );
        state.merge(EnvelopeState::from_ops([unlink]));
        assert_eq!(
            state.authority(owner.id).rights_of(&laptop.id),
            Rights::NONE
        );
    }

    #[test]
    fn a_stranger_cannot_link_themselves_in() {
        let owner = Peer::new(1);
        let mut stranger = Peer::new(9);

        let link = stranger.sign(
            Rights::NONE,
            kind::LINK_DEVICE,
            DeviceBody {
                device: stranger.id,
            }
            .encode(),
        );
        let state = EnvelopeState::from_ops([link]);
        assert_eq!(
            state.authority(owner.id).rights_of(&stranger.id),
            Rights::NONE
        );
    }

    #[test]
    fn merging_is_commutative_and_idempotent() {
        let mut owner = Peer::new(1);
        let a = owner.grant(MemberId([2; 32]), Rights::MEMBER);
        let b = owner.grant(MemberId([3; 32]), Rights::MEMBER);

        let mut left = EnvelopeState::from_ops([a.clone()]);
        left.merge(EnvelopeState::from_ops([b.clone()]));
        let mut right = EnvelopeState::from_ops([b.clone()]);
        right.merge(EnvelopeState::from_ops([a.clone()]));
        assert_eq!(left, right);

        let snapshot = left.clone();
        assert_eq!(left.merge(EnvelopeState::from_ops([a, b])), 0);
        assert_eq!(left, snapshot);
    }

    #[test]
    fn authority_does_not_depend_on_the_order_ops_arrive_in() {
        let mut owner = Peer::new(1);
        let mut admin = Peer::new(2);
        let outsider = MemberId([7; 32]);

        let promote = owner.grant(admin.id, Rights::ADMIN);
        let onward = admin.grant(outsider, Rights::MEMBER);

        let forwards = EnvelopeState::from_ops([promote.clone(), onward.clone()]);
        let backwards = EnvelopeState::from_ops([onward, promote]);

        assert_eq!(
            forwards.authority(owner.id).rights_of(&outsider),
            backwards.authority(owner.id).rights_of(&outsider)
        );
    }

    /// An op whose grant arrives "after" it must still be accepted — the set,
    /// not the arrival order, decides.
    #[test]
    fn an_op_predating_its_own_grant_is_still_valid() {
        let mut owner = Peer::new(1);
        let mut member = Peer::new(2);

        // The member writes at lamport 1; the grant is signed later.
        let work = member.task();
        let grant = owner.grant(member.id, Rights::MEMBER);

        let state = EnvelopeState::from_ops([work, grant]);
        assert!(state.validate(HERE, owner.id).is_ok());
    }

    /// The property the whole design exists for.
    #[test]
    fn an_unknown_application_kind_is_carried_not_rejected() {
        let mut owner = Peer::new(1);
        let from_the_future = owner.sign(Rights::WRITE_TASKS, 4095, vec![1, 2, 3]);

        let state = EnvelopeState::from_ops([from_the_future.clone()]);
        assert!(
            state.validate(HERE, owner.id).is_ok(),
            "carried, not refused"
        );

        // And it survives a round trip through storage untouched.
        let read_back = EnvelopeState::decode(&state.encode())
            .expect("must decode: this test produced the bytes");
        assert_eq!(read_back, state);
        assert_eq!(read_back.application_ops().len(), 1);
    }

    #[test]
    fn accept_is_all_or_nothing() {
        let mut owner = Peer::new(1);
        let mut stranger = Peer::new(9);

        let mut state = EnvelopeState::new();
        let good = owner.grant(MemberId([4; 32]), Rights::MEMBER);
        let bad = stranger.task();

        assert!(state.accept(vec![good, bad], HERE, owner.id).is_err());
        assert!(state.is_empty(), "nothing was applied");
    }

    #[test]
    fn a_forged_signature_is_refused() {
        let owner = Peer::new(1);
        let mut member = Peer::new(2);

        let mut forged = member.task();
        forged.payload.body = b"tampered".to_vec();

        let mut state = EnvelopeState::new();
        assert!(state.accept(vec![forged], HERE, owner.id).is_err());
    }

    #[test]
    fn encoding_round_trips_and_an_empty_state_decodes() {
        let mut owner = Peer::new(1);
        let state = EnvelopeState::from_ops([owner.grant(MemberId([2; 32]), Rights::MEMBER)]);
        assert_eq!(
            EnvelopeState::decode(&state.encode())
                .expect("must decode: this test produced the bytes"),
            state
        );
        assert!(
            EnvelopeState::decode(&[])
                .expect("must decode: this test produced the bytes")
                .is_empty()
        );
    }
}

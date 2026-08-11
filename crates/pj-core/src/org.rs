//! Organizations: shared ownership of projects by a group rather than a person.
//!
//! An organization is its own Freenet contract, built on the same op set as
//! everything else here — an [`EnvelopeState`] of signed envelopes keyed by content
//! hash, merged by union, with authority derived from the three op kinds a contract
//! understands. What differs from a board is only the application ops.
//!
//! # Authority
//!
//! Rooted at [`OrgParameters::founder`], which lives in the contract parameters and
//! therefore in the organization's address: it can never change. The founder holds
//! [`Rights::ALL`], which includes [`Rights::MAY_APPOINT`] — so the founder alone
//! can create admins, while admins can only bring in members.
//!
//! That two-tier limit is deliberate. If admins could promote admins, demoting one
//! would raise the question of what happens to everyone they promoted, and a CRDT
//! has no ordering with which to answer it. `MAY_APPOINT` is what keeps the closure
//! two levels deep, and it is enforced in the shared authority fold rather than
//! here.
//!
//! # Membership and projects
//!
//! Joining is by invitation: an admin grants you rights by public key. There is no
//! self-service join op, which means the contract never has to accept a write from
//! a key it does not already know — no request queue, and no spam surface.
//!
//! Leaving is self-service, and needs no special op: a grant of [`Rights::NONE`] to
//! yourself is a resignation, and the authority fold permits it from anybody
//! because renouncing your own rights exercises authority over nobody.
//!
//! Being in an organization grants nothing on its projects by itself. Members are
//! assigned to individual projects, and one member may be assigned to as many as
//! they like — a project's own grants decide that.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::envelope::{Envelope, Scope, Stamp, kind};
use crate::envelope_state::{Authority, EnvelopeState};
use crate::error::{Error, Result};
use crate::ids::{BoardId, MemberId, OrgId};
use crate::rights::{Rights, Role};

/// The immutable identity of an organization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgParameters {
    /// The founding key: the root of all authority here, and unchangeable because
    /// it is hashed into the organization's address.
    pub founder: MemberId,
    pub name: String,
    /// Random, so the same founder can create two organizations with one name.
    pub salt: [u8; 16],
}

impl OrgParameters {
    pub fn new(founder: MemberId, name: impl Into<String>, salt: [u8; 16]) -> Self {
        Self {
            founder,
            name: name.into(),
            salt,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("OrgParameters is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("OrgParameters", e))
    }

    /// What ops written here are signed against, so an op made on one organization
    /// cannot be replayed onto another — or onto a board. See [`Scope`].
    pub fn scope(&self) -> Scope {
        Scope::of(&self.encode())
    }
}

/// A change to an organization.
///
/// Membership is absent, and so is leaving: both are grants, which the contract
/// reads. What is here is the part it has no opinion about, carried as opaque bytes
/// so that adding to this list never moves the organization's address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrgOp {
    /// What to call somebody. Presentation, so it travels as an ordinary op —
    /// renaming yourself must not require the authority to change membership.
    SetMemberName {
        member: MemberId,
        name: String,
    },
    /// Record a project as belonging to this organization.
    AddProject {
        board: BoardId,
        name: String,
    },
    RemoveProject {
        board: BoardId,
    },
}

impl OrgOp {
    pub fn needs(&self) -> Rights {
        match self {
            // Enough to name *yourself*; naming anyone else additionally takes
            // `ADMINISTER`, which the fold checks because it depends on who is
            // being named.
            OrgOp::SetMemberName { .. } => Rights::SET_NAME,
            OrgOp::AddProject { .. } | OrgOp::RemoveProject { .. } => Rights::ADMINISTER,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("OrgOp is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("org op", e))
    }

    /// Takes `self` for the same reason [`crate::op::Op::envelope`] does.
    pub fn envelope(self, stamp: Stamp) -> Envelope {
        Envelope::stamped(
            stamp,
            self.needs(),
            kind::FIRST_APPLICATION_KIND,
            self.encode(),
        )
    }
}

/// A write to an organization waiting for a clock — see [`crate::op::Draft`], of
/// which this is the organization's equivalent.
pub enum OrgDraft {
    Op(OrgOp),
    Grant { member: MemberId, rights: Rights },
}

impl OrgDraft {
    pub fn envelope(self, stamp: Stamp) -> Envelope {
        match self {
            OrgDraft::Op(op) => op.envelope(stamp),
            OrgDraft::Grant { member, rights } => Envelope::grant(stamp, member, rights),
        }
    }
}

impl From<OrgOp> for OrgDraft {
    fn from(op: OrgOp) -> Self {
        OrgDraft::Op(op)
    }
}

// ------------------------------------------------------------------ the view

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgMember {
    pub id: MemberId,
    pub name: String,
    /// What they may do now. The authority; `role` reads it as a word.
    pub rights: Rights,
    pub role: Role,
    /// False once removed, or once they left. Kept so their name still resolves.
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgProject {
    pub board: BoardId,
    pub name: String,
}

/// An organization, folded out of its op set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Organization {
    pub members: BTreeMap<MemberId, OrgMember>,
    pub projects: BTreeMap<BoardId, OrgProject>,
    /// Each linked device key mapped to the member it belongs to.
    pub devices: BTreeMap<MemberId, MemberId>,
    pub next_lamport: u64,
    /// Ops this build could not interpret. Carried, not lost.
    pub unreadable_ops: usize,
}

impl Organization {
    pub fn from_state(state: &EnvelopeState, params: &OrgParameters) -> Self {
        let authority = state.authority(params.founder);

        let next_lamport = state
            .ops
            .values()
            .map(|op| op.payload.lamport)
            .max()
            .map_or(1, |max| max.saturating_add(1));

        let mut unreadable_ops = 0;
        let mut names: BTreeMap<MemberId, String> = BTreeMap::new();
        let mut projects: BTreeMap<BoardId, OrgProject> = BTreeMap::new();
        let mut removed_projects: BTreeSet<BoardId> = BTreeSet::new();

        for envelope in state.application_ops() {
            if envelope.payload.kind != kind::FIRST_APPLICATION_KIND {
                unreadable_ops += 1;
                continue;
            }
            let Ok(op) = OrgOp::decode(&envelope.payload.body) else {
                unreadable_ops += 1;
                continue;
            };
            let author = authority.ever_person(&envelope.author());
            let held = authority.ever_rights_of(&author);
            if !held.contains(op.needs()) {
                continue;
            }

            match op {
                OrgOp::SetMemberName { member, name } => {
                    // Yourself always; anybody else only as an administrator.
                    if member != author && !held.contains(Rights::ADMINISTER) {
                        continue;
                    }
                    names.insert(member, name);
                }
                OrgOp::AddProject { board, name } => {
                    projects.insert(board, OrgProject { board, name });
                }
                OrgOp::RemoveProject { board } => {
                    removed_projects.insert(board);
                }
            }
        }

        projects.retain(|board, _| !removed_projects.contains(board));

        let members = fold_members(&authority, params, &names);
        // `person_of` is resolved to a person when the link is folded, so nothing
        // is left to walk here.
        let devices = authority.person_of.clone();

        Self {
            members,
            projects,
            devices,
            next_lamport,
            unreadable_ops,
        }
    }

    /// The member a key belongs to, following a device link if that is what it is.
    pub fn person_of(&self, id: &MemberId) -> MemberId {
        if self.members.contains_key(id) {
            return *id;
        }
        self.devices.get(id).copied().unwrap_or(*id)
    }

    pub fn member_name(&self, id: &MemberId) -> String {
        let person = self.person_of(id);
        self.members
            .get(&person)
            .map_or_else(|| id.short(), |m| m.name.clone())
    }

    pub fn active_members(&self) -> Vec<&OrgMember> {
        self.members.values().filter(|m| m.active).collect()
    }

    /// What a key may do now, following a device link to its owner.
    pub fn rights_of(&self, id: &MemberId) -> Rights {
        self.members
            .get(&self.person_of(id))
            .filter(|member| member.active)
            .map_or(Rights::NONE, |member| member.rights)
    }

    pub fn may(&self, id: &MemberId, rights: Rights) -> bool {
        self.rights_of(id).contains(rights)
    }

    /// Whether this key currently acts with administrative authority.
    pub fn is_admin(&self, id: &MemberId) -> bool {
        self.may(id, Rights::MAY_GRANT)
    }

    pub fn is_member(&self, id: &MemberId) -> bool {
        let person = self.person_of(id);
        self.members.get(&person).is_some_and(|m| m.active)
    }

    /// Everyone who currently holds admin authority, which is who a new project
    /// seeds as its board admins.
    pub fn admins(&self) -> Vec<&OrgMember> {
        self.members
            .values()
            .filter(|m| m.active && m.rights.contains(Rights::MAY_GRANT))
            .collect()
    }

    pub fn projects_sorted(&self) -> Vec<&OrgProject> {
        let mut projects: Vec<&OrgProject> = self.projects.values().collect();
        projects.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.board.cmp(&b.board)));
        projects
    }
}

/// Membership from the authority ops, names from the ordinary ones.
fn fold_members(
    authority: &Authority,
    params: &OrgParameters,
    names: &BTreeMap<MemberId, String>,
) -> BTreeMap<MemberId, OrgMember> {
    let mut members = BTreeMap::new();
    // The founder is a member by definition — no op confers it, and none can take
    // it away, because the parameters are the organization's address.
    for id in authority.ever_members().into_iter().chain([params.founder]) {
        let rights = authority.held.get(&id).copied().unwrap_or(Rights::NONE);
        members.insert(
            id,
            OrgMember {
                id,
                name: names.get(&id).cloned().unwrap_or_else(|| id.short()),
                rights,
                role: Role::of(rights),
                active: !rights.is_empty(),
            },
        );
    }
    members
}

/// The ops that turn an empty organization contract into a usable one.
///
/// Just the founder's name: their authority is in the parameters, so there is no
/// grant to write.
pub fn genesis_ops(founder: MemberId, founder_name: &str) -> Vec<OrgOp> {
    vec![OrgOp::SetMemberName {
        member: founder,
        name: founder_name.to_owned(),
    }]
}

/// Convenience for the id type the registry records organizations under.
pub fn org_id_from_bytes(bytes: &[u8]) -> Option<OrgId> {
    let array: [u8; 32] = bytes.try_into().ok()?;
    Some(OrgId(array))
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;
    use crate::envelope::SignedEnvelope;

    struct Peer {
        key: SigningKey,
        id: MemberId,
        scope: Scope,
        lamport: u64,
        nonce: u8,
    }

    /// Every test here is about one organization — peer 1's — so peers all sign
    /// against its scope. An op signed for anywhere else is refused, which has its
    /// own tests in `envelope`.
    fn the_org() -> OrgParameters {
        let founder = SigningKey::from_bytes(&[1; 32]);
        OrgParameters::new(
            MemberId(founder.verifying_key().to_bytes()),
            "Acme",
            [0; 16],
        )
    }

    impl Peer {
        fn new(seed: u8) -> Self {
            let key = SigningKey::from_bytes(&[seed; 32]);
            let id = MemberId(key.verifying_key().to_bytes());
            Self {
                key,
                id,
                scope: the_org().scope(),
                lamport: 0,
                nonce: 0,
            }
        }

        fn stamp(&mut self, lamport: u64) -> Stamp {
            self.nonce = self.nonce.wrapping_add(1);
            self.lamport = self.lamport.max(lamport);
            Stamp::new(
                self.scope,
                self.id,
                lamport,
                1_700_000_000_000 + lamport,
                [self.nonce; 16],
            )
        }

        fn sign_at(&mut self, op: OrgOp, lamport: u64) -> SignedEnvelope {
            let stamp = self.stamp(lamport);
            op.envelope(stamp).sign(&self.key)
        }

        fn grant_at(&mut self, member: MemberId, rights: Rights, lamport: u64) -> SignedEnvelope {
            let stamp = self.stamp(lamport);
            Envelope::grant(stamp, member, rights).sign(&self.key)
        }

        fn link_at(&mut self, device: MemberId, lamport: u64) -> SignedEnvelope {
            let stamp = self.stamp(lamport);
            Envelope::link_device(stamp, device).sign(&self.key)
        }
    }

    /// What an invited admin gets, and what an invited member gets. The founder's
    /// `MAY_APPOINT` is what makes the first of these the founder's alone.
    const ADMIN: Rights = Rights::ADMIN;
    const MEMBER: Rights = Rights::MEMBER;

    fn org(ops: Vec<SignedEnvelope>) -> Organization {
        Organization::from_state(&EnvelopeState::from_ops(ops), &the_org())
    }

    #[test]
    fn a_founder_is_an_admin_by_definition() {
        let founder = Peer::new(1);
        let view = org(vec![]);

        assert!(view.is_admin(&founder.id));
        assert!(view.is_member(&founder.id));
        assert_eq!(view.active_members().len(), 1);
        assert_eq!(view.members[&founder.id].role, Role::Admin);
    }

    #[test]
    fn the_founder_can_invite_admins_and_members() {
        let mut founder = Peer::new(1);
        let admin = Peer::new(2);
        let member = Peer::new(3);

        let view = org(vec![
            founder.grant_at(admin.id, ADMIN, 1),
            founder.sign_at(
                OrgOp::SetMemberName {
                    member: admin.id,
                    name: "Ada".into(),
                },
                2,
            ),
            founder.grant_at(member.id, MEMBER, 3),
        ]);

        assert!(view.is_admin(&admin.id));
        assert_eq!(view.member_name(&admin.id), "Ada");
        assert!(view.is_member(&member.id));
        assert!(!view.is_admin(&member.id));
    }

    #[test]
    fn an_admin_can_invite_members() {
        let mut founder = Peer::new(1);
        let mut admin = Peer::new(2);
        let member = Peer::new(3);

        let view = org(vec![
            founder.grant_at(admin.id, ADMIN, 1),
            admin.grant_at(member.id, MEMBER, 10),
        ]);

        assert!(view.is_member(&member.id));
    }

    /// The two-tier rule, now enforced by a bit rather than by a special case in
    /// this file.
    #[test]
    fn an_admin_cannot_promote_another_admin() {
        let mut founder = Peer::new(1);
        let mut admin = Peer::new(2);
        let hopeful = Peer::new(3);

        let view = org(vec![
            founder.grant_at(admin.id, ADMIN, 1),
            admin.grant_at(hopeful.id, ADMIN, 10),
        ]);

        assert!(
            view.is_member(&hopeful.id),
            "the invitation still counts as membership"
        );
        assert!(!view.is_admin(&hopeful.id), "but not as an appointment");
    }

    #[test]
    fn a_member_cannot_invite_anyone() {
        let mut founder = Peer::new(1);
        let mut member = Peer::new(3);
        let stranger = Peer::new(9);

        let view = org(vec![
            founder.grant_at(member.id, MEMBER, 1),
            member.grant_at(stranger.id, MEMBER, 10),
        ]);

        assert!(!view.is_member(&stranger.id));
    }

    #[test]
    fn members_can_leave_on_their_own() {
        let mut founder = Peer::new(1);
        let mut member = Peer::new(3);

        let view = org(vec![
            founder.grant_at(member.id, MEMBER, 1),
            // Leaving is a grant of nothing, to yourself.
            member.grant_at(member.id, Rights::NONE, 10),
        ]);

        assert!(!view.is_member(&member.id));
        assert!(view.members.contains_key(&member.id), "name still resolves");
    }

    #[test]
    fn the_founder_can_neither_be_removed_nor_leave() {
        let mut founder = Peer::new(1);
        let mut admin = Peer::new(2);

        let view = org(vec![
            founder.grant_at(admin.id, ADMIN, 1),
            admin.grant_at(founder.id, Rights::NONE, 10),
            founder.grant_at(founder.id, Rights::NONE, 11),
        ]);

        assert!(
            view.is_admin(&founder.id),
            "the root of trust is in the parameters, not in an op"
        );
    }

    #[test]
    fn an_admin_may_remove_a_member_but_not_another_admin() {
        let mut founder = Peer::new(1);
        let mut admin = Peer::new(2);
        let other_admin = Peer::new(4);
        let member = Peer::new(3);

        let view = org(vec![
            founder.grant_at(admin.id, ADMIN, 1),
            founder.grant_at(other_admin.id, ADMIN, 2),
            founder.grant_at(member.id, MEMBER, 3),
            admin.grant_at(member.id, Rights::NONE, 10),
            admin.grant_at(other_admin.id, Rights::NONE, 11),
        ]);

        assert!(!view.is_member(&member.id), "a member, yes");
        assert!(
            view.is_admin(&other_admin.id),
            "an admin, no — that needs MAY_APPOINT, which only the founder holds"
        );
    }

    #[test]
    fn admins_manage_the_project_list() {
        let mut founder = Peer::new(1);
        let mut admin = Peer::new(2);
        let mut member = Peer::new(3);

        let view = org(vec![
            founder.grant_at(admin.id, ADMIN, 1),
            founder.grant_at(member.id, MEMBER, 2),
            admin.sign_at(
                OrgOp::AddProject {
                    board: BoardId([10; 32]),
                    name: "Q3".into(),
                },
                10,
            ),
            member.sign_at(
                OrgOp::AddProject {
                    board: BoardId([11; 32]),
                    name: "not mine to add".into(),
                },
                11,
            ),
        ]);

        assert_eq!(
            view.projects_sorted()
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Q3"],
            "a plain member cannot enrol a project"
        );
    }

    #[test]
    fn removing_a_project_takes_it_off_the_list() {
        let mut founder = Peer::new(1);
        let board = BoardId([10; 32]);

        let view = org(vec![
            founder.sign_at(
                OrgOp::AddProject {
                    board,
                    name: "Q3".into(),
                },
                1,
            ),
            founder.sign_at(OrgOp::RemoveProject { board }, 2),
        ]);

        assert!(view.projects.is_empty());
    }

    #[test]
    fn a_linked_device_acts_as_its_person() {
        let mut founder = Peer::new(1);
        let mut admin = Peer::new(2);
        let mut laptop = Peer::new(7);
        let member = Peer::new(3);

        let view = org(vec![
            founder.grant_at(admin.id, ADMIN, 1),
            admin.link_at(laptop.id, 10),
            laptop.grant_at(member.id, MEMBER, 11),
        ]);

        assert_eq!(view.person_of(&laptop.id), admin.id);
        assert!(
            view.is_admin(&laptop.id),
            "the device carries the authority"
        );
        assert!(view.is_member(&member.id), "so its invitation counts");
        assert_eq!(
            view.active_members().len(),
            3,
            "a device is not a separate member"
        );
    }

    #[test]
    fn the_view_is_independent_of_arrival_order() {
        let mut founder = Peer::new(1);
        let mut admin = Peer::new(2);
        let member = Peer::new(3);

        let mut ops = vec![
            founder.grant_at(admin.id, ADMIN, 1),
            admin.grant_at(member.id, MEMBER, 10),
            admin.sign_at(
                OrgOp::AddProject {
                    board: BoardId([10; 32]),
                    name: "Q3".into(),
                },
                11,
            ),
        ];

        let forwards = org(ops.clone());
        ops.reverse();
        assert_eq!(forwards, org(ops));
    }

    #[test]
    fn a_stranger_cannot_write_to_the_organization() {
        let params = the_org();
        let mut stranger = Peer::new(9);

        let mut state = EnvelopeState::new();
        let refused = state.accept(
            vec![stranger.sign_at(
                OrgOp::AddProject {
                    board: BoardId([1; 32]),
                    name: "mine now".into(),
                },
                1,
            )],
            params.scope(),
            params.founder,
        );

        assert!(matches!(refused, Err(Error::Unauthorized { .. })));
        assert!(state.is_empty());
    }

    #[test]
    fn merging_is_commutative_and_idempotent_and_encoding_round_trips() {
        let params = the_org();
        let mut founder = Peer::new(1);
        let a = founder.sign_at(
            OrgOp::AddProject {
                board: BoardId([1; 32]),
                name: "A".into(),
            },
            1,
        );
        let b = founder.grant_at(Peer::new(2).id, ADMIN, 2);

        let mut left = EnvelopeState::from_ops([a.clone()]);
        left.merge(EnvelopeState::from_ops([b.clone()]));
        let mut right = EnvelopeState::from_ops([b.clone()]);
        right.merge(EnvelopeState::from_ops([a.clone()]));
        assert_eq!(left, right);

        let snapshot = left.clone();
        assert_eq!(left.merge(EnvelopeState::from_ops([a, b])), 0);
        assert_eq!(left, snapshot);

        assert_eq!(
            EnvelopeState::decode(&left.encode())
                .expect("must decode: this test produced the bytes"),
            left
        );
        assert!(left.validate(params.scope(), params.founder).is_ok());
    }

    #[test]
    fn summary_and_delta_close_the_gap() {
        let mut founder = Peer::new(1);
        let shared = founder.grant_at(Peer::new(2).id, ADMIN, 1);
        let extra = founder.sign_at(
            OrgOp::RemoveProject {
                board: BoardId([3; 32]),
            },
            2,
        );

        let behind = EnvelopeState::from_ops([shared.clone()]);
        let ahead = EnvelopeState::from_ops([shared, extra.clone()]);

        let delta = ahead.delta_since(&behind.summary());
        assert_eq!(delta.ops, vec![extra]);

        let mut caught_up = behind;
        caught_up.merge(EnvelopeState::from_ops(delta.ops));
        assert_eq!(caught_up, ahead);
    }
}

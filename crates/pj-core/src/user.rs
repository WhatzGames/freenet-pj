//! A user's own contract: their devices, their memberships, and their name.
//!
//! # Why this exists
//!
//! Device links and board membership both live inside the op set of whatever board
//! or organization they were made on. That is right for authority — the contract
//! enforcing a rule has to be able to see the evidence for it — but it means there
//! is nowhere to answer two questions a person will obviously ask: *which devices
//! are mine?* and *what am I a member of?* Freenet has no reverse index, so neither
//! can be computed by searching.
//!
//! So each user gets a contract of their own, addressed by their own public key.
//! It is a personal index: written only by them, readable by anyone (like all
//! Freenet state), and authoritative for nothing except their own view.
//!
//! Note the split of responsibilities. Unlinking a device here removes it from *this*
//! list; it does not revoke the key's authority on boards, because that authority
//! comes from the device links held by those boards. The client emits an unlink on
//! the boards it knows about, which is best-effort — see the README on revocation.
//!
//! # It shares the board's op machinery, and that is the point
//!
//! A profile is an [`EnvelopeState`] like everything else, with the same three
//! authority kinds and the same arithmetic. Its authority model is simply the
//! degenerate case: the owner holds everything, their linked devices act as them,
//! and nobody else can write. So the ops below are opaque bodies, and adding one
//! rebuilds no contract — which matters more here than anywhere, because orphaning
//! this contract loses everyone's device list and their index of projects.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::envelope::{DeviceBody, Envelope, Scope, Stamp, kind};
use crate::envelope_state::EnvelopeState;
use crate::error::{Error, Result};
use crate::ids::{BoardId, MemberId, OrgId};
use crate::rights::Rights;

/// The immutable identity of a user's profile: their own key.
///
/// Hashed with the contract code to form the address, so anyone who knows a public
/// key can find that person's profile — and, more to the point, so a person's own
/// client can find theirs without being told where it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserParameters {
    pub owner: MemberId,
}

impl UserParameters {
    pub fn new(owner: MemberId) -> Self {
        Self { owner }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("UserParameters is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("UserParameters", e))
    }

    /// What ops written here are signed against, so a grant made on a board cannot
    /// be replayed into its author's profile. See [`Scope`].
    pub fn scope(&self) -> Scope {
        Scope::of(&self.encode())
    }
}

/// A change to a person's own profile.
///
/// Linking and unlinking a device are *not* here: those are authority ops, the
/// same three kinds every contract understands. What is here is only the part a
/// contract has no opinion about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserOp {
    /// A display name that finally survives a reload, unlike the browser-local one.
    SetName {
        name: String,
    },
    /// What to call one of your keys. The link itself is an authority op; this is
    /// the human-readable part of it, and it lives here so that renaming "laptop"
    /// to "work laptop" is not an exercise of authority.
    SetDeviceLabel {
        device: MemberId,
        label: String,
    },
    /// Remember a project this person belongs to, and the organization it sits under.
    JoinedBoard {
        board: BoardId,
        name: String,
        org: Option<OrgId>,
    },
    LeftBoard {
        board: BoardId,
    },
    JoinedOrg {
        org: OrgId,
        name: String,
    },
    LeftOrg {
        org: OrgId,
    },
}

impl UserOp {
    /// Every profile op needs the same thing, because a profile has exactly one
    /// question of authority: is this the owner, or a key acting for them?
    ///
    /// The owner holds [`Rights::ALL`] from the parameters and their devices
    /// inherit it, so this passes for them and for nobody else.
    pub fn needs(&self) -> Rights {
        Rights::ADMINISTER
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("UserOp is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("user op", e))
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

// ------------------------------------------------------------------ the view

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserDevice {
    pub id: MemberId,
    pub label: String,
    /// When it was vouched for, for display only.
    pub linked_ms: u64,
    /// True for the key named in the parameters — the one that cannot be unlinked,
    /// since it is the root of this profile.
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserBoard {
    pub board: BoardId,
    pub name: String,
    pub org: Option<OrgId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserOrg {
    pub org: OrgId,
    pub name: String,
}

/// A person's own view of themselves.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProfile {
    pub name: String,
    pub devices: BTreeMap<MemberId, UserDevice>,
    pub boards: BTreeMap<BoardId, UserBoard>,
    pub orgs: BTreeMap<OrgId, UserOrg>,
    pub next_lamport: u64,
}

impl UserProfile {
    pub fn from_state(state: &EnvelopeState, params: &UserParameters) -> Self {
        let authority = state.authority(params.owner);

        let next_lamport = state
            .ops
            .values()
            .map(|op| op.payload.lamport)
            .max()
            .map_or(1, |max| max.saturating_add(1));

        // When each *surviving* link was made. An unlinked device is gone from
        // `person_of`, so a link/unlink/relink pair leaves no ghost.
        let mut linked_ms: BTreeMap<MemberId, u64> = BTreeMap::new();
        for op in state.ordered() {
            if op.payload.kind != kind::LINK_DEVICE {
                continue;
            }
            let Ok(body) = DeviceBody::decode(&op.payload.body) else {
                continue;
            };
            if authority.person_of.contains_key(&body.device) {
                linked_ms
                    .entry(body.device)
                    .or_insert(op.payload.wall_clock_ms);
            }
        }

        let mut name = params.owner.short();
        let mut labels: BTreeMap<MemberId, String> = BTreeMap::new();
        let mut boards: BTreeMap<BoardId, UserBoard> = BTreeMap::new();
        let mut orgs: BTreeMap<OrgId, UserOrg> = BTreeMap::new();

        for envelope in state.application_ops() {
            if envelope.payload.kind != kind::FIRST_APPLICATION_KIND {
                continue;
            }
            let Ok(op) = UserOp::decode(&envelope.payload.body) else {
                continue;
            };
            if !authority
                .ever_rights_of(&envelope.author())
                .contains(op.needs())
            {
                continue;
            }

            match op {
                UserOp::SetName { name: new } => name = new,
                UserOp::SetDeviceLabel { device, label } => {
                    labels.insert(device, label);
                }

                UserOp::JoinedBoard { board, name, org } => {
                    boards.insert(board, UserBoard { board, name, org });
                }
                UserOp::LeftBoard { board } => {
                    boards.remove(&board);
                }

                UserOp::JoinedOrg { org, name } => {
                    orgs.insert(org, UserOrg { org, name });
                }
                UserOp::LeftOrg { org } => {
                    orgs.remove(&org);
                    // Projects belonging to an organization you have left are no
                    // longer yours to list under it.
                    boards.retain(|_, board| board.org != Some(org));
                }
            }
        }

        let mut devices: BTreeMap<MemberId, UserDevice> = BTreeMap::new();
        // The primary key is the root of this profile and cannot be unlinked: the
        // contract's authority comes from the parameters, so no op can take it away
        // and a list without it would be a lie.
        devices.insert(
            params.owner,
            UserDevice {
                id: params.owner,
                label: labels
                    .get(&params.owner)
                    .cloned()
                    .unwrap_or_else(|| "this identity".to_owned()),
                linked_ms: 0,
                primary: true,
            },
        );
        for device in authority.person_of.keys() {
            if *device == params.owner {
                continue;
            }
            devices.insert(
                *device,
                UserDevice {
                    id: *device,
                    label: labels
                        .get(device)
                        .cloned()
                        .unwrap_or_else(|| device.short()),
                    linked_ms: linked_ms.get(device).copied().unwrap_or(0),
                    primary: false,
                },
            );
        }

        Self {
            name,
            devices,
            boards,
            orgs,
            next_lamport,
        }
    }

    /// Projects grouped under their organization, then the ones that belong to none.
    ///
    /// Organizations with no projects still appear, because being a member of one is
    /// worth seeing even before it has any work in it.
    pub fn grouped(&self) -> (Vec<(UserOrg, Vec<UserBoard>)>, Vec<UserBoard>) {
        let mut grouped: Vec<(UserOrg, Vec<UserBoard>)> = self
            .orgs
            .values()
            .map(|org| {
                let mut projects: Vec<UserBoard> = self
                    .boards
                    .values()
                    .filter(|board| board.org == Some(org.org))
                    .cloned()
                    .collect();
                projects.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.board.cmp(&b.board)));
                (org.clone(), projects)
            })
            .collect();
        grouped.sort_by(|a, b| a.0.name.cmp(&b.0.name).then_with(|| a.0.org.cmp(&b.0.org)));

        let mut loose: Vec<UserBoard> = self
            .boards
            .values()
            .filter(|board| match board.org {
                // A project whose organization we are not a member of is listed on
                // its own rather than hidden.
                Some(org) => !self.orgs.contains_key(&org),
                None => true,
            })
            .cloned()
            .collect();
        loose.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.board.cmp(&b.board)));

        (grouped, loose)
    }

    /// Every key that acts as this person, primary first.
    pub fn device_list(&self) -> Vec<&UserDevice> {
        let mut devices: Vec<&UserDevice> = self.devices.values().collect();
        devices.sort_by(|a, b| {
            b.primary
                .cmp(&a.primary)
                .then_with(|| a.linked_ms.cmp(&b.linked_ms))
                .then_with(|| a.id.cmp(&b.id))
        });
        devices
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;
    use crate::envelope::{Envelope, SignedEnvelope, Stamp};

    struct Peer {
        key: SigningKey,
        id: MemberId,
        scope: Scope,
        nonce: u8,
    }

    impl Peer {
        /// Every peer in a test signs for the same profile — peer 1's.
        fn new(seed: u8) -> Self {
            let key = SigningKey::from_bytes(&[seed; 32]);
            let id = MemberId(key.verifying_key().to_bytes());
            let root = SigningKey::from_bytes(&[1; 32]);
            let scope = UserParameters::new(MemberId(root.verifying_key().to_bytes())).scope();
            Self {
                key,
                id,
                scope,
                nonce: 0,
            }
        }

        fn stamp(&mut self, lamport: u64) -> Stamp {
            self.nonce = self.nonce.wrapping_add(1);
            Stamp::new(
                self.scope,
                self.id,
                lamport,
                1_700_000_000_000 + lamport,
                [self.nonce; 16],
            )
        }

        fn sign_at(&mut self, op: UserOp, lamport: u64) -> SignedEnvelope {
            let stamp = self.stamp(lamport);
            op.envelope(stamp).sign(&self.key)
        }

        fn link_at(&mut self, device: MemberId, lamport: u64) -> SignedEnvelope {
            let stamp = self.stamp(lamport);
            Envelope::link_device(stamp, device).sign(&self.key)
        }

        fn unlink_at(&mut self, device: MemberId, lamport: u64) -> SignedEnvelope {
            let stamp = self.stamp(lamport);
            Envelope::unlink_device(stamp, device).sign(&self.key)
        }
    }

    fn profile(ops: Vec<SignedEnvelope>, params: &UserParameters) -> UserProfile {
        UserProfile::from_state(&EnvelopeState::from_ops(ops), params)
    }

    #[test]
    fn a_fresh_profile_has_just_the_primary_key() {
        let me = Peer::new(1);
        let params = UserParameters::new(me.id);
        let view = profile(vec![], &params);

        assert_eq!(view.device_list().len(), 1);
        assert!(view.device_list()[0].primary);
        assert_eq!(view.name, me.id.short());
    }

    #[test]
    fn the_name_persists_and_the_latest_wins() {
        let mut me = Peer::new(1);
        let params = UserParameters::new(me.id);
        let view = profile(
            vec![
                me.sign_at(UserOp::SetName { name: "Ada".into() }, 1),
                me.sign_at(
                    UserOp::SetName {
                        name: "Ada L".into(),
                    },
                    2,
                ),
            ],
            &params,
        );
        assert_eq!(view.name, "Ada L");
    }

    #[test]
    fn devices_are_listed_and_can_be_unlinked() {
        let mut me = Peer::new(1);
        let phone = Peer::new(2);
        let params = UserParameters::new(me.id);

        let linked = profile(
            vec![
                me.link_at(phone.id, 1),
                me.sign_at(
                    UserOp::SetDeviceLabel {
                        device: phone.id,
                        label: "phone".into(),
                    },
                    2,
                ),
            ],
            &params,
        );
        assert_eq!(linked.device_list().len(), 2);
        assert_eq!(linked.devices[&phone.id].label, "phone");

        let unlinked = profile(
            vec![me.link_at(phone.id, 1), me.unlink_at(phone.id, 3)],
            &params,
        );
        assert_eq!(unlinked.device_list().len(), 1, "only the primary remains");
        assert!(!unlinked.devices.contains_key(&phone.id));
    }

    /// The primary key is this profile's root; removing it would leave the profile
    /// unwritable.
    #[test]
    fn the_primary_key_cannot_be_unlinked() {
        let mut me = Peer::new(1);
        let params = UserParameters::new(me.id);
        let view = profile(vec![me.unlink_at(me.id, 1)], &params);
        assert!(view.devices.contains_key(&me.id));
        assert!(view.device_list()[0].primary);
    }

    #[test]
    fn a_linked_device_may_write_to_the_profile() {
        let mut me = Peer::new(1);
        let mut phone = Peer::new(2);
        let params = UserParameters::new(me.id);

        let mut state = EnvelopeState::new();
        state
            .accept(
                vec![
                    me.link_at(phone.id, 1),
                    phone.sign_at(UserOp::SetName { name: "Ada".into() }, 2),
                ],
                params.scope(),
                params.owner,
            )
            .expect("must decode: this test produced the bytes");

        assert_eq!(UserProfile::from_state(&state, &params).name, "Ada");
    }

    #[test]
    fn nobody_else_may_write_to_your_profile() {
        let me = Peer::new(1);
        let mut stranger = Peer::new(9);
        let params = UserParameters::new(me.id);

        let mut state = EnvelopeState::new();
        let refused = state.accept(
            vec![stranger.sign_at(
                UserOp::SetName {
                    name: "hijacked".into(),
                },
                1,
            )],
            params.scope(),
            params.owner,
        );
        assert!(matches!(refused, Err(Error::Unauthorized { .. })));
        assert!(state.is_empty());
    }

    /// The reason profiles carry a scope: `ADMINISTER` on somebody's board must not
    /// be `ADMINISTER` on their profile.
    #[test]
    fn an_op_signed_for_a_board_cannot_be_replayed_into_a_profile() {
        let mut me = Peer::new(1);
        let params = UserParameters::new(me.id);

        // Same author, same rights, written against some other contract.
        let mut elsewhere = Peer::new(1);
        elsewhere.scope = Scope([99; 32]);
        let smuggled = elsewhere.sign_at(
            UserOp::SetName {
                name: "not from here".into(),
            },
            1,
        );

        let mut state = EnvelopeState::new();
        assert!(matches!(
            state.accept(vec![smuggled], params.scope(), params.owner),
            Err(Error::MisdirectedOp { .. })
        ));

        // …and the same op written here is fine, so it is the scope being refused
        // and not the op.
        assert!(
            state
                .accept(
                    vec![me.sign_at(UserOp::SetName { name: "Ada".into() }, 1)],
                    params.scope(),
                    params.owner,
                )
                .is_ok()
        );
    }

    #[test]
    fn projects_group_under_their_organization() {
        let mut me = Peer::new(1);
        let params = UserParameters::new(me.id);
        let acme = OrgId([1; 32]);

        let view = profile(
            vec![
                me.sign_at(
                    UserOp::JoinedOrg {
                        org: acme,
                        name: "Acme".into(),
                    },
                    1,
                ),
                me.sign_at(
                    UserOp::JoinedBoard {
                        board: BoardId([10; 32]),
                        name: "Q3 Launch".into(),
                        org: Some(acme),
                    },
                    2,
                ),
                me.sign_at(
                    UserOp::JoinedBoard {
                        board: BoardId([11; 32]),
                        name: "Personal".into(),
                        org: None,
                    },
                    3,
                ),
            ],
            &params,
        );

        let (grouped, loose) = view.grouped();
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].0.name, "Acme");
        assert_eq!(
            grouped[0]
                .1
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Q3 Launch"]
        );
        assert_eq!(
            loose.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
            vec!["Personal"],
            "a project with no organization stands alone"
        );
    }

    /// An organization you are a member of shows even with no projects, and leaving
    /// it takes its projects off your list too.
    #[test]
    fn leaving_an_organization_removes_it_and_its_projects() {
        let mut me = Peer::new(1);
        let params = UserParameters::new(me.id);
        let acme = OrgId([1; 32]);

        let ops = vec![
            me.sign_at(
                UserOp::JoinedOrg {
                    org: acme,
                    name: "Acme".into(),
                },
                1,
            ),
            me.sign_at(
                UserOp::JoinedBoard {
                    board: BoardId([10; 32]),
                    name: "Q3".into(),
                    org: Some(acme),
                },
                2,
            ),
        ];

        let before = profile(ops.clone(), &params);
        assert_eq!(before.grouped().0.len(), 1);
        assert_eq!(before.grouped().0[0].1.len(), 1);

        let mut after_ops = ops;
        after_ops.push(me.sign_at(UserOp::LeftOrg { org: acme }, 3));
        let after = profile(after_ops, &params);
        assert!(after.grouped().0.is_empty());
        assert!(after.grouped().1.is_empty(), "its projects go with it");
    }

    /// A project whose organization we are not in is still listed, just not nested.
    #[test]
    fn a_project_of_an_unknown_organization_is_listed_loose() {
        let mut me = Peer::new(1);
        let params = UserParameters::new(me.id);

        let view = profile(
            vec![me.sign_at(
                UserOp::JoinedBoard {
                    board: BoardId([10; 32]),
                    name: "Contract work".into(),
                    org: Some(OrgId([7; 32])),
                },
                1,
            )],
            &params,
        );

        let (grouped, loose) = view.grouped();
        assert!(grouped.is_empty());
        assert_eq!(loose.len(), 1);
    }

    #[test]
    fn leaving_a_board_removes_it() {
        let mut me = Peer::new(1);
        let params = UserParameters::new(me.id);
        let board = BoardId([10; 32]);

        let view = profile(
            vec![
                me.sign_at(
                    UserOp::JoinedBoard {
                        board,
                        name: "Q3".into(),
                        org: None,
                    },
                    1,
                ),
                me.sign_at(UserOp::LeftBoard { board }, 2),
            ],
            &params,
        );
        assert!(view.boards.is_empty());
    }

    #[test]
    fn merging_is_commutative_and_encoding_round_trips() {
        let mut me = Peer::new(1);
        let params = UserParameters::new(me.id);
        let a = me.sign_at(UserOp::SetName { name: "A".into() }, 1);
        let b = me.sign_at(
            UserOp::JoinedOrg {
                org: OrgId([1; 32]),
                name: "Acme".into(),
            },
            2,
        );

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
        assert!(
            EnvelopeState::decode(&[])
                .expect("must decode: this test produced the bytes")
                .is_empty()
        );
        assert!(left.validate(params.scope(), params.owner).is_ok());
    }
}

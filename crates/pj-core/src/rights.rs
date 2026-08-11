//! What a member is allowed to do, as a set of bits.
//!
//! # Why a bitset and not an enum of roles
//!
//! A contract's address is `hash(code + parameters)`, so any change to its wasm
//! moves every board to a new address and orphans the old one. An enum of roles
//! puts every future permission on that path: adding "may edit columns but not
//! members" would be a new variant, a new wasm, a new address, and everybody's
//! data left behind.
//!
//! A bitset moves the meaning out of the contract. The contract's whole
//! authorization check becomes
//!
//! ```text
//! held & needed == needed
//! ```
//!
//! which is arithmetic. It never learns what bit 5 means, so allocating bit 5
//! is a client-side change and the contract is never rebuilt.
//!
//! # The one bit the contract does understand
//!
//! [`Rights::MAY_GRANT`]. Without it, a client could send a membership op
//! declaring `needs: NONE` and appoint itself; something has to be pinned. One
//! bit, fixed forever, is the smallest thing that can be.
//!
//! # Why intersection keeps the fold convergent
//!
//! A grant confers at most what the granter holds, which here is
//! `granted & granter_held`. Intersection is commutative, associative and
//! idempotent, so peers folding the same grants in different orders reach the
//! same answer — the property the whole CRDT rests on. An ordered role ladder
//! only had that property by accident of there being one dimension to compare.

use serde::{Deserialize, Serialize};

/// A set of permissions.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Rights(pub u64);

impl Rights {
    /// Nothing at all. Also how a member is removed: a grant of `NONE`.
    pub const NONE: Rights = Rights(0);

    /// May confer rights on others. **The only bit the contract interprets** —
    /// see the module docs.
    pub const MAY_GRANT: Rights = Rights(1 << 0);

    /// May create, edit, move and delete tasks.
    pub const WRITE_TASKS: Rights = Rights(1 << 1);

    /// May add, rename and remove columns.
    pub const WRITE_COLUMNS: Rights = Rights(1 << 2);

    /// May link tasks to tasks, including across projects.
    pub const LINK_TASKS: Rights = Rights(1 << 3);

    /// May change board-wide settings — which organization owns the project,
    /// and anybody's display name.
    ///
    /// Separate from [`Rights::MAY_GRANT`] on purpose: "may hand out rights" and
    /// "may retitle the project" are different powers, and conflating them would
    /// mean the only way to let somebody rename things was to let them appoint
    /// admins.
    pub const ADMINISTER: Rights = Rights(1 << 4);

    /// May set one's *own* display name. Held by everyone, including plain
    /// members; renaming somebody else takes [`Rights::ADMINISTER`].
    pub const SET_NAME: Rights = Rights(1 << 5);

    /// May pass on [`Rights::MAY_GRANT`] itself — that is, may create another
    /// administrator.
    ///
    /// **The second bit the authority fold interprets**, and the only reason it
    /// exists: without it, `MAY_GRANT` would be transitive by construction, because
    /// a grant confers `asked ∩ held` and an admin holds `MAY_GRANT`. Every admin
    /// could then mint admins, which is not what an owner agreed to when they
    /// appointed one. An admin invites people; the owner decides who else may.
    pub const MAY_APPOINT: Rights = Rights(1 << 6);

    // Bits 7..63 are unallocated. Using one is a change to this file and the
    // folds — never to a contract.

    /// Everything, including bits not yet named. What a board's owner holds, so
    /// that an owner is never short of a permission invented after their board.
    pub const ALL: Rights = Rights(u64::MAX);

    /// What an invited member gets by default.
    pub const MEMBER: Rights = Rights(
        Rights::WRITE_TASKS.0 | Rights::WRITE_COLUMNS.0 | Rights::LINK_TASKS.0 | Rights::SET_NAME.0,
    );

    /// What an administrator gets: everything a member has, plus the ability to
    /// bring others in and to change the board's settings.
    pub const ADMIN: Rights = Rights(Rights::MEMBER.0 | Rights::MAY_GRANT.0 | Rights::ADMINISTER.0);

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every bit in `needed` is held.
    pub const fn contains(self, needed: Rights) -> bool {
        self.0 & needed.0 == needed.0
    }

    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }

    /// The bits both hold. This is what caps a grant at the granter's own
    /// authority, and its commutativity is why the fold converges.
    pub const fn intersect(self, other: Rights) -> Rights {
        Rights(self.0 & other.0)
    }

    pub const fn without(self, other: Rights) -> Rights {
        Rights(self.0 & !other.0)
    }
}

/// A label for a set of rights, for the two places a person wants a word rather
/// than a bit pattern: a badge in the UI, and the two buttons on the invite form.
///
/// Deliberately *derived* and not stored. The authority is the bitset; this is a
/// reading of it. Anything that widens over time — a "may link tasks only"
/// collaborator — gets bits without needing a word for it, and reads as `Member`
/// until somebody decides it deserves one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Role {
    /// Holds [`Rights::MAY_GRANT`]: can change who is on the board.
    Admin,
    /// Can do work, but not decide who else may.
    Member,
}

impl Role {
    /// How to read a bitset as a word.
    pub const fn of(rights: Rights) -> Role {
        if rights.contains(Rights::MAY_GRANT) {
            Role::Admin
        } else {
            Role::Member
        }
    }

    /// What the word means when someone picks it off an invite form.
    pub const fn rights(self) -> Rights {
        match self {
            Role::Admin => Rights::ADMIN,
            Role::Member => Rights::MEMBER,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_contains_nothing_but_everything_contains_all() {
        assert!(Rights::NONE.contains(Rights::NONE));
        assert!(!Rights::NONE.contains(Rights::WRITE_TASKS));
        assert!(Rights::ALL.contains(Rights::MAY_GRANT));
        assert!(Rights::ALL.contains(Rights::MEMBER));
    }

    #[test]
    fn a_member_may_write_but_not_grant() {
        assert!(Rights::MEMBER.contains(Rights::WRITE_TASKS));
        assert!(!Rights::MEMBER.contains(Rights::MAY_GRANT));
        assert!(Rights::ADMIN.contains(Rights::MAY_GRANT));
    }

    /// The property the fold's convergence depends on.
    #[test]
    fn intersection_is_order_independent() {
        let a = Rights::ADMIN;
        let b = Rights::MEMBER;
        let c = Rights::WRITE_TASKS;

        assert_eq!(a.intersect(b), b.intersect(a));
        assert_eq!(a.intersect(b).intersect(c), a.intersect(b.intersect(c)));
        assert_eq!(a.intersect(a), a);
    }

    /// A granter cannot confer what it does not hold, however generous the op.
    #[test]
    fn a_grant_is_capped_by_the_granter() {
        let granter = Rights::MEMBER; // no MAY_GRANT
        let asked_for = Rights::ALL;
        assert_eq!(asked_for.intersect(granter), Rights::MEMBER);
        assert!(!asked_for.intersect(granter).contains(Rights::MAY_GRANT));
    }

    /// Removal is a grant of nothing, so it needs no separate op.
    #[test]
    fn removal_is_an_empty_grant() {
        assert!(Rights::NONE.is_empty());
        assert!(!Rights::NONE.contains(Rights::WRITE_TASKS));
    }

    /// The point of the whole design: a bit nobody has named yet still works,
    /// because the check is arithmetic.
    #[test]
    fn an_unallocated_bit_needs_no_new_code() {
        let future = Rights(1 << 42);
        let holder = Rights::MEMBER.union(future);
        assert!(holder.contains(future));
        assert!(!Rights::MEMBER.contains(future));
        assert!(Rights::ALL.contains(future));
    }
}

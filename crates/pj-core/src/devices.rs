//! Resolving a person's keys to the set of keys that act for them.
//!
//! One person may hold several keys — a browser, a laptop, a phone — and a key
//! vouches for another by signing a link. Authority therefore attaches to the
//! transitive closure of those vouchings, not to a single key.
//!
//! Shared by the organization and user-profile folds, which each root the closure
//! somewhere different (a founder, a profile owner) but need the same walk.

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::MemberId;

/// Every key that acts for `root`, including `root` itself.
pub fn device_closure(root: MemberId, links: &BTreeMap<MemberId, MemberId>) -> BTreeSet<MemberId> {
    let mut set = BTreeSet::new();
    set.insert(root);
    grow_devices(&mut set, links);
    set
}

/// Adds every device vouched for by a key already in `set`, repeatedly, until the
/// set stops growing.
///
/// Monotone, so it terminates (each pass either adds a key or stops, and there are
/// finitely many) and its result is independent of iteration order.
pub fn grow_devices(set: &mut BTreeSet<MemberId>, links: &BTreeMap<MemberId, MemberId>) {
    loop {
        let mut grew = false;
        for (device, vouched_by) in links {
            if set.contains(vouched_by) && set.insert(*device) {
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(seed: u8) -> MemberId {
        MemberId([seed; 32])
    }

    #[test]
    fn a_chain_of_vouchings_all_resolves_to_the_root() {
        let links = BTreeMap::from([(id(2), id(1)), (id(3), id(2)), (id(9), id(8))]);
        let closure = device_closure(id(1), &links);
        assert_eq!(closure, BTreeSet::from([id(1), id(2), id(3)]));
    }

    #[test]
    fn a_cycle_terminates_instead_of_looping() {
        let links = BTreeMap::from([(id(1), id(2)), (id(2), id(1))]);
        assert_eq!(
            device_closure(id(1), &links),
            BTreeSet::from([id(1), id(2)])
        );
    }
}

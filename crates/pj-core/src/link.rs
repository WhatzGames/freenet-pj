//! Typed links between tasks.
//!
//! A link is stored as a single directed edge with a kind. The reverse direction is
//! not stored: it is *derived*, because every kind has an inverse. Recording
//! "A is the parent of B" therefore automatically makes B a child of A, with no
//! second op to keep consistent and no way for the two directions to disagree.
//!
//! # Why a link no longer names a board
//!
//! It used to be `(board, task)`, because a task only existed inside a board and
//! naming the task alone would not have found it. A task is its own contract now,
//! so its address *is* the reference — see [`crate::task`]. Nothing has to be
//! carried alongside it, which is what lets a pasted link work with no context at
//! all, on a network with no reverse index to look anything up in.
//!
//! # Which direction is stored, and why that is a permissions question
//!
//! The forward edge lives on the linking task. The mirrored edge on the other task
//! is a second write, to a second contract, and therefore only possible where the
//! author holds rights — which is within their own organization. So links are
//! bidirectional inside an org and one-way across, not by policy but because that
//! is exactly the set of cases where the write can happen at all.

use serde::{Deserialize, Serialize};

use crate::ids::TaskAddr;

/// What one task is to another.
///
/// Exactly one kind applies to a given pair: setting a new one replaces the old,
/// resolved last-writer-wins like any other field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LinkKind {
    /// Symmetric: its own inverse.
    RelatedTo,
    Causes,
    CausedBy,
    ParentOf,
    ChildOf,
}

impl LinkKind {
    /// The kind the *other* task holds towards this one.
    pub fn inverse(self) -> Self {
        match self {
            Self::RelatedTo => Self::RelatedTo,
            Self::Causes => Self::CausedBy,
            Self::CausedBy => Self::Causes,
            Self::ParentOf => Self::ChildOf,
            Self::ChildOf => Self::ParentOf,
        }
    }

    /// The kinds a user picks from. The inverses are reachable by linking from the
    /// other task, so offering all five would only be a way to say the same thing
    /// twice.
    pub const CHOICES: [LinkKind; 3] = [Self::RelatedTo, Self::Causes, Self::ParentOf];

    pub fn label(self) -> &'static str {
        match self {
            Self::RelatedTo => "related to",
            Self::Causes => "causes",
            Self::CausedBy => "caused by",
            Self::ParentOf => "parent of",
            Self::ChildOf => "child of",
        }
    }

    /// Stable identifier for a `<select>` value.
    pub fn slug(self) -> &'static str {
        match self {
            Self::RelatedTo => "related",
            Self::Causes => "causes",
            Self::CausedBy => "caused-by",
            Self::ParentOf => "parent-of",
            Self::ChildOf => "child-of",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        [
            Self::RelatedTo,
            Self::Causes,
            Self::CausedBy,
            Self::ParentOf,
            Self::ChildOf,
        ]
        .into_iter()
        .find(|kind| kind.slug() == slug)
    }
}

/// The prefix that distinguishes a task route from a board route.
///
/// Both ids are 32 bytes and base58, so nothing about the id itself says which it
/// is. The prefix is the only thing that does.
pub const TASK_ROUTE_PREFIX: &str = "task/";

/// Reads a pasted reference to a task.
///
/// Accepts, in decreasing order of how much somebody had to know to produce it:
///
/// - a whole URL copied out of the address bar — everything up to and including
///   the last `#` is ignored, so the origin and path do not matter and a link from
///   a different node still parses
/// - a bare `task/<address>` route
/// - a lone address
///
/// The point is that a link should be makeable by pasting the thing you were
/// already given. Asking for an address in a field of its own means the person has
/// to take a URL apart by hand first, and know that it comes apart at all.
pub fn parse_task(text: &str) -> Option<TaskAddr> {
    // The fragment is the whole route; anything before the last `#` is the address
    // of whichever node the link was copied from.
    let route = text.rsplit('#').next().unwrap_or_default();
    let route = route.trim().trim_matches('/');
    let route = route.strip_prefix(TASK_ROUTE_PREFIX).unwrap_or(route);
    TaskAddr::from_base58(route.trim())
}

/// The route [`parse_task`] reads back, for putting on a clipboard.
///
/// Carries no board. A task's boards are recorded on the task itself, so a link
/// opened cold can still offer its way back to one — see
/// [`crate::task::Task::boards`].
pub fn task_route(task: TaskAddr) -> String {
    format!("{TASK_ROUTE_PREFIX}{}", task.to_base58())
}

/// One end of a link, as rendered on a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLink {
    pub other: TaskAddr,
    pub kind: LinkKind,
    /// True when this side was derived from an edge stored the other way round.
    /// Purely informational — both directions are equally real.
    pub derived: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASK: TaskAddr = TaskAddr([9; 32]);

    /// The whole point: whatever somebody pastes, it works.
    #[test]
    fn a_pasted_reference_parses_in_every_shape_it_arrives_in() {
        let route = task_route(TASK);

        // Straight out of the address bar.
        let url = format!("http://127.0.0.1:7509/v1/contract/web/6LpX8WFj/#{route}");
        assert_eq!(parse_task(&url), Some(TASK));

        // Just the fragment, with and without the `#`.
        assert_eq!(parse_task(&format!("#{route}")), Some(TASK));
        assert_eq!(parse_task(&route), Some(TASK));

        // Whitespace from a sloppy copy, and a trailing slash.
        assert_eq!(parse_task(&format!("  {route}/  ")), Some(TASK));

        // The bare address, which is what someone reading it off a screen would
        // type.
        assert_eq!(parse_task(&TASK.to_base58()), Some(TASK));
    }

    #[test]
    fn nonsense_does_not_parse() {
        assert_eq!(parse_task(""), None);
        assert_eq!(parse_task("not an id"), None);
        assert_eq!(parse_task("task/"), None);
        // Right alphabet, wrong length: a board id is the same 32 bytes but a
        // column id is not.
        assert_eq!(parse_task("task/2VfUX"), None);
    }

    #[test]
    fn a_route_round_trips() {
        assert_eq!(parse_task(&task_route(TASK)), Some(TASK));
        assert!(task_route(TASK).starts_with(TASK_ROUTE_PREFIX));
    }

    #[test]
    fn every_kind_is_its_own_inverse_twice_over() {
        for kind in [
            LinkKind::RelatedTo,
            LinkKind::Causes,
            LinkKind::CausedBy,
            LinkKind::ParentOf,
            LinkKind::ChildOf,
        ] {
            assert_eq!(kind.inverse().inverse(), kind);
            assert_eq!(LinkKind::from_slug(kind.slug()), Some(kind));
        }
    }
}

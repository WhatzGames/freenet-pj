//! Domain model for a Freenet-hosted project board.
//!
//! This crate has no dependency on Freenet or on the browser, so the exact same
//! types compile into the contract wasm, into the Leptos frontend, and into
//! native test binaries. The Freenet-specific glue lives in `pj-board-contract`.
//!
//! # Why a CRDT
//!
//! Freenet requires that `update_state` be **commutative**: applying a set of
//! deltas in any order must converge on the same state. A board is therefore not
//! stored as "the current tasks" but as a grow-only set of signed operations
//! keyed by their content hash ([`envelope_state::EnvelopeState`]). Set union is
//! trivially commutative, associative and idempotent, so convergence is
//! structural rather than something the merge code has to be careful about.
//!
//! The tasks-and-columns view a user actually sees is folded out of that op set
//! on demand ([`board::Board::from_state`]), with last-writer-wins on individual
//! fields. Because the fold walks ops in a deterministic total order derived
//! from the ops themselves, two peers holding the same op set always render the
//! same board.
//!
//! # Where the line between contract and client falls
//!
//! An op travels as an [`envelope::Envelope`]: a signed header the contract reads
//! and an opaque body it does not. The contract knows three op kinds — grant, link
//! device, unlink device — plus one piece of arithmetic, `held & needs == needs`.
//! Everything else, [`op::Op`] included, is bytes to it.
//!
//! That is not tidiness. A contract's address is `hash(code + parameters)`, so
//! every rule it learns is a future migration for everybody's data. Ops it never
//! has to understand can be added freely, and a client that meets an op kind from
//! the future carries it intact rather than failing to decode the state around it.

pub mod board;
pub mod bootstrap;
pub mod devices;
pub mod envelope;
pub mod envelope_state;
pub mod error;
pub mod ids;
pub mod legacy;
pub mod link;
pub mod op;
pub mod org;
pub mod params;
pub mod rank;
pub mod registry;
pub mod rights;
pub mod task;
pub mod user;

mod serde_bytes64;

pub use board::{Board, BoardOrganization, Column, Member, Placement};
pub use devices::{device_closure, grow_devices};
pub use envelope::{
    DeviceBody, Envelope, GrantBody, Scope, SignedEnvelope, Stamp, kind, permitted,
};
pub use envelope_state::{Authority, EnvelopeDelta, EnvelopeState, EnvelopeSummary, Org, Trust};
pub use error::{Error, Result};
pub use ids::{BoardId, ColumnId, ListingId, MemberId, OpId, OrgId, TaskAddr, TaskId};
pub use link::{LinkKind, TaskLink, parse_task, task_route};
pub use op::{Draft, Op};
pub use org::{OrgDraft, OrgOp, OrgParameters, Organization};
pub use params::BoardParameters;
pub use rank::Rank;
pub use registry::{
    Listing, ListingTarget, RegistryDelta, RegistryParameters, RegistryState, RegistrySummary,
    SignedListing,
};
pub use rights::{Rights, Role};
// `task::Task` is deliberately not re-exported while `board::Task` still exists
// under that name. The board's inline task is what this replaces.
pub use task::{TaskOp, TaskOrg, TaskParameters, TaskSummary};
pub use user::{UserBoard, UserDevice, UserOp, UserOrg, UserParameters, UserProfile};

//! freenet-pj — a project board that lives on Freenet.
//!
//! The app is a client-side Leptos application. It is served by a Freenet node
//! and talks to that same node over its WebSocket client API, so there is no
//! backend of ours anywhere: the shared state is a Freenet contract and the
//! rules about who may change it are enforced by that contract on every peer.

mod contract;
mod identity;
mod node;
mod store;
mod ui;

use pj_core::ColumnId;
use wasm_bindgen::prelude::wasm_bindgen;

/// Fresh ids for the columns a new board starts with.
///
/// Column ids are random rather than derived from titles so that renaming a
/// column does not orphan the tasks in it.
pub(crate) fn bootstrap_columns() -> Vec<ColumnId> {
    (0..pj_core::bootstrap::DEFAULT_COLUMNS.len())
        .map(|_| ColumnId(identity::random_bytes::<16>()))
        .collect()
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(ui::App);
}

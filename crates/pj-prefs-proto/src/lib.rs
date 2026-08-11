//! The wire protocol between the app and the preferences delegate.
//!
//! # Why a delegate
//!
//! The app runs in a sandboxed iframe on an opaque origin, where `localStorage`,
//! `sessionStorage` and `IndexedDB` all throw. Anything the app wants to remember
//! between loads has to live somewhere else, and the only local somewhere else is
//! the node. A delegate runs inside the node and has persistent secret storage.
//!
//! # Why the delegate never sees the schema
//!
//! A delegate's key is `hash(code + parameters)` and its stored secrets hang off
//! that key, so *any* change to its wasm moves every stored value into a new
//! namespace and silently loses it. If the delegate understood what a preference
//! was, adding a second preference would be a migration.
//!
//! So it does not. [`Prefs`] is encoded here, in the client, and travels as an
//! opaque blob; the delegate only ever stores and returns bytes. New preferences
//! are a change to this map and nothing else — the delegate's wasm, and therefore
//! everyone's stored settings, stay exactly where they are.
//!
//! # Why not the user contract
//!
//! Preferences of this kind describe the machine you are sitting at, not the
//! person. A theme synced across every device would flip your laptop because you
//! chose light on your phone. Node-local is the correct scope, not merely the
//! convenient one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Bumped only on a breaking change to these messages. A delegate is immutable
/// once published, so a mismatch has to be detectable rather than misparsed.
pub const PROTOCOL_VERSION: u16 = 1;

/// The preference the theme toggle writes.
pub const THEME: &str = "theme";

/// Everything this node remembers for this app, as plain key/value pairs.
///
/// A map rather than a struct so that an older build reading a newer node's
/// preferences keeps the keys it does not recognise instead of dropping them on
/// the next save.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    pub entries: BTreeMap<String, String>,
}

impl Prefs {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.entries.insert(key.into(), value.into());
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("Prefs is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        bincode::deserialize(bytes).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrefsRequest {
    /// Return the stored blob, or nothing if this node has never saved one.
    Load { version: u16 },
    /// Replace the stored blob wholesale.
    Save { version: u16, blob: Vec<u8> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrefsResponse {
    Loaded { blob: Option<Vec<u8>> },
    Saved,
    Failed { reason: String },
}

impl PrefsRequest {
    pub fn load() -> Self {
        Self::Load {
            version: PROTOCOL_VERSION,
        }
    }

    pub fn save(prefs: &Prefs) -> Self {
        Self::Save {
            version: PROTOCOL_VERSION,
            blob: prefs.encode(),
        }
    }

    pub fn version(&self) -> u16 {
        match self {
            Self::Load { version } | Self::Save { version, .. } => *version,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("PrefsRequest is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        bincode::deserialize(bytes).map_err(|e| e.to_string())
    }
}

impl PrefsResponse {
    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("PrefsResponse is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        bincode::deserialize(bytes).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_responses_round_trip() {
        let mut prefs = Prefs::default();
        prefs.set(THEME, "light");

        let request = PrefsRequest::save(&prefs);
        assert_eq!(
            PrefsRequest::decode(&request.encode())
                .expect("must decode: this test produced the bytes"),
            request
        );
        assert_eq!(request.version(), PROTOCOL_VERSION);

        let response = PrefsResponse::Loaded {
            blob: Some(prefs.encode()),
        };
        assert_eq!(
            PrefsResponse::decode(&response.encode())
                .expect("must decode: this test produced the bytes"),
            response
        );
    }

    #[test]
    fn a_blob_round_trips_through_the_map() {
        let mut prefs = Prefs::default();
        prefs.set(THEME, "dark");
        assert_eq!(
            Prefs::decode(&prefs.encode())
                .expect("must decode: this test produced the bytes")
                .get(THEME),
            Some("dark")
        );
    }

    /// The reason preferences are a map: a build that has never heard of a key
    /// must not delete it by saving.
    #[test]
    fn unknown_keys_survive_a_read_and_write_cycle() {
        let mut written_by_a_newer_build = Prefs::default();
        written_by_a_newer_build.set(THEME, "light");
        written_by_a_newer_build.set("density", "compact");

        let mut round_tripped = Prefs::decode(&written_by_a_newer_build.encode())
            .expect("must decode: this test produced the bytes");
        round_tripped.set(THEME, "dark");

        assert_eq!(round_tripped.get("density"), Some("compact"));
    }

    #[test]
    fn a_garbled_payload_is_an_error_not_a_panic() {
        assert!(PrefsRequest::decode(&[0xff; 4]).is_err());
        assert!(Prefs::decode(&[0xff; 4]).is_err());
    }
}

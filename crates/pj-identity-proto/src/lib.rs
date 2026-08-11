//! The wire protocol between the app and the identity delegate.
//!
//! # Why this is its own crate
//!
//! A delegate's key is `hash(code + parameters)`, and the secrets it stores hang off
//! that key. So *any* change to the delegate's compiled wasm moves every stored
//! identity into a new namespace, and users silently come back as strangers.
//!
//! When the delegate depended on `pj-core`, every unrelated change there — a new
//! board op, a new field on a view — rebuilt the delegate and orphaned everyone's
//! key. That happened in practice. Splitting the protocol out means the delegate's
//! code now changes only when the protocol itself does, which is rare and
//! deliberate.
//!
//! Keep this crate's dependencies minimal for the same reason: a dependency bump
//! here is a key migration for every user.
//!
//! # Why a delegate at all
//!
//! A node serves a web app inside a sandboxed iframe on an opaque origin, where
//! `localStorage`, `sessionStorage`, and `IndexedDB` all throw. The app therefore has
//! nowhere durable of its own to keep a signing key. A delegate runs inside the node
//! and does.

use serde::{Deserialize, Serialize};

/// Bumped only on a breaking change to these messages. A delegate is immutable once
/// published, so a mismatch has to be detectable rather than silently misparsed.
pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentityRequest {
    /// Return the stored seed, creating one from `entropy` if there is none yet.
    ///
    /// The app supplies the entropy because a delegate has no RNG of its own, and
    /// the browser does (`crypto.getRandomValues`, which the sandbox permits even
    /// though it denies storage).
    GetOrCreate { version: u16, entropy: [u8; 32] },
    /// Overwrite the stored seed, for restoring from a recovery key.
    Replace { version: u16, seed: [u8; 32] },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentityResponse {
    Seed {
        seed: [u8; 32],
        /// True when this call is what created it, so the UI can tell a first visit
        /// from a returning one.
        created: bool,
    },
    Failed {
        reason: String,
    },
}

impl IdentityRequest {
    pub fn get_or_create(entropy: [u8; 32]) -> Self {
        Self::GetOrCreate {
            version: PROTOCOL_VERSION,
            entropy,
        }
    }

    pub fn replace(seed: [u8; 32]) -> Self {
        Self::Replace {
            version: PROTOCOL_VERSION,
            seed,
        }
    }

    pub fn version(&self) -> u16 {
        match self {
            Self::GetOrCreate { version, .. } | Self::Replace { version, .. } => *version,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("IdentityRequest is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        bincode::deserialize(bytes).map_err(|e| e.to_string())
    }
}

impl IdentityResponse {
    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("IdentityResponse is always serializable")
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
        let request = IdentityRequest::get_or_create([7; 32]);
        assert_eq!(
            IdentityRequest::decode(&request.encode())
                .expect("must decode: this test produced the bytes"),
            request
        );
        assert_eq!(request.version(), PROTOCOL_VERSION);

        let response = IdentityResponse::Seed {
            seed: [9; 32],
            created: true,
        };
        assert_eq!(
            IdentityResponse::decode(&response.encode())
                .expect("must decode: this test produced the bytes"),
            response
        );
    }

    #[test]
    fn a_garbled_payload_is_an_error_not_a_panic() {
        assert!(IdentityRequest::decode(&[0xff; 4]).is_err());
    }
}

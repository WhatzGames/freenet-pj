//! The user's identity: an ed25519 keypair, kept by the node's identity delegate.
//!
//! Freenet serves a web app inside a sandboxed iframe on an opaque origin, where
//! `localStorage`, `sessionStorage`, and `IndexedDB` all throw. So the app has
//! nowhere durable of its own to keep a key, and the seed comes from
//! [`crate::node::ask_identity`] instead — the delegate runs inside the node and
//! its storage is unaffected by the sandbox.
//!
//! Web crypto still works under the sandbox, which is why [`random_bytes`] can
//! supply the entropy a new identity is minted from.

use ed25519_dalek::SigningKey;
use pj_core::MemberId;

const NAME_KEY: &str = "freenet-pj:name";

#[derive(Clone)]
pub(crate) struct Identity {
    signing_key: SigningKey,
    pub(crate) member: MemberId,
    pub(crate) name: String,
}

impl Identity {
    /// Builds an identity from the seed the delegate handed back.
    pub(crate) fn from_seed(seed: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&seed);
        let member = MemberId(signing_key.verifying_key().to_bytes());
        // A display name is best-effort local decoration; the authoritative one
        // lives in the board's own `AddMember` op, which is why losing this is
        // harmless.
        let name = read(NAME_KEY).unwrap_or_else(|| format!("anon-{}", member.short()));

        Self {
            signing_key,
            member,
            name,
        }
    }

    /// Parses an exported recovery key.
    pub(crate) fn seed_from_recovery_key(encoded: &str) -> Option<[u8; 32]> {
        let mut seed = [0u8; 32];
        let written = bs58::decode(encoded.trim()).onto(&mut seed).ok()?;
        (written == 32).then_some(seed)
    }

    pub(crate) fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// The secret key, encoded so it can be carried to another node — the one
    /// thing delegates cannot do, since their storage is node-local.
    pub(crate) fn recovery_key(&self) -> String {
        bs58::encode(self.signing_key.to_bytes()).into_string()
    }

    pub(crate) fn with_name(&self, name: impl Into<String>) -> Self {
        let name = name.into();
        write(NAME_KEY, &name);
        Self {
            signing_key: self.signing_key.clone(),
            member: self.member,
            name,
        }
    }
}

/// Cryptographically strong random bytes from the browser.
///
/// `pj-core` has no RNG so that it compiles unchanged into the contract and the
/// delegate, neither of which has entropy to draw on; it enters here.
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    let crypto = web_sys::window()
        .and_then(|window| window.crypto().ok())
        .expect("a browser with web crypto");
    crypto
        .get_random_values_with_u8_array(&mut buf)
        .expect("web crypto refused to produce randomness");
    buf
}

/// `None` whenever the browser denies storage, which under an opaque origin it
/// does by throwing rather than returning empty.
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

fn read(key: &str) -> Option<String> {
    storage()?.get_item(key).ok()?.filter(|v| !v.is_empty())
}

fn write(key: &str, value: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(key, value);
    }
}

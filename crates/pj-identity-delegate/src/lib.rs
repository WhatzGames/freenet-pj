//! The delegate that remembers who you are.
//!
//! Freenet serves a web app inside a sandboxed iframe on an opaque origin, where
//! every browser storage API throws. Without somewhere to keep a key, the app
//! would mint a new identity on every reload and the user would silently lose
//! access to boards they own. A delegate runs inside the *node*, has persistent
//! secret storage, and is therefore unaffected by the sandbox.
//!
//! # Scope of trust
//!
//! Secrets are keyed by the calling app's origin, so a different web contract
//! that discovers this delegate's key cannot read this app's identity. Requests
//! that do not come from a web app are refused outright — an inter-delegate
//! caller has no business reading a user's signing key.
//!
//! This is a *keystore*: it hands the seed back and the app signs locally. The
//! stronger design is for the delegate to hold the key and sign on request, so
//! the app never touches the secret at all; that costs an async signing path in
//! the client and is noted in the README as the next step.

use freenet_stdlib::prelude::*;
use pj_identity_proto::{IdentityRequest, IdentityResponse, PROTOCOL_VERSION};

/// Bumped whenever this delegate's wasm is deliberately rebuilt, purely so the
/// generation is visible in diagnostics. Changing it changes the delegate's key, so
/// the old key must be appended to the frontend's predecessor list at the same time.
const GENERATION: u32 = 4;

pub struct IdentityDelegate;

#[delegate]
impl DelegateInterface for IdentityDelegate {
    fn process(
        ctx: &mut DelegateCtx,
        _parameters: Parameters<'static>,
        origin: Option<MessageOrigin>,
        message: InboundDelegateMsg<'_>,
    ) -> Result<Vec<OutboundDelegateMsg>, DelegateError> {
        // Only application messages carry requests; the contract-response and
        // notification variants are for delegates that drive contracts, which this
        // one does not.
        let InboundDelegateMsg::ApplicationMessage(incoming) = message else {
            return Ok(Vec::new());
        };

        let response = match secret_key_for(origin.as_ref()) {
            Some(secret_key) => handle(ctx, &secret_key, &incoming.payload),
            None => {
                IdentityResponse::failed("an identity is only served to the web app that owns it")
            }
        };

        Ok(vec![OutboundDelegateMsg::ApplicationMessage(
            ApplicationMessage::new(response.encode()).processed(true),
        )])
    }
}

fn handle(ctx: &mut DelegateCtx, secret_key: &[u8], payload: &[u8]) -> IdentityResponse {
    let request = match IdentityRequest::decode(payload) {
        Ok(request) => request,
        Err(err) => return IdentityResponse::failed(format!("unreadable request: {err}")),
    };

    // A delegate cannot be upgraded in place, so a client speaking a newer
    // protocol has to be told plainly rather than have its bytes misread.
    if request.version() != PROTOCOL_VERSION {
        return IdentityResponse::failed(format!(
            "this delegate (generation {GENERATION}) speaks identity protocol \
             v{PROTOCOL_VERSION}, the app asked for v{}",
            request.version()
        ));
    }

    match request {
        IdentityRequest::GetOrCreate { entropy, .. } => match read_seed(ctx, secret_key) {
            Some(seed) => IdentityResponse::Seed {
                seed,
                created: false,
            },
            None => {
                if ctx.set_secret(secret_key, &entropy) {
                    IdentityResponse::Seed {
                        seed: entropy,
                        created: true,
                    }
                } else {
                    IdentityResponse::failed("the node refused to store the identity")
                }
            }
        },

        IdentityRequest::Replace { seed, .. } => {
            if ctx.set_secret(secret_key, &seed) {
                IdentityResponse::Seed {
                    seed,
                    created: false,
                }
            } else {
                IdentityResponse::failed("the node refused to store the identity")
            }
        }
    }
}

fn read_seed(ctx: &DelegateCtx, secret_key: &[u8]) -> Option<[u8; 32]> {
    let stored = ctx.get_secret(secret_key)?;
    // A wrong-length secret means something other than a seed is under this key;
    // treat it as absent rather than panicking inside the node.
    stored.try_into().ok()
}

/// Prints the delegate key of the currently staged wasm.
///
/// A delegate's key is `hash(code + parameters)`, and its secret namespace hangs off
/// that key — so rebuilding the delegate differently moves every stored identity to
/// a new namespace. The cure is to register the new generation with the old keys as
/// predecessors, which makes the node copy the secrets forward, and that needs the
/// old key written down. This is how to read it:
///
/// ```text
/// cargo test -p pj-identity-delegate -- --nocapture delegate_key
/// ```
#[cfg(test)]
#[test]
fn delegate_key_of_staged_wasm() {
    let staged = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../pj-web/contract/pj_identity_delegate.wasm"
    );
    match std::fs::read(staged) {
        Ok(wasm) => {
            let code = DelegateCode::from(wasm);
            let params = Parameters::from(Vec::new());
            let delegate = Delegate::from((&code, &params));
            println!("staged delegate key: {}", delegate.key().encode());
        }
        Err(err) => println!("no staged delegate wasm ({err}); run scripts/build.sh first"),
    }
}

/// The secret key an origin's identity is filed under.
///
/// Including the calling contract's id is what isolates apps from each other: a
/// different web app talking to this delegate gets a different namespace, not this
/// app's key.
fn secret_key_for(origin: Option<&MessageOrigin>) -> Option<Vec<u8>> {
    match origin {
        Some(MessageOrigin::WebApp(contract)) => {
            let mut key = b"pj:identity:v1:".to_vec();
            key.extend_from_slice(contract.as_bytes());
            Some(key)
        }
        // Refuse inter-delegate callers and unattributed messages.
        _ => None,
    }
}

//! The delegate that remembers this node's settings for this app.
//!
//! The app is served inside a sandboxed iframe on an opaque origin, where every
//! browser storage API throws. Anything it wants to keep across a reload has to be
//! kept by the node, and a delegate is the node's persistent store.
//!
//! # It stores bytes, not preferences
//!
//! This delegate has no idea what a preference is. It takes an opaque blob, keeps
//! it, and hands it back. That is deliberate: a delegate's key is
//! `hash(code + parameters)` and its secrets hang off that key, so any change to
//! this wasm would move every stored setting into a new namespace and lose it.
//! Keeping the schema on the client means new preferences never touch this file.
//!
//! Kept separate from the identity delegate for the same reason in reverse: adding
//! this to that one would have moved its key, and with it everybody's signing seed.
//!
//! # Scope of trust
//!
//! Secrets are keyed by the calling app's origin, so another web contract that
//! learns this delegate's key cannot read this app's settings. Requests that do not
//! come from a web app are refused: no other delegate has business here.

use freenet_stdlib::prelude::*;
use pj_prefs_proto::{PROTOCOL_VERSION, PrefsRequest, PrefsResponse};

/// Bumped only when this wasm is deliberately rebuilt, so the generation is
/// visible in diagnostics. Changing it changes the delegate's key, which discards
/// every stored preference on every node — see the module docs.
const GENERATION: u32 = 1;

/// Namespace for this app's blob, qualified by the caller so two web contracts
/// sharing this delegate cannot read each other's settings.
const SECRET_PREFIX: &[u8] = b"pj:prefs:v1:";

pub struct PrefsDelegate;

#[delegate]
impl DelegateInterface for PrefsDelegate {
    fn process(
        ctx: &mut DelegateCtx,
        _parameters: Parameters<'static>,
        origin: Option<MessageOrigin>,
        message: InboundDelegateMsg<'_>,
    ) -> Result<Vec<OutboundDelegateMsg>, DelegateError> {
        let InboundDelegateMsg::ApplicationMessage(incoming) = message else {
            return Ok(Vec::new());
        };

        let response = match secret_key_for(origin.as_ref()) {
            Some(secret_key) => handle(ctx, &secret_key, &incoming.payload),
            None => {
                PrefsResponse::failed("preferences are only served to the web app that owns them")
            }
        };

        Ok(vec![OutboundDelegateMsg::ApplicationMessage(
            ApplicationMessage::new(response.encode()).processed(true),
        )])
    }
}

fn handle(ctx: &mut DelegateCtx, secret_key: &[u8], payload: &[u8]) -> PrefsResponse {
    let request = match PrefsRequest::decode(payload) {
        Ok(request) => request,
        Err(err) => return PrefsResponse::failed(format!("unreadable request: {err}")),
    };

    // A delegate cannot be upgraded in place, so a client speaking a newer protocol
    // has to be told plainly rather than have its bytes misread.
    if request.version() != PROTOCOL_VERSION {
        return PrefsResponse::failed(format!(
            "this delegate (generation {GENERATION}) speaks preferences protocol \
             v{PROTOCOL_VERSION}, the app asked for v{}",
            request.version()
        ));
    }

    match request {
        PrefsRequest::Load { .. } => PrefsResponse::Loaded {
            blob: ctx.get_secret(secret_key),
        },
        PrefsRequest::Save { blob, .. } => {
            if ctx.set_secret(secret_key, &blob) {
                PrefsResponse::Saved
            } else {
                PrefsResponse::failed("the node refused to store the preferences")
            }
        }
    }
}

/// The storage key for a caller, or `None` for callers that may not have one.
fn secret_key_for(origin: Option<&MessageOrigin>) -> Option<Vec<u8>> {
    match origin? {
        MessageOrigin::WebApp(instance) => {
            let mut key = SECRET_PREFIX.to_vec();
            key.extend_from_slice(instance.as_bytes());
            Some(key)
        }
        // Another delegate has no settings of its own here.
        _ => None,
    }
}

/// Prints the delegate key of the currently staged wasm.
///
/// ```text
/// cargo test -p pj-prefs-delegate -- --nocapture delegate_key
/// ```
#[cfg(test)]
#[test]
fn delegate_key_of_staged_wasm() {
    let staged = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../pj-web/contract/pj_prefs_delegate.wasm"
    );
    match std::fs::read(staged) {
        Ok(wasm) => {
            let code = DelegateCode::from(wasm);
            let params = Parameters::from(Vec::new());
            let delegate = Delegate::from((&code, &params));
            println!("staged prefs delegate key: {}", delegate.key().encode());
        }
        Err(err) => println!("no staged delegate wasm ({err}); run scripts/build.sh first"),
    }
}

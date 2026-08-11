//! The WebSocket link to the local Freenet node.
//!
//! The app is served *by* the node it talks to, so the socket URL is derived from
//! the page's own origin rather than hardcoded — that way the same build works
//! whatever port the node is on.
//!
//! Everything here is a thin translation layer over `freenet_stdlib::client_api`,
//! turning its request/response types into a small [`NodeEvent`] the store can
//! react to.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use wasm_bindgen::closure::Closure;

use freenet_stdlib::client_api::{
    ClientError, ClientRequest, ContractRequest, ContractResponse, DelegateRequest, HostResponse,
    WebApi,
};
use freenet_stdlib::prelude::{
    ApplicationMessage, ContractContainer, ContractInstanceId, ContractKey, DelegateKey,
    InboundDelegateMsg, OutboundDelegateMsg, Parameters, RelatedContracts, UpdateData,
    WrappedState,
};
use pj_identity_proto::{IdentityRequest, IdentityResponse};
use pj_prefs_proto::{PrefsRequest, PrefsResponse};

use crate::contract;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;

#[wasm_bindgen]
extern "C" {
    /// Opens a connection to the node — see `bridge.js`.
    ///
    /// Served from a node, the app runs inside a sandboxed iframe on an opaque
    /// origin and cannot reach the node's socket itself; the shell proxies it.
    /// The returned object has a `WebSocket` shape either way.
    #[wasm_bindgen(js_name = __freenetSocket, catch)]
    fn open_socket(url: &str) -> Result<JsValue, JsValue>;
}

/// The node's client API endpoint. Verified against a live 0.2.105 node.
const API_PATH: &str = "/v1/contract/command?encodingProtocol=native";
/// Used only when the page is not being served by a node (e.g. opened from disk
/// during development).
const DEV_FALLBACK_ORIGIN: &str = "ws://127.0.0.1:7509";

/// What the node told us, reduced to the cases this app cares about.
#[derive(Clone)]
pub(crate) enum NodeEvent {
    Open,
    Closed(String),
    Error(String),
    /// State came back for a board we asked for. `parameters` is present when the
    /// contract code was returned with it, and is how a peer joining a board
    /// learns the board's owner and name.
    ///
    /// `key` is the board's real address as the network knows it, which is not
    /// necessarily the one this build would compute — see
    /// [`crate::store::Store::contract_key`].
    Got {
        key: ContractKey,
        parameters: Option<Vec<u8>>,
        state: Vec<u8>,
    },
    Missing(ContractInstanceId),
    Put(ContractKey),
    /// A subscribed contract changed underneath us.
    ///
    /// `key` matters: the app subscribes to both a board and the registry, and
    /// keeps its subscription to a board after navigating away from it. Without
    /// the key there is no way to tell whose update this is, and a stale one would
    /// be merged into the wrong state.
    Changed {
        key: ContractKey,
        state: Option<Vec<u8>>,
        delta: Option<Vec<u8>>,
    },
    Subscribed {
        subscribed: bool,
    },
    /// The socket dropped and another attempt is scheduled.
    Reconnecting {
        attempt: u32,
        in_ms: u32,
    },
    /// The number of queued writes changed, so the UI can show it.
    OutboxChanged,
    /// The node completed something that returns no data — notably registering the
    /// delegate, which has to finish before its secrets can be asked for.
    Acknowledged,
    /// A delegate finished registering — including, for a registration carrying
    /// predecessors, the node's synchronous copy-forward of their secrets.
    DelegateRegistered(DelegateKey),
    /// The identity delegate answered.
    Identity {
        /// Which delegate answered — the current one, or a retired generation
        /// being asked whether it still holds a seed.
        from: DelegateKey,
        response: IdentityResponse,
    },
    /// The preferences delegate answered.
    Preferences(PrefsResponse),
}

type Handler = Rc<dyn Fn(NodeEvent)>;

/// Longest wait between reconnection attempts.
const MAX_BACKOFF_MS: u32 = 30_000;

thread_local! {
    /// The live connection. `Rc<RefCell<_>>` rather than a bare `WebApi` so a
    /// send can take a handle out of the thread-local and then await on it.
    static API: RefCell<Option<Rc<RefCell<WebApi>>>> = const { RefCell::new(None) };
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
    /// Requests made while the socket was down, sent in order once it is back.
    ///
    /// Without this, a write attempted during an outage is applied locally and then
    /// silently dropped — the user sees their edit, and a reload loses it.
    static OUTBOX: RefCell<Vec<ClientRequest<'static>>> = const { RefCell::new(Vec::new()) };
    static IS_OPEN: Cell<bool> = const { Cell::new(false) };
    /// Consecutive failed attempts, for backoff.
    static ATTEMPT: Cell<u32> = const { Cell::new(0) };
}

/// How many writes are waiting for the connection to come back.
pub(crate) fn pending_writes() -> usize {
    OUTBOX.with(|outbox| outbox.borrow().len())
}

pub(crate) fn is_open() -> bool {
    IS_OPEN.with(Cell::get)
}

/// Opens the connection, reopening it by itself if it drops.
///
/// Events arrive on `on_event` until the page goes away.
pub(crate) fn connect(on_event: impl Fn(NodeEvent) + 'static) -> Result<(), String> {
    HANDLER.with(|slot| *slot.borrow_mut() = Some(Rc::new(on_event)));
    open_socket_now()
}

/// Reconnects immediately, regardless of where the backoff had got to.
pub(crate) fn reconnect_now() {
    ATTEMPT.with(|attempt| attempt.set(0));
    if let Err(err) = open_socket_now() {
        report(NodeEvent::Error(err));
    }
}

fn open_socket_now() -> Result<(), String> {
    let Some(handler) = HANDLER.with(|slot| slot.borrow().clone()) else {
        return Err("connect() has not been called".to_owned());
    };
    let url = ws_url()?;
    // The bridge hands back either a real WebSocket or a shell-proxied stand-in
    // with the same surface; `WebApi` only ever touches the members both provide.
    let socket: web_sys::WebSocket = open_socket(&url)
        .map_err(|err| format!("could not open a socket to {url}: {err:?}"))?
        .unchecked_into();

    let on_result = {
        let handler = handler.clone();
        move |result: Result<HostResponse, ClientError>| match result {
            Ok(response) => {
                if let Some(event) = translate(response) {
                    handler(event);
                }
            }
            Err(err) => handler(NodeEvent::Error(err.to_string())),
        }
    };

    // `WebApi::start` requires this one to be `Clone`, which a closure capturing
    // only an `Rc` satisfies.
    //
    // A dropped socket arrives here rather than through a separate close callback:
    // `WebApi`'s own `onclose` reports it as a connection error whose message says
    // "closed".
    let on_error = {
        let handler = handler.clone();
        move |err: freenet_stdlib::client_api::Error| {
            let message = err.to_string();
            if message.contains("closed") {
                IS_OPEN.with(|open| open.set(false));
                handler(NodeEvent::Closed(message));
                schedule_reconnect();
            } else {
                handler(NodeEvent::Error(message));
            }
        }
    };

    let on_open = {
        let handler = handler.clone();
        move || {
            IS_OPEN.with(|open| open.set(true));
            ATTEMPT.with(|attempt| attempt.set(0));
            handler(NodeEvent::Open);
            // Anything attempted during the outage goes out now, in order.
            flush_outbox();
        }
    };

    let api = WebApi::start(socket, on_result, on_error, on_open);
    API.with(|slot| *slot.borrow_mut() = Some(Rc::new(RefCell::new(api))));
    Ok(())
}

/// Tries again later, backing off so a node that is down does not get hammered.
fn schedule_reconnect() {
    let attempt = ATTEMPT.with(|a| {
        let next = a.get().saturating_add(1);
        a.set(next);
        next
    });
    // 1s, 2s, 4s … capped. `attempt` is 1 on the first retry.
    let delay = 1000u32
        .saturating_mul(1u32 << attempt.min(5))
        .min(MAX_BACKOFF_MS);

    report(NodeEvent::Reconnecting {
        attempt,
        in_ms: delay,
    });

    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once_into_js(move || {
        // Give up on this generation of the socket and build a fresh one.
        if let Err(err) = open_socket_now() {
            report(NodeEvent::Error(err));
            schedule_reconnect();
        }
    });
    // `setTimeout` takes a signed millisecond count. `delay` is capped at
    // `MAX_BACKOFF_MS`, so the conversion cannot fail; falling back to the cap
    // rather than wrapping means a future change to that cap degrades into a long
    // wait instead of a negative one, which fires immediately and busy-loops.
    let delay = i32::try_from(delay).unwrap_or(i32::MAX);
    let _ = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay);
}

/// Runs a future that is expected to finish without ever suspending, returning
/// `None` if it suspends anyway.
///
/// `WebApi::send` is `async fn` but contains no await points — it serialises the
/// request and calls `WebSocket.send`. Awaiting it while holding a `RefCell` borrow
/// would be a deadlock risk in general, and clippy is right to say so; the only
/// reason it is safe is that assumption, which is about somebody else's crate and
/// could stop being true in a version bump.
///
/// So the assumption is checked rather than asserted in a comment. If `send` ever
/// grows a suspend point, this returns `None` and the caller reports it, instead of
/// the borrow silently overlapping another.
fn finish_now<F: Future>(future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// Sends everything waiting, oldest first.
fn flush_outbox() {
    let Some(api) = API.with(|slot| slot.borrow().clone()) else {
        return;
    };

    // Deferred to a microtask even though nothing below suspends. `send` can call
    // its error handler synchronously, which reaches the store, which may queue
    // another request — and that re-entrant flush must not land inside this one
    // while the borrow below is live.
    spawn_local(async move {
        loop {
            if !is_open() {
                break;
            }
            // Taken one at a time so a request queued *during* the flush still goes
            // out in order behind the rest.
            let next = OUTBOX.with(|outbox| {
                let mut outbox = outbox.borrow_mut();
                (!outbox.is_empty()).then(|| outbox.remove(0))
            });
            let Some(request) = next else {
                break;
            };

            // The borrow lives exactly as long as the send, and never across a
            // suspend point — see `finish_now`.
            let sent = {
                let mut api = api.borrow_mut();
                finish_now(api.send(request))
            };

            let failure = match sent {
                Some(Ok(())) => continue,
                // `send` consumed the request, so it cannot be put back. The store's
                // resync-on-reconnect is what makes this recoverable: it re-pushes
                // the whole local state, which is idempotent.
                Some(Err(err)) => format!(
                    "a queued request could not be sent ({err}); it will be recovered by the \
                     next resync"
                ),
                None => "the node client suspended mid-send, which this build does not \
                         expect; the request will be recovered by the next resync"
                    .to_owned(),
            };

            report(NodeEvent::Error(failure));
            IS_OPEN.with(|open| open.set(false));
            break;
        }
        report(NodeEvent::OutboxChanged);
    });
}

/// Fetches a board's state and subscribes to further changes.
///
/// `return_contract_code` is what makes joining a board by id possible: the
/// response carries the contract's parameters, which is the only way a peer that
/// was handed nothing but an id can learn who owns the board.
pub(crate) fn get(instance: ContractInstanceId) {
    send(ContractRequest::Get {
        key: instance,
        return_contract_code: true,
        subscribe: true,
        blocking_subscribe: false,
    });
}

/// Creates a board by instantiating the contract with its parameters and genesis
/// state.
pub(crate) fn put(contract: ContractContainer, state: WrappedState) {
    send(ContractRequest::Put {
        contract,
        state,
        related_contracts: RelatedContracts::default(),
        subscribe: true,
        blocking_subscribe: false,
    });
}

/// Pushes new ops to a board.
pub(crate) fn update(key: ContractKey, delta: Vec<u8>) {
    send(ContractRequest::Update {
        key,
        data: UpdateData::Delta(delta.into()),
    });
}

/// Pushes a whole local state, rather than a delta.
///
/// This is the recovery primitive. Every state here is a grow-only set merged by
/// union, so re-sending all of it is idempotent — which means that after an outage
/// the client does not need to know *which* writes got through. It re-offers
/// everything and the contract sorts it out.
pub(crate) fn push_state(key: ContractKey, state: Vec<u8>) {
    send(ContractRequest::Update {
        key,
        data: UpdateData::State(state.into()),
    });
}

/// Registers the identity delegate with the node, which has to happen before it
/// will answer any request — and asks the node to carry forward the secrets of
/// earlier generations of the delegate.
///
/// The carry-forward is what keeps identities alive across a delegate rebuild. A
/// delegate's key is `hash(code + parameters)` and its secrets hang off that key, so
/// changing the wasm at all files every user's seed under a new namespace and they
/// come back as strangers. Listing the old keys as predecessors makes the node copy
/// the sealed secret bytes across; it never executes the old wasm, the copy is
/// idempotent per pair, and unknown predecessors are ignored.
///
/// `cipher` and `nonce` are vestigial — the node has ignored both since secrets
/// moved to being sealed client-side — but the variant still carries them.
pub(crate) fn register_delegate() {
    let predecessors: Vec<_> = contract::predecessor_delegate_keys();

    dispatch(ClientRequest::DelegateOp(if predecessors.is_empty() {
        DelegateRequest::RegisterDelegate {
            delegate: contract::delegate_container(),
            cipher: [0; 32],
            nonce: [0; 24],
        }
    } else {
        DelegateRequest::RegisterDelegateWithPredecessors {
            delegate: contract::delegate_container(),
            cipher: [0; 32],
            nonce: [0; 24],
            predecessors,
        }
    }));
}

/// Asks the identity delegate for this user's signing seed.
pub(crate) fn ask_identity(request: &IdentityRequest) {
    ask_identity_of(contract::delegate_key(), request);
}

/// Asks a *particular* delegate, which is how a retired generation is questioned
/// about the seed it still holds.
///
/// The node routes by key and does not need us to hold that delegate's code — it
/// is already registered, from back when it was current. That is what makes an
/// app-side migration possible at all after
/// `RegisterDelegateWithPredecessors` turned out to be unusable.
pub(crate) fn ask_identity_of(key: DelegateKey, request: &IdentityRequest) {
    dispatch(ClientRequest::DelegateOp(
        DelegateRequest::ApplicationMessages {
            key,
            params: Parameters::from(Vec::new()),
            inbound: vec![InboundDelegateMsg::ApplicationMessage(
                ApplicationMessage::new(request.encode()),
            )],
        },
    ));
}

/// Registers the preferences delegate.
///
/// No predecessor list: this delegate is new, so there is nothing to carry
/// forward, and it is designed never to be rebuilt — it stores opaque bytes so
/// that adding a preference is a client change.
pub(crate) fn register_prefs_delegate() {
    dispatch(ClientRequest::DelegateOp(
        DelegateRequest::RegisterDelegate {
            delegate: contract::prefs_delegate_container(),
            // Vestigial, as in `register_delegate`.
            cipher: [0; 32],
            nonce: [0; 24],
        },
    ));
}

/// Reads or writes this node's saved settings for this app.
pub(crate) fn ask_prefs(request: &PrefsRequest) {
    dispatch(ClientRequest::DelegateOp(
        DelegateRequest::ApplicationMessages {
            key: contract::prefs_delegate_key(),
            params: Parameters::from(Vec::new()),
            inbound: vec![InboundDelegateMsg::ApplicationMessage(
                ApplicationMessage::new(request.encode()),
            )],
        },
    ));
}

fn send(request: ContractRequest<'static>) {
    dispatch(ClientRequest::ContractOp(request));
}

/// Queues a request and sends it as soon as the socket allows.
///
/// Everything goes through the queue rather than straight out, so an outage is a
/// delay rather than a silent loss, and ordering is the same either way.
fn dispatch(request: ClientRequest<'static>) {
    OUTBOX.with(|outbox| outbox.borrow_mut().push(request));
    report(NodeEvent::OutboxChanged);
    if is_open() {
        flush_outbox();
    }
}

fn report(event: NodeEvent) {
    HANDLER.with(|slot| {
        if let Some(handler) = slot.borrow().as_ref() {
            handler(event);
        }
    });
}

fn translate(response: HostResponse) -> Option<NodeEvent> {
    match response {
        HostResponse::ContractResponse(ContractResponse::GetResponse {
            key,
            contract,
            state,
        }) => Some(NodeEvent::Got {
            key,
            parameters: contract.map(|container| container.params().as_ref().to_vec()),
            state: state.as_ref().to_vec(),
        }),
        HostResponse::ContractResponse(ContractResponse::NotFound { instance_id }) => {
            Some(NodeEvent::Missing(instance_id))
        }
        HostResponse::ContractResponse(ContractResponse::PutResponse { key }) => {
            Some(NodeEvent::Put(key))
        }
        HostResponse::ContractResponse(ContractResponse::UpdateNotification { key, update }) => {
            let (state, delta) = split_update(update);
            Some(NodeEvent::Changed { key, state, delta })
        }
        HostResponse::Ok => Some(NodeEvent::Acknowledged),
        // Two delegates answer on this channel, and their payloads are different
        // enum encodings — so route on the key rather than guessing from the bytes.
        // Registration answers with an *empty* `values` list, which the old
        // `find_map` swallowed — so the app never saw the one signal that says the
        // delegate is ready, and any wait for it had nothing to wait on.
        HostResponse::DelegateResponse { key, values } if values.is_empty() => {
            Some(NodeEvent::DelegateRegistered(key))
        }
        HostResponse::DelegateResponse { key, values } => {
            let from_prefs = key == contract::prefs_delegate_key();
            values.into_iter().find_map(|value| match value {
                OutboundDelegateMsg::ApplicationMessage(message) if from_prefs => {
                    match PrefsResponse::decode(&message.payload) {
                        Ok(response) => Some(NodeEvent::Preferences(response)),
                        Err(err) => Some(NodeEvent::Error(format!(
                            "the preferences delegate sent something unreadable: {err}"
                        ))),
                    }
                }
                OutboundDelegateMsg::ApplicationMessage(message) => {
                    match IdentityResponse::decode(&message.payload) {
                        // The key matters: a retired generation can be asked for
                        // the seed it still holds, and its answer has to be
                        // distinguishable from the current delegate's. See
                        // `Store::adopt_previous_identity`.
                        Ok(response) => Some(NodeEvent::Identity {
                            from: key.clone(),
                            response,
                        }),
                        Err(err) => Some(NodeEvent::Error(format!(
                            "the identity delegate sent something unreadable: {err}"
                        ))),
                    }
                }
                _ => None,
            })
        }
        HostResponse::ContractResponse(ContractResponse::SubscribeResponse {
            subscribed, ..
        }) => Some(NodeEvent::Subscribed { subscribed }),
        // `UpdateResponse` only confirms our own write landed, and the resulting
        // state arrives as a notification anyway.
        _ => None,
    }
}

fn split_update(update: UpdateData<'static>) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    match update {
        UpdateData::State(state) => (Some(state.as_ref().to_vec()), None),
        UpdateData::Delta(delta) => (None, Some(delta.as_ref().to_vec())),
        UpdateData::StateAndDelta { state, delta } => {
            (Some(state.as_ref().to_vec()), Some(delta.as_ref().to_vec()))
        }
        // Related-contract variants: a board never uses them.
        _ => (None, None),
    }
}

/// Where to reach the node's client API.
///
/// When the node is serving us, it does so under `/v1/contract/web/<id>/`, and the
/// API is on that same origin — so the page's own location is the answer, whatever
/// port the node happens to use. That path is also the reliable signal for *not*
/// being node-served: anywhere else (a static dev server, a file:// URL) the
/// origin belongs to something that is not a Freenet node, so fall back to the
/// default local node instead of talking to the wrong host.
///
/// A `?node=host:port` query parameter overrides both, for a node on a custom port.
fn ws_url() -> Result<String, String> {
    let window = web_sys::window().ok_or("no browser window")?;
    let location = window.location();

    if let Some(origin) = node_override(&location) {
        return Ok(format!("{origin}{API_PATH}"));
    }

    let protocol = location.protocol().unwrap_or_default();
    let host = location.host().unwrap_or_default();
    let path = location.pathname().unwrap_or_default();

    if host.is_empty() || !path.contains("/v1/contract/web/") {
        return Ok(format!("{DEV_FALLBACK_ORIGIN}{API_PATH}"));
    }

    let scheme = if protocol == "https:" { "wss" } else { "ws" };
    Ok(format!("{scheme}://{host}{API_PATH}"))
}

fn node_override(location: &web_sys::Location) -> Option<String> {
    let search = location.search().ok()?;
    let host = search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix("node="))?;
    if host.is_empty() {
        return None;
    }
    Some(if host.starts_with("ws://") || host.starts_with("wss://") {
        host.to_owned()
    } else {
        format!("ws://{host}")
    })
}

//! Reactive application state, and the bridge between the UI and the node.
//!
//! The store holds two representations of a board: the raw op set, which is what
//! the network exchanges, and the folded [`Board`], which is what the UI renders.
//! Every user action becomes a signed op that is applied locally *and* sent to the
//! node. Applying it locally first is safe rather than optimistic in the usual
//! sense: the op is already signed and authorised, so the contract will accept it,
//! and if a concurrent edit arrives the fold resolves both the same way on every
//! peer.
//!
//! It also holds the public registry — the directory contract that makes boards
//! findable at all, since Freenet has no enumeration — and the identity handed
//! over by the node's delegate.

use freenet_stdlib::prelude::{ContractInstanceId, ContractKey, DelegateKey, WrappedState};
use leptos::prelude::*;
use pj_core::legacy::LegacyTask;
use pj_core::org::{OrgDraft, OrgOp, OrgParameters, Organization};
use pj_core::task::{Task, TaskOp, TaskOrg, TaskParameters, TaskSummary};
use pj_core::user::{UserOp, UserParameters, UserProfile};
use pj_core::{
    Board, BoardId, BoardParameters, ColumnId, Draft, Envelope, EnvelopeDelta, EnvelopeState,
    GrantBody, LinkKind, Listing, ListingTarget, MemberId, Op, OrgId, Rank, RegistryDelta,
    RegistryState, Rights, Role, SignedEnvelope, SignedListing, Stamp, TaskAddr, kind,
};
use pj_identity_proto::{IdentityRequest, IdentityResponse};
use pj_prefs_proto::{Prefs, PrefsRequest, PrefsResponse};
use wasm_bindgen::prelude::wasm_bindgen;

use crate::identity::{Identity, random_bytes};
use crate::node::{self, NodeEvent};
use crate::{bootstrap_columns, contract};

/// How many boards the start page lists.
pub(crate) const BROWSE_LIMIT: usize = 25;

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Connection {
    Connecting,
    Open,
    /// Dropped, with another attempt scheduled.
    Reconnecting {
        attempt: u32,
        in_ms: u32,
    },
    Lost(String),
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum BoardStatus {
    /// No board open — show the start page.
    Idle,
    Loading(String),
    /// The node searched and found nothing under that id.
    Missing(String),
    Ready,
}

/// Whether something we created is known to exist on the network yet.
///
/// Deliberately separate from [`BoardStatus`], which answers a different question.
/// `BoardStatus` is about *opening*: have we got the state. This is about
/// *storing*: does the network have it.
///
/// Creating a board builds its genesis state locally, so it is renderable and
/// editable at once — blocking on the acknowledgement would strand the UI on a
/// spinner whenever the network is slow, which says nothing about whether the
/// state was stored. But "renderable" is not "stored", and the difference matters:
/// until the node acknowledges, the address in the URL, the public listing and the
/// bookmark on your own profile all refer to something that might not be there.
/// A reload would meet "the network has no board under …".
///
/// So the board stays usable and this reports, out of the way, what is actually
/// known. Optimistic, but not silent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Publish {
    /// Nothing outstanding.
    Settled,
    /// A `PUT` for this instance is in flight.
    Storing(ContractInstanceId),
    /// Long enough has passed with no acknowledgement that saying nothing would be
    /// misleading. Retryable: a `PUT` of the same state is idempotent, because the
    /// contract merges rather than replaces.
    Unconfirmed(ContractInstanceId),
}

/// How long to leave between asking the node whether it has the new board yet.
const PUBLISH_CONFIRM_EVERY_MS: i32 = 1_500;

/// How many times to ask before calling it unconfirmed.
///
/// Four attempts at 1.5s is generous against a local node, which installs a
/// contract in tens of milliseconds, and still short enough that somebody about to
/// share a link finds out before they send it.
const PUBLISH_CONFIRM_ATTEMPTS: u32 = 4;

/// Which contract a node reply concerns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Registry,
    Organization,
    /// The signed-in user's own profile.
    Profile,
    /// One task, fetched because somebody opened a card.
    Task,
    Board,
}

/// A card that cannot be written yet, because the task it points at has not been
/// confirmed stored. See [`Store::create_task`].
#[derive(Clone)]
struct Placing {
    task: TaskAddr,
    column: ColumnId,
    rank: Rank,
    summary: TaskSummary,
}

/// Which of the things the app can be showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    Start,
    Board,
    /// One task, on its own page — see `#<board>/<task>`.
    Task,
    Organization,
    /// The signed-in user's own page.
    User,
}

/// Whether the shared directory contract exists yet.
///
/// Matters because listing a board is an `Update` when it does and a `Put` when it
/// does not, and getting that backwards would either fail or overwrite the whole
/// directory with a single entry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Registry {
    Unknown,
    Absent,
    Present,
}

#[derive(Clone, Copy)]
pub(crate) struct Store {
    /// `None` until the delegate answers — the app has no key of its own.
    pub(crate) identity: RwSignal<Option<Identity>>,
    pub(crate) connection: RwSignal<Connection>,
    pub(crate) status: RwSignal<BoardStatus>,
    pub(crate) params: RwSignal<Option<BoardParameters>>,
    pub(crate) board: RwSignal<Option<Board>>,
    /// Something went wrong, or an action was refused.
    pub(crate) notice: RwSignal<Option<String>>,
    /// Something went right. Kept apart from `notice` because both used to share
    /// one bar tinted with `--danger`, which announced a successful reconnection
    /// in the same red as a rejected write.
    pub(crate) flash: RwSignal<Option<String>>,
    /// The task currently being dragged, so drop targets know what landed on them.
    pub(crate) dragging: RwSignal<Option<TaskAddr>>,
    /// The task whose detail panel is open.
    ///
    /// Selecting one starts a fetch: a board carries no task bodies, so opening a
    /// card is the moment its contents are asked for. See [`Store::task`].
    pub(crate) selected: RwSignal<Option<TaskAddr>>,
    /// The open task's body, `None` while it is still being fetched.
    pub(crate) task: RwSignal<Option<Task>>,
    task_params: RwSignal<Option<TaskParameters>>,
    task_ops: RwSignal<EnvelopeState>,
    task_key: RwSignal<Option<ContractKey>>,
    /// The task fetch in flight, so a reply that arrives after the user has moved
    /// on can be recognised and dropped.
    pending_task_fetch: RwSignal<Option<ContractInstanceId>>,
    /// A card waiting for the network to confirm the task behind it exists.
    pending_placement: RwSignal<Option<Placing>>,
    /// Cards on the open board that were written before tasks had contracts of
    /// their own, and are therefore invisible until converted.
    pub(crate) legacy_tasks: RwSignal<Vec<LegacyTask>>,
    /// How many *ops* those cards account for, which is not their count — see
    /// [`pj_core::legacy::legacy_op_count`].
    pub(crate) legacy_ops: RwSignal<usize>,
    /// The conversion in progress, one card at a time.
    legacy_queue: RwSignal<Vec<LegacyTask>>,
    /// The old card whose replacement is being stored, to be retired once it is.
    converting: RwSignal<Option<pj_core::TaskId>>,
    /// Whether a whole-board conversion is running, so its end can be announced
    /// once rather than after every card.
    migrating_board: RwSignal<bool>,
    /// The open board's address as the *network* knows it, learned from the node's
    /// own response rather than recomputed locally.
    ///
    /// A board's address is `hash(contract code + parameters)`, so any change to
    /// the contract — even one as incidental as bumping the Rust edition — gives
    /// the same board a different address under a new build. Deriving the address
    /// from the embedded code would therefore aim updates at a contract that does
    /// not exist, and the writes would vanish without an error. The node knows the
    /// real one; believe it.
    pub(crate) contract_key: RwSignal<Option<ContractKey>>,
    /// This node's saved settings for this app. `None` until the delegate answers,
    /// so a save cannot race ahead of the load and wipe what is stored.
    prefs: RwSignal<Option<Prefs>>,
    /// An explicit choice about the sidebar, or `None` to follow the width.
    sidebar_pref: RwSignal<Option<bool>>,
    /// Whether the viewport can currently afford the sidebar.
    sidebar_wide: RwSignal<bool>,
    pub(crate) registry: RwSignal<RegistryState>,
    pub(crate) registry_presence: RwSignal<Registry>,
    pub(crate) search: RwSignal<String>,
    pub(crate) org_search: RwSignal<String>,

    pub(crate) view: RwSignal<View>,
    /// The open organization. Kept loaded while a board is open so a project can
    /// offer its owning organization's members for assignment.
    pub(crate) org: RwSignal<Option<Organization>>,
    pub(crate) org_params: RwSignal<Option<OrgParameters>>,
    pub(crate) org_key: RwSignal<Option<ContractKey>>,
    org_ops: RwSignal<EnvelopeState>,

    /// What we last asked the node for. A `GetResponse` carries a key but not what
    /// kind of thing it is, and the app has three contract types in flight, so the
    /// request has to be remembered to route the reply.
    pending_board: RwSignal<Option<ContractInstanceId>>,
    pending_org: RwSignal<Option<ContractInstanceId>>,
    /// Whether what we just created has reached the network. See [`Publish`].
    pub(crate) publish: RwSignal<Publish>,
    /// A task a link asked for, waiting for the board that holds it to arrive.
    pending_task: RwSignal<Option<TaskAddr>>,
    /// This user's own profile: their canonical device list and memberships.
    pub(crate) profile: RwSignal<Option<UserProfile>>,
    profile_ops: RwSignal<EnvelopeState>,
    profile_key: RwSignal<Option<ContractKey>>,
    /// Whether the profile contract exists yet — same Put-versus-Update question the
    /// registry has.
    profile_presence: RwSignal<Registry>,
    /// Writes waiting for the connection to come back.
    pub(crate) pending_writes: RwSignal<usize>,
    /// A public key handed over by a `#link/<key>` URL, waiting for confirmation.
    pub(crate) pending_device_key: RwSignal<Option<String>>,
    /// Guards against asking the delegate for a seed more than once.
    identity_requested: RwSignal<bool>,
    /// Whether retired delegate generations have been asked for a seed and none
    /// has been accepted yet. See [`Store::ask_predecessors`].
    migrating: RwSignal<bool>,
    /// A listing waiting for us to find out whether the directory exists.
    pending_listing: RwSignal<Option<SignedListing>>,
    /// The raw op set. Never rendered directly; the folded board is.
    ops: RwSignal<EnvelopeState>,
}

impl Store {
    pub(crate) fn new() -> Self {
        Self {
            identity: RwSignal::new(None),
            connection: RwSignal::new(Connection::Connecting),
            status: RwSignal::new(BoardStatus::Idle),
            params: RwSignal::new(None),
            board: RwSignal::new(None),
            notice: RwSignal::new(None),
            flash: RwSignal::new(None),
            dragging: RwSignal::new(None),
            selected: RwSignal::new(None),
            task: RwSignal::new(None),
            task_params: RwSignal::new(None),
            task_ops: RwSignal::new(EnvelopeState::new()),
            task_key: RwSignal::new(None),
            pending_task_fetch: RwSignal::new(None),
            pending_placement: RwSignal::new(None),
            legacy_tasks: RwSignal::new(Vec::new()),
            legacy_ops: RwSignal::new(0),
            converting: RwSignal::new(None),
            migrating_board: RwSignal::new(false),
            legacy_queue: RwSignal::new(Vec::new()),
            contract_key: RwSignal::new(None),
            prefs: RwSignal::new(None),
            sidebar_pref: RwSignal::new(None),
            sidebar_wide: RwSignal::new(wide_enough()),
            registry: RwSignal::new(RegistryState::new()),
            registry_presence: RwSignal::new(Registry::Unknown),
            search: RwSignal::new(String::new()),
            org_search: RwSignal::new(String::new()),
            view: RwSignal::new(View::Start),
            org: RwSignal::new(None),
            org_params: RwSignal::new(None),
            org_key: RwSignal::new(None),
            org_ops: RwSignal::new(EnvelopeState::new()),
            pending_board: RwSignal::new(None),
            pending_org: RwSignal::new(None),
            publish: RwSignal::new(Publish::Settled),
            pending_task: RwSignal::new(None),
            pending_writes: RwSignal::new(0),
            pending_device_key: RwSignal::new(None),
            profile: RwSignal::new(None),
            profile_ops: RwSignal::new(EnvelopeState::new()),
            profile_key: RwSignal::new(None),
            profile_presence: RwSignal::new(Registry::Unknown),
            identity_requested: RwSignal::new(false),
            migrating: RwSignal::new(false),
            pending_listing: RwSignal::new(None),
            ops: RwSignal::new(EnvelopeState::new()),
        }
    }

    /// Connects to the node, claims an identity, and loads the directory.
    pub(crate) fn boot(self) {
        if let Err(err) = node::connect(move |event| self.handle(event)) {
            self.connection.set(Connection::Lost(err));
        }
    }

    pub(crate) fn me(self) -> Option<Identity> {
        self.identity.get_untracked()
    }

    /// Recovers after the connection has been away.
    ///
    /// Rather than tracking which writes made it out — which the client cannot know
    /// once a send has failed — it re-offers the entire local state of everything
    /// open, and re-fetches each to pick up whatever it missed. Both halves are safe
    /// to repeat: every state here is a grow-only set merged by union, so pushing all
    /// of it is idempotent, and that is exactly what makes this simple enough to
    /// trust.
    pub(crate) fn resync(self) {
        // Re-subscribe as well as re-read: a subscription belongs to the socket that
        // made it, and that socket is gone.
        node::get(contract::registry_id());

        if let Some(key) = self.profile_key.get_untracked() {
            let state = self.profile_ops.get_untracked();
            if !state.is_empty() {
                node::push_state(key, state.encode());
            }
            node::get(*key.id());
        }

        if let Some(key) = self.org_key.get_untracked() {
            let state = self.org_ops.get_untracked();
            if !state.is_empty() {
                node::push_state(key, state.encode());
            }
            node::get(*key.id());
        }

        if let Some(key) = self.contract_key.get_untracked() {
            let state = self.ops.get_untracked();
            if !state.is_empty() {
                node::push_state(key, state.encode());
            }
            node::get(*key.id());
        }

        // Whatever the connection failure was, it is over. Leaving it on screen
        // next to a green "node connected" badge states two contradictory things
        // at once, and errors do not retire on their own.
        self.notice.set(None);
        self.say_ok("reconnected — your local changes have been re-sent and everything reloaded");
    }

    /// Reports something that went right, and retires it on its own.
    ///
    /// A confirmation that stays on screen stops being a confirmation: it becomes
    /// furniture, and the next one is indistinguishable from the last.
    pub(crate) fn say_ok(&self, text: impl Into<String>) {
        let text = text.into();
        self.flash.set(Some(text.clone()));
        let flash = self.flash;
        crate::ui::after_ms(6000, move || {
            // Only retire the message we posted; a newer one keeps its full time.
            if flash.get_untracked().as_deref() == Some(text.as_str()) {
                flash.set(None);
            }
        });
    }

    /// Retries the connection right away instead of waiting out the backoff.
    pub(crate) fn retry_connection(self) {
        self.connection.set(Connection::Connecting);
        node::reconnect_now();
    }

    /// A contract the node has served back to us, routed by what we asked for.
    fn on_got(self, key: ContractKey, parameters: Option<&[u8]>, state: &[u8]) {
        match self.kind_of(key.id()) {
            Kind::Registry => {
                self.registry_presence.set(Registry::Present);
                self.receive_registry(Some(state.to_vec()), None);
                self.flush_pending_listing();
            }
            Kind::Profile => {
                self.profile_presence.set(Registry::Present);
                self.receive_profile(Some(state.to_vec()), None);
            }
            Kind::Organization => {
                self.org_key.set(Some(key));
                self.receive_org(parameters, state);
            }
            Kind::Task => {
                self.task_key.set(Some(key));
                self.receive_task(parameters, state);
            }
            // Serving it back is the proof that it was stored, whether or not it
            // is still the board on screen.
            Kind::Board if !self.awaiting_board(key.id()) => self.board_stored(&key),
            Kind::Board => {
                self.board_stored(&key);
                self.contract_key.set(Some(key));
                self.receive_board(parameters, state);
            }
        }
    }

    fn handle(self, event: NodeEvent) {
        match event {
            NodeEvent::Reconnecting { attempt, in_ms } => {
                self.connection
                    .set(Connection::Reconnecting { attempt, in_ms });
            }
            NodeEvent::OutboxChanged => self.pending_writes.set(node::pending_writes()),

            NodeEvent::Open => self.on_connected(),
            NodeEvent::Closed(reason) => self.connection.set(Connection::Lost(reason)),
            NodeEvent::DelegateRegistered(key) => {
                if key == contract::delegate_key() {
                    self.ask_identity_once();
                }
            }
            NodeEvent::Preferences(response) => self.receive_prefs(response),
            NodeEvent::Error(message) => {
                // An outright rejection is the likeliest way a publish fails, and it
                // arrives as an error rather than as a silence. No reason to make
                // somebody watch a spinner for eight seconds to learn what the node
                // has already said.
                if let Publish::Storing(instance) = self.publish.get_untracked() {
                    self.publish.set(Publish::Unconfirmed(instance));
                }
                self.notice.set(Some(humanise(&message)));
            }

            // Retried here only if the request made on connect somehow did not go
            // out; a repeat `GetOrCreate` is harmless because it is idempotent.
            NodeEvent::Acknowledged => {
                if !self.identity_requested.get_untracked() {
                    self.identity_requested.set(true);
                    node::ask_identity(&IdentityRequest::get_or_create(random_bytes::<32>()));
                }
            }

            NodeEvent::Identity { from, response } => self.receive_identity(&from, response),

            NodeEvent::Got {
                key,
                parameters,
                state,
            } => self.on_got(key, parameters.as_deref(), &state),
            NodeEvent::Missing(instance) => match self.kind_of(&instance) {
                Kind::Registry => {
                    // Nobody has created the directory yet. Normal on a fresh
                    // network; the first board published will instantiate it.
                    self.registry_presence.set(Registry::Absent);
                    self.flush_pending_listing();
                }
                // A first-time user has no profile yet; the first write creates it.
                Kind::Profile => self.profile_presence.set(Registry::Absent),
                Kind::Organization => self.notice.set(Some(format!(
                    "the network has no organization under {}",
                    instance.encode()
                ))),
                // A card whose task has not arrived, or was never stored. The card
                // stays; only its body is missing, and the cached summary is what
                // keeps the board usable meanwhile.
                Kind::Task => {
                    self.pending_task_fetch.set(None);
                    self.notice
                        .set(Some("that task has not reached this node yet".to_owned()));
                }
                // While a publish is being confirmed, "not there" is expected: the
                // `PUT` is processed asynchronously and the read-back can beat it.
                // Leave the board on screen and let `confirm_publish` decide — the
                // alternative replaces a board somebody is typing into with "the
                // network has no board under …", which would be both alarming and
                // wrong.
                Kind::Board if self.publish.get_untracked() == Publish::Storing(instance) => {}
                // The answer to a question we have stopped asking.
                Kind::Board if !self.awaiting_board(&instance) => {}
                Kind::Board => self.status.set(BoardStatus::Missing(instance.encode())),
            },

            NodeEvent::Put(key) => match self.kind_of(key.id()) {
                Kind::Registry => self.registry_presence.set(Registry::Present),
                Kind::Profile => self.profile_presence.set(Registry::Present),
                Kind::Organization => self.org_key.set(Some(key)),
                Kind::Task => self.task_key.set(Some(key)),
                Kind::Board => {
                    self.board_stored(&key);
                    self.contract_key.set(Some(key));
                    self.status.set(BoardStatus::Ready);
                    set_board_in_url(&key.encoded_contract_id());
                }
            },

            NodeEvent::Changed { key, state, delta } => match self.kind_of(key.id()) {
                Kind::Registry => self.receive_registry(state, delta),
                Kind::Profile => self.receive_profile(state, delta),
                Kind::Organization => self.receive_org_change(state, delta),
                Kind::Task => self.receive_task_change(state, delta),
                Kind::Board => {
                    if self.contract_key.get_untracked().map(|open| *open.id()) == Some(*key.id()) {
                        self.receive_change(state, delta);
                    }
                    // Otherwise it belongs to a board we have navigated away from —
                    // the node keeps that subscription alive, and merging its ops
                    // into whatever is open now would corrupt the wrong board.
                }
            },

            NodeEvent::Subscribed { subscribed } => {
                if !subscribed {
                    self.notice.set(Some(
                        "the node did not subscribe; changes from others may not arrive live"
                            .to_owned(),
                    ));
                }
            }
        }
    }

    /// Everything that has to happen once there is a socket.
    ///
    /// Split out of [`Self::handle`] because it is the one arm that is a whole
    /// sequence rather than a line, and reading the other twenty meant scrolling
    /// past all of this first.
    fn on_connected(self) {
        let reopened = self.connection.get_untracked() != Connection::Connecting;
        self.connection.set(Connection::Open);

        if reopened {
            // Coming back from an outage: re-offer everything we hold and
            // re-subscribe, rather than assuming we know what got through.
            self.resync();
            return;
        }

        // The delegate has to be registered before it will answer, and the
        // seed it holds is the only reason an identity survives a reload.
        //
        // Registration is not acknowledged by the node in a way this client
        // can observe — gating the identity request on a `HostResponse::Ok`
        // never fires — so the request goes out immediately after it. That
        // leaves a race against the predecessor secret copy-forward that
        // registration also triggers; see the README on delegate
        // generations.
        node::register_delegate();

        // Asking before registration finishes is how an identity gets
        // lost: the delegate finds no secret, mints one from the entropy
        // we sent, and stores it — permanently, over the top of whatever
        // the copy-forward was about to bring across. So wait for the
        // acknowledgement, which arrives as an empty `DelegateResponse`.
        //
        // The fallback matters as much as the wait. An earlier attempt
        // gated on `HostResponse::Ok`, which the node never sends for a
        // registration, and identity stopped loading entirely.
        crate::ui::after_ms(3000, move || self.ask_identity_once());

        // This node's settings. Independent of identity: the theme should
        // come back even if the delegate refuses to hand over a key.
        node::register_prefs_delegate();
        node::ask_prefs(&PrefsRequest::load());

        // Subscribe to the directory so newly created boards appear live.
        node::get(contract::registry_id());

        match route_in_url() {
            Route::Board(id) => self.open(&id),
            Route::Organization(id) => self.open_org(&id),
            Route::Task(task) => self.open_task(&task),
            Route::Me => self.view.set(View::User),
            // A key arriving by link is only a *proposal*; the user confirms
            // it on their own page, which is why it is parked rather than
            // acted on.
            Route::Link(key) => {
                self.pending_device_key.set(Some(key));
                self.view.set(View::User);
            }
            Route::None => {}
        }
    }

    // ------------------------------------------------------------- identity

    fn receive_identity(self, from: &DelegateKey, response: IdentityResponse) {
        // An answer from a retired generation is a migration in progress, not this
        // session's identity.
        if *from != contract::delegate_key() {
            self.adopt_previous_identity(&response);
            return;
        }
        match response {
            IdentityResponse::Seed { seed, created } => {
                self.adopt(seed);
                // A seed this call *created* means the current delegate's namespace
                // was empty. For a genuinely new user that is correct; for a
                // returning one whose delegate was rebuilt it is the moment their
                // boards silently became somebody else's. Ask the retired
                // generations before believing it.
                if created {
                    self.ask_predecessors();
                }
            }
            IdentityResponse::Failed { reason } => self
                .notice
                .set(Some(format!("could not load your identity: {reason}"))),
        }
    }

    /// Takes a seed as this session's identity.
    fn adopt(self, seed: [u8; 32]) {
        self.identity.set(Some(Identity::from_seed(seed)));
        // Ownership is derived from the identity, so anything already rendered has
        // to be recomputed against it.
        self.refold();
        // The profile's address comes from this key, so it can only be fetched
        // once the key is known.
        self.load_profile();
    }

    /// Asks every retired delegate generation whether it still holds a seed.
    ///
    /// # Why this exists
    ///
    /// A delegate's secrets hang off `hash(code + parameters)`, so rebuilding it
    /// for any reason at all — an edition bump, a dependency moving — points the
    /// app at an empty namespace and mints a fresh identity. The user is still
    /// there; their boards are still there; they just are not the owner any more.
    ///
    /// The node has a mechanism for this, `RegisterDelegateWithPredecessors`, and
    /// it does not work: it refuses to copy from any predecessor with no recorded
    /// origin, and registration requests carry no origin, so every predecessor is
    /// refused. Tested to destruction against 0.2.105 — see
    /// `contract::PREDECESSOR_DELEGATE_KEYS`. Do not try it again.
    ///
    /// This is the path the node's own log names instead: the client reads its
    /// seed out of the old generation and writes it into the new one. Nothing
    /// about it needs the node's cooperation, because both delegates are already
    /// registered and both answer application messages.
    fn ask_predecessors(self) {
        if self.migrating.get_untracked() {
            return;
        }
        let keys = contract::predecessor_delegate_keys();
        if keys.is_empty() {
            return;
        }
        self.migrating.set(true);
        // Newest first: the most recently retired generation is the likeliest to
        // hold the seed actually in use.
        for key in keys.into_iter().rev() {
            // Fresh entropy each time, and deliberately not reused: if a
            // predecessor holds nothing it will mint one, and two predecessors
            // minting the *same* junk seed would look like a real identity found
            // twice.
            node::ask_identity_of(key, &IdentityRequest::get_or_create(random_bytes::<32>()));
        }
    }

    /// Considers a retired generation's answer.
    ///
    /// Accepted only when the delegate says it did *not* create the seed: that is
    /// the one signal distinguishing "this generation was holding your identity"
    /// from "this generation was empty and has just minted a throwaway". Without
    /// the check, asking an empty predecessor would replace a good identity with
    /// a junk one.
    fn adopt_previous_identity(self, response: &IdentityResponse) {
        let IdentityResponse::Seed {
            seed,
            created: false,
        } = *response
        else {
            return;
        };
        // First answer wins, and the rest are ignored: several generations may
        // still hold seeds, and adopting each in turn would leave the identity
        // depending on which reply the network happened to deliver last.
        if !self.migrating.get_untracked() {
            return;
        }
        self.migrating.set(false);

        if self.identity.get_untracked().map(|me| me.member)
            == Some(Identity::from_seed(seed).member)
        {
            return;
        }
        self.adopt(seed);
        // Written into the current generation so this happens once rather than on
        // every load.
        node::ask_identity(&IdentityRequest::replace(seed));
        self.say_ok("recovered the identity from your previous installation");
    }

    /// Replaces the stored identity from an exported recovery key — the way to
    /// carry an identity between nodes, which delegate storage cannot do.
    pub(crate) fn restore_identity(self, recovery_key: &str) {
        match Identity::seed_from_recovery_key(recovery_key) {
            Some(seed) => {
                self.identity.set(Some(Identity::from_seed(seed)));
                self.refold();
                // A different identity has a different profile.
                self.load_profile();
                // Persist it so the next reload comes back as this identity.
                node::ask_identity(&IdentityRequest::replace(seed));
                self.notice
                    .set(Some("identity restored from your recovery key".to_owned()));
            }
            None => self
                .notice
                .set(Some("that is not a valid recovery key".to_owned())),
        }
    }

    /// Vouches for another key as belonging to this same person, so a second
    /// browser or machine can act as you without any secret being moved.
    ///
    /// Writes to two places, and they do different jobs: the user's profile is the
    /// canonical list of their devices, while a board's own `LinkDevice` op is what
    /// gives the key authority *there* — a board contract cannot read a profile, so
    /// it has to hold its own evidence.
    pub(crate) fn link_device(self, encoded_key: &str, label: &str) {
        let encoded = encoded_key.trim();
        let Some(device) = MemberId::from_base58(encoded) else {
            self.notice
                .set(Some(format!("{encoded:?} is not a valid key")));
            return;
        };
        if self.me().map(|identity| identity.member) == Some(device) {
            self.notice
                .set(Some("that is already this device's own key".to_owned()));
            return;
        }

        let label = label.trim();
        let label = if label.is_empty() {
            "linked device".to_owned()
        } else {
            label.to_owned()
        };

        // The link is authority; the label is presentation. Two ops, on purpose —
        // it means renaming "laptop" to "work laptop" is not a change of who may
        // act as you.
        self.profile_emit_envelope(|stamp| Envelope::link_device(stamp, device));
        self.profile_emit(UserOp::SetDeviceLabel { device, label });
        if self.params.get_untracked().is_some() {
            // The board only needs to know the key acts for this person; what the
            // key is *called* lives in the profile, which is where the account page
            // reads it from.
            self.emit_envelope(|stamp| Envelope::link_device(stamp, device));
        }
        self.pending_device_key.set(None);
    }

    /// Which contract a reply belongs to.
    ///
    /// The registry sits at a fixed address; an organization is whatever we last
    /// asked for as one. Anything else is treated as a board, so an unexpected reply
    /// lands somewhere harmless rather than being silently dropped.
    fn kind_of(self, instance: &ContractInstanceId) -> Kind {
        if *instance == contract::registry_id() {
            return Kind::Registry;
        }
        if self.profile_key.get_untracked().map(|key| *key.id()) == Some(*instance) {
            return Kind::Profile;
        }
        let matches_open_org = self.org_key.get_untracked().map(|key| *key.id()) == Some(*instance);
        if matches_open_org || self.pending_org.get_untracked() == Some(*instance) {
            return Kind::Organization;
        }
        let matches_open_task =
            self.task_key.get_untracked().map(|key| *key.id()) == Some(*instance);
        if matches_open_task || self.pending_task_fetch.get_untracked() == Some(*instance) {
            return Kind::Task;
        }
        Kind::Board
    }

    // ------------------------------------------------------- the user's profile

    /// Loads (or creates) the profile belonging to the current identity.
    ///
    /// Its address comes from the identity's own public key, so this needs nothing
    /// but the key — there is no directory of profiles to consult.
    fn load_profile(self) {
        let Some(identity) = self.me() else {
            return;
        };
        let params = UserParameters::new(identity.member);
        let key = contract::user_key(&params);

        self.profile_key.set(Some(key));
        self.profile_presence.set(Registry::Unknown);
        self.profile_ops.set(EnvelopeState::new());
        self.refold_profile();
        node::get(*key.id());
    }

    fn refold_profile(self) {
        let Some(identity) = self.me() else {
            return;
        };
        let params = UserParameters::new(identity.member);
        let profile = UserProfile::from_state(&self.profile_ops.get_untracked(), &params);
        self.profile.set(Some(profile));
    }

    fn receive_profile(self, state: Option<Vec<u8>>, delta: Option<Vec<u8>>) {
        let mut merged = false;
        if let Some(bytes) = state {
            if let Ok(incoming) = EnvelopeState::decode(&bytes) {
                self.profile_ops.update(|current| {
                    current.merge(incoming);
                });
                merged = true;
            }
        }
        if let Some(bytes) = delta {
            if let Ok(incoming) = EnvelopeDelta::decode(&bytes) {
                self.profile_ops.update(|current| {
                    current.merge(EnvelopeState::from_ops(incoming.ops));
                });
                merged = true;
            }
        }
        if merged {
            self.refold_profile();
        }
    }

    /// Signs a profile op, applies it locally, and sends it — creating the profile
    /// contract on first write, since until then there is nothing to update.
    fn profile_emit(self, op: UserOp) {
        self.profile_emit_envelope(|stamp| op.envelope(stamp));
    }

    fn profile_emit_envelope(self, build: impl FnOnce(Stamp) -> Envelope) {
        let Some(identity) = self.me() else {
            return;
        };
        let params = UserParameters::new(identity.member);
        let key = self
            .profile_key
            .get_untracked()
            .unwrap_or_else(|| contract::user_key(&params));

        let lamport = self
            .profile
            .get_untracked()
            .map_or(1, |profile| profile.next_lamport);

        let stamp = Stamp::new(
            params.scope(),
            identity.member,
            lamport,
            now_ms(),
            random_bytes::<16>(),
        );
        let signed = build(stamp).sign(identity.signing_key());

        self.profile_ops.update(|state| {
            state.merge(EnvelopeState::from_ops([signed.clone()]));
        });
        self.refold_profile();

        match self.profile_presence.get_untracked() {
            Registry::Absent | Registry::Unknown => {
                // No profile on the network yet: instantiate it carrying everything
                // we hold. Safe when it does exist too, because the contract merges
                // rather than replaces.
                self.profile_presence.set(Registry::Present);
                node::put(
                    contract::user_container(&params),
                    WrappedState::new(self.profile_ops.get_untracked().encode()),
                );
            }
            Registry::Present => {
                node::update(key, EnvelopeDelta::new(vec![signed]).encode());
            }
        }
    }

    pub(crate) fn set_display_name(self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        // Keep the local copy in step so the UI updates before the round trip.
        if let Some(identity) = self.me() {
            self.identity.set(Some(identity.with_name(name)));
        }
        self.profile_emit(UserOp::SetName {
            name: name.to_owned(),
        });

        // A board carries its own copy of each member's name. Without this the one
        // field that says "Display name" would rename you everywhere except the
        // place people actually read your name.
        //
        // Anyone on the board may do this now: naming yourself takes `SET_NAME`,
        // which every member holds. It used to take the authority to change
        // membership, because a name arrived inside the op that granted it —
        // splitting the two is most of the point of the envelope design.
        //
        // Only what is open, because Freenet has no reverse index: there is no way
        // to enumerate the boards you belong to in order to rename yourself on each.
        // Your profile is the canonical answer; these are copies kept in step when
        // we happen to be looking at them.
        if let Some(me) = self.me().map(|identity| identity.member) {
            if self.params.get_untracked().is_some() {
                self.emit(Op::SetMemberName {
                    member: me,
                    name: name.to_owned(),
                });
            }
            if self.org_params.get_untracked().is_some() {
                self.org_emit(OrgOp::SetMemberName {
                    member: me,
                    name: name.to_owned(),
                });
            }
        }
    }

    /// Forgets a key.
    ///
    /// Removes it from the canonical list, and revokes it on the open board if there
    /// is one. Boards not currently open keep their `LinkDevice` op, so this is
    /// best-effort — see the README on revocation.
    pub(crate) fn unlink_device(self, device: MemberId) {
        self.profile_emit_envelope(|stamp| Envelope::unlink_device(stamp, device));
        if self.params.get_untracked().is_some() {
            self.emit_envelope(|stamp| Envelope::unlink_device(stamp, device));
        }
        self.notice.set(Some(
            "device unlinked. Projects you are not currently viewing keep their copy of the \
             link until you open them."
                .to_owned(),
        ));
    }

    /// Notes that this user belongs to the open board, so it shows on their page.
    fn remember_board(self) {
        let (Some(params), Some(key), Some(board), Some(identity)) = (
            self.params.get_untracked(),
            self.contract_key.get_untracked(),
            self.board.get_untracked(),
            self.me(),
        ) else {
            return;
        };
        // Only worth remembering somewhere we can actually act.
        if !board
            .members
            .contains_key(&board.person_of(&identity.member))
        {
            return;
        }
        let board_id = board_id_of(&key);
        let org = board.organization.as_ref().map(|owner| owner.org);
        let already = self
            .profile
            .get_untracked()
            .and_then(|profile| profile.boards.get(&board_id).cloned())
            .is_some_and(|known| known.name == params.name && known.org == org);
        if already {
            return;
        }
        self.profile_emit(UserOp::JoinedBoard {
            board: board_id,
            name: params.name,
            org,
        });
    }

    /// Notes that this user belongs to the open organization.
    fn remember_org(self) {
        let (Some(params), Some(key), Some(org), Some(identity)) = (
            self.org_params.get_untracked(),
            self.org_key.get_untracked(),
            self.org.get_untracked(),
            self.me(),
        ) else {
            return;
        };
        if !org.is_member(&identity.member) {
            return;
        }
        let org_id = OrgId(id_bytes(key.id()));
        let already = self
            .profile
            .get_untracked()
            .and_then(|profile| profile.orgs.get(&org_id).cloned())
            .is_some_and(|known| known.name == params.name);
        if already {
            return;
        }
        self.profile_emit(UserOp::JoinedOrg {
            org: org_id,
            name: params.name,
        });
    }

    pub(crate) fn leave_board_bookmark(self, board: BoardId) {
        self.profile_emit(UserOp::LeftBoard { board });
    }

    pub(crate) fn open_user_page(self) {
        self.view.set(View::User);
        set_route_in_url("me");
    }

    /// Moves a task above `before`, or to the end of `column` when `before` is
    /// `None`.
    ///
    /// Shared by the drop zones and by the column picker in the task drawer, which
    /// is the only way to move a card on a touch screen — HTML5 drag-and-drop
    /// never fires there.
    pub(crate) fn move_task(self, task: TaskAddr, column: ColumnId, before: Option<TaskAddr>) {
        let Some(board) = self.board.get_untracked() else {
            return;
        };

        // Ranks are computed against the column *without* the moving card, so
        // dropping one slot down actually moves it rather than landing it back
        // where it started.
        let remaining: Vec<TaskAddr> = board
            .tasks_in(&column)
            .into_iter()
            .map(|candidate| candidate.task)
            .filter(|id| *id != task)
            .collect();

        let target = match before {
            Some(anchor) => remaining
                .iter()
                .position(|id| *id == anchor)
                .unwrap_or(remaining.len()),
            None => remaining.len(),
        };

        let rank = board.rank_for_drop(&column, target, Some(task));
        // A move is a re-place: status belongs to the board, so this touches the
        // task's own contract not at all.
        self.emit(Op::Place { task, column, rank });
        self.dragging.set(None);
    }

    // ----------------------------------------------------------------- columns

    /// Adds a column at the end of the board.
    pub(crate) fn add_column(self, title: &str) {
        let title = title.trim().to_owned();
        if title.is_empty() {
            self.notice.set(Some("a column needs a name".to_owned()));
            return;
        }
        let Some(board) = self.board.get_untracked() else {
            return;
        };
        let rank = board
            .columns
            .last()
            .map_or_else(Rank::middle, |last| Rank::after(&last.rank));
        self.emit(Op::SetColumn {
            column: ColumnId(random_bytes::<16>()),
            title,
            rank,
        });
    }

    /// Renames a column, keeping its id and therefore its cards.
    ///
    /// One op covers naming and ordering because the assignment is total, so a
    /// rename has to carry the current rank forward — sending a default would
    /// silently reorder the board.
    pub(crate) fn rename_column(self, column: ColumnId, title: &str) {
        let title = title.trim().to_owned();
        if title.is_empty() {
            self.notice.set(Some("a column needs a name".to_owned()));
            return;
        }
        let Some(rank) = self.board.get_untracked().and_then(|board| {
            board
                .columns
                .iter()
                .find(|candidate| candidate.id == column)
                .map(|candidate| candidate.rank.clone())
        }) else {
            return;
        };
        self.emit(Op::SetColumn {
            column,
            title,
            rank,
        });
    }

    /// Moves a column one place left or right.
    ///
    /// Steps rather than drags: a column header is a drop target for cards
    /// already, and making it draggable too would mean two different things
    /// happening depending on what was picked up.
    pub(crate) fn shift_column(self, column: ColumnId, by: isize) {
        let Some(board) = self.board.get_untracked() else {
            return;
        };
        let Some(at) = board
            .columns
            .iter()
            .position(|candidate| candidate.id == column)
        else {
            return;
        };
        let Some(target) = at.checked_add_signed(by) else {
            return;
        };
        if target >= board.columns.len() {
            return;
        }
        let title = board.columns[at].title.clone();

        // Ranked between the two columns it is landing among — which are its
        // neighbours *after* it leaves, so the one it swaps with is skipped.
        let (lo, hi) = if by < 0 {
            (
                target.checked_sub(1).map(|i| &board.columns[i].rank),
                Some(&board.columns[target].rank),
            )
        } else {
            (
                Some(&board.columns[target].rank),
                board.columns.get(target + 1).map(|next| &next.rank),
            )
        };
        self.emit(Op::SetColumn {
            column,
            title,
            rank: Rank::between(lo, hi),
        });
    }

    /// Removes a column. Its cards are rehomed to the first surviving one by the
    /// fold, so nothing on the board disappears with it.
    pub(crate) fn remove_column(self, column: ColumnId) {
        if self
            .board
            .get_untracked()
            .is_some_and(|board| board.columns.len() <= 1)
        {
            self.notice
                .set(Some("a project needs at least one column".to_owned()));
            return;
        }
        self.emit(Op::RemoveColumn { column });
    }

    /// Takes a card off the board, leaving the task itself alone.
    pub(crate) fn unplace_task(self, task: TaskAddr) {
        self.emit(Op::Unplace { task });
        if let Some(board) = self.open_board_id()
            && self.selected.get_untracked() == Some(task)
        {
            self.task_emit(TaskOp::Detach { board });
        }
        self.close_task();
    }

    // ------------------------------------------------------------- task links

    /// Links two tasks.
    ///
    /// The forward edge always lands. The mirrored one is written only when both
    /// tasks answer to the same organization, because that is when this author
    /// holds a certificate for the other task and can write to it at all — so
    /// bidirectionality inside an org and one-way across is a consequence of what
    /// is possible, not a policy laid on top.
    pub(crate) fn link_task(self, to: TaskAddr, kind: LinkKind) {
        self.task_emit(TaskOp::Link { to, kind });
        if self
            .task_params
            .get_untracked()
            .is_none_or(|params| params.org.is_none())
        {
            self.notice.set(Some(format!(
                "linked. This task belongs to no organization, so the other task will not \
                 show it as {} until somebody with access adds it there.",
                kind.inverse().label()
            )));
        }
    }

    pub(crate) fn unlink_task(self, to: TaskAddr) {
        self.task_emit(TaskOp::Unlink { to });
    }

    // --------------------------------------------------------- organizations

    /// Founds an organization with this user as its founder — the root of all
    /// authority in it, fixed forever because it is hashed into the address.
    pub(crate) fn create_org(self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.notice
                .set(Some("an organization needs a name".to_owned()));
            return;
        }
        let Some(identity) = self.me() else {
            self.notice.set(Some(
                "still waiting for your identity — try again in a moment".to_owned(),
            ));
            return;
        };

        let params = OrgParameters::new(identity.member, name, random_bytes::<16>());
        let scope = params.scope();
        let at = now_ms();
        let signed: Vec<SignedEnvelope> =
            pj_core::org::genesis_ops(identity.member, &identity.name)
                .into_iter()
                .map(OrgDraft::from)
                .enumerate()
                .map(|(index, draft)| {
                    let stamp = Stamp::new(
                        scope,
                        identity.member,
                        index as u64 + 1,
                        at,
                        random_bytes::<16>(),
                    );
                    draft.envelope(stamp).sign(identity.signing_key())
                })
                .collect();

        let state = EnvelopeState::from_ops(signed);
        let key = contract::org_key(&params);

        self.org_params.set(Some(params.clone()));
        self.org_key.set(Some(key));
        self.org_ops.set(state.clone());
        self.pending_org.set(Some(*key.id()));
        self.refold_org();
        self.view.set(View::Organization);
        set_route_in_url(&format!("org/{}", key.encoded_contract_id()));

        node::put(
            contract::org_container(&params),
            WrappedState::new(state.encode()),
        );

        // Organizations are listed publicly so people can find the one they were
        // told about.
        self.publish_listing(
            ListingTarget::Organization(OrgId(id_bytes(key.id()))),
            params.name.clone(),
        );
        // Creating is a form of joining, and nothing will `Got` this org back to us,
        // so record it here rather than relying on the load path.
        self.remember_org();
    }

    pub(crate) fn open_org(self, id: &str) {
        let id = id.trim();
        match ContractInstanceId::from_bytes(id) {
            Ok(instance) => {
                // A different organization: drop what we hold so its state cannot
                // bleed into the new one.
                if self.org_key.get_untracked().map(|key| *key.id()) != Some(instance) {
                    self.org.set(None);
                    self.org_params.set(None);
                    self.org_key.set(None);
                    self.org_ops.set(EnvelopeState::new());
                }
                self.pending_org.set(Some(instance));
                self.view.set(View::Organization);
                set_route_in_url(&format!("org/{}", instance.encode()));
                node::get(instance);
            }
            Err(_) => self
                .notice
                .set(Some(format!("{id:?} is not a valid organization id"))),
        }
    }

    fn receive_org(self, parameters: Option<&[u8]>, state: &[u8]) {
        let params = match parameters.map(OrgParameters::decode).transpose() {
            Ok(Some(params)) => Some(params),
            Ok(None) => self.org_params.get_untracked(),
            Err(err) => {
                self.notice.set(Some(format!(
                    "organization parameters are unreadable: {err}"
                )));
                return;
            }
        };
        let Some(params) = params else {
            self.notice.set(Some(
                "the node returned an organization without its parameters".to_owned(),
            ));
            return;
        };

        match EnvelopeState::decode(state) {
            Ok(incoming) => {
                self.org_params.set(Some(params));
                self.org_ops.update(|current| {
                    current.merge(incoming);
                });
                self.refold_org();
                self.remember_org();
            }
            Err(err) => self
                .notice
                .set(Some(format!("organization state is unreadable: {err}"))),
        }
    }

    fn receive_org_change(self, state: Option<Vec<u8>>, delta: Option<Vec<u8>>) {
        let mut merged = false;
        if let Some(bytes) = state {
            if let Ok(incoming) = EnvelopeState::decode(&bytes) {
                self.org_ops.update(|current| {
                    current.merge(incoming);
                });
                merged = true;
            }
        }
        if let Some(bytes) = delta {
            if let Ok(incoming) = EnvelopeDelta::decode(&bytes) {
                self.org_ops.update(|current| {
                    current.merge(EnvelopeState::from_ops(incoming.ops));
                });
                merged = true;
            }
        }
        if merged {
            self.refold_org();
        }
    }

    fn refold_org(self) {
        let Some(params) = self.org_params.get_untracked() else {
            return;
        };
        let org = Organization::from_state(&self.org_ops.get_untracked(), &params);
        self.org.set(Some(org));
    }

    /// Signs an organization op, applies it locally, and sends it.
    pub(crate) fn org_emit(self, op: OrgOp) {
        self.org_emit_envelope(|stamp| op.envelope(stamp));
    }

    /// Confers rights in the open organization, or takes them away.
    pub(crate) fn org_grant(self, member: MemberId, rights: Rights) {
        self.org_emit_envelope(|stamp| Envelope::grant(stamp, member, rights));
    }

    fn org_emit_envelope(self, build: impl FnOnce(Stamp) -> Envelope) {
        let Some(params) = self.org_params.get_untracked() else {
            self.notice.set(Some("no organization open".to_owned()));
            return;
        };
        let Some(identity) = self.me() else {
            return;
        };
        let key = self
            .org_key
            .get_untracked()
            .unwrap_or_else(|| contract::org_key(&params));

        let lamport = self.org.get_untracked().map_or(1, |org| org.next_lamport);

        let stamp = Stamp::new(
            params.scope(),
            identity.member,
            lamport,
            now_ms(),
            random_bytes::<16>(),
        );
        let signed = build(stamp).sign(identity.signing_key());

        // The same membership-proof trick boards use: a peer that has not seen the
        // op appointing us would otherwise reject this write.
        let mut batch = Vec::new();
        if let Some(proof) = self.org_authority_proof() {
            batch.push(proof);
        }
        batch.push(signed);

        self.org_ops.update(|state| {
            state.merge(EnvelopeState::from_ops(batch.clone()));
        });
        self.refold_org();

        node::update(key, EnvelopeDelta::new(batch).encode());
    }

    /// The founder-signed grant that appoints us, if we are not the founder.
    fn org_authority_proof(self) -> Option<SignedEnvelope> {
        let params = self.org_params.get_untracked()?;
        let me = self.me()?.member;
        if me == params.founder {
            return None;
        }
        self.org_ops
            .get_untracked()
            .ops
            .values()
            .find(|op| {
                op.author() == params.founder
                    && op.kind() == kind::GRANT
                    && GrantBody::decode(&op.payload.body)
                        .is_ok_and(|body| body.member == me && !body.rights.is_empty())
            })
            .cloned()
    }

    /// Whether this user currently holds admin authority in the open organization.
    pub(crate) fn is_org_admin(self) -> bool {
        match (self.org.get(), self.identity.get()) {
            (Some(org), Some(identity)) => org.is_admin(&identity.member),
            _ => false,
        }
    }

    /// Whether this user founded the open organization — the only role that may
    /// appoint admins or create projects.
    pub(crate) fn is_org_founder(self) -> bool {
        let Some(params) = self.org_params.get() else {
            return false;
        };
        let Some(me) = self.identity.get().map(|identity| identity.member) else {
            return false;
        };
        if me == params.founder {
            return true;
        }
        self.org
            .get()
            .is_some_and(|org| org.person_of(&me) == params.founder)
    }

    pub(crate) fn is_org_member(self) -> bool {
        match (self.org.get(), self.identity.get()) {
            (Some(org), Some(identity)) => org.is_member(&identity.member),
            _ => false,
        }
    }

    pub(crate) fn invite_to_org(self, encoded_key: &str, name: &str, role: Role) {
        let encoded = encoded_key.trim();
        let Some(member) = MemberId::from_base58(encoded) else {
            self.notice
                .set(Some(format!("{encoded:?} is not a valid key")));
            return;
        };
        let name = name.trim();
        let name = if name.is_empty() {
            member.short()
        } else {
            name.to_owned()
        };
        self.org_grant(member, role.rights());
        self.org_emit(OrgOp::SetMemberName { member, name });
    }

    /// Promotes an existing member. Only a grant carrying `MAY_APPOINT` can create
    /// an admin, which is the founder's alone — the UI merely hides the control
    /// from everyone else.
    pub(crate) fn promote_in_org(self, member: MemberId) {
        self.org_grant(member, Rights::ADMIN);
    }

    pub(crate) fn remove_from_org(self, member: MemberId) {
        self.org_grant(member, Rights::NONE);
    }

    /// Leaving is a grant of nothing, to yourself — which needs nobody's
    /// permission, and so is the one membership change a plain member can make.
    pub(crate) fn leave_org(self) {
        let Some(me) = self.me().map(|identity| identity.member) else {
            return;
        };
        self.org_grant(me, Rights::NONE);
    }

    /// The organizations to show on the start page.
    pub(crate) fn browse_orgs(self) -> Vec<Listing> {
        self.registry
            .get()
            .browse(&self.org_search.get(), true, BROWSE_LIMIT, now_ms())
    }

    // ------------------------------------------------------------- registry

    /// The boards to show on the start page: newest first, filtered by the search
    /// box, capped at [`BROWSE_LIMIT`].
    pub(crate) fn browse(self) -> Vec<Listing> {
        self.registry
            .get()
            .browse(&self.search.get(), false, BROWSE_LIMIT, now_ms())
    }

    /// How many projects match the search before the cap.
    ///
    /// Counted the same way `browse` counts, rather than off the raw listing set:
    /// that set also holds organizations, so "2 of 4" was comparing projects
    /// against projects *and* organizations.
    pub(crate) fn browse_total(self) -> usize {
        self.registry
            .get()
            .browse(&self.search.get(), false, usize::MAX, now_ms())
            .len()
    }

    fn receive_registry(self, state: Option<Vec<u8>>, delta: Option<Vec<u8>>) {
        if let Some(bytes) = state {
            match RegistryState::decode(&bytes) {
                Ok(incoming) => self.registry.update(|current| {
                    current.merge(incoming);
                }),
                Err(err) => self
                    .notice
                    .set(Some(format!("ignored an unreadable directory: {err}"))),
            }
        }
        if let Some(bytes) = delta {
            match RegistryDelta::decode(&bytes) {
                Ok(incoming) => self.registry.update(|current| {
                    current.merge(RegistryState::from_listings(incoming.listings));
                }),
                Err(err) => self.notice.set(Some(format!(
                    "ignored an unreadable directory update: {err}"
                ))),
            }
        }
    }

    /// Advertises a board in the public directory.
    fn publish_listing(self, target: ListingTarget, name: String) {
        let Some(identity) = self.me() else {
            return;
        };

        let listing = Listing {
            target,
            name,
            owner: identity.member,
            created_ms: now_ms(),
        }
        .sign(identity.signing_key());

        self.registry.update(|current| {
            current.merge(RegistryState::from_listings([listing.clone()]));
        });

        match self.registry_presence.get_untracked() {
            Registry::Present | Registry::Absent => self.send_listing(listing),
            // We do not yet know whether the directory exists, and guessing wrong
            // either fails or clobbers it. Hold the listing until we find out.
            Registry::Unknown => self.pending_listing.set(Some(listing)),
        }
    }

    fn send_listing(self, listing: SignedListing) {
        match self.registry_presence.get_untracked() {
            Registry::Absent => {
                // First board on this network — instantiate the directory itself,
                // seeded with everything we hold for it.
                node::put(
                    contract::registry_container(),
                    WrappedState::new(self.registry.get_untracked().encode()),
                );
            }
            _ => node::update(
                contract::registry_key(),
                RegistryDelta::new(vec![listing]).encode(),
            ),
        }
    }

    fn flush_pending_listing(self) {
        if let Some(listing) = self.pending_listing.get_untracked() {
            self.pending_listing.set(None);
            self.send_listing(listing);
        }
    }

    // ------------------------------------------------------------- boards

    /// Opens a board by its base58 contract instance id.
    pub(crate) fn open(self, id: &str) {
        let id = id.trim();
        match ContractInstanceId::from_bytes(id) {
            Ok(instance) => {
                // A different board: drop what we hold first, exactly as `open_org`
                // does. The board on screen is a fold over whatever is in `ops`, and
                // `receive_board` *merges* — so without this, opening a second board
                // renders both at once, with one board's tasks sitting in the
                // other's columns.
                if self.contract_key.get_untracked().map(|key| *key.id()) != Some(instance) {
                    self.params.set(None);
                    self.board.set(None);
                    self.contract_key.set(None);
                    self.ops.set(EnvelopeState::new());
                    self.selected.set(None);
                    self.publish.set(Publish::Settled);
                }
                self.pending_board.set(Some(instance));
                self.view.set(View::Board);
                self.status.set(BoardStatus::Loading(instance.encode()));
                set_board_in_url(&instance.encode());
                node::get(instance);
            }
            Err(_) => self
                .notice
                .set(Some(format!("{id:?} is not a valid board id"))),
        }
    }

    /// Opens a card on the board it is already on: a drawer, and a fetch.
    ///
    /// The board carries no task bodies, so this is the moment the contents are
    /// asked for. Until they arrive the drawer renders from the cached summary,
    /// which is the whole reason the board keeps one.
    pub(crate) fn select_task(self, task: TaskAddr) {
        self.selected.set(Some(task));
        set_route_in_url(&pj_core::task_route(task));
        self.fetch_task(task);
    }

    /// Opens a task's own page, from a link or from the drawer's "open page".
    ///
    /// Needs no board: the address is the whole reference, so a link opened cold
    /// resolves without one. Which boards it sits on comes back with the task
    /// itself.
    pub(crate) fn open_task(self, task: &str) {
        let Some(task) = pj_core::parse_task(task) else {
            self.notice
                .set(Some(format!("{task:?} is not a valid task address")));
            return;
        };
        self.open_task_page(task);
    }

    /// Opens a task's page, and puts it in the address bar.
    ///
    /// The URL is the point: it is what the Copy link button hands out, what a
    /// reload restores, and what the Back button steps out of.
    pub(crate) fn open_task_page(self, task: TaskAddr) {
        self.selected.set(Some(task));
        self.view.set(View::Task);
        set_route_in_url(&pj_core::task_route(task));
        self.fetch_task(task);
    }

    /// Leaves a task for whatever it was opened from.
    pub(crate) fn close_task(self) {
        self.selected.set(None);
        self.pending_task.set(None);
        self.drop_task();
        if self.view.get_untracked() == View::Task {
            self.view.set(View::Board);
        }
        match self.contract_key.get_untracked() {
            Some(key) => set_board_in_url(&key.encoded_contract_id()),
            None => clear_board_in_url(),
        }
    }

    /// Asks the node for a task's contents.
    fn fetch_task(self, task: TaskAddr) {
        let instance = contract::task_instance(task);
        // Already the open one: its state is in hand and re-fetching would only
        // blank the drawer while the answer came back.
        if self.task_key.get_untracked().map(|key| *key.id()) == Some(instance) {
            return;
        }
        self.drop_task();
        self.pending_task_fetch.set(Some(instance));
        node::get(instance);
    }

    /// Forgets whatever task was open, so a fetch cannot render one task's body
    /// under another's title.
    fn drop_task(self) {
        self.task.set(None);
        self.task_params.set(None);
        self.task_key.set(None);
        self.task_ops.set(EnvelopeState::new());
        self.pending_task_fetch.set(None);
    }

    fn receive_task(self, parameters: Option<&[u8]>, state: &[u8]) {
        let Some(params) = parameters.and_then(|bytes| TaskParameters::decode(bytes).ok()) else {
            self.notice
                .set(Some("that task's parameters did not decode".to_owned()));
            return;
        };
        self.pending_task_fetch.set(None);
        self.task_params.set(Some(params));
        self.receive_task_change(Some(state.to_vec()), None);
        // A task fetched because it was just created still owes the board a card.
        self.finish_placement();
    }

    fn receive_task_change(self, state: Option<Vec<u8>>, delta: Option<Vec<u8>>) {
        if let Some(bytes) = state
            && let Ok(incoming) = EnvelopeState::decode(&bytes)
        {
            self.task_ops.update(|current| {
                current.merge(incoming);
            });
        }
        if let Some(bytes) = delta
            && let Ok(incoming) = EnvelopeDelta::decode(&bytes)
        {
            self.task_ops.update(|current| {
                current.merge(EnvelopeState::from_ops(incoming.ops));
            });
        }
        self.refold_task();
    }

    fn refold_task(self) {
        let Some(params) = self.task_params.get_untracked() else {
            return;
        };
        let task = Task::from_state(&self.task_ops.get_untracked(), &params);
        self.task.set(Some(task));
        self.reconcile_summary();
    }

    /// The open board's id, as links name it.
    pub(crate) fn open_board_id(self) -> Option<BoardId> {
        self.contract_key.get().map(|key| board_id_of(&key))
    }

    /// A link to one task, for the clipboard.
    ///
    /// Carries no board, and needs none — which is what makes it work when pasted
    /// by somebody who has never seen the board it came from.
    pub(crate) fn task_route(task: TaskAddr) -> String {
        pj_core::task_route(task)
    }

    /// Whether a reply about a board concerns the one we are actually waiting for.
    ///
    /// Replies arrive whenever the network gets round to them, and somebody can ask
    /// for a second board before the first has answered. Without this the late
    /// answer to an abandoned question wins: a `NotFound` for a board nobody is
    /// looking at any more replaced the board that *was* loading with "the network
    /// has no board under …", which is how switching views mid-load dropped you on
    /// the start page.
    fn awaiting_board(self, instance: &ContractInstanceId) -> bool {
        self.pending_board.get_untracked() == Some(*instance)
            || self.contract_key.get_untracked().map(|key| *key.id()) == Some(*instance)
    }

    /// Re-routes when the fragment changes underneath us — which is what the
    /// browser's Back and Forward buttons do.
    ///
    /// The app has always written the route into the URL but never read it back
    /// except at boot, so Back moved the address bar and left the view where it
    /// was. Each arm checks whether it is already where the URL says before acting:
    /// our own writes raise this same event, and without the check a navigation
    /// would re-enter itself.
    pub(crate) fn route_changed(self) {
        // The visible address bar is the shell's, and it only ever shows what this
        // app last pushed to it. Back and Forward move our own fragment without
        // going through us, so without this the shell keeps the route from before
        // the button was pressed — and a reload restores *that*, dropping you on
        // the account page while you are looking at a board.
        if let Some(hash) = web_sys::window().and_then(|window| window.location().hash().ok()) {
            // Its handler ignores an empty string, so an empty route is a bare '#'.
            set_shell_hash(if hash.is_empty() { "#" } else { &hash });
        }

        match route_in_url() {
            // Two separate questions: do we already hold this thing, and are we
            // already looking at it? Conflating them is what broke going back from
            // the account page — the board was still loaded, so this returned
            // early and left the view on the account.
            Route::Board(id) => {
                let asked = self
                    .pending_board
                    .get_untracked()
                    .map(|instance| instance.encode());
                let open_now = self
                    .contract_key
                    .get_untracked()
                    .map(|key| key.encoded_contract_id());
                if asked.as_deref() == Some(id.as_str()) || open_now.as_deref() == Some(id.as_str())
                {
                    // Held already, so nothing to fetch — but still show it.
                    self.view.set(View::Board);
                    return;
                }
                self.open(&id);
            }
            Route::Organization(id) => {
                let asked = self
                    .pending_org
                    .get_untracked()
                    .map(|instance| instance.encode());
                let open_now = self
                    .org_key
                    .get_untracked()
                    .map(|key| key.encoded_contract_id());
                if asked.as_deref() == Some(id.as_str()) || open_now.as_deref() == Some(id.as_str())
                {
                    self.view.set(View::Organization);
                    return;
                }
                self.open_org(&id);
            }
            Route::Task(task) => {
                // Opening a card on a board writes this very route, so without this
                // check the browser's own hashchange event reads it straight back
                // and navigates to the task's *page* — turning every click on a
                // card into leaving the board. Already showing that task means the
                // URL is describing what just happened rather than asking for
                // something new.
                let already = pj_core::parse_task(&task)
                    .is_some_and(|addr| self.selected.get_untracked() == Some(addr));
                if !already {
                    self.open_task(&task);
                }
            }
            Route::Me => self.view.set(View::User),
            Route::Link(key) => {
                self.pending_device_key.set(Some(key));
                self.view.set(View::User);
            }
            Route::None => {
                if self.view.get_untracked() != View::Start {
                    self.back();
                }
            }
        }
    }

    /// Asks the identity delegate for a seed, at most once per session.
    ///
    /// Two callers race here on purpose — the registration acknowledgement and a
    /// timeout — and `identity_requested` is what makes the loser harmless.
    fn ask_identity_once(self) {
        if self.identity_requested.get_untracked() {
            return;
        }
        self.identity_requested.set(true);
        node::ask_identity(&IdentityRequest::get_or_create(random_bytes::<32>()));
    }

    // ------------------------------------------------------------- preferences

    /// A saved setting, or `None` while the delegate has yet to answer.
    pub(crate) fn preference(self, key: &str) -> Option<String> {
        self.prefs
            .get()
            .and_then(|prefs| prefs.get(key).map(str::to_owned))
    }

    /// Records a setting on this node.
    ///
    /// Read-modify-write over the whole blob, so a key this build has never heard
    /// of survives being saved by it.
    pub(crate) fn set_preference(self, key: &str, value: &str) {
        let mut prefs = self.prefs.get_untracked().unwrap_or_default();
        prefs.set(key, value);
        self.prefs.set(Some(prefs.clone()));
        node::ask_prefs(&PrefsRequest::save(&prefs));
    }

    fn receive_prefs(self, response: PrefsResponse) {
        match response {
            PrefsResponse::Loaded { blob } => {
                let prefs = match blob {
                    Some(bytes) => Prefs::decode(&bytes).unwrap_or_default(),
                    None => Prefs::default(),
                };
                self.prefs.set(Some(prefs));
                self.apply_preferences();
            }
            PrefsResponse::Saved => {}
            // Not worth a red bar: nothing the person did has failed, and the app
            // works without ever reading a preference.
            PrefsResponse::Failed { reason } => {
                leptos::logging::warn!("preferences unavailable: {reason}");
                self.prefs.set(Some(Prefs::default()));
            }
        }
    }

    /// Puts the loaded settings into effect.
    fn apply_preferences(self) {
        if let Some(theme) = self.preference(pj_prefs_proto::THEME) {
            crate::ui::apply_theme(&theme);
        }
    }

    // ------------------------------------------------------------- the sidebar

    /// Whether the sidebar is showing.
    ///
    /// Automatic by default and manual once you say otherwise: above
    /// [`SIDEBAR_MIN_WIDTH`] the sidebar costs 280px that the board does not miss,
    /// and below it the two are competing for the same pixels.
    pub(crate) fn sidebar_open(self) -> bool {
        self.sidebar_pref
            .get()
            .unwrap_or_else(|| self.sidebar_wide.get())
    }

    pub(crate) fn toggle_sidebar(self) {
        let showing = self.sidebar_open();
        self.sidebar_pref.set(Some(!showing));
    }

    /// Re-applies the automatic rule when the window is resized across the
    /// threshold.
    ///
    /// Crossing it also discards any explicit choice: a decision made at one width
    /// says nothing about the other, and remembering it is how you end up with a
    /// sidebar you asked for on a wide screen crushing a narrow one.
    pub(crate) fn viewport_changed(self) {
        let wide = wide_enough();
        if wide != self.sidebar_wide.get_untracked() {
            self.sidebar_wide.set(wide);
            self.sidebar_pref.set(None);
        }
    }

    /// Closes whatever is open and returns to the start page.
    pub(crate) fn back(self) {
        self.view.set(View::Start);
        self.org.set(None);
        self.org_params.set(None);
        self.org_key.set(None);
        self.org_ops.set(EnvelopeState::new());
        self.pending_org.set(None);
        self.pending_board.set(None);
        self.status.set(BoardStatus::Idle);
        self.params.set(None);
        self.board.set(None);
        self.contract_key.set(None);
        self.selected.set(None);
        self.dragging.set(None);
        // The publish badge belongs to the board it was about, and `retry_publish`
        // needs that board's parameters, which are being cleared here. Anyone who
        // was told their project is unconfirmed keeps that information by staying
        // on it; leaving carries the offer of a retry away with the rest.
        self.publish.set(Publish::Settled);
        self.pending_task.set(None);
        self.ops.set(EnvelopeState::new());
        clear_board_in_url();
    }

    /// Creates a board owned by this user, publishes it, and lists it publicly.
    pub(crate) fn create(self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.notice.set(Some("a board needs a name".to_owned()));
            return;
        }
        let Some(identity) = self.me() else {
            self.notice.set(Some(
                "still waiting for your identity — try again in a moment".to_owned(),
            ));
            return;
        };

        let params = BoardParameters::new(identity.member, name, random_bytes::<16>());

        let drafts = bootstrap_ops(identity.member, &identity.name)
            .into_iter()
            .map(Draft::from)
            .collect();
        let state = EnvelopeState::from_ops(Self::sign_batch(&identity, &params, drafts));

        // A board we are creating has no prior address, so here the locally
        // computed key is the authoritative one.
        let key = contract::board_key(&params);

        self.params.set(Some(params.clone()));
        self.contract_key.set(Some(key));
        self.ops.set(state.clone());
        self.pending_board.set(Some(*key.id()));
        self.refold();
        // Without this the board is created, published and routed to, and the app
        // carries on showing the start page: the view is what `App` renders from,
        // and only `open` and `create_org_project` were setting it. Every board in
        // testing happened to come from an organization, which is why the start
        // page's primary action could sit broken.
        self.view.set(View::Board);
        // Ready at once, not Loading: we just built this state, so there is nothing
        // to wait for. Blocking on the PUT acknowledgement would strand the UI on a
        // spinner whenever the network op is slow to ack — which happens, and which
        // says nothing about whether the state was stored.
        self.status.set(BoardStatus::Ready);
        set_board_in_url(&key.encoded_contract_id());

        self.store_board(&params, &state);
        self.publish_listing(ListingTarget::Board(board_id_of(&key)), params.name.clone());
        self.remember_board();
    }

    /// Signs an op, applies it locally, and sends it to the node.
    pub(crate) fn emit(self, op: Op) {
        self.emit_envelope(|stamp| op.envelope(stamp));
    }

    /// Writes to the open *task* rather than to the board.
    ///
    /// Two contracts are involved in an edit and this is only the first: refolding
    /// runs [`Self::reconcile_summary`], which is what puts the new title on the
    /// card.
    pub(crate) fn task_emit(self, op: TaskOp) {
        let (Some(params), Some(identity)) = (self.task_params.get_untracked(), self.me()) else {
            self.notice.set(Some("no task open".to_owned()));
            return;
        };
        let key = self
            .task_key
            .get_untracked()
            .unwrap_or_else(|| contract::task_key(&params));
        let lamport = self
            .task
            .get_untracked()
            .map_or(1, |task| task.next_lamport);

        let stamp = Stamp::new(
            params.scope(),
            identity.member,
            lamport,
            now_ms(),
            random_bytes::<16>(),
        );
        let signed = op.envelope(stamp).sign(identity.signing_key());

        // The certificate proving this author is in the task's organization rides
        // along, for the same reason a board invite does: a peer that has not seen
        // it would reject the write. Deduped by content hash, so re-sending is free.
        let mut batch = self.org_certificates();
        batch.push(signed);

        self.task_ops.update(|state| {
            state.merge(EnvelopeState::from_ops(batch.clone()));
        });
        self.refold_task();

        node::update(key, EnvelopeDelta::new(batch).encode());
    }

    /// The org-scoped grants that entitle this identity to write to a task.
    ///
    /// Copied out of the organization's own state, signatures intact. They verify
    /// wherever they are presented as long as the contract names the same org,
    /// which is exactly what makes membership checkable by a contract that cannot
    /// read the org.
    fn org_certificates(self) -> Vec<SignedEnvelope> {
        // A task with no org honours no certificates, and presenting one anyway
        // would be worse than useless: a grant scoped elsewhere is a misdirected
        // op, and the contract rejects the whole batch rather than that one entry.
        let Some(org) = self
            .task_params
            .get_untracked()
            .and_then(|params| params.org)
        else {
            return Vec::new();
        };
        // Every grant this org made, not just the ones naming this identity: being
        // a member is a *chain* from the founder, and a missing link partway up
        // would leave a write that is stored, ignored, and silent about why. The
        // org's state is membership only, so the whole set is small, and the
        // receiving state dedupes by content hash.
        self.org_ops
            .get_untracked()
            .ops
            .values()
            .filter(|envelope| {
                envelope.kind() == kind::GRANT && envelope.payload.scope == org.scope
            })
            .cloned()
            .collect()
    }

    /// Puts the open task's current title on the card, if the board is showing an
    /// older one.
    ///
    /// This is both halves of keeping the cache honest, and deliberately so:
    ///
    /// - after an edit, the refold lands here and the card follows immediately;
    /// - on opening a task, the same check runs against whatever the board had
    ///   cached, so a summary somebody else left stale is repaired by the next
    ///   person to look at it.
    ///
    /// It fires at most once per open because it compares content rather than
    /// clocks — an untouched task writes nothing.
    ///
    /// What it cannot do is repair *other* boards the task sits on: signing for a
    /// board needs that board's parameters, and only the open one is in hand. Those
    /// heal when somebody opens the card there.
    fn reconcile_summary(self) {
        let (Some(task), Some(board), Some(identity)) = (
            self.task.get_untracked(),
            self.board.get_untracked(),
            self.me(),
        ) else {
            return;
        };
        let Some(addr) = self.selected.get_untracked() else {
            return;
        };
        let Some(placement) = board.tasks.get(&addr) else {
            return;
        };
        if !task.summary_is_stale(&placement.summary) {
            return;
        }
        // No rights here, no repair. Saying so is better than emitting a write the
        // fold would ignore.
        if !board.may(&identity.member, Rights::WRITE_TASKS) {
            return;
        }
        self.emit(Op::Summarize {
            task: addr,
            summary: task.summary(),
        });
    }

    /// Confers rights on somebody, or takes them away — a grant of
    /// [`Rights::NONE`] *is* the removal, so there is no second op for it.
    ///
    /// This is one of the three kinds the contract itself reads. Everything else
    /// the app sends is opaque to it.
    pub(crate) fn grant(self, member: MemberId, rights: Rights) {
        self.emit_envelope(|stamp| Envelope::grant(stamp, member, rights));
    }

    /// Stamps an envelope with this session's identity and the board's clock,
    /// signs it, applies it locally, and sends it to the node.
    fn emit_envelope(self, build: impl FnOnce(Stamp) -> Envelope) {
        let Some(params) = self.params.get_untracked() else {
            self.notice.set(Some("no board open".to_owned()));
            return;
        };
        let Some(identity) = self.me() else {
            self.notice
                .set(Some("still waiting for your identity".to_owned()));
            return;
        };
        // Address the board as the network knows it, falling back to the local
        // derivation only for a board this session created.
        let key = self
            .contract_key
            .get_untracked()
            .unwrap_or_else(|| contract::board_key(&params));

        let lamport = self
            .board
            .get_untracked()
            .map_or(1, |board| board.next_lamport);

        // Signed against this board specifically, so the op cannot be lifted onto
        // another board — or into a profile — where its author also holds rights.
        let stamp = Stamp::new(
            params.scope(),
            identity.member,
            lamport,
            now_ms(),
            random_bytes::<16>(),
        );
        let signed = build(stamp).sign(identity.signing_key());

        // A peer that has not yet seen our invite would reject this write, so the
        // invite rides along. Re-sending it costs nothing: the op set dedupes by
        // content hash.
        let mut batch = Vec::new();
        if let Some(proof) = self.membership_proof() {
            batch.push(proof);
        }
        batch.push(signed);

        self.ops.update(|state| {
            state.merge(EnvelopeState::from_ops(batch.clone()));
        });
        self.refold();

        node::update(key, EnvelopeDelta::new(batch).encode());
    }

    fn receive_board(self, parameters: Option<&[u8]>, state: &[u8]) {
        // Prefer the parameters the node returned — for a board we are joining,
        // they are the only source of the owner key.
        let params = match parameters.map(BoardParameters::decode).transpose() {
            Ok(Some(params)) => Some(params),
            Ok(None) => self.params.get_untracked(),
            Err(err) => {
                self.notice
                    .set(Some(format!("board parameters are unreadable: {err}")));
                return;
            }
        };

        let Some(params) = params else {
            self.notice.set(Some(
                "the node returned a board without its parameters, so it cannot be opened"
                    .to_owned(),
            ));
            return;
        };

        match EnvelopeState::decode(state) {
            Ok(incoming) => {
                self.params.set(Some(params));
                self.ops.update(|current| {
                    current.merge(incoming);
                });
                self.refold();
                self.status.set(BoardStatus::Ready);
                // Now that membership is known, note it on the user's own page.
                self.remember_board();
            }
            Err(err) => self
                .notice
                .set(Some(format!("board state is unreadable: {err}"))),
        }
    }

    fn receive_change(self, state: Option<Vec<u8>>, delta: Option<Vec<u8>>) {
        let mut merged = false;

        if let Some(bytes) = state {
            match EnvelopeState::decode(&bytes) {
                Ok(incoming) => {
                    self.ops.update(|current| {
                        current.merge(incoming);
                    });
                    merged = true;
                }
                Err(err) => self
                    .notice
                    .set(Some(format!("ignored an unreadable state update: {err}"))),
            }
        }

        if let Some(bytes) = delta {
            match EnvelopeDelta::decode(&bytes) {
                Ok(incoming) => {
                    self.ops.update(|current| {
                        current.merge(EnvelopeState::from_ops(incoming.ops));
                    });
                    merged = true;
                }
                Err(err) => self
                    .notice
                    .set(Some(format!("ignored an unreadable delta: {err}"))),
            }
        }

        if merged {
            self.refold();
        }
    }

    /// The owner-signed grant that authorises this user to write, if they are not
    /// the owner themselves.
    fn membership_proof(self) -> Option<SignedEnvelope> {
        let params = self.params.get_untracked()?;
        let me = self.me()?.member;
        if me == params.owner {
            return None;
        }
        self.ops
            .get_untracked()
            .ops
            .values()
            .find(|op| {
                op.author() == params.owner
                    && op.kind() == kind::GRANT
                    && GrantBody::decode(&op.payload.body)
                        .is_ok_and(|body| body.member == me && !body.rights.is_empty())
            })
            .cloned()
    }

    fn refold(self) {
        let Some(params) = self.params.get_untracked() else {
            return;
        };
        let board = Board::from_state(&self.ops.get_untracked(), &params);
        self.board.set(Some(board));
        // The board arriving can be what makes a cached summary provably stale: the
        // task may already be in hand from a link opened before the board was.
        self.reconcile_summary();
        self.detect_legacy();
    }

    /// Notices cards written before tasks had contracts of their own.
    ///
    /// They are still in the op set — the board contract never understood them, it
    /// stored bytes — but this build cannot render them, so without this they would
    /// look like a board that had simply lost its work.
    fn detect_legacy(self) {
        // Nothing to do while a conversion is running: the queue is the truth then,
        // and re-detecting mid-flight would re-offer cards already converted.
        if !self.legacy_queue.get_untracked().is_empty() {
            return;
        }
        let state = self.ops.get_untracked();
        self.legacy_tasks
            .set(pj_core::legacy::recover_tasks(&state));
        self.legacy_ops
            .set(pj_core::legacy::legacy_op_count(&state));
    }

    /// Converts every recovered card into a task contract with a card of its own.
    ///
    /// One at a time, each waiting for its `PUT` to read back before its card is
    /// written, for the same reason [`Self::create_task`] does: a card pointing at
    /// a contract that was never stored opens onto nothing.
    ///
    /// Offered rather than run automatically on open. It is one `PUT` per card and
    /// therefore not free, and two tabs on the same board would otherwise both
    /// start converting it.
    pub(crate) fn migrate_legacy(self) {
        let pending = self.legacy_tasks.get_untracked();
        if pending.is_empty() {
            return;
        }
        self.say_ok(format!("converting {} cards…", pending.len()));

        // Names and the organization link were renumbered out of readability by
        // the same change that hid the cards, so they are rewritten in the current
        // encoding here. Otherwise a converted board comes back as a list of key
        // prefixes belonging to no organization.
        let state = self.ops.get_untracked();
        for (member, name) in pj_core::legacy::recover_names(&state) {
            self.emit(Op::SetMemberName { member, name });
        }
        if let Some((org, name)) = pj_core::legacy::recover_organization(&state) {
            self.emit(Op::SetOrganization { org, name });
        }

        self.legacy_queue.set(pending);
        self.migrating_board.set(true);
        self.migrate_next();
    }

    /// Retires an old card once its replacement exists.
    ///
    /// Written in the *old* encoding, because that is the only thing that reads
    /// it: `legacy::recover_tasks` honours these tombstones, so after this the
    /// card stops being offered for conversion — to every client, including one
    /// opening the board for the first time. Without it the ops stay in the state
    /// and every visit would convert them again.
    fn retire_legacy(self, task: pj_core::TaskId) {
        self.emit_envelope(|stamp| {
            Envelope::stamped(
                stamp,
                Rights::WRITE_TASKS,
                kind::FIRST_APPLICATION_KIND,
                pj_core::legacy::tombstone(task),
            )
        });
    }

    fn migrate_next(self) {
        let next = self.legacy_queue.try_update(Vec::pop).flatten();
        let Some(legacy) = next else {
            return;
        };
        let Some(identity) = self.me() else {
            return;
        };
        // Remembered so the old card can be retired once its replacement is
        // provably stored — never before, or a failed PUT would lose it.
        self.converting.set(Some(legacy.id));

        let org = self
            .org_params
            .get_untracked()
            .zip(self.org_key.get_untracked())
            .map(|(params, key)| TaskOrg {
                id: OrgId(id_bytes(key.id())),
                scope: params.scope(),
                founder: params.founder,
            });
        let params = TaskParameters::new(identity.member, org, now_ms(), random_bytes::<16>());
        let addr = contract::task_addr(&params);

        // The whole card, in the task's genesis: three ops so that a title, notes
        // and an assignee all survive rather than only the title.
        let mut genesis = Vec::new();
        let mut write = |lamport: u64, op: TaskOp| {
            let stamp = Stamp::new(
                params.scope(),
                identity.member,
                lamport,
                now_ms(),
                random_bytes::<16>(),
            );
            genesis.push(op.envelope(stamp).sign(identity.signing_key()));
        };
        write(
            1,
            TaskOp::SetTitle {
                title: legacy.title.clone(),
            },
        );
        if !legacy.description.is_empty() {
            write(
                2,
                TaskOp::SetDescription {
                    description: legacy.description.clone(),
                },
            );
        }
        if legacy.assignee.is_some() {
            write(
                3,
                TaskOp::SetAssignee {
                    assignee: legacy.assignee,
                },
            );
        }
        let seen_lamport = genesis.len() as u64;

        node::put(
            contract::task_container(&params),
            WrappedState::new(EnvelopeState::from_ops(genesis).encode()),
        );
        self.place_when_stored(
            addr,
            legacy.column,
            legacy.rank.clone(),
            TaskSummary {
                title: legacy.title,
                assignee: legacy.assignee,
                seen_lamport,
            },
            1,
        );
    }

    /// Whether this session's key may do a particular thing on the open board.
    ///
    /// The question every disabled button should be asking. Rights follow a device
    /// link to the person behind it, so a second browser is not a lesser one.
    pub(crate) fn may(self, rights: Rights) -> bool {
        let (Some(board), Some(me)) = (self.board.get(), self.identity.get()) else {
            return false;
        };
        board.may(&me.member, rights)
    }

    /// Whether this session's key may do a particular thing to the open *task*.
    ///
    /// A separate question from [`Self::may`], and it has to be: a task is its own
    /// contract with its own membership, so someone who can move a card around a
    /// board may still not be able to rename what is on it, and someone who can
    /// edit a task may be looking at it from a board they cannot write to at all.
    pub(crate) fn task_may(self, rights: Rights) -> bool {
        let (Some(task), Some(me)) = (self.task.get(), self.identity.get()) else {
            // Unknown until the body arrives. Treated as "not yet" rather than
            // "no", so the controls appear with the content instead of flashing
            // disabled first.
            return false;
        };
        task.may(&me.member, rights)
    }

    /// Creates a project owned by the open organization.
    ///
    /// Only the founder can do this, and the reason is structural rather than a
    /// policy choice: a board's root of trust is the owner key in its immutable
    /// parameters, so for the *organization* to own the project that key has to be
    /// the organization's founder key — and only whoever holds it can sign the
    /// board's genesis ops. Admins run projects once they exist; founding one takes
    /// the founder.
    pub(crate) fn create_org_project(self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.notice.set(Some("a project needs a name".to_owned()));
            return;
        }
        let (Some(org_params), Some(org), Some(org_key), Some(identity)) = (
            self.org_params.get_untracked(),
            self.org.get_untracked(),
            self.org_key.get_untracked(),
            self.me(),
        ) else {
            return;
        };

        if identity.member != org_params.founder {
            self.notice.set(Some(
                "only the organization's founding key can create a project, because it becomes \
                 the project's root of trust"
                    .to_owned(),
            ));
            return;
        }

        let params = BoardParameters::new(org_params.founder, name, random_bytes::<16>());
        let key = contract::board_key(&params);
        let org_id = OrgId(id_bytes(org_key.id()));

        let mut drafts: Vec<Draft> = bootstrap_ops(org_params.founder, &identity.name)
            .into_iter()
            .map(Draft::from)
            .collect();
        // Record the owning organization on the board itself, so the project can
        // show where it belongs without anyone having to be told.
        drafts.push(
            Op::SetOrganization {
                org: org_id,
                name: org_params.name.clone(),
            }
            .into(),
        );
        // Seed the organization's current admins as board admins, so they can staff
        // the project without the founder. Admins appointed later need resyncing —
        // see `sync_org_admins`.
        //
        // Two writes each now, not one: the grant is authority and the name is
        // presentation, and they travel separately so that renaming somebody never
        // has to route through the code that decides what they may do.
        for admin in org.admins() {
            if admin.id == org_params.founder {
                continue;
            }
            drafts.push(Draft::Grant {
                member: admin.id,
                rights: Rights::ADMIN,
            });
            drafts.push(
                Op::SetMemberName {
                    member: admin.id,
                    name: admin.name.clone(),
                }
                .into(),
            );
        }

        let state = EnvelopeState::from_ops(Self::sign_batch(&identity, &params, drafts));

        self.params.set(Some(params.clone()));
        self.contract_key.set(Some(key));
        self.ops.set(state.clone());
        self.pending_board.set(Some(*key.id()));
        self.refold();
        self.view.set(View::Board);
        // Ready at once — see `create`.
        self.status.set(BoardStatus::Ready);
        set_board_in_url(&key.encoded_contract_id());

        self.store_board(&params, &state);

        let board = board_id_of(&key);
        self.org_emit(OrgOp::AddProject {
            board,
            name: name.to_owned(),
        });
        self.publish_listing(ListingTarget::Board(board), name.to_owned());
        self.remember_board();
    }

    /// Sends a board's genesis state to the node, then confirms the node has it.
    ///
    /// `node::put` asks the node to subscribe as well, so the same request that
    /// stores the board starts changes from other peers flowing back.
    ///
    /// # Why this asks rather than waits for an acknowledgement
    ///
    /// It would be neater to wait for `PutResponse`. Measured against freenet
    /// 0.2.105: a successful `PUT` produces no observable reply at all. The node's
    /// own log says `initial_state_installed` and the board is genuinely there, but
    /// nothing comes back down the socket, so an indicator that waited for an ack
    /// would report every single creation as failed.
    ///
    /// So confirmation is a **read-back**: ask the node for the board and see
    /// whether it can serve it. That is independent of which reply the node chooses
    /// to send, and it is the stronger claim anyway — "the node will hand this to
    /// the next person who asks" is what a user means by created.
    fn store_board(self, params: &BoardParameters, state: &EnvelopeState) {
        let instance = *contract::board_key(params).id();
        self.publish.set(Publish::Storing(instance));

        node::put(
            contract::board_container(params),
            WrappedState::new(state.encode()),
        );
        self.confirm_publish(instance, 1);
    }

    /// Creates a task and, once the network confirms it exists, puts a card for it
    /// on the open board.
    ///
    /// The order is the point. A placement naming a contract that was never stored
    /// is a card that opens onto nothing — the dead-reference failure this codebase
    /// has already fixed once for boards. So the `PUT` is confirmed by read-back
    /// first, and only then are `Place` and the first `Summarize` written. The card
    /// appears a beat later in exchange for never being a lie.
    pub(crate) fn create_task(self, column: ColumnId, title: &str) {
        let title = title.trim().to_owned();
        if title.is_empty() {
            self.notice.set(Some("a task needs a title".to_owned()));
            return;
        }
        let (Some(identity), Some(board)) = (self.me(), self.board.get_untracked()) else {
            return;
        };
        if !board.may(&identity.member, Rights::WRITE_TASKS) {
            self.notice
                .set(Some("you cannot add tasks to this project".to_owned()));
            return;
        }

        // The org the task answers to, taken from the board. A personal board has
        // none, and then only the creator and whoever they grant may edit it.
        let org = self
            .org_params
            .get_untracked()
            .zip(self.org_key.get_untracked())
            .map(|(params, key)| TaskOrg {
                id: OrgId(id_bytes(key.id())),
                scope: params.scope(),
                founder: params.founder,
            });

        let params = TaskParameters::new(identity.member, org, now_ms(), random_bytes::<16>());
        let addr = contract::task_addr(&params);
        let rank = board.rank_for_drop(&column, board.tasks_in(&column).len(), None);

        // Genesis: the title, written by the creator, who owns the contract.
        let stamp = Stamp::new(
            params.scope(),
            identity.member,
            1,
            now_ms(),
            random_bytes::<16>(),
        );
        let genesis = TaskOp::SetTitle {
            title: title.clone(),
        }
        .envelope(stamp)
        .sign(identity.signing_key());
        let summary = TaskSummary {
            title,
            assignee: None,
            seen_lamport: 1,
        };

        node::put(
            contract::task_container(&params),
            WrappedState::new(EnvelopeState::from_ops(vec![genesis]).encode()),
        );
        self.place_when_stored(addr, column, rank, summary, 1);
    }

    /// Waits for a freshly created task to read back, then places it.
    ///
    /// Same shape as [`Self::confirm_publish`], and for the same reason: a `PUT` is
    /// processed asynchronously, so the first read-back can beat it and treating
    /// that as failure would be wrong.
    fn place_when_stored(
        self,
        task: TaskAddr,
        column: ColumnId,
        rank: Rank,
        summary: TaskSummary,
        attempt: u32,
    ) {
        if attempt > PUBLISH_CONFIRM_ATTEMPTS {
            self.notice.set(Some(
                "the task could not be stored on the network, so no card was added".to_owned(),
            ));
            return;
        }
        // Parked, then acted on by `receive_task` when the read-back lands. Routing
        // it through the ordinary fetch path rather than a second one means the
        // task's parameters are in hand by the time the card is written, which is
        // what `Attach` needs.
        self.pending_placement.set(Some(Placing {
            task,
            column,
            rank: rank.clone(),
            summary: summary.clone(),
        }));
        let instance = contract::task_instance(task);
        self.pending_task_fetch.set(Some(instance));
        crate::ui::after_ms(PUBLISH_CONFIRM_EVERY_MS, move || {
            if self.pending_placement.get_untracked().is_none() {
                return;
            }
            node::get(instance);
            self.place_when_stored(task, column, rank, summary, attempt + 1);
        });
    }

    /// Writes the card, now that the task behind it provably exists.
    ///
    /// Three writes to two contracts: the placement and its first summary on the
    /// board, and on the task its own record of which board it landed on. That last
    /// one is what lets a link opened cold find its way back — see
    /// [`pj_core::task::Task::boards`].
    fn finish_placement(self) {
        let Some(placing) = self.pending_placement.get_untracked() else {
            return;
        };
        self.pending_placement.set(None);
        self.emit(Op::Place {
            task: placing.task,
            column: placing.column,
            rank: placing.rank,
        });
        self.emit(Op::Summarize {
            task: placing.task,
            summary: placing.summary,
        });
        if let Some(board) = self.open_board_id() {
            self.task_emit(TaskOp::Attach { board });
        }
        // Converting a board is a queue of these; retire the card this replaced
        // and keep going.
        if let Some(old) = self.converting.get_untracked() {
            self.retire_legacy(old);
            self.converting.set(None);
        }
        if !self.legacy_queue.get_untracked().is_empty() {
            self.migrate_next();
        } else if self.migrating_board.get_untracked() {
            self.migrating_board.set(false);
            self.detect_legacy();
            self.say_ok("every card converted");
        }
    }

    /// Asks the node whether it has the board yet, and keeps asking for a while.
    ///
    /// Retried rather than asked once because a `PUT` is processed asynchronously:
    /// the first read-back can genuinely arrive before the contract is installed,
    /// and treating that as failure would be a lie with a scary badge on it.
    fn confirm_publish(self, instance: ContractInstanceId, attempt: u32) {
        if self.publish.get_untracked() != Publish::Storing(instance) {
            return;
        }
        if attempt > PUBLISH_CONFIRM_ATTEMPTS {
            self.publish.set(Publish::Unconfirmed(instance));
            return;
        }

        crate::ui::after_ms(PUBLISH_CONFIRM_EVERY_MS, move || {
            if self.publish.get_untracked() != Publish::Storing(instance) {
                return;
            }
            node::get(instance);
            self.confirm_publish(instance, attempt + 1);
        });
    }

    /// The node has the board: it either acknowledged the `PUT` or served it back.
    fn board_stored(self, key: &ContractKey) {
        // Matched on the instance so a late answer for a board created two boards
        // ago cannot mark the current one as safely stored.
        let settled = match self.publish.get_untracked() {
            Publish::Storing(pending) | Publish::Unconfirmed(pending) => pending == *key.id(),
            Publish::Settled => false,
        };
        if settled {
            self.publish.set(Publish::Settled);
        }
    }

    /// Sends the open board's state again, for a publish that was never confirmed.
    ///
    /// Safe to repeat: a `PUT` carries the whole state and the contract merges it,
    /// so re-sending converges rather than overwriting. Which is also why the local
    /// edits made while it was unconfirmed ride along.
    pub(crate) fn retry_publish(self) {
        let Some(params) = self.params.get_untracked() else {
            return;
        };
        self.store_board(&params, &self.ops.get_untracked());
        self.say_ok("sending the project to your node again");
    }

    /// Brings the open project's board admins back in line with the organization's,
    /// for admins appointed after the project was created.
    pub(crate) fn sync_org_admins(self) {
        let (Some(org), Some(board)) = (self.org.get_untracked(), self.board.get_untracked())
        else {
            return;
        };
        let mut added = 0;
        for admin in org.admins() {
            let already = board
                .members
                .get(&admin.id)
                .is_some_and(|member| member.role == Role::Admin && member.active);
            if already {
                continue;
            }
            self.grant(admin.id, Rights::ADMIN);
            self.emit(Op::SetMemberName {
                member: admin.id,
                name: admin.name.clone(),
            });
            added += 1;
        }
        self.say_ok(if added == 0 {
            "every organization admin already administers this project".to_owned()
        } else {
            format!("added {added} organization admin(s) to this project")
        });
    }

    /// Assigns an organization member to the open project.
    ///
    /// A member may be assigned to as many projects as they like — each assignment
    /// is just a grant on that project's own board.
    pub(crate) fn assign_from_org(self, member: MemberId) {
        let Some(org) = self.org.get_untracked() else {
            return;
        };
        let name = org.member_name(&member);
        self.grant(member, Rights::MEMBER);
        self.emit(Op::SetMemberName { member, name });
    }

    /// Organization members who are not yet on the open project.
    pub(crate) fn assignable_from_org(self) -> Vec<(MemberId, String)> {
        let (Some(org), Some(board)) = (self.org.get(), self.board.get()) else {
            return Vec::new();
        };
        org.active_members()
            .into_iter()
            .filter(|member| {
                board
                    .members
                    .get(&member.id)
                    .is_none_or(|existing| !existing.active)
            })
            .map(|member| (member.id, member.name.clone()))
            .collect()
    }

    /// Stamps a genesis batch with consecutive lamports and signs it.
    ///
    /// Consecutive rather than all-equal because the fold's total order starts with
    /// the lamport: the grant that admits an admin has to sort before anything that
    /// relies on it, and the columns have to keep the order they were laid out in.
    fn sign_batch(
        identity: &Identity,
        params: &BoardParameters,
        drafts: Vec<Draft>,
    ) -> Vec<SignedEnvelope> {
        let at = now_ms();
        let scope = params.scope();
        drafts
            .into_iter()
            .enumerate()
            .map(|(index, draft)| {
                let stamp = Stamp::new(
                    scope,
                    identity.member,
                    index as u64 + 1,
                    at,
                    random_bytes::<16>(),
                );
                draft.envelope(stamp).sign(identity.signing_key())
            })
            .collect()
    }

    pub(crate) fn op_count(self) -> usize {
        self.ops.get().len()
    }

    /// Whether the open board is governed by the same contract this build carries.
    ///
    /// This used to matter for every new op kind: the contract decoded a typed enum,
    /// so a variant it had never seen failed the decode and took the whole delta
    /// with it. It no longer does — op bodies are opaque to the contract, and an
    /// unknown kind is stored and carried.
    ///
    /// What it still catches is a board created before that change, whose contract
    /// speaks a different state encoding entirely. Those cannot be written to at
    /// all, so it is worth knowing before offering a button that would silently do
    /// nothing.
    pub(crate) fn contract_matches_build(self) -> bool {
        match self.contract_key.get() {
            Some(key) => key.encoded_code_hash() == contract::board_code_hash(),
            // Nothing open, or a board this session created with the embedded code.
            None => true,
        }
    }
}

/// The ops a new board starts with: the owner named, and the default columns.
fn bootstrap_ops(owner: MemberId, owner_name: &str) -> Vec<Op> {
    let columns: Vec<ColumnId> = bootstrap_columns();
    pj_core::bootstrap::genesis_ops(owner, owner_name, &columns)
}

/// Wall clock, in milliseconds since the epoch.
///
/// Comes from `Temporal.Now`, via `bridge.js`, as a **`BigInt`** — which is the
/// whole point. `Date.now()` is a `Number`, and reading a `Number` into a `u64`
/// means an `f64` conversion that Rust can only spell as `as`: unchecked,
/// silently saturating, and something a reader has to take on trust. Temporal's
/// `epochNanoseconds` is integral, so the path from clock to signed op is integral
/// end to end and the conversion below can actually fail.
///
/// It fails to 0 rather than panicking. A clock this far outside `u64`
/// milliseconds is a broken machine, not a case to handle, and every peer folds
/// the resulting op the same way. Wall clock is only ever a tiebreak here; the
/// lamport is what carries causality.
pub(crate) fn now_ms() -> u64 {
    u64::try_from(epoch_millis()).unwrap_or(0)
}

fn board_id_of(key: &ContractKey) -> BoardId {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(key.id().as_bytes());
    BoardId(bytes)
}

/// What the URL fragment is pointing at.
enum Route {
    None,
    Board(String),
    /// A single task, as `#task/<address>`. What the copy button on a task hands
    /// out, and what makes a task something you can send somebody.
    ///
    /// Carries no board, and needs none: a task is its own contract, so its
    /// address is the whole reference. Which projects it sits on comes back with
    /// the task.
    Task(String),
    Organization(String),
    /// The signed-in user's own page.
    Me,
    /// A device offering its public key to be linked — see `#link/<key>`.
    Link(String),
}

/// Below this the board and the sidebar are fighting over the same pixels.
const SIDEBAR_MIN_WIDTH: f64 = 1200.0;

fn wide_enough() -> bool {
    web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|width| width.as_f64())
        // No window to measure means no reason to hide anything.
        .is_none_or(|width| width >= SIDEBAR_MIN_WIDTH)
}

/// Turns what the client API says into something meant for a person.
///
/// Only the cases worth rewording. Everything else passes through: an error nobody
/// anticipated is more useful verbatim than flattened into a shrug.
fn humanise(message: &str) -> String {
    if message.contains("timed out") {
        return "the network did not confirm that write in time, so it may not have been \
                stored. Your copy is intact — reload to see what the network actually has."
            .to_owned();
    }
    message.to_owned()
}

/// The route is kept in the URL fragment so a board or organization is linkable and
/// a reload returns to it. A bare id means a board, for backwards compatibility with
/// links shared before organizations existed.
fn route_in_url() -> Route {
    let Some(hash) = web_sys::window().and_then(|window| window.location().hash().ok()) else {
        return Route::None;
    };
    let route = hash.trim_start_matches('#').trim();
    if route.is_empty() {
        Route::None
    } else if route == "me" {
        Route::Me
    } else if let Some(org) = route.strip_prefix("org/") {
        Route::Organization(org.to_owned())
    } else if let Some(key) = route.strip_prefix("link/") {
        Route::Link(key.to_owned())
    } else if let Some(task) = route.strip_prefix("task/") {
        Route::Task(task.trim_end_matches('/').to_owned())
    } else if let Some((board, _)) = route.split_once('/') {
        // A link from before tasks had their own contracts: `#<board>/<task>`. The
        // task no longer exists at that id, but the board does, so open that rather
        // than refusing the whole URL.
        Route::Board(board.to_owned())
    } else {
        Route::Board(route.to_owned())
    }
}

/// Sets the fragment to an arbitrary route (`<boardId>` or `org/<orgId>`).
fn set_route_in_url(route: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_hash(route);
    }
    set_shell_hash(&format!("#{route}"));
}

fn id_bytes(instance: &ContractInstanceId) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(instance.as_bytes());
    bytes
}

fn set_board_in_url(id: &str) {
    // Our own fragment: what a reload of this frame reads back.
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_hash(id);
    }
    // And the shell's address bar, which is the URL the user can actually see and
    // copy — inside the sandboxed frame ours is invisible to them.
    set_shell_hash(&format!("#{id}"));
}

fn clear_board_in_url() {
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_hash("");
    }
    // A bare "#" is the shortest thing the shell accepts: its handler requires a
    // leading '#' and ignores an empty string outright.
    set_shell_hash("#");
}

#[wasm_bindgen]
extern "C" {
    /// Mirrors the route onto the shell's address bar — see `bridge.js`.
    #[wasm_bindgen(js_name = __freenetSetHash)]
    fn set_shell_hash(hash: &str);

    /// Milliseconds since the epoch, from `Temporal` — see `bridge.js`.
    ///
    /// A `BigInt` rather than a `Number` on purpose: see [`now_ms`].
    #[wasm_bindgen(js_name = __freenetNowMs)]
    fn epoch_millis() -> js_sys::BigInt;
}

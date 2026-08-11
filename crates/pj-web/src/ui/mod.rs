//! The user interface.

mod board;
mod org;
mod panels;
mod user;

use leptos::prelude::*;
use pj_core::MemberId;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::{Closure, wasm_bindgen};

#[wasm_bindgen]
extern "C" {
    /// Platform share sheet, falling back to the clipboard — see `bridge.js`.
    ///
    /// Returns a word describing what happened, so the UI can say something true.
    #[wasm_bindgen(js_name = __freenetShare)]
    pub(crate) fn share(title: &str, url: &str) -> String;
}

use crate::store::{BoardStatus, Connection, Publish, Store, View};
use board::{BoardView, TaskPage};
use org::OrgPage;
use panels::{Sidebar, StartPage};
use user::UserPage;

#[component]
pub(crate) fn App() -> impl IntoView {
    let store = Store::new();
    store.boot();

    // Escape closes whatever is layered on top. The app had no keyboard handling
    // at all beyond Enter-to-submit, so an open drawer could only be dismissed by
    // finding and clicking its close link.
    window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" {
            store.close_task();
        }
    });

    // Back and Forward move the fragment; this is what makes the view follow.
    window_event_listener(leptos::ev::hashchange, move |_| store.route_changed());

    // The sidebar shows or hides itself as the window crosses the width where the
    // board starts needing those pixels.
    window_event_listener(leptos::ev::resize, move |_| store.viewport_changed());

    view! {
        <div class="app">
            <TopBar store=store />
            <Notices store=store />
            <main class="main">
                {move || match store.view.get() {
                    View::User => view! { <UserPage store=store /> }.into_any(),
                    View::Organization => view! { <OrgPage store=store /> }.into_any(),
                    View::Start => view! {
                        <div class="centered wide">
                            <StartPage store=store />
                        </div>
                    }
                    .into_any(),
                    View::Board => view! { <BoardRoute store=store /> }.into_any(),
                    View::Task => view! { <TaskPage store=store /> }.into_any(),
                }}
            </main>
        </div>
    }
}

#[component]
fn BoardRoute(store: Store) -> impl IntoView {
    view! {
        {move || match store.status.get() {
                    BoardStatus::Ready => view! { <Workspace store=store /> }.into_any(),

                    BoardStatus::Loading(id) => view! {
                        <p class="muted centered">
                            "Looking for board " <code>{id}</code> " on the network…"
                        </p>
                    }
                    .into_any(),

                    // Its own page rather than the start page with a sentence on
                    // top. You asked for a specific board, the URL still says so,
                    // and a reload is a retry — none of which is true if this
                    // quietly becomes the front door. Being dropped into the full
                    // project list is also what made a slow lookup feel like a
                    // navigation you did not ask for.
                    BoardStatus::Missing(id) => view! {
                        <div class="centered stack">
                            <p class="muted">
                                "The network has no board under " <code>{id.clone()}</code> "."
                            </p>
                            <p class="muted small">
                                "Either it was never stored, or no peer holding it has answered
                                 yet. Looking again is worth a try; the id is still in the
                                 address bar either way."
                            </p>
                            <div class="row">
                                <button
                                    class="button primary"
                                    on:click={
                                        let id = id.clone();
                                        move |_| store.open(&id)
                                    }
                                >
                                    "Look again"
                                </button>
                                <button class="button" on:click=move |_| store.back()>
                                    "All projects"
                                </button>
                            </div>
                        </div>
                    }
                    .into_any(),

                    BoardStatus::Idle => view! {
                        <div class="centered wide">
                            <StartPage store=store />
                        </div>
                    }
                    .into_any(),
                }}
    }
}

#[component]
fn Workspace(store: Store) -> impl IntoView {
    let hidden = move || !store.sidebar_open();

    view! {
        <div class="workspace" class:sidebar-hidden=hidden>
            <BoardView store=store />
            // No vertical peek rail any more. It occupied a grid track in order to
            // hand back horizontal space, and the top bar's toggle is always there.
            <Show when=move || store.sidebar_open() fallback=|| ()>
                <Sidebar store=store />
            </Show>
        </div>
    }
}

#[component]
fn TopBar(store: Store) -> impl IntoView {
    let on_board = move || store.view.get() == View::Board;
    let anything_open = move || store.view.get() != View::Start;

    // The bar used to show the open *board* whatever you were looking at, so
    // standing on your own account page it still announced a project name.
    let where_you_are = move || match store.view.get() {
        View::Start => None,
        View::User => Some(("Your account".to_owned(), None)),
        View::Organization => Some((
            store
                .org_params
                .get()
                .map_or_else(|| "Organization".to_owned(), |params| params.name),
            Some("organization".to_owned()),
        )),
        // A task's page names its project, because that is the context you need in
        // order to know where "← Board" goes.
        View::Board | View::Task => Some((
            store
                .params
                .get()
                .map_or_else(|| "Project".to_owned(), |params| params.name),
            store
                .board
                .get()
                .and_then(|board| board.organization)
                .map(|owner| owner.name),
        )),
    };

    let me = move || match (store.profile.get(), store.identity.get()) {
        (Some(profile), Some(identity)) => display_name(&profile.name, &identity.member),
        (None, Some(identity)) => identity.name,
        _ => "You".to_owned(),
    };

    view! {
        <header class="topbar">
            <div class="brand">
                <span class="logo" aria-hidden="true">"◈"</span>
                <span class="wordmark">"freenet-pj"</span>
            </div>

            <Show when=anything_open fallback=|| ()>
                <button
                    class="button small"
                    title="Back to the start page"
                    on:click=move |_| store.back()
                >
                    "← All"
                </button>
            </Show>

            <div class="board-name">
                {move || where_you_are().map(|(name, context)| view! {
                    {name}
                    {context.map(|context| view! { <span class="where">" · " {context}</span> })}
                })}
            </div>

            <button
                class="button small account"
                title="Your devices, organizations and projects"
                on:click=move |_| store.open_user_page()
            >
                // The same face the board gives you, in the same colour, so the way
                // back to your account is recognisable rather than merely labelled.
                <span
                    class="avatar"
                    aria-hidden="true"
                    style=("--member-h", move || {
                        store
                            .identity
                            .get()
                            .map_or(199, |identity| member_hue(&identity.member))
                            .to_string()
                    })
                >
                    {move || me().chars().next().unwrap_or('?').to_string()}
                </span>
                <span class="account-name">{me}</span>
            </button>

            // Only meaningful on a board: elsewhere it toggled its own label and
            // nothing else.
            //
            // Named for what it holds rather than "info". The point of the sidebar
            // is the member list, and a count tells you what is behind it while it
            // is shut.
            <Show when=on_board fallback=|| ()>
                <button
                    class="button small info-toggle"
                    aria-expanded=move || store.sidebar_open().to_string()
                    title=move || if store.sidebar_open() {
                        "Hide the member list and project details"
                    } else {
                        "Show the member list and project details"
                    }
                    on:click=move |_| store.toggle_sidebar()
                >
                    {move || {
                        let count = store
                            .board
                            .get()
                            .map_or(0, |board| board.active_members().len());
                        format!("Members · {count}")
                    }}
                </button>
            </Show>

            <ThemeToggle store=store />

            <ConnectionBadge store=store />
        </header>
    }
}

/// Light or dark, by choice rather than by whatever the operating system says.
///
/// The choice is kept by the *node*, in the preferences delegate. The app runs on
/// the opaque origin of a sandboxed frame where `localStorage` throws, so the node
/// is the only local place to put it. `index.html` applies the system preference
/// before the first paint; the saved one lands a moment later, once the delegate
/// has answered.
#[component]
fn ThemeToggle(store: Store) -> impl IntoView {
    let dark = RwSignal::new(current_theme_is_dark());

    // The delegate's answer arrives after this component exists, so follow it
    // rather than reading it once.
    Effect::new(move |_| {
        if let Some(saved) = store.preference(pj_prefs_proto::THEME) {
            dark.set(saved != "light");
        }
    });

    Effect::new(move |_| {
        apply_theme(if dark.get() { "dark" } else { "light" });
    });

    let describe = move || {
        if dark.get() {
            "Switch to the light theme"
        } else {
            "Switch to the dark theme"
        }
    };

    view! {
        <button
            class="button small"
            title=describe
            aria-label=describe
            on:click=move |_| {
                let now_dark = !dark.get_untracked();
                dark.set(now_dark);
                store.set_preference(
                    pj_prefs_proto::THEME,
                    if now_dark { "dark" } else { "light" },
                );
            }
        >
            // The icon shows the theme you would be switching *to*.
            <span aria-hidden="true">{move || if dark.get() { "☀" } else { "☾" }}</span>
        </button>
    }
}

/// Puts a theme on the document, where the stylesheet keys off it.
pub(crate) fn apply_theme(theme: &str) {
    if let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
    {
        let _ = root.set_attribute("data-theme", theme);
    }
}

/// What `index.html` resolved before the app booted.
fn current_theme_is_dark() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
        .and_then(|root| root.get_attribute("data-theme"))
        .is_none_or(|theme| theme != "light")
}

#[component]
fn ConnectionBadge(store: Store) -> impl IntoView {
    // Identity arrives a round trip after the socket opens, and nothing can be
    // written until it does, so it is worth surfacing separately.
    let identity_pending =
        move || store.identity.get().is_none() && store.connection.get() == Connection::Open;

    view! {
        // Connection state changes on its own schedule rather than in response to
        // anything the user did, which is exactly what a live region is for.
        <div class="row" aria-live="polite">
        {move || match store.connection.get() {
            Connection::Open if identity_pending() => view! {
                <span class="badge pending">"loading identity…"</span>
            }
            .into_any(),
            // The healthy state is the one you never need to read, so on a narrow
            // screen it shrinks to a dot and gives its row back to the board.
            // Every other state keeps its words.
            Connection::Open => view! {
                <span
                    class="badge ok quiet"
                    title="connected to your local Freenet node"
                    aria-label="node connected"
                >
                    <span class="dot" aria-hidden="true" />
                    <span class="badge-text">"node connected"</span>
                </span>
            }
            .into_any(),
            Connection::Connecting => view! {
                <span class="badge pending">"connecting to node…"</span>
            }
            .into_any(),
            Connection::Reconnecting { attempt, in_ms } => view! {
                <span
                    class="badge bad"
                    title="the connection dropped; retrying with backoff"
                >
                    {format!("reconnecting in {}s (try {attempt})", in_ms / 1000)}
                </span>
                <button class="button small" on:click=move |_| store.retry_connection()>
                    "Retry now"
                </button>
            }
            .into_any(),
            Connection::Lost(reason) => view! {
                <span class="badge bad" title=reason>"node unreachable"</span>
                <button class="button small" on:click=move |_| store.retry_connection()>
                    "Retry"
                </button>
            }
            .into_any(),
        }}

        // Nothing is lost while the connection is away — it is queued, and re-sent
        // together with a full state push once the socket is back.
        <Show when=move || store.pending_writes.get() != 0 fallback=|| ()>
            <span class="badge pending" title="queued until the connection returns">
                {move || format!("{} unsent", store.pending_writes.get())}
            </span>
        </Show>

        // A new project is usable before the network has it. This is the difference,
        // which was previously assumed away.
        {move || match store.publish.get() {
            Publish::Settled => ().into_any(),
            Publish::Storing(_) => view! {
                <span
                    class="badge pending"
                    title="the board works already; this is the node confirming it has it"
                >
                    "publishing…"
                </span>
            }
            .into_any(),
            Publish::Unconfirmed(_) => view! {
                <span
                    class="badge bad"
                    title="your node never confirmed storing this project. It still works here, \
                           but a reload — or anyone you send the link to — may not find it."
                >
                    "not confirmed"
                </span>
                <button class="button small" on:click=move |_| store.retry_publish()>
                    "Publish again"
                </button>
            }
            .into_any(),
        }}
        </div>
    }
}

/// Problems and confirmations, kept apart.
///
/// One channel used to carry both, tinted with `--danger`, so a successful
/// reconnection was announced in the same red bar as a rejected write.
#[component]
fn Notices(store: Store) -> impl IntoView {
    view! {
        <div aria-live="polite">
            <Show when=move || store.flash.get().is_some() fallback=|| ()>
                <div class="notice ok" role="status">
                    <span class="tone-mark" aria-hidden="true">"✓"</span>
                    <span class="grow">{move || store.flash.get().unwrap_or_default()}</span>
                    <button class="link" on:click=move |_| store.flash.set(None)>"dismiss"</button>
                </div>
            </Show>
        </div>

        <div aria-live="assertive">
            <Show when=move || store.notice.get().is_some() fallback=|| ()>
                <div class="notice error" role="alert">
                    <span class="tone-mark" aria-hidden="true">"!"</span>
                    <span class="grow">{move || store.notice.get().unwrap_or_default()}</span>
                    <button class="link" on:click=move |_| store.notice.set(None)>"dismiss"</button>
                </div>
            </Show>
        </div>
    }
}

/// A destructive control that asks twice.
///
/// Deliberately not `window.confirm`: a modal dialog blocks the whole page, and
/// this reads better anyway — the button turns into its own warning and forgets
/// about it a few seconds later.
#[component]
pub(crate) fn Confirm(
    #[prop(into)] label: String,
    #[prop(into)] confirm: String,
    #[prop(into)] on_confirm: Callback<()>,
    #[prop(default = "link danger".to_owned(), into)] class: String,
) -> impl IntoView {
    let armed = RwSignal::new(false);
    // Each closure below needs its own copy for the lifetime of the view, so these
    // are moves rather than clones: nothing here reads `class`, `label` or
    // `confirm` again afterwards.
    let base = class;
    let idle = label.clone();
    let asked = confirm;

    view! {
        <button
            class=move || if armed.get() { format!("{base} arming") } else { base.clone() }
            title=move || if armed.get() {
                "click again to confirm".to_owned()
            } else {
                format!("{idle} — asks for confirmation")
            }
            on:click=move |_| {
                if armed.get_untracked() {
                    armed.set(false);
                    on_confirm.run(());
                } else {
                    armed.set(true);
                    after_ms(4000, move || armed.set(false));
                }
            }
        >
            {move || if armed.get() { asked.clone() } else { label.clone() }}
        </button>
    }
}

// ============================================================ helpers

/// A shareable link to a route in this app.
///
/// Built from the page's own origin and path so a link points at the node the
/// person is using. `TaskRef::parse` throws the origin away when reading one back,
/// so a link copied here still resolves on somebody else's node.
pub(crate) fn app_url(route: &str) -> String {
    match web_sys::window().map(|window| window.location()) {
        Some(location) => {
            let origin = location.origin().unwrap_or_default();
            let path = location.pathname().unwrap_or_default();
            format!("{origin}{path}#{route}")
        }
        None => format!("#{route}"),
    }
}

/// Runs `body` once, `ms` from now.
pub(crate) fn after_ms(ms: i32, body: impl FnOnce() + 'static) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once_into_js(body);
    let _ =
        window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), ms);
}

/// A stable hue for a member, so who-owns-what is visible before it is read.
///
/// Keys are uniformly distributed, so two bytes of one are as good a hue source as
/// anything and cost no state: everyone who folds the same board derives the same
/// colour for the same person without agreeing on anything.
///
/// Confined to Icy Blue → Dusty Mauve. A full turn of the wheel would put people in
/// greens and reds that mean something else here, and in the brand teal at 175.
pub(crate) fn member_hue(member: &MemberId) -> u16 {
    const FIRST: u16 = 199;
    const SPAN: u16 = 142;
    FIRST + (((u16::from(member.0[0]) << 8) | u16::from(member.0[1])) % SPAN)
}

/// A hue for a column, by its position on the board.
///
/// Position is all we can read as progress: columns are user-defined, so their
/// names mean nothing to us. The default four run Dusty Mauve → Indigo Velvet →
/// Icy Blue → Blue Spruce, so a board cools and brightens as work moves right and
/// lands on the one colour in this palette that reads as green.
///
/// Blue Spruce is also the interactive colour. The overlap is deliberate: it is the
/// palette's only positive hue, and "finished" and "actionable" are both good news.
/// They stay apart by shape — a 3px rail and a filled pill will not be mistaken for
/// a link.
pub(crate) fn stage_hue(index: usize) -> u16 {
    const RAMP: [u16; 6] = [308, 265, 205, 175, 240, 290];
    RAMP[index % RAMP.len()]
}

/// A profile's name, with a friendlier stand-in while it is still the default.
///
/// A fresh profile is named after its own key, so the account page was titled with
/// an eight-character blob while every board called the same person `anon-…`. Two
/// auto-generated names for one person is one too many. Fixed here rather than in
/// `pj-core`, because the profile contract embeds that crate and changing it would
/// re-address every profile that already exists.
pub(crate) fn display_name(name: &str, member: &MemberId) -> String {
    if name == member.short() {
        format!("anon-{name}")
    } else {
        name.to_owned()
    }
}

pub(crate) fn copy(text: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.navigator().clipboard().write_text(text);
    }
}

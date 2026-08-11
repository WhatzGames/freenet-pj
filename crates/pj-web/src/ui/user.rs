//! The signed-in user's own page: their identity, their devices, and everything
//! they are a member of.

use leptos::prelude::*;
use pj_core::MemberId;

use crate::store::Store;
use crate::ui::{Confirm, copy, display_name, member_hue, share};

#[component]
pub(crate) fn UserPage(store: Store) -> impl IntoView {
    let ready = move || store.identity.get().is_some();

    view! {
        <Show
            when=ready
            fallback=|| view! {
                <p class="muted centered">"Asking the node's identity delegate for your key…"</p>
            }
        >
            <div class="org-page">
                // The name used to appear three times within 200px — top bar,
                // heading, and the field below it — and the heading sat in its own
                // flexible column, which pushed the form's left edge out of line
                // with the panels underneath. One block, one left edge, name once.
                <h1 class="sr-only">"Your account"</h1>
                <Identity store=store />
                <div class="org-grid">
                    <Devices store=store />
                    <Memberships store=store />
                </div>
            </div>
        </Show>
    }
}

#[component]
fn Identity(store: Store) -> impl IntoView {
    let my_key = move || {
        store
            .identity
            .get()
            .map(|identity| identity.member.to_base58())
            .unwrap_or_default()
    };
    let name = move || who(store);
    let hue = move || {
        store
            .identity
            .get()
            .map_or(199, |identity| member_hue(&identity.member))
            .to_string()
    };

    view! {
        <section class="panel identity">
            <div class="row identity-head">
                <span class="avatar large" aria-hidden="true" style=("--member-h", hue)>
                    {move || name().chars().next().unwrap_or('?').to_string()}
                </span>
                <label class="field grow">
                    <span>"Display name"</span>
                    <input
                        class="input"
                        prop:value=name
                        on:change=move |ev| store.set_display_name(&event_target_value(&ev))
                    />
                </label>
            </div>
            <div class="field">
                <span>"Your public key"</span>
                <div class="row">
                    <code class="grow ellipsis">{my_key}</code>
                    <button
                        class="button small"
                        on:click=move |_| {
                            copy(&my_key());
                            store.say_ok("your public key is on the clipboard");
                        }
                    >
                        "copy"
                    </button>
                </div>
            </div>
        </section>
    }
}

/// This person's name as it should be shown.
fn who(store: Store) -> String {
    match (store.profile.get(), store.identity.get()) {
        (Some(profile), Some(identity)) => display_name(&profile.name, &identity.member),
        _ => String::new(),
    }
}

#[component]
fn Devices(store: Store) -> impl IntoView {
    let devices = move || {
        store
            .profile
            .get()
            .map(|profile| {
                profile
                    .device_list()
                    .into_iter()
                    .map(|device| (device.id, device.label.clone(), device.primary))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    view! {
        <section class="panel">
            <h2>"Devices"</h2>
            <p class="muted small">
                "Every key that acts as you. Linking never moves a secret — the new browser
                 shows its public key and this one vouches for it."
            </p>

            <ul class="members">
                <For
                    each=devices
                    key=|(id, _, _)| *id
                    children=move |(id, label, primary)| view! {
                        <li style=("--member-h", member_hue(&id).to_string())>
                            <span class="avatar" aria-hidden="true">
                                {if primary { "★" } else { "▪" }}
                            </span>
                            // Own flex row: the chip used to follow a bare text node
                            // inside one span, rendering "this identitythis browser".
                            <span class="who grow">
                                <span class="ellipsis">
                                    // The stored label for the root key is generic;
                                    // the chip beside it already says which browser
                                    // this is, so say what the key *is* instead.
                                    {if primary { "primary key".to_owned() } else { label }}
                                </span>
                                <Show when=move || primary fallback=|| ()>
                                    <span class="chip subtle">"this browser"</span>
                                </Show>
                            </span>
                            <code class="muted small">{id.short()}</code>
                            <Show when=move || !primary fallback=|| ()>
                                <Confirm
                                    label="unlink"
                                    confirm="unlink?"
                                    on_confirm=Callback::new(move |()| store.unlink_device(id))
                                />
                            </Show>
                        </li>
                    }
                />
            </ul>

            // Followed the linking controls over from the board sidebar: the
            // refusal happens per board, so it is worth saying while you are
            // standing over the button that would trigger it.
            <Show
                when=move || store.board.get().is_some() && !store.contract_matches_build()
                fallback=|| ()
            >
                <p class="warn small">
                    "The project you have open predates device linking. Its contract cannot
                     read the op, so linking will be refused there — newer projects are fine."
                </p>
            </Show>

            <PendingLink store=store />
            <AddDevice store=store />
            <RecoveryKey store=store />
        </section>
    }
}

/// The secret behind this identity.
///
/// Lived in the board sidebar until now, which put a key that *is* you one
/// disclosure away on the screen people keep open all day. It belongs with the
/// account, next to the devices it authorises.
#[component]
fn RecoveryKey(store: Store) -> impl IntoView {
    let recovery_key = move || {
        store
            .identity
            .get()
            .map(|identity| identity.recovery_key())
            .unwrap_or_default()
    };

    view! {
        <details class="reveal">
            <summary>"Recovery key"</summary>
            <p class="warn small">
                "This is your secret key: whoever holds it is you. It is the only way to carry
                 this identity to a different node — save it somewhere safe, and paste one here
                 to switch back to that identity."
            </p>
            <div class="row">
                <code class="grow ellipsis">{recovery_key}</code>
                <button
                    class="button small"
                    on:click=move |_| {
                        copy(&recovery_key());
                        store.say_ok("recovery key copied — store it somewhere safe");
                    }
                >
                    "copy"
                </button>
            </div>
            <input
                class="input mono"
                placeholder="Paste a recovery key to restore"
                aria-label="Paste a recovery key to restore"
                on:change=move |ev| store.restore_identity(&event_target_value(&ev))
            />
        </details>
    }
}

/// The confirmation step for a key that arrived by `#link/<key>` URL.
///
/// A link in a URL is a *request*, not authority — it is acted on only once the
/// person holding an already-trusted key says yes here.
#[component]
fn PendingLink(store: Store) -> impl IntoView {
    let pending = move || store.pending_device_key.get();
    let label = RwSignal::new(String::new());

    view! {
        {move || {
            pending().map(|key| {
                let for_link = key.clone();
                let shown = MemberId::from_base58(&key)
                    .map_or_else(|| "unreadable key".to_owned(), |id| id.short());
                view! {
                    <div class="stack pending-link">
                        <p class="warn small">
                            "A device is asking to be linked as you: " <code>{shown}</code>
                            ". Only accept this if you opened the link from your own other browser."
                        </p>
                        <div class="row">
                            <input
                                class="input"
                                placeholder="Label (e.g. phone)"
                                prop:value=move || label.get()
                                on:input=move |ev| label.set(event_target_value(&ev))
                            />
                            <button
                                class="button primary"
                                on:click=move |_| store.link_device(&for_link, &label.get_untracked())
                            >
                                "Link it"
                            </button>
                            <button
                                class="button"
                                on:click=move |_| store.pending_device_key.set(None)
                            >
                                "Dismiss"
                            </button>
                        </div>
                    </div>
                }
            })
        }}
    }
}

/// Which end of the handshake this browser is.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    /// This browser is the one being added.
    New,
    /// This browser already acts as you, and is here to vouch for another.
    Trusted,
}

/// Linking, framed by where you are standing.
///
/// There is one operation here, not two features: a new browser offers its public
/// key, and a browser that is already you accepts it. Presented as two peer
/// disclosures — "link this device from another one" and "link a device by pasting
/// its key" — you had to read both and work out which browser you were supposed to
/// be sitting at. Asking outright costs one click and removes the guess.
#[component]
fn AddDevice(store: Store) -> impl IntoView {
    let side = RwSignal::new(Option::<Side>::None);
    let chosen = move |which: Side| {
        if side.get() == Some(which) {
            "button chosen"
        } else {
            "button"
        }
    };

    view! {
        <div class="stack add-device">
            <h3 class="panel-heading">"Add a device"</h3>
            <p class="muted small">
                "Linking has two ends: the new browser offers its public key, and a browser
                 that is already you accepts it. Nothing secret travels either way."
            </p>
            <div class="row wrap">
                <button
                    class=move || chosen(Side::New)
                    aria-pressed=move || (side.get() == Some(Side::New)).to_string()
                    on:click=move |_| side.set(Some(Side::New))
                >
                    "I'm on the new browser"
                </button>
                <button
                    class=move || chosen(Side::Trusted)
                    aria-pressed=move || (side.get() == Some(Side::Trusted)).to_string()
                    on:click=move |_| side.set(Some(Side::Trusted))
                >
                    "I'm on a browser that's already me"
                </button>
            </div>

            {move || match side.get() {
                Some(Side::New) => view! { <LinkThisDevice store=store /> }.into_any(),
                Some(Side::Trusted) => view! { <LinkByKey store=store /> }.into_any(),
                None => ().into_any(),
            }}
        </div>
    }
}

/// The new browser's side: it offers its own key as a shareable link.
#[component]
fn LinkThisDevice(store: Store) -> impl IntoView {
    let link_url = move || {
        let key = store
            .identity
            .get()
            .map(|identity| identity.member.to_base58())
            .unwrap_or_default();
        match web_sys::window().map(|w| w.location()) {
            Some(location) => {
                let origin = location.origin().unwrap_or_default();
                let path = location.pathname().unwrap_or_default();
                format!("{origin}{path}#link/{key}")
            }
            None => String::new(),
        }
    };

    let outcome = RwSignal::new(Option::<String>::None);
    let send = move || {
        let result = share("Link this device to my freenet-pj identity", &link_url());
        outcome.set(Some(
            match result.as_str() {
                "shared" => "opened your share sheet",
                "copied" => "link copied to the clipboard",
                _ => "your browser offers neither sharing nor clipboard access",
            }
            .to_owned(),
        ));
    };

    view! {
        <div class="stack side">
            <p class="muted small">
                "Send this link to a browser that is already you, and accept it there. Easier
                 on a phone than typing a key: no transcription, and nothing secret travels —
                 the link carries only your public key."
            </p>
            <div class="row wrap">
                <button class="button primary" on:click=move |_| send()>"Share link"</button>
                <button
                    class="button"
                    on:click=move |_| {
                        copy(&link_url());
                        store.say_ok("link copied to the clipboard");
                    }
                >
                    "Copy link"
                </button>
            </div>
            {move || outcome.get().map(|text| view! { <p class="muted small">{text}</p> })}

            // The point of linking a phone is not having to move a URL between two
            // machines. A share sheet needs them already connected somehow; a code
            // on the screen needs only a camera.
            <div class="qr" inner_html=move || qr_svg(&link_url()) />
            <p class="muted small">"Point the other device's camera at this."</p>

            // Wrapped in a row so `.grow`'s `min-width: 0` lets it shrink. As a bare
            // child it could not, and a 90-character URL ran out past the panel.
            <div class="row">
                <code class="grow ellipsis muted small">{link_url}</code>
            </div>
        </div>
    }
}

/// The link as a scannable code.
///
/// Rendered as SVG rather than a canvas so it scales and inherits `currentColor`,
/// which matters because the page has two themes and a QR needs real contrast
/// between its modules and their background either way.
fn qr_svg(url: &str) -> String {
    use qrcode::render::svg;
    use qrcode::{EcLevel, QrCode};

    // Low correction: this is a long URL on a bright screen at short range, and
    // every level above L costs modules, which makes each one smaller to scan.
    match QrCode::with_error_correction_level(url.as_bytes(), EcLevel::L) {
        Ok(code) => code
            .render::<svg::Color<'_>>()
            .min_dimensions(180, 180)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .quiet_zone(true)
            .build(),
        // A missing code is a missing convenience; the link below it still works.
        Err(_) => String::new(),
    }
}

/// The original flow, kept for when pasting a key is easier than opening a link.
#[component]
fn LinkByKey(store: Store) -> impl IntoView {
    let key = RwSignal::new(String::new());
    let label = RwSignal::new(String::new());
    let link = move || {
        store.link_device(&key.get_untracked(), &label.get_untracked());
        key.set(String::new());
        label.set(String::new());
    };

    view! {
        <div class="stack side">
            <p class="muted small">
                "Open the app on the new browser, copy the public key it shows you, and paste
                 it here. It then acts as you."
            </p>
            <input
                class="input mono"
                placeholder="Other browser's public key"
                prop:value=move || key.get()
                on:input=move |ev| key.set(event_target_value(&ev))
            />
            <div class="row">
                <input
                    class="input"
                    placeholder="Label (e.g. laptop)"
                    prop:value=move || label.get()
                    on:input=move |ev| label.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key() == "Enter" { link() }
                />
                <button class="button primary" on:click=move |_| link()>"Link"</button>
            </div>
        </div>
    }
}

#[component]
fn Memberships(store: Store) -> impl IntoView {
    let grouped = move || {
        store
            .profile
            .get()
            .map(|profile| profile.grouped())
            .unwrap_or_default()
    };
    let organizations = move || grouped().0;
    let loose = move || grouped().1;
    let nothing = move || organizations().is_empty() && loose().is_empty();

    view! {
        <section class="panel">
            <h2>"Your organizations and projects"</h2>
            <p class="muted small">
                "Recorded as you open things you belong to. Freenet has no way to search for
                 them, so this list is your own index rather than a query."
            </p>

            <Show
                when=move || !nothing()
                fallback=|| view! {
                    <p class="muted small">
                        "Nothing yet. Open a project or organization you are a member of and it
                         will appear here."
                    </p>
                }
            >
                <ul class="memberships">
                    {move || organizations()
                        .into_iter()
                        .map(|(org, projects)| {
                            let org_route = org.org.to_base58();
                            let empty = projects.is_empty();
                            view! {
                                <li>
                                    <button
                                        class="directory-entry"
                                        on:click=move |_| store.open_org(&org_route)
                                    >
                                        <span class="directory-name">{org.name.clone()}</span>
                                        <span class="muted small">"organization"</span>
                                    </button>
                                    <ul class="nested">
                                        <Show when=move || !empty fallback=|| view! {
                                            <li class="muted small">"no projects yet"</li>
                                        }>
                                            {projects
                                                .clone()
                                                .into_iter()
                                                .map(|project| {
                                                    let route = project.board.to_base58();
                                                    view! {
                                                        <li>
                                                            <button
                                                                class="directory-entry"
                                                                on:click=move |_| store.open(&route)
                                                            >
                                                                <span class="directory-name">
                                                                    {project.name.clone()}
                                                                </span>
                                                            </button>
                                                            // Keeps this row the same
                                                            // width as the ones that
                                                            // carry a "forget".
                                                            <span class="gutter" />
                                                        </li>
                                                    }
                                                })
                                                .collect::<Vec<_>>()}
                                        </Show>
                                    </ul>
                                </li>
                            }
                        })
                        .collect::<Vec<_>>()}

                    {move || {
                        let solo = loose();
                        (!solo.is_empty()).then(|| view! {
                            <li>
                                // Parallel to an organization's name above it, so it
                                // reads as the same kind of grouping rather than a
                                // stray sentence.
                                <p class="panel-heading group-label">"Not in an organization"</p>
                                <ul class="nested">
                                    {solo
                                        .into_iter()
                                        .map(|project| {
                                            let route = project.board.to_base58();
                                            let board = project.board;
                                            view! {
                                                <li>
                                                    <button
                                                        class="directory-entry"
                                                        on:click=move |_| store.open(&route)
                                                    >
                                                        <span class="directory-name">
                                                            {project.name.clone()}
                                                        </span>
                                                    </button>
                                                    <span class="gutter">
                                                        <button
                                                            class="link"
                                                            title="Remove from this list"
                                                            on:click=move |_| {
                                                                store.leave_board_bookmark(board);
                                                            }
                                                        >
                                                            "forget"
                                                        </button>
                                                    </span>
                                                </li>
                                            }
                                        })
                                        .collect::<Vec<_>>()}
                                </ul>
                            </li>
                        })
                    }}
                </ul>
            </Show>
        </section>
    }
}

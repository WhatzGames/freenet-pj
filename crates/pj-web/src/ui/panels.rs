//! The start page (create, browse, open) and the board sidebar.

use leptos::prelude::*;
use pj_core::{MemberId, Op, Rights, Role};

use crate::contract;
use crate::store::{BROWSE_LIMIT, Store, now_ms};
use crate::ui::org::{OrgAssignPanel, OrgDirectory};
use crate::ui::{Confirm, app_url, copy, member_hue, share};

// ============================================================ start page

/// Discovery first.
///
/// This used to be four equally-weighted panels stacked into a 640px column —
/// 1279px of page in a 724px viewport, with the list of projects third and two
/// paste-an-id escape hatches given the same prominence as the primary actions.
#[component]
pub(crate) fn StartPage(store: Store) -> impl IntoView {
    view! {
        <div class="start-page">
            <section class="start-hero" aria-labelledby="start-title">
                <div class="stack">
                    <p class="kicker">"Peer-owned project boards"</p>
                    <h1 id="start-title">"freenet-pj"</h1>
                    <p class="start-copy">
                        "Create, find, and work on project boards whose state lives on Freenet.
                         Your browser signs changes, the network carries them, and the board keeps
                         working without an application server."
                    </p>
                </div>
                <div class="hero-stats" aria-label="What this app supports">
                    <span>"Boards"</span>
                    <span>"Organizations"</span>
                    <span>"Task links"</span>
                </div>
            </section>

            <div class="picker grid">
                <div class="span-all">
                    <Directory store=store />
                </div>
                <CreateBoard store=store />
                <OrgDirectory store=store />
            </div>
        </div>
    }
}

#[component]
fn CreateBoard(store: Store) -> impl IntoView {
    let new_name = RwSignal::new(String::new());
    let create = move || {
        store.create(&new_name.get_untracked());
        new_name.set(String::new());
    };

    view! {
        <section class="panel">
            <h2>"Start a project"</h2>
            <p class="muted small">
                "You become its owner — the one key nobody can remove, and the only one that
                 starts out able to invite. You can make others admins, and they can invite too.
                 It is listed publicly so anyone can find it."
            </p>
            <div class="row">
                <input
                    class="input"
                    placeholder="Project name"
                    aria-label="New project name"
                    prop:value=move || new_name.get()
                    on:input=move |ev| new_name.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key() == "Enter" { create() }
                />
                <button class="button primary" on:click=move |_| create()>"Create"</button>
            </div>
        </section>
    }
}

/// The public directory, which is the only way to discover a board on a network
/// that has no enumeration and no search of its own.
#[component]
fn Directory(store: Store) -> impl IntoView {
    let listings = move || store.browse();
    let total = move || store.browse_total();

    let join_id = RwSignal::new(String::new());
    let join = move || {
        store.open(&join_id.get_untracked());
        join_id.set(String::new());
    };

    view! {
        <section class="panel">
            <div class="row">
                <h2 class="grow">"All projects"</h2>
                <Show when=move || !store.search.get().trim().is_empty() fallback=|| ()>
                    <button class="link" on:click=move |_| store.search.set(String::new())>
                        "clear search"
                    </button>
                </Show>
                <span class="muted small">
                    {move || {
                        let shown = listings().len();
                        let all = total();
                        if all > shown {
                            format!("{shown} of {all}")
                        } else {
                            format!("{shown}")
                        }
                    }}
                </span>
            </div>

            <input
                class="input"
                placeholder="Search projects by name…"
                aria-label="Search projects by name"
                prop:value=move || store.search.get()
                on:input=move |ev| store.search.set(event_target_value(&ev))
            />

            <Show
                when=move || !listings().is_empty()
                fallback=move || view! {
                    <p class="muted small">
                        {move || if store.search.get().trim().is_empty() {
                            "No projects listed yet. Create the first one.".to_owned()
                        } else {
                            "Nothing matches that search.".to_owned()
                        }}
                    </p>
                }
            >
                <ul class="directory">
                    <For
                        each=listings
                        key=|listing| listing.id()
                        children=move |listing| {
                            let id = listing.encoded_id();
                            let open_id = id.clone();
                            view! {
                                <li>
                                    <button
                                        class="directory-entry"
                                        on:click=move |_| store.open(&open_id)
                                    >
                                        <span class="directory-name">{listing.name.clone()}</span>
                                        <span class="muted small">
                                            // Names are not unique and nothing stops two
                                            // projects sharing one; without the address
                                            // the rows were indistinguishable.
                                            <code>{short_id(&id)}</code>
                                            " · " {ago(listing.created_ms, now_ms())}
                                            " · owner " {listing.owner.short()}
                                        </span>
                                    </button>
                                </li>
                            }
                        }
                    />
                </ul>
            </Show>
            // Only worth saying when the list was actually cut short. It used to sit
            // under "Nothing matches that search", promising 25 results in the same
            // breath as reporting none.
            // `<` rather than `>`: a bare `>` would close the tag in `view!`.
            <Show when=move || listings().len() < total() fallback=|| ()>
                <p class="muted small">
                    {format!("Showing the {BROWSE_LIMIT} most recent matches.")}
                </p>
            </Show>

            <details class="reveal">
                <summary>"Open a project by id"</summary>
                <p class="muted small">
                    "For a project that is not listed, or one you have a direct link to."
                </p>
                <div class="row">
                    <input
                        class="input mono"
                        placeholder="Board id (base58)"
                        aria-label="Board id"
                        prop:value=move || join_id.get()
                        on:input=move |ev| join_id.set(event_target_value(&ev))
                        on:keydown=move |ev| if ev.key() == "Enter" { join() }
                    />
                    <button class="button" on:click=move |_| join()>"Open"</button>
                </div>
            </details>
        </section>
    }
}

// ============================================================ sidebar

/// What this sidebar holds is everything true of *this project* — who is on it,
/// which organization owns it, and where it lives on the network.
///
/// Your own identity used to sit here too, which was the wrong place twice over: it
/// is account data, not project data, and it duplicated the account page down to a
/// second "Display name" field that wrote somewhere else. Your secret recovery key
/// was one disclosure away on the main working screen. All of that now lives on the
/// account page, reachable from the top bar.
#[component]
pub(crate) fn Sidebar(store: Store) -> impl IntoView {
    view! {
        <aside class="sidebar">
            <MembersPanel store=store />
            <OrgAssignPanel store=store />
            <ProjectPanel store=store />
        </aside>
    }
}

/// Everything about the project that is not a person.
///
/// Both halves are folded shut. The address is needed exactly once — when you
/// invite someone — and the contract hashes are diagnostics, yet between them they
/// used to occupy a permanent eight-line panel, which is most of the reason anyone
/// wanted to hide the sidebar in the first place.
#[component]
fn ProjectPanel(store: Store) -> impl IntoView {
    // The network's address for this board, not the one this build would compute
    // — they differ for a board created against an older contract.
    let board_id = move || {
        store
            .contract_key
            .get()
            .or_else(|| store.params.get().as_ref().map(contract::board_key))
            .map(|key| key.encoded_contract_id())
            .unwrap_or_default()
    };

    // The contract actually governing this board, which for a board created by an
    // older build is not the one embedded here.
    let governing_code = move || {
        store.contract_key.get().map_or_else(
            || short_hash(&contract::board_code_hash()),
            |key| short_hash(&key.encoded_code_hash()),
        )
    };

    // An openable link rather than 44 characters of base58. Handing someone an id
    // and leaving them to work out where to paste it was never an invitation.
    let invite_link = move || app_url(&board_id());

    view! {
        <section class="panel">
            <h2>"Project"</h2>

            <details class="reveal">
                <summary>"Invite people"</summary>
                <p class="muted small">
                    "Anyone with this link can open the project. Being able to change it is a
                     separate matter — that takes an invitation from the owner, above."
                </p>
                <div class="row wrap">
                    <button
                        class="button primary small"
                        on:click=move |_| {
                            let outcome = share("Open this project in freenet-pj", &invite_link());
                            store.say_ok(match outcome.as_str() {
                                "shared" => "opened your share sheet",
                                "copied" => "link copied to the clipboard",
                                _ => "your browser offers neither sharing nor clipboard access",
                            });
                        }
                    >
                        "Share link"
                    </button>
                    <button
                        class="button small"
                        on:click=move |_| {
                            copy(&invite_link());
                            store.say_ok("link copied to the clipboard");
                        }
                    >
                        "Copy link"
                    </button>
                </div>
                <div class="row">
                    <code class="grow ellipsis">{board_id}</code>
                    <button
                        class="button small"
                        title="The raw id, for pasting into Open by id"
                        on:click=move |_| {
                            copy(&board_id());
                            store.say_ok("project id copied");
                        }
                    >
                        "copy id"
                    </button>
                </div>
            </details>

            <details class="reveal">
                <summary>"Contract details"</summary>
                <dl class="facts">
                    <dt>"contract code"</dt>
                    <dd><code>{governing_code}</code></dd>
                    <dt>"ops in state"</dt>
                    <dd>{move || store.op_count()}</dd>
                </dl>
                <Show
                    when=move || governing_code() != short_hash(&contract::board_code_hash())
                    fallback=|| ()
                >
                    <p class="muted small">
                        "This board was created by an older build, so it is governed by that
                         build's contract rather than the one shipped here. Reads and writes
                         work normally."
                    </p>
                </Show>
            </details>
        </section>
    }
}

#[component]
fn MembersPanel(store: Store) -> impl IntoView {
    // Ids only. A keyed `For` reuses a surviving row without re-rendering it, so
    // anything captured by value here would freeze at the value it had when the
    // row first appeared — a member who links a device later would never show it.
    let member_ids = move || {
        store
            .board
            .get()
            .map(|board| board.members.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default()
    };

    let invite_id = RwSignal::new(String::new());
    let invite_name = RwSignal::new(String::new());

    let invite = move || {
        let raw = invite_id.get_untracked().trim().to_owned();
        let Some(member) = MemberId::from_base58(&raw) else {
            store
                .notice
                .set(Some(format!("{raw:?} is not a valid member id")));
            return;
        };
        let name = invite_name.get_untracked().trim().to_owned();
        let name = if name.is_empty() {
            member.short()
        } else {
            name
        };
        // Two writes: what they may do, and what to call them. The first is
        // authority and the contract reads it; the second is presentation and it
        // does not.
        store.grant(member, Rights::MEMBER);
        store.emit(Op::SetMemberName { member, name });
        invite_id.set(String::new());
        invite_name.set(String::new());
    };

    view! {
        <section class="panel">
            <h2>"Members"</h2>
            <ul class="members">
                <For
                    each=member_ids
                    key=|id| *id
                    children=move |id| view! { <MemberRow store=store id=id /> }
                />
            </ul>

            <Show
                when=move || store.may(Rights::MAY_GRANT)
                fallback=move || view! {
                    <p class="muted small">
                        "Only an admin can change membership."
                    </p>
                    // Your key belongs on the account page, except here: standing on
                    // a project you cannot write to, it is the one thing you need,
                    // and asking you to go and find it would be perverse.
                    <Show when=move || !a_member(store) fallback=|| ()>
                        <p class="muted small">
                            "You are not a member of this project. Send the owner your key
                             and ask to be added."
                        </p>
                        <div class="row">
                            <code class="grow ellipsis">{move || my_key(store)}</code>
                            <button
                                class="button small"
                                on:click=move |_| {
                                    copy(&my_key(store));
                                    store.say_ok("your public key is on the clipboard");
                                }
                            >
                                "copy"
                            </button>
                        </div>
                    </Show>
                }
            >
                <div class="stack">
                    <input
                        class="input mono"
                        placeholder="Member key (base58)"
                        aria-label="Member key to invite"
                        prop:value=move || invite_id.get()
                        on:input=move |ev| invite_id.set(event_target_value(&ev))
                    />
                    <div class="row">
                        <input
                            class="input"
                            placeholder="Their name"
                            aria-label="Name for the invited member"
                            prop:value=move || invite_name.get()
                            on:input=move |ev| invite_name.set(event_target_value(&ev))
                            on:keydown=move |ev| if ev.key() == "Enter" { invite() }
                        />
                        <button class="button" on:click=move |_| invite()>"Invite"</button>
                    </div>
                </div>
            </Show>
        </section>
    }
}

/// One row of the member list, deriving everything it shows from the store so that
/// a member who is renamed, deactivated, or gains a device updates in place.
#[component]
fn MemberRow(store: Store, id: MemberId) -> impl IntoView {
    let member = move || {
        store
            .board
            .get()
            .and_then(|board| board.members.get(&id).cloned())
    };
    let name = move || member().map_or_else(|| id.short(), |m| m.name);
    let active = move || member().is_some_and(|m| m.active);
    let is_owner = move || owner_of(store) == id;
    let is_admin = move || member().is_some_and(|m| m.role == Role::Admin);
    let device_count = move || {
        store
            .board
            .get()
            .map_or(0, |board| board.devices_of(&id).len())
    };
    // The owner is not removable at any price: their authority is in the contract
    // parameters, so a grant taking it away would be ignored and the button would
    // lie.
    let removable = move || store.may(Rights::MAY_GRANT) && active() && !is_owner();

    view! {
        <li class:inactive=move || !active() style=("--member-h", member_hue(&id).to_string())>
            <span class="avatar" aria-hidden="true">
                {move || name().chars().next().unwrap_or('?').to_string()}
            </span>
            // The name and its badges are siblings in their own flex row. As a bare
            // text node followed by a chip they rendered glued together.
            <span class="who grow">
                <span class="ellipsis">{name}</span>
                <Show when=is_owner fallback=|| ()>
                    <span class="chip subtle" title="created this project">"owner"</span>
                </Show>
                <Show when=move || is_admin() && !is_owner() fallback=|| ()>
                    <span class="chip">"admin"</span>
                </Show>
                // `!= 0` rather than `> 0`: a bare `>` would close the tag in `view!`.
                <Show when=move || device_count() != 0 fallback=|| ()>
                    <span class="muted small">
                        {move || format!("+{} device", device_count())}
                    </span>
                </Show>
            </span>
            <code class="muted small">{id.short()}</code>
            <Show when=removable fallback=|| ()>
                <Confirm
                    label="remove"
                    confirm="remove?"
                    // Removal is a grant of nothing — there is no separate op.
                    on_confirm=Callback::new(move |()| store.grant(id, Rights::NONE))
                />
            </Show>
        </li>
    }
}

// ============================================================ helpers

fn short_hash(hash: &str) -> String {
    hash.chars().take(12).collect()
}

fn my_key(store: Store) -> String {
    store
        .identity
        .get()
        .map(|identity| identity.member.to_base58())
        .unwrap_or_default()
}

/// Whether this person is on the open board, counting any device they have linked.
fn a_member(store: Store) -> bool {
    let Some(me) = store.identity.get().map(|identity| identity.member) else {
        return false;
    };
    store.board.get().is_some_and(|board| {
        let person = board.person_of(&me);
        board
            .members
            .get(&person)
            .is_some_and(|member| member.active)
    })
}

/// Enough of an address to tell two same-named projects apart.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn owner_of(store: Store) -> MemberId {
    store
        .params
        .get()
        // No board open means nothing is rendered anyway; a zero key matches nobody.
        .map_or(MemberId([0; 32]), |params| params.owner)
}

/// Coarse relative time. Deliberately vague: `created_ms` is written by whoever
/// made the listing, so precision would imply a trust the value does not deserve.
fn ago(created_ms: u64, now_ms: u64) -> String {
    let elapsed = now_ms.saturating_sub(created_ms) / 1000;
    match elapsed {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{}m ago", elapsed / 60),
        3600..=86_399 => format!("{}h ago", elapsed / 3600),
        _ => format!("{}d ago", elapsed / 86_400),
    }
}

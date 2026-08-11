//! The organization page: members, roles, and the projects the organization owns.

use leptos::prelude::*;

use pj_core::{MemberId, Rights, Role};

use crate::store::Store;
use crate::ui::{Confirm, copy, member_hue};

#[component]
pub(crate) fn OrgPage(store: Store) -> impl IntoView {
    let name = move || {
        store
            .org_params
            .get()
            .map(|params| params.name)
            .unwrap_or_default()
    };
    let loaded = move || store.org.get().is_some();

    view! {
        <Show
            when=loaded
            fallback=|| view! {
                <p class="muted centered">"Looking for that organization on the network…"</p>
            }
        >
            <div class="org-page">
                <header class="org-head">
                    <h1>{name}</h1>
                    <OrgIdentity store=store />
                </header>
                <div class="org-grid">
                    <OrgProjects store=store />
                    <OrgMembers store=store />
                </div>
            </div>
        </Show>
    }
}

#[component]
fn OrgIdentity(store: Store) -> impl IntoView {
    let org_id = move || {
        store
            .org_key
            .get()
            .map(|key| key.encoded_contract_id())
            .unwrap_or_default()
    };
    let standing = move || {
        if store.is_org_founder() {
            "you founded this organization"
        } else if store.is_org_admin() {
            "you administer this organization"
        } else if store.is_org_member() {
            "you are a member"
        } else {
            "you are not a member"
        }
    };

    view! {
        <div class="stack">
            <p class="muted small">{standing} " · share this id to invite people to open it"</p>
            <div class="row">
                <code class="grow ellipsis">{org_id}</code>
                <button
                    class="button small"
                    on:click=move |_| {
                        copy(&org_id());
                        store.say_ok("organization id copied");
                    }
                >
                    "copy"
                </button>
            </div>
        </div>
    }
}

#[component]
fn OrgProjects(store: Store) -> impl IntoView {
    let projects = move || {
        store
            .org
            .get()
            .map(|org| {
                org.projects_sorted()
                    .into_iter()
                    .map(|project| (project.board, project.name.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    let new_name = RwSignal::new(String::new());
    let create = move || {
        store.create_org_project(&new_name.get_untracked());
        new_name.set(String::new());
    };

    view! {
        <section class="panel">
            <h2>"Projects"</h2>

            <Show
                when=move || !projects().is_empty()
                fallback=|| view! {
                    <p class="muted small">"This organization has no projects yet."</p>
                }
            >
                <ul class="directory">
                    <For
                        each=projects
                        key=|(board, _)| *board
                        children=move |(board, name)| {
                            let id = board.to_base58();
                            view! {
                                <li>
                                    <button
                                        class="directory-entry"
                                        on:click=move |_| store.open(&id)
                                    >
                                        <span class="directory-name">{name.clone()}</span>
                                        <span class="muted small">{board.short()}</span>
                                    </button>
                                </li>
                            }
                        }
                    />
                </ul>
            </Show>

            <Show
                when=move || store.is_org_founder()
                fallback=move || view! {
                    <Show when=move || store.is_org_admin() fallback=|| ()>
                        <p class="muted small">
                            "Only the founding key can create a project: it becomes the
                             project's root of trust. You can staff any existing project."
                        </p>
                    </Show>
                }
            >
                <div class="row">
                    <input
                        class="input"
                        placeholder="New project name"
                        prop:value=move || new_name.get()
                        on:input=move |ev| new_name.set(event_target_value(&ev))
                        on:keydown=move |ev| if ev.key() == "Enter" { create() }
                    />
                    <button class="button primary" on:click=move |_| create()>"Create"</button>
                </div>
            </Show>
        </section>
    }
}

#[component]
fn OrgMembers(store: Store) -> impl IntoView {
    let member_ids = move || {
        store
            .org
            .get()
            .map(|org| org.members.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default()
    };

    let invite_key = RwSignal::new(String::new());
    let invite_name = RwSignal::new(String::new());
    let invite_admin = RwSignal::new(false);

    let invite = move || {
        let role = if invite_admin.get_untracked() {
            Role::Admin
        } else {
            Role::Member
        };
        store.invite_to_org(
            &invite_key.get_untracked(),
            &invite_name.get_untracked(),
            role,
        );
        invite_key.set(String::new());
        invite_name.set(String::new());
    };

    view! {
        <section class="panel">
            <h2>"Members"</h2>
            <ul class="members">
                <For
                    each=member_ids
                    key=|id| *id
                    children=move |id| view! { <OrgMemberRow store=store id=id /> }
                />
            </ul>

            <Show
                when=move || store.is_org_admin()
                fallback=move || view! {
                    <p class="muted small">
                        "Membership is by invitation. Send an admin your public key to be added."
                    </p>
                }
            >
                <div class="stack">
                    <input
                        class="input mono"
                        placeholder="Their public key (base58)"
                        prop:value=move || invite_key.get()
                        on:input=move |ev| invite_key.set(event_target_value(&ev))
                    />
                    <div class="row">
                        <input
                            class="input"
                            placeholder="Their name"
                            prop:value=move || invite_name.get()
                            on:input=move |ev| invite_name.set(event_target_value(&ev))
                            on:keydown=move |ev| if ev.key() == "Enter" { invite() }
                        />
                        <button class="button" on:click=move |_| invite()>"Invite"</button>
                    </div>
                    <Show when=move || store.is_org_founder() fallback=|| ()>
                        <label class="check">
                            <input
                                type="checkbox"
                                prop:checked=move || invite_admin.get()
                                on:change=move |ev| invite_admin.set(event_target_checked(&ev))
                            />
                            <span class="muted small">"invite as an administrator"</span>
                        </label>
                    </Show>
                </div>
            </Show>

            <Show
                when=move || store.is_org_member() && !store.is_org_founder()
                fallback=|| ()
            >
                <Confirm
                    label="Leave organization"
                    confirm="Leave — click to confirm"
                    class="button danger"
                    on_confirm=Callback::new(move |()| store.leave_org())
                />
            </Show>
        </section>
    }
}

/// One member row, deriving everything from the store: a keyed `For` reuses a
/// surviving row without re-rendering it, so a member who is promoted or removed
/// would otherwise stay visibly stale.
#[component]
fn OrgMemberRow(store: Store, id: MemberId) -> impl IntoView {
    let member = move || {
        store
            .org
            .get()
            .and_then(|org| org.members.get(&id).cloned())
    };
    let name = move || member().map_or_else(|| id.short(), |m| m.name);
    let active = move || member().is_some_and(|m| m.active);
    let is_admin = move || member().is_some_and(|m| m.role == Role::Admin);
    let is_founder = move || {
        store
            .org_params
            .get()
            .is_some_and(|params| params.founder == id)
    };

    // Only the founder promotes, and nobody unseats the founder.
    let can_promote = move || store.is_org_founder() && active() && !is_admin();
    // Admins remove members; unseating an admin takes the founder.
    let can_remove = move || {
        active()
            && !is_founder()
            && if is_admin() {
                store.is_org_founder()
            } else {
                store.is_org_admin()
            }
    };

    view! {
        <li class:inactive=move || !active() style=("--member-h", member_hue(&id).to_string())>
            <span class="avatar" aria-hidden="true">
                {move || name().chars().next().unwrap_or('?').to_string()}
            </span>
            // The badges are siblings of the name, not a chip glued to the end of a
            // bare text node — which is how "anon-DYm77XzUfounder" happened.
            <span class="who grow">
                <span class="ellipsis">{name}</span>
                <Show when=is_founder fallback=|| ()>
                    <span class="chip subtle">"founder"</span>
                </Show>
                <Show when=move || is_admin() && !is_founder() fallback=|| ()>
                    <span class="chip">"admin"</span>
                </Show>
            </span>
            <code class="muted small">{id.short()}</code>
            <Show when=can_promote fallback=|| ()>
                <button class="link" on:click=move |_| store.promote_in_org(id)>
                    "make admin"
                </button>
            </Show>
            <Show when=can_remove fallback=|| ()>
                <Confirm
                    label="remove"
                    confirm="remove?"
                    on_confirm=Callback::new(move |()| store.remove_from_org(id))
                />
            </Show>
        </li>
    }
}

/// The organizations section of the start page.
#[component]
pub(crate) fn OrgDirectory(store: Store) -> impl IntoView {
    let listings = move || store.browse_orgs();
    let new_name = RwSignal::new(String::new());
    let create = move || {
        store.create_org(&new_name.get_untracked());
        new_name.set(String::new());
    };
    let join_id = RwSignal::new(String::new());
    let open = move || {
        store.open_org(&join_id.get_untracked());
        join_id.set(String::new());
    };

    view! {
        <section class="panel">
            <h2>"Organizations"</h2>
            <p class="muted small">
                "An organization owns projects on behalf of a group. Membership is by
                 invitation; admins staff the projects."
            </p>

            <input
                class="input"
                placeholder="Search organizations by name…"
                prop:value=move || store.org_search.get()
                on:input=move |ev| store.org_search.set(event_target_value(&ev))
            />

            <Show
                when=move || !listings().is_empty()
                fallback=move || view! {
                    <p class="muted small">
                        {move || if store.org_search.get().trim().is_empty() {
                            "No organizations yet.".to_owned()
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
                            view! {
                                <li>
                                    <button
                                        class="directory-entry"
                                        on:click=move |_| store.open_org(&id)
                                    >
                                        <span class="directory-name">{listing.name.clone()}</span>
                                        <span class="muted small">
                                            "founder " {listing.owner.short()}
                                        </span>
                                    </button>
                                </li>
                            }
                        }
                    />
                </ul>
            </Show>

            <div class="row">
                <input
                    class="input"
                    placeholder="New organization name"
                    aria-label="New organization name"
                    prop:value=move || new_name.get()
                    on:input=move |ev| new_name.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key() == "Enter" { create() }
                />
                // "Found" read as a past-tense verb next to a search field.
                <button class="button primary" on:click=move |_| create()>"Create"</button>
            </div>

            <details class="reveal">
                <summary>"Open an organization by id"</summary>
                <div class="row">
                    <input
                        class="input mono"
                        placeholder="Organization id (base58)"
                        aria-label="Organization id"
                        prop:value=move || join_id.get()
                        on:input=move |ev| join_id.set(event_target_value(&ev))
                        on:keydown=move |ev| if ev.key() == "Enter" { open() }
                    />
                    <button class="button" on:click=move |_| open()>"Open"</button>
                </div>
            </details>
        </section>
    }
}

/// Shown in a project's sidebar when the project belongs to an organization we hold
/// state for: assign its members, and resync admins appointed after the project was
/// created.
#[component]
pub(crate) fn OrgAssignPanel(store: Store) -> impl IntoView {
    let owning_org = move || store.board.get()?.organization;
    let assignable = move || store.assignable_from_org();
    let have_org_state = move || store.org.get().is_some();

    view! {
        {move || {
            owning_org().map(|owner| {
                let org_route = owner.org.to_base58();
                view! {
                    <section class="panel">
                        <h2>"Organization"</h2>
                        <div class="row">
                            <span class="grow">{owner.name.clone()}</span>
                            <button
                                class="link"
                                on:click=move |_| store.open_org(&org_route)
                            >
                                "open"
                            </button>
                        </div>

                        <Show
                            when=have_org_state
                            fallback=|| view! {
                                <p class="muted small">
                                    "Open the organization to assign its members here."
                                </p>
                            }
                        >
                            <Show
                                when=move || !assignable().is_empty()
                                fallback=|| view! {
                                    <p class="muted small">
                                        "Every organization member is already on this project."
                                    </p>
                                }
                            >
                                <p class="muted small">"Assign a member to this project:"</p>
                                <ul class="members">
                                    {move || assignable()
                                        .into_iter()
                                        .map(|(id, name)| view! {
                                            <li>
                                                <span class="grow">{name}</span>
                                                <button
                                                    class="link"
                                                    on:click=move |_| store.assign_from_org(id)
                                                >
                                                    "assign"
                                                </button>
                                            </li>
                                        })
                                        .collect::<Vec<_>>()}
                                </ul>
                            </Show>
                            <Show when=move || store.may(Rights::MAY_GRANT) fallback=|| ()>
                                <button
                                    class="button small"
                                    title="Add organization admins appointed after this project was created"
                                    on:click=move |_| store.sync_org_admins()
                                >
                                    "Sync admins"
                                </button>
                            </Show>
                        </Show>
                    </section>
                }
            })
        }}
    }
}

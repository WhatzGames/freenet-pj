//! The kanban board: columns, cards, drag-and-drop, and the task detail drawer.

use leptos::prelude::*;
use pj_core::task::TaskOp;
use pj_core::{ColumnId, LinkKind, MemberId, Placement, Rights, TaskAddr};

use crate::store::Store;
use crate::ui::{Confirm, app_url, member_hue, share, stage_hue};

#[component]
pub(crate) fn BoardView(store: Store) -> impl IntoView {
    let columns = move || {
        store
            .board
            .get()
            .map(|board| board.columns)
            .unwrap_or_default()
    };

    view! {
        // One element, because `.workspace` is a two-column grid and counts its
        // children: with the hint as a sibling the sidebar was pushed onto a second
        // row and the hint took its cell — visible only on an empty board, which is
        // exactly when the hint shows.
        <div class="board-area">
        <LegacyCards store=store />
        <UnreadableOps store=store />
        <section class="board">
            <div class="columns">
                <For
                    each=columns
                    key=|column| column.id
                    children=move |column| view! { <ColumnView store=store column=column.id /> }
                />
                <NewColumn store=store />
            </div>

            <TaskDetail store=store />
        </section>

        // A board with nothing on it is the first thing anyone sees after creating
        // a project, and it used to be four empty boxes and no clue.
        <Show when=move || empty(store) fallback=|| ()>
            <p class="muted small empty-hint">
                "Nothing here yet. Type into a column and press Enter to add your first task.
                 Drag cards between columns, or open a task and use its Column picker — which
                 also works on a phone, where dragging does not."
            </p>
        </Show>
        </div>
    }
}

/// Admits that this build is not showing everything the board holds.
///
/// The fold has always counted ops it could not interpret — from a newer client,
/// or with a body this version cannot decode — and carried them intact so they
/// survive the next write. Nothing ever said so, which made a board written by a
/// newer app look complete and quietly wasn't.
///
/// Deliberately not an error. Carrying what you cannot read is the design working:
/// the alternative, in the typed-enum days, was one unknown op making the whole
/// board fail to decode.
#[component]
fn UnreadableOps(store: Store) -> impl IntoView {
    let count = move || store.board.get().map_or(0, |board| board.unreadable_ops);
    // Old cards are unreadable too, and have their own banner offering to fix
    // them, so they are subtracted here — in *ops*, not in cards. Subtracting the
    // card count instead left a board reporting phantom entries "from a newer
    // version" that were really its own three-op history of one old card.
    let unexplained = move || count().saturating_sub(store.legacy_ops.get());

    view! {
        <Show when=move || unexplained() != 0 fallback=|| ()>
            <p class="muted small empty-hint">
                {move || {
                    let n = unexplained();
                    format!(
                        "{n} {} on this project {} written by a newer version of the app and \
                         {} not shown here. They are kept intact — updating will reveal them.",
                        if n == 1 { "entry" } else { "entries" },
                        if n == 1 { "was" } else { "were" },
                        if n == 1 { "is" } else { "are" },
                    )
                }}
            </p>
        </Show>
    }
}

/// Adds a column at the end of the board.
///
/// The contract has always allowed this; there was simply no way to ask for it, so
/// whatever columns a project was created with were the columns it had forever.
#[component]
fn NewColumn(store: Store) -> impl IntoView {
    let draft = RwSignal::new(String::new());
    let add = move || {
        let title = draft.get_untracked();
        if title.trim().is_empty() {
            return;
        }
        store.add_column(&title);
        draft.set(String::new());
    };

    view! {
        <Show when=move || store.may(Rights::WRITE_COLUMNS) fallback=|| ()>
            <div class="column column-new">
                <div class="new-card">
                    <input
                        class="input"
                        placeholder="New column…"
                        aria-label="New column"
                        prop:value=move || draft.get()
                        on:input=move |ev| draft.set(event_target_value(&ev))
                        on:keydown=move |ev| if ev.key() == "Enter" { add() }
                    />
                    <button class="button primary" on:click=move |_| add()>"Add"</button>
                </div>
            </div>
        </Show>
    }
}

/// Offers to convert cards written before tasks had contracts of their own.
///
/// Without this such a board looks like one that has lost its work. The cards are
/// still in the op set and still readable; they just describe a shape this build
/// no longer has. Converting writes each as a task of its own and puts a card back
/// on the board — one `PUT` per card, which is why it is a button and not
/// something that happens the moment the board opens.
#[component]
fn LegacyCards(store: Store) -> impl IntoView {
    let count = move || store.legacy_tasks.get().len();

    view! {
        <Show when=move || count() != 0 fallback=|| ()>
            <div class="banner">
                <span class="grow">
                    {move || {
                        let n = count();
                        format!(
                            "{n} {} on this project {} made before tasks had addresses of \
                             their own, so {} cannot be shown yet.",
                            if n == 1 { "card" } else { "cards" },
                            if n == 1 { "was" } else { "were" },
                            if n == 1 { "it" } else { "they" },
                        )
                    }}
                </span>
                <button class="button" on:click=move |_| store.migrate_legacy()>
                    "Convert them"
                </button>
            </div>
        </Show>
    }
}

/// Whether the open board has no tasks at all.
fn empty(store: Store) -> bool {
    store
        .board
        .get()
        .is_some_and(|board| board.tasks.is_empty())
}

/// Takes an id, not a [`Column`]: a keyed `For` reuses a surviving row without
/// re-rendering it, so anything captured by value here would be frozen at the
/// value it had when the row first appeared. Deriving from the store instead keeps
/// the row live.
#[component]
fn ColumnView(store: Store, column: ColumnId) -> impl IntoView {
    let column_id = column;
    let position = move || {
        store.board.get().and_then(|board| {
            board
                .columns
                .iter()
                .position(|candidate| candidate.id == column_id)
        })
    };
    let title = move || {
        store
            .board
            .get()
            .and_then(|board| {
                board
                    .columns
                    .iter()
                    .find(|candidate| candidate.id == column_id)
                    .map(|candidate| candidate.title.clone())
            })
            .unwrap_or_default()
    };

    // Cloned out of the folded board: the UI renders owned data so it never holds
    // a borrow across a reactive update.
    let tasks = move || {
        store
            .board
            .get()
            .map(|board| {
                board
                    .tasks_in(&column_id)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<Placement>>()
            })
            .unwrap_or_default()
    };

    let count = move || tasks().len();
    let column_count = move || store.board.get().map_or(0, |board| board.columns.len());

    let draft = RwSignal::new(String::new());
    let add_task = move || {
        let title = draft.get_untracked().trim().to_owned();
        if title.is_empty() {
            return;
        }
        // A task is its own contract now, so this is a PUT and the card appears
        // once the network confirms it — see `Store::create_task`.
        store.create_task(column_id, &title);
        draft.set(String::new());
    };

    view! {
        <div
            class="column"
            style=("--stage-h", move || stage_hue(position().unwrap_or(0)).to_string())
        >
            <div class="column-head">
                <Show
                    when=move || store.may(Rights::WRITE_COLUMNS)
                    fallback=move || view! { <h3 class="grow">{title}</h3> }
                >
                    // Editable in place. A column's name is the one thing on a
                    // board that everybody reads and nobody could change.
                    <input
                        class="input column-title"
                        aria-label=move || format!("Rename column {}", title())
                        prop:value=title
                        on:change=move |ev| {
                            store.rename_column(column_id, &event_target_value(&ev));
                        }
                    />
                </Show>
                <span class="count">{count}</span>
                <Show when=move || store.may(Rights::WRITE_COLUMNS) fallback=|| ()>
                    <span class="column-tools">
                        <button
                            class="link"
                            title="Move this column left"
                            disabled=move || position() == Some(0)
                            on:click=move |_| store.shift_column(column_id, -1)
                        >
                            "←"
                        </button>
                        <button
                            class="link"
                            title="Move this column right"
                            disabled=move || position().is_some_and(|at| at + 1 == column_count())
                            on:click=move |_| store.shift_column(column_id, 1)
                        >
                            "→"
                        </button>
                        <Confirm
                            label="remove"
                            confirm="remove?"
                            on_confirm=Callback::new(move |()| store.remove_column(column_id))
                        />
                    </span>
                </Show>
            </div>

            <div class="cards">
                <For
                    each=tasks
                    key=|placement| placement.task
                    children=move |placement| {
                        let id = placement.task;
                        view! {
                            <DropZone store=store column=column_id before=Some(id) />
                            <TaskCard store=store id=id />
                        }
                    }
                />
                // Trailing zone so a card can be dropped at the very bottom.
                <DropZone store=store column=column_id before=None />
            </div>

            // Hidden rather than disabled for somebody who cannot write: a field
            // that takes typing and then refuses it is worse than no field.
            <Show when=move || store.may(Rights::WRITE_TASKS) fallback=|| ()>
                <div class="new-card">
                    <input
                        class="input"
                        placeholder="New task…"
                        aria-label=move || format!("New task in {}", title())
                        prop:value=move || draft.get()
                        on:input=move |ev| draft.set(event_target_value(&ev))
                        on:keydown=move |ev| {
                            if ev.key() == "Enter" {
                                add_task();
                            }
                        }
                    />
                    <button class="button" on:click=move |_| add_task()>"Add"</button>
                </div>
            </Show>
        </div>
    }
}

/// The thin strip between two cards that accepts a drop.
///
/// Identified by the card it sits *above* rather than by a position, and `None`
/// for the trailing zone at the bottom of a column. A position would be wrong the
/// moment the column changes: Leptos's keyed `For` reuses surviving rows without
/// re-rendering them, so a card that leaves the column leaves every zone below it
/// holding a stale index. The anchor stays valid, and the index is resolved
/// against the current board at drop time.
#[component]
fn DropZone(store: Store, column: ColumnId, before: Option<TaskAddr>) -> impl IntoView {
    let active = move || store.dragging.get().is_some();
    // Every zone on the board arms at once, so arming must not change any
    // geometry — it used to grow each strip from 8px to 20px, which reflowed the
    // whole board the instant a card was lifted. Only the zone under the pointer
    // opens up, which is also the only feedback saying where the card will land.
    let over = RwSignal::new(false);

    let drop = move || {
        over.set(false);
        let Some(task) = store.dragging.get_untracked() else {
            return;
        };
        store.move_task(task, column, before);
    };

    view! {
        <div
            class="dropzone"
            class:armed=active
            class:over=move || over.get()
            on:dragover=move |ev: web_sys::DragEvent| ev.prevent_default()
            on:dragenter=move |ev: web_sys::DragEvent| {
                ev.prevent_default();
                over.set(true);
            }
            on:dragleave=move |_| over.set(false)
            on:drop=move |ev: web_sys::DragEvent| {
                ev.prevent_default();
                drop();
            }
        />
    }
}

/// Takes an id for the same reason [`ColumnView`] does: a keyed row is not
/// re-rendered when its data changes, so every field a card displays has to be
/// derived from the store rather than captured. Otherwise an edited title stays
/// visibly stale until the column is rebuilt.
#[component]
fn TaskCard(store: Store, id: TaskAddr) -> impl IntoView {
    let task = move || {
        store
            .board
            .get()
            .and_then(|board| board.tasks.get(&id).cloned())
    };

    // Everything a card shows comes from the cached summary. The task's body is
    // not here and is not fetched to draw this — that is what keeps a board one
    // fetch regardless of how many cards are on it.
    let title = move || {
        task()
            .map(|placement| placement.title())
            .unwrap_or_default()
    };
    let assignee = move || task()?.summary.assignee;
    let assignee_name = move || {
        let member = assignee()?;
        Some(
            store
                .board
                .get()
                .map_or_else(|| member.short(), |board| board.member_name(&member)),
        )
    };

    view! {
        // A button rather than an article: cards had no tabindex and no role, so
        // the board's primary object could not be reached, let alone opened,
        // without a mouse.
        <button
            class="card"
            class:selected=move || store.selected.get() == Some(id)
            class:lifted=move || store.dragging.get() == Some(id)
            // A card that lifts and then refuses to land is a worse answer than
            // one that does not lift.
            draggable=move || store.may(Rights::WRITE_TASKS).to_string()
            aria-label=move || format!("Open task {}", title())
            on:dragstart=move |ev: web_sys::DragEvent| {
                store.dragging.set(Some(id));
                if let Some(transfer) = ev.data_transfer() {
                    // Firefox will not start a drag without payload on the event.
                    let _ = transfer.set_data("text/plain", &id.to_base58());
                    transfer.set_effect_allowed("move");
                }
            }
            on:dragend=move |_| store.dragging.set(None)
            on:click=move |_| store.select_task(id)
        >
            <span class="card-title">{title}</span>
            <span class="card-meta">
                // The face, not the name. A chip spelling out "anon-DYm77XzU" ate a
                // third of a card on its own, and a board of real names would be a
                // wall of name tags. The colour and the initial carry it; the full
                // name is a hover away and in the drawer.
                {move || {
                    assignee().map(|member| {
                        let name = assignee_name().unwrap_or_else(|| member.short());
                        view! {
                            <span
                                class="avatar tiny"
                                title=format!("Assigned to {name}")
                                style=("--member-h", member_hue(&member).to_string())
                            >
                                <span aria-hidden="true">
                                    {name.chars().next().unwrap_or('?').to_string()}
                                </span>
                                <span class="sr-only">{format!("Assigned to {name}")}</span>
                            </span>
                        }
                    })
                }}
            </span>
        </button>
    }
}

/// Moving a task without a mouse.
///
/// HTML5 drag-and-drop never fires on touch, so on a phone the board was
/// effectively read-only — the one thing a kanban board is for could not be done
/// at all. This is also the keyboard path.
#[component]
fn MoveTask(store: Store, id: TaskAddr) -> impl IntoView {
    let columns = move || {
        store
            .board
            .get()
            .map(|board| {
                board
                    .columns
                    .iter()
                    .map(|column| (column.id, column.title.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let current = move || {
        store
            .board
            .get()
            .and_then(|board| board.tasks.get(&id).map(|task| task.column))
    };

    view! {
        <label class="field">
            <span>"Column"</span>
            <select
                class="input"
                prop:value=move || current().map(|c| c.to_base58()).unwrap_or_default()
                // Board rights, not task rights: which column a card sits in
                // belongs to the board, so this is the one control in the drawer
                // governed by where you are rather than by what you are looking at.
                disabled=move || !store.may(Rights::WRITE_TASKS)
                on:change=move |ev| {
                    let value = event_target_value(&ev);
                    if let Some(column) = ColumnId::from_base58(&value) {
                        // To the end of the target column: there is no pointer
                        // here to say where within it.
                        store.move_task(id, column, None);
                    }
                }
            >
                {move || columns()
                    .into_iter()
                    .map(|(column, title)| view! {
                        <option value=column.to_base58()>{title}</option>
                    })
                    .collect::<Vec<_>>()}
            </select>
        </label>
    }
}

/// Who is on the hook for this task.
#[component]
fn TaskAssignee(store: Store) -> impl IntoView {
    let members = move || {
        store.board.get().map_or_else(Vec::new, |board| {
            board
                .active_members()
                .into_iter()
                .map(|member| (member.id, member.name.clone()))
                .collect::<Vec<_>>()
        })
    };
    // From the fetched task, not the board's cached summary: the drawer is the one
    // place that holds the real thing.
    let current = move || {
        store
            .task
            .get()
            .and_then(|task| task.assignee)
            .map_or_else(String::new, |member| member.to_base58())
    };

    view! {
        <label class="field">
            <span>"Assignee"</span>
            <select
                class="input"
                prop:value=current
                disabled=move || !store.task_may(Rights::WRITE_TASKS)
                on:change=move |ev| {
                    let value = event_target_value(&ev);
                    let assignee = if value.is_empty() {
                        None
                    } else {
                        MemberId::from_base58(&value)
                    };
                    store.task_emit(TaskOp::SetAssignee { assignee });
                }
            >
                <option value="">"unassigned"</option>
                {move || members()
                    .into_iter()
                    .map(|(member, name)| view! {
                        <option value=member.to_base58()>{name}</option>
                    })
                    .collect::<Vec<_>>()}
            </select>
        </label>
    }
}

/// Where this task lives, as one link somebody can paste anywhere.
///
/// One link, not two ids. This used to show the board id and the task id side by
/// side, with a sentence explaining that you had to paste them into two separate
/// fields somewhere else — which made the person do the work of assembling a
/// reference the app could assemble itself.
#[component]
fn TaskAddress(store: Store, id: TaskAddr) -> impl IntoView {
    // Needs no board. The address is the whole reference, so this link resolves
    // for somebody who has never opened the project it sits in.
    let link = move || app_url(&Store::task_route(id));

    view! {
        <div class="field">
            <span>"Link to this task"</span>
            <p class="muted small">
                "Opens this task directly. Paste it into another task's link form to
                 connect the two, in this project or any other."
            </p>
            <div class="row">
                <code class="grow ellipsis">{move || link()}</code>
                <button
                    class="button small"
                    title="Copy to the clipboard"
                    on:click=move |_| {
                        let outcome = share("A task in freenet-pj", &link());
                        store.say_ok(match outcome.as_str() {
                            "copied" => "link copied to the clipboard",
                            "shared" => "opened your share sheet",
                            _ => "your browser offers neither sharing nor clipboard access",
                        });
                    }
                >
                    "Copy link"
                </button>
            </div>
        </div>
    }
}

/// A task's links, and the controls to add and remove them.
///
/// Only the edges this task stores. The inverse direction lives on the other task,
/// and is written there when both belong to one organization — which is when this
/// author can write there at all.
#[component]
fn TaskLinks(store: Store, id: TaskAddr) -> impl IntoView {
    let links = move || {
        store
            .task
            .get()
            .map(|task| task.links.into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
    };

    // Every other card on this board, as link targets. A board holds summaries, so
    // these have titles without anything being fetched.
    let candidates = move || {
        store
            .board
            .get()
            .map(|board| {
                board
                    .tasks
                    .values()
                    .filter(|placement| placement.task != id)
                    .map(|placement| (placement.task, placement.title()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    // A link can point anywhere, including at a task on no board this user has
    // open, so the title is only available when the target happens to be on the
    // board in front of them.
    let name_of = move |other: TaskAddr| -> String {
        store
            .board
            .get()
            .and_then(|board| board.tasks.get(&other).map(Placement::title))
            .unwrap_or_else(|| other.short())
    };

    let kind = RwSignal::new(LinkKind::RelatedTo.slug().to_owned());
    let target = RwSignal::new(String::new());
    let pasted = RwSignal::new(String::new());

    let add_local = move || {
        let Some(kind) = LinkKind::from_slug(&kind.get_untracked()) else {
            return;
        };
        let Some(task) = TaskAddr::from_base58(&target.get_untracked()) else {
            store
                .notice
                .set(Some("choose a task to link to".to_owned()));
            return;
        };
        store.link_task(task, kind);
    };

    // One field, and it takes whatever you have: the link off a task's copy button,
    // a bare route, or a bare address. It used to be two fields wanting base58 ids,
    // which meant taking a URL apart by hand before you could use it.
    let add_pasted = move || {
        let Some(kind) = LinkKind::from_slug(&kind.get_untracked()) else {
            return;
        };
        let Some(other) = pj_core::parse_task(&pasted.get_untracked()) else {
            store.notice.set(Some(
                "that does not look like a link to a task. Paste the link from the task's \
                 Copy link button."
                    .to_owned(),
            ));
            return;
        };
        if other == id {
            store
                .notice
                .set(Some("a task cannot be linked to itself".to_owned()));
            return;
        }
        store.link_task(other, kind);
        pasted.set(String::new());
    };

    view! {
        <div class="field">
            <span>"Links"</span>

            <Show
                when=move || !links().is_empty()
                fallback=|| view! { <p class="muted small">"No links yet."</p> }
            >
                <ul class="task-links">
                    {move || links()
                        .into_iter()
                        .map(|(other, kind)| {
                            view! {
                                <li>
                                    <span class="chip subtle">{kind.label()}</span>
                                    <span class="grow ellipsis">{name_of(other)}</span>
                                    <button
                                        class="button small ghost"
                                        title="Open the linked task"
                                        on:click=move |_| store.open_task_page(other)
                                    >
                                        "open"
                                    </button>
                                    <Show
                                        when=move || store.task_may(Rights::LINK_TASKS)
                                        fallback=|| ()
                                    >
                                        <Confirm
                                            label="remove"
                                            confirm="remove?"
                                            on_confirm=Callback::new(move |()| {
                                                store.unlink_task(other);
                                            })
                                        />
                                    </Show>
                                </li>
                            }
                        })
                        .collect::<Vec<_>>()}
                </ul>
            </Show>

            <Show when=move || store.task_may(Rights::LINK_TASKS) fallback=|| ()>
            <select
                class="input"
                aria-label="Link type"
                prop:value=move || kind.get()
                on:change=move |ev| kind.set(event_target_value(&ev))
            >
                {LinkKind::CHOICES
                    .iter()
                    .map(|choice| view! {
                        <option value=choice.slug()>
                            {format!("this task {} …", choice.label())}
                        </option>
                    })
                    .collect::<Vec<_>>()}
            </select>

            <div class="row">
                <select
                    class="input"
                    aria-label="Task to link to"
                    prop:value=move || target.get()
                    on:change=move |ev| target.set(event_target_value(&ev))
                >
                    <option value="">"a task in this project…"</option>
                    {move || candidates()
                        .into_iter()
                        .map(|(task, title)| view! {
                            <option value=task.to_base58()>{title}</option>
                        })
                        .collect::<Vec<_>>()}
                </select>
                <button class="button" on:click=move |_| add_local()>"Link"</button>
            </div>

            <details class="reveal">
                <summary>"Link by pasting a link"</summary>
                <p class="muted small">
                    "Open the other task, press its "<em>"Copy link"</em>" button, and paste it
                     here. Works across projects and organizations. Within one organization
                     the link shows from both ends; across organizations only from this one,
                     because writing to the other task needs access to it."
                </p>
                <div class="row">
                    <input
                        class="input"
                        placeholder="Paste a link to a task…"
                        aria-label="Link to the task to connect"
                        prop:value=move || pasted.get()
                        on:input=move |ev| pasted.set(event_target_value(&ev))
                        on:keydown=move |ev| if ev.key() == "Enter" { add_pasted() }
                    />
                    <button class="button" on:click=move |_| add_pasted()>"Link"</button>
                </div>
            </details>
            </Show>
        </div>
    }
}

/// The task drawer, over the board.
///
/// Still a drawer, because opening a card while you are working through a column
/// should not take the column away. The *page* is for links: a URL somebody sent
/// you, or a reload, arrives at [`TaskPage`] instead.
#[component]
pub(crate) fn TaskDetail(store: Store) -> impl IntoView {
    // The fetched title once it lands, and the board's cached one until then — so
    // the heading is right immediately rather than blank for a round trip.
    let title = move || {
        let id = store.selected.get()?;
        store
            .task
            .get()
            .map(|task| task.title)
            .filter(|title| !title.is_empty())
            .or_else(|| store.board.get()?.tasks.get(&id).map(Placement::title))
    };

    view! {
        {move || store.selected.get().map(|id| view! {
            // Only dims and dismisses on narrow screens; on a wide one the board
            // beside the drawer stays usable.
            <div
                class="drawer-backdrop"
                role="presentation"
                on:click=move |_| store.close_task()
            />
            <aside class="detail" aria-label="Task detail">
                <div class="detail-head">
                    <h3 class="grow ellipsis">{move || title().unwrap_or_default()}</h3>
                    // The way from the overlay to the address. Same content, but on
                    // a page you can link to, reload, and hand to somebody.
                    <button
                        class="link"
                        title="Open this task on its own page"
                        on:click=move |_| store.open_task_page(id)
                    >
                        "open page"
                    </button>
                    <button class="link" on:click=move |_| store.close_task()>"close"</button>
                </div>
                <TaskBody store=store id=id />
            </aside>
        })}
    }
}

/// A task on its own page, at `#<board>/<task>`.
///
/// What every task link opens. The same body as the drawer, so the two cannot
/// drift apart.
#[component]
pub(crate) fn TaskPage(store: Store) -> impl IntoView {
    // The fetched title once it lands, and the board's cached one until then — so
    // the heading is right immediately rather than blank for a round trip.
    let title = move || {
        let id = store.selected.get()?;
        store
            .task
            .get()
            .map(|task| task.title)
            .filter(|title| !title.is_empty())
            .or_else(|| store.board.get()?.tasks.get(&id).map(Placement::title))
    };

    view! {
        {move || store.selected.get().map(|id| view! {
            <section class="task-page" aria-label="Task">
                <div class="detail-head">
                    <button class="button small" on:click=move |_| store.close_task()>
                        "← Board"
                    </button>
                    <h3 class="grow ellipsis">{move || title().unwrap_or_default()}</h3>
                </div>
                <TaskBody store=store id=id />
            </section>
        })}
    }
}

/// Everything a task is, rendered the same whether it is in the drawer or on its
/// own page.
///
/// The task's own fields come from [`Store::task`], which is fetched when the card
/// is opened and is `None` until it lands. Position comes from the board, because
/// that is where position lives.
#[component]
fn TaskBody(store: Store, id: TaskAddr) -> impl IntoView {
    let placed_here = move || {
        store
            .board
            .get()
            .is_some_and(|board| board.tasks.contains_key(&id))
    };

    view! {
        // Position is a property of the board, and is editable as soon as the board
        // is in hand — no waiting on the task itself.
        <Show when=placed_here fallback=|| ()>
            <MoveTask store=store id=id />
        </Show>

        {move || match store.task.get() {
            // Everything else waits for the body. A spinner rather than empty
            // fields: blank inputs invite typing into something that is about to be
            // overwritten by the answer.
            None => view! {
                <p class="muted small">"Loading this task…"</p>
                <TaskAddress store=store id=id />
            }.into_any(),
            Some(task) => {
                let boards = task.boards.clone();
                view! {
                    // Read-only rather than hidden for someone without rights: the
                    // content is the point of opening a task, and a card you can
                    // see but not change is an ordinary thing to be looking at.
                    <label class="field">
                        <span>"Title"</span>
                        <input
                            class="input"
                            prop:value=task.title.clone()
                            readonly=move || !store.task_may(Rights::WRITE_TASKS)
                            on:change=move |ev| {
                                let title = event_target_value(&ev).trim().to_owned();
                                if !title.is_empty() {
                                    store.task_emit(TaskOp::SetTitle { title });
                                }
                            }
                        />
                    </label>

                    <label class="field">
                        <span>"Notes"</span>
                        <textarea
                            class="input"
                            rows="6"
                            prop:value=task.description.clone()
                            readonly=move || !store.task_may(Rights::WRITE_TASKS)
                            on:change=move |ev| {
                                store.task_emit(TaskOp::SetDescription {
                                    description: event_target_value(&ev),
                                });
                            }
                        />
                    </label>

                    <Show when=move || !store.task_may(Rights::WRITE_TASKS) fallback=|| ()>
                        <p class="muted small">
                            "You can read this task but not change it. Editing is open to
                             members of the organization it belongs to."
                        </p>
                    </Show>

                    <TaskAssignee store=store />

                    <TaskLinks store=store id=id />

                    <TaskAddress store=store id=id />

                    // Where else this task is. The task's own record, which is what
                    // lets a link opened cold offer a way back to a board at all.
                    <Show when={let boards = boards.clone(); move || !boards.is_empty()} fallback=|| ()>
                        <div class="field">
                            <span>"On projects"</span>
                            <ul class="task-links">
                                {boards
                                    .iter()
                                    .map(|board| {
                                        let board = *board;
                                        view! {
                                            <li>
                                                <span class="grow ellipsis">
                                                    {board.short()}
                                                </span>
                                                <button
                                                    class="button small ghost"
                                                    on:click=move |_| {
                                                        store.open(&board.to_base58());
                                                    }
                                                >
                                                    "open"
                                                </button>
                                            </li>
                                        }
                                    })
                                    .collect::<Vec<_>>()}
                            </ul>
                        </div>
                    </Show>

                    <Show
                        when=move || placed_here() && store.may(Rights::WRITE_TASKS)
                        fallback=|| ()
                    >
                        <Confirm
                            label="Remove from this project"
                            confirm="Remove — click to confirm"
                            class="button danger"
                            on_confirm=Callback::new(move |()| store.unplace_task(id))
                        />
                        <p class="muted small">
                            "The task itself is kept, and its link goes on working. It can be
                             added to another project."
                        </p>
                    </Show>
                }.into_any()
            }
        }}
    }
}

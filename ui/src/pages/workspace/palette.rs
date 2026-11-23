//! Befehlspalette (Strg+K / ⌘K, Lupe oben rechts): ein Feld für alles – Befehle, Chats und
//! Projekte (sofort, aus dem Zustand der Oberfläche) und Nachrichten aus allen Chats
//! (`GET /search`, kurz nach dem letzten Tastendruck). ↑/↓ wählt, Enter öffnet, Esc schliesst.

use super::layout::{choose_theme, settings_route};
use crate::Route;
use crate::api;
use crate::i18n::{tr, trf};
use crate::state::{self, ME, PROJECTS, THREADS};
use crate::util;
use dioxus::prelude::*;
use lucode_shared::extras::SearchResults;

/// Palette offen?
pub static PALETTE: GlobalSignal<bool> = Signal::global(|| false);

/// Strg+K / ⌘K öffnet und schliesst die Palette (einmal beim Start einrichten).
pub fn install_shortcut() {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    let Some(w) = web_sys::window() else { return };
    let key = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(|e: web_sys::KeyboardEvent| {
        if e.key().eq_ignore_ascii_case("k") && (e.meta_key() || e.ctrl_key()) && !e.alt_key() && !e.shift_key() {
            e.prevent_default();
            let open = *PALETTE.peek();
            *PALETTE.write() = !open;
        }
    });
    let _ = w.add_event_listener_with_callback("keydown", key.as_ref().unchecked_ref());
    key.forget();
}

/// Was ein Eintrag auslöst.
#[derive(Clone, PartialEq)]
enum Go {
    To(Route),
    Theme(&'static str),
    Canvas,
}

#[derive(Clone, Copy, PartialEq)]
enum Ico {
    New,
    Chat,
    Bot,
    Project,
    Message,
    Settings,
    Theme,
    Canvas,
    Live,
    Gauge,
    Import,
    Notes,
}

fn ico(i: Ico) -> Element {
    match i {
        Ico::New => rsx! { icons::SquarePen {} },
        Ico::Chat => rsx! { icons::MessageSquare {} },
        Ico::Bot => rsx! { icons::Bot {} },
        Ico::Project => rsx! { icons::Folder {} },
        Ico::Message => rsx! { icons::TextSearch {} },
        Ico::Settings => rsx! { icons::Settings {} },
        Ico::Theme => rsx! { icons::Palette {} },
        Ico::Canvas => rsx! { icons::Frame {} },
        Ico::Live => rsx! { icons::Activity {} },
        Ico::Gauge => rsx! { icons::Gauge {} },
        Ico::Import => rsx! { icons::Download {} },
        Ico::Notes => rsx! { icons::ScrollText {} },
    }
}

#[derive(Clone, PartialEq)]
struct Entry {
    group: String,
    icon: Ico,
    title: String,
    sub: String,
    right: String,
    go: Go,
}

/// Feste Befehle (Name, Symbol, Ziel).
fn commands(admin: bool) -> Vec<(String, Ico, Go)> {
    let settings = |s: &str| Go::To(Route::Settings { section: s.to_string() });
    let mut v = vec![
        (tr("Neuer Thread"), Ico::New, Go::To(Route::Home {})),
        (tr("Projekte"), Ico::Project, Go::To(Route::Projects {})),
        (tr("Bots"), Ico::Bot, Go::To(Route::Bots {})),
        (tr("Live: arbeitenden Agenten zusehen (Text und CLI)"), Ico::Live, Go::To(Route::Live { scope: "all".into() })),
        (tr("Canvas-Modus"), Ico::Canvas, Go::Canvas),
        (tr("Einstellungen"), Ico::Settings, Go::To(settings_route())),
        (tr("Nutzung und Limits"), Ico::Gauge, settings("usage")),
        (tr("Eigene Anweisungen an die Agenten"), Ico::Notes, settings("instructions")),
        (tr("Erscheinungsbild: Thema, Akzentfarbe, Sterne"), Ico::Theme, settings("appearance")),
        (tr("Thema: Nacht"), Ico::Theme, Go::Theme("dark")),
        (tr("Thema: Tag"), Ico::Theme, Go::Theme("light")),
    ];
    if admin {
        v.push((tr("Umzug von nrzCode"), Ico::Import, settings("import")));
        v.push((tr("Sicherungen der Datenbank"), Ico::Settings, settings("instance")));
    }
    v
}

#[component]
pub fn Palette() -> Element {
    if !PALETTE() {
        return rsx! {};
    }
    // Eigene Komponente: jedes Öffnen beginnt mit leerem Feld
    rsx! { Body {} }
}

#[component]
fn Body() -> Element {
    let nav = use_navigator();
    let mut query = use_signal(String::new);
    let mut selected = use_signal(|| 0usize);
    let mut results = use_signal(SearchResults::default);
    let mut searching = use_signal(|| false);
    // Nachrichten auf dem Server suchen – erst, wenn kurz nichts mehr getippt wurde
    use_effect(move || {
        let q = query().trim().to_string();
        spawn(async move {
            if q.chars().count() < 2 {
                results.set(SearchResults::default());
                return;
            }
            gloo_timers::future::TimeoutFuture::new(220).await;
            if query.peek().trim() != q {
                return;
            }
            searching.set(true);
            if let Ok(r) = api::get::<SearchResults>(&format!("/search?q={}", api::enc(&q))).await {
                if query.peek().trim() == q {
                    results.set(r);
                }
            }
            searching.set(false);
        });
    });

    let q = query().trim().to_lowercase();
    let admin = ME.read().as_ref().is_some_and(|m| m.is_admin);
    let mut entries: Vec<Entry> = Vec::new();
    // Chats: ohne Eingabe die letzten, sonst alle, deren Titel passt
    let mut threads: Vec<lucode_shared::ThreadSummary> =
        THREADS.read().values().filter(|t| if q.is_empty() { t.parent_id.is_none() } else { t.title.to_lowercase().contains(&q) }).cloned().collect();
    threads.sort_by_key(|t| std::cmp::Reverse(t.last_activity_at));
    let projects = PROJECTS.read().clone();
    let mut listed = Vec::new();
    for t in threads.into_iter().take(if q.is_empty() { 5 } else { 8 }) {
        listed.push(t.id);
        let project = t.project_id.and_then(|p| projects.get(&p)).map(|p| p.name.clone()).unwrap_or_default();
        entries.push(Entry {
            group: if q.is_empty() { tr("Letzte Chats") } else { tr("Chats") },
            icon: if t.bot_id.is_some() { Ico::Bot } else { Ico::Chat },
            title: if t.title.is_empty() { tr("Neuer Thread") } else { t.title.clone() },
            sub: project,
            right: util::ago(t.last_activity_at),
            go: Go::To(match t.bot_id {
                Some(b) => Route::BotPage { id: b },
                None => Route::ThreadPage { id: t.id },
            }),
        });
    }
    // Chats, die nur der Server kennt (noch nicht geladen)
    for h in results().threads.iter().filter(|h| !listed.contains(&h.thread_id)).take(4) {
        entries.push(Entry {
            group: tr("Chats"),
            icon: if h.bot_id.is_some() { Ico::Bot } else { Ico::Chat },
            title: h.title.clone(),
            sub: h.project.clone(),
            right: util::ago(h.at),
            go: Go::To(match h.bot_id {
                Some(b) => Route::BotPage { id: b },
                None => Route::ThreadPage { id: h.thread_id },
            }),
        });
    }
    if !q.is_empty() {
        let mut found: Vec<lucode_shared::ProjectSummary> = projects.values().filter(|p| p.name.to_lowercase().contains(&q)).cloned().collect();
        found.sort_by_key(|p| std::cmp::Reverse(p.last_activity_at));
        for p in found.into_iter().take(5) {
            entries.push(Entry { group: tr("Projekte"), icon: Ico::Project, title: p.name.clone(), sub: p.repo.clone(), right: String::new(), go: Go::To(Route::ProjectPage { id: p.id }) });
        }
    }
    for (label, icon, go) in commands(admin) {
        if q.is_empty() || label.to_lowercase().contains(&q) {
            entries.push(Entry { group: tr("Befehle"), icon, title: label, sub: String::new(), right: String::new(), go });
        }
    }
    for h in results().messages.iter() {
        let who = if h.role == "user" { tr("Du") } else { tr("Agent") };
        entries.push(Entry {
            group: tr("In Nachrichten"),
            icon: Ico::Message,
            title: h.snippet.clone(),
            sub: if h.project.is_empty() { format!("{who} · {}", h.title) } else { format!("{who} · {} · {}", h.title, h.project) },
            right: util::ago(h.at),
            go: Go::To(match h.bot_id {
                Some(b) => Route::BotPage { id: b },
                None => Route::ThreadPage { id: h.thread_id },
            }),
        });
    }
    let count = entries.len();
    let sel = selected().min(count.saturating_sub(1));
    // Gewählten Eintrag sichtbar halten
    use_effect(move || {
        let i = selected();
        let _ = js_sys::eval(&format!("document.getElementById('lu-pal-{i}')?.scrollIntoView({{block:'nearest'}})"));
    });
    let run = move |go: Go| {
        *PALETTE.write() = false;
        match go {
            Go::To(route) => {
                nav.push(route);
            }
            Go::Theme(t) => choose_theme(t),
            Go::Canvas => {
                state::set_mode(state::UiMode::Canvas);
                nav.push(Route::Canvas {});
            }
        }
    };
    let chosen = entries.get(sel).map(|e| e.go.clone());
    let short = util::keys("Strg+K");
    rsx! {
        div { class: "fixed inset-0 z-[70] bg-black/45", onclick: move |_| *PALETTE.write() = false }
        div {
            class: "lu-pane fixed left-1/2 top-[13vh] z-[71] flex max-h-[70vh] w-[min(42rem,calc(100vw-1.5rem))] -translate-x-1/2 flex-col overflow-hidden rounded-2xl border shadow-2xl animate-in",
            role: "dialog",
            "aria-label": tr("Suchen und Befehle"),
            div { class: "flex h-13 shrink-0 items-center gap-3 border-b px-4 [&_svg]:size-[18px]",
                if searching() {
                    icons::LoaderCircle { class: "animate-spin text-muted-foreground" }
                } else {
                    icons::Search { class: "text-muted-foreground" }
                }
                input {
                    class: "h-13 min-w-0 flex-1 bg-transparent text-[15px] outline-none placeholder:text-muted-foreground",
                    placeholder: tr("Chats, Nachrichten, Projekte und Befehle durchsuchen …"),
                    value: "{query}",
                    spellcheck: "false",
                    onmounted: move |e| async move {
                        let _ = e.set_focus(true).await;
                    },
                    oninput: move |e| {
                        query.set(e.value());
                        selected.set(0);
                    },
                    onkeydown: move |e| match e.key() {
                        Key::ArrowDown => {
                            e.prevent_default();
                            selected.set(if sel + 1 >= count { 0 } else { sel + 1 });
                        }
                        Key::ArrowUp => {
                            e.prevent_default();
                            selected.set(if sel == 0 { count.saturating_sub(1) } else { sel - 1 });
                        }
                        Key::Enter => {
                            e.prevent_default();
                            if let Some(go) = chosen.clone() {
                                run(go);
                            }
                        }
                        Key::Escape => *PALETTE.write() = false,
                        _ => {}
                    },
                }
                kbd { class: "rounded border bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground", "Esc" }
            }
            div { class: "min-h-0 flex-1 overflow-y-auto p-1.5",
                if count == 0 {
                    p { class: "px-3 py-8 text-center text-sm text-muted-foreground",
                        if searching() { {tr("Sucht …")} } else { {trf("Nichts gefunden für „{0}“.", &[&query().trim()])} }
                    }
                }
                for (i, e) in entries.iter().cloned().enumerate() {
                    if i == 0 || entries[i - 1].group != e.group {
                        p { class: "px-3 pt-2.5 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase", "{e.group}" }
                    }
                    button {
                        id: "lu-pal-{i}",
                        class: if i == sel { "flex w-full items-center gap-3 rounded-lg bg-accent px-3 py-2 text-left text-sm [&_svg]:size-4 [&_svg]:shrink-0" } else { "flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm [&_svg]:size-4 [&_svg]:shrink-0" },
                        onmousemove: move |_| {
                            if *selected.peek() != i {
                                selected.set(i);
                            }
                        },
                        onclick: {
                            let go = e.go.clone();
                            move |_| run(go.clone())
                        },
                        span { class: if i == sel { "text-primary" } else { "text-muted-foreground" }, {ico(e.icon)} }
                        span { class: "min-w-0 flex-1",
                            span { class: if e.icon == Ico::Message { "line-clamp-2 text-[13px] leading-snug" } else { "block truncate" }, "{e.title}" }
                            if !e.sub.is_empty() {
                                span { class: "block truncate text-xs text-muted-foreground", "{e.sub}" }
                            }
                        }
                        if !e.right.is_empty() {
                            span { class: "shrink-0 text-xs tabular-nums text-muted-foreground", "{e.right}" }
                        }
                    }
                }
            }
            div { class: "flex shrink-0 items-center gap-4 border-t px-4 py-2 text-[11px] text-muted-foreground",
                span { "↑↓ " {tr("wählen")} }
                span { "↵ " {tr("öffnen")} }
                span { class: "flex-1" }
                span { "{short} " {tr("öffnet diese Suche überall")} }
            }
        }
    }
}

//! Umzug von nrzCode (Einstellungen → Umzug, nur Administratoren): Die Seite findet den
//! Datenordner von nrzCode, zeigt, was dort liegt und was davon noch fehlt, lässt wählen, was
//! mitkommt, und zeigt den Fortschritt. Ist der Umzug fertig, lädt die Oberfläche Benutzer,
//! Projekte, Threads, Agent-Konten und Bots von selbst neu ([`watch_import`]). Auf der Startseite
//! weist [`ImportBanner`] einmal darauf hin, solange noch nichts übernommen wurde.

use crate::Route;
use crate::api;
use crate::components::common::*;
use crate::components::overlay::*;
use crate::i18n::{tr, trf};
use crate::state::ME;
use crate::util;
use dioxus::prelude::*;
use lucode_shared::import::{ImportCounts, ImportOptions, ImportSource, ImportStatus};

/// Hinweis auf der Startseite ausgeblendet („Später“)?
const DISMISSED: &str = "lu-import-dismissed";

async fn load_source(path: &str) -> Option<ImportSource> {
    let url = if path.trim().is_empty() { "/import/nrzcode".to_string() } else { format!("/import/nrzcode?path={}", api::enc(path.trim())) };
    api::get::<ImportSource>(&url).await.ok()
}

/// Wartet schon jemand auf das Ende des laufenden Umzugs? (Einer genügt.)
static WATCHING: GlobalSignal<bool> = Signal::global(|| false);

/// Auf das Ende des laufenden Umzugs warten und die Oberfläche dann auf den neuen Stand bringen:
/// Benutzer mit seinen Git-Verbindungen, Projekte, Threads, Agent-Konten und Bots. Läuft im
/// Wurzel-Scope – auch wenn die Einstellungen inzwischen geschlossen sind. Ohne das kannte die
/// Oberfläche das Übernommene erst nach einem Neuladen (die Startseite bot weiter „verbinden“
/// an, die Listen blieben leer).
fn watch_import() {
    if *WATCHING.peek() {
        return;
    }
    *WATCHING.write() = true;
    dioxus::core::spawn_forever(async {
        loop {
            gloo_timers::future::TimeoutFuture::new(700).await;
            match api::get::<ImportStatus>("/import/nrzcode/status").await {
                Ok(s) if s.is_running() => continue,
                _ => break,
            }
        }
        crate::state::load_me().await;
        super::parts::load_projects().await;
        super::parts::load_threads().await;
        crate::pages::agents::data::reload_agents().await;
        crate::state::load_bots().await;
        *WATCHING.write() = false;
    });
}

/// Zahlen als Kacheln; `only_set`: Nullen weglassen.
#[component]
fn Counts(counts: ImportCounts, #[props(default)] only_set: bool) -> Element {
    let items = [
        (counts.projects, tr("Projekte")),
        (counts.threads, tr("Chats")),
        (counts.messages, tr("Nachrichten")),
        (counts.accounts, tr("Agent-Konten")),
        (counts.connections, tr("Git-Verbindungen")),
        (counts.bots, tr("Bots")),
        (counts.skills, tr("Skills")),
        (counts.notes, tr("Notizen")),
        (counts.boards, tr("Canvas-Flächen")),
        (counts.attachments, tr("Anhänge")),
        (counts.users, tr("Benutzer")),
    ];
    rsx! {
        div { class: "grid grid-cols-2 gap-2 sm:grid-cols-4",
            for (n, label) in items.into_iter().filter(|(n, _)| !only_set || *n > 0) {
                div { class: "rounded-lg border bg-surface px-3 py-2",
                    p { class: "text-lg font-semibold tabular-nums leading-tight", {util::number(n)} }
                    p { class: "truncate text-xs text-muted-foreground", "{label}" }
                }
            }
        }
    }
}

/// Einstellungen → Umzug.
#[component]
pub fn ImportSection() -> Element {
    let mut source = use_signal(|| None::<ImportSource>);
    let mut path = use_signal(String::new);
    let mut opts = use_signal(ImportOptions::default);
    let mut status = use_signal(ImportStatus::default);
    let mut searching = use_signal(|| false);
    // Quelle an den üblichen Orten suchen; läuft schon ein Import, seinen Stand zeigen
    use_future(move || async move {
        if let Ok(s) = api::get::<ImportStatus>("/import/nrzcode/status").await {
            if s.is_running() {
                watch_import();
            }
            status.set(s);
        }
        let found = load_source("").await;
        if let Some(f) = &found {
            path.set(f.path.clone());
        }
        source.set(Some(found.unwrap_or_default()));
    });
    // Solange ein Import läuft: Stand jede halbe Sekunde holen; danach die Quelle neu zählen
    use_future(move || async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(500).await;
            if !status.peek().is_running() {
                continue;
            }
            if let Ok(s) = api::get::<ImportStatus>("/import/nrzcode/status").await {
                let finished = !s.is_running();
                status.set(s);
                if finished {
                    let p = path.peek().clone();
                    if let Some(f) = load_source(&p).await {
                        source.set(Some(f));
                    }
                }
            }
        }
    });
    let search = move |e: FormEvent| {
        e.prevent_default();
        searching.set(true);
        spawn(async move {
            let p = path.peek().clone();
            source.set(Some(load_source(&p).await.unwrap_or_default()));
            searching.set(false);
        });
    };
    let start = move |_| {
        let mut o = opts.peek().clone();
        o.path = path.peek().trim().to_string();
        spawn(async move {
            match api::post::<ImportStatus, _>("/import/nrzcode", &o).await {
                Ok(s) => {
                    // Am Ende von selbst zeigen, was dazugekommen ist
                    watch_import();
                    status.set(s)
                }
                Err(e) => toast_error(e),
            }
        });
    };
    let st = status();
    let src = source();
    let o = opts();
    let running = st.is_running();
    let percent = if st.total > 0 { (st.done * 100 / st.total).clamp(0, 100) } else { 0 };
    rsx! {
        Section {
            title: tr("Von nrzCode umziehen"),
            description: tr("luCode übernimmt alles aus nrzCode: Agent-Konten samt Anmeldung, Git-Verbindungen, Projekte mit ihren Checkouts und Worktrees, Chats mit Verlauf, Bots, Skills, Notizen, Canvas, KI-Ketten und Werkzeuge. nrzCode bleibt dabei unberührt – du kannst beide weiter benutzen und den Umzug jederzeit wiederholen: Schon Übernommenes wird erkannt, nur Neues kommt dazu."),
            div { class: "grid gap-5",
                form { class: "flex max-w-2xl flex-wrap items-end gap-2", onsubmit: search,
                    Field { label: tr("Datenordner von nrzCode"), hint: tr("Der Ordner mit der Datei nrzcode.db – unter Windows meist %APPDATA%\\nrzCode, am Mac ~/Library/Application Support/nrzCode."), class: "min-w-64 flex-1",
                        input {
                            class: "h-9 w-full rounded-md border border-input bg-transparent px-3 font-mono text-[13px] outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50",
                            value: "{path}",
                            placeholder: tr("wird automatisch gesucht"),
                            disabled: running,
                            oninput: move |e| path.set(e.value()),
                        }
                    }
                    button { r#type: "submit", class: "mb-[1.35rem] inline-flex h-9 items-center gap-1.5 rounded-md border px-3 text-sm hover:bg-accent disabled:opacity-50 [&_svg]:size-4", disabled: running || searching(),
                        icons::Search {}
                        {tr("Suchen")}
                    }
                }
                match src {
                    None => rsx! { Loading {} },
                    Some(s) if !s.found => rsx! {
                        Callout { text: if s.error.is_empty() { tr("Kein nrzCode gefunden. Trage oben den Datenordner ein und klicke auf „Suchen“.") } else { tr(&s.error) }, error: !s.error.is_empty() }
                    },
                    Some(s) => rsx! {
                        div { class: "grid gap-3",
                            div { class: "flex flex-wrap items-center gap-x-3 gap-y-1 text-sm",
                                span { class: "inline-flex items-center gap-1.5 font-medium text-success [&_svg]:size-4", icons::CircleCheck {} {tr("nrzCode gefunden")} }
                                if !s.users.is_empty() {
                                    span { class: "text-muted-foreground", {trf("Benutzer: {0}", &[&s.users.join(", ")])} }
                                }
                                if s.last_import_at > 0 {
                                    span { class: "text-muted-foreground", {trf("zuletzt übernommen {0}", &[&util::ago(s.last_import_at)])} }
                                }
                            }
                            Counts { counts: s.counts }
                            if s.last_import_at > 0 {
                                if s.pending.content() > 0 {
                                    p { class: "text-sm font-medium", {tr("Seit dem letzten Umzug neu in nrzCode:")} }
                                    Counts { counts: s.pending, only_set: true }
                                } else {
                                    p { class: "text-sm text-muted-foreground", {tr("Alles aus nrzCode ist schon hier.")} }
                                }
                            }
                        }
                        div { class: "grid max-w-2xl gap-3 border-t pt-5",
                            p { class: "text-sm font-medium", {tr("Was soll mitkommen?")} }
                            Checkbox { checked: o.accounts, onchange: move |v| opts.write().accounts = v, label: tr("Agent-Konten samt Anmeldung"), hint: tr("Claude Code, Codex, Gemini … mit ihren Home-Ordnern – du musst dich nicht neu anmelden. Nutzt du dasselbe Abo weiter in nrzCode, kann der Anbieter eine der beiden Anmeldungen später abmelden; dann dort einfach neu anmelden.") }
                            Checkbox { checked: o.connections, onchange: move |v| opts.write().connections = v, label: tr("Git-Verbindungen"), hint: tr("Die in nrzCode verbundenen Git-Plattformen mit ihren Zugriffstokens.") }
                            Checkbox { checked: o.workspaces, onchange: move |v| opts.write().workspaces = v, label: tr("Projekte und Chats"), hint: tr("Checkouts mit allem Uneingecheckten, Worktrees der Subagenten, Verlauf, Anhänge, Nutzung. Die Sitzungen von Claude Code ziehen mit – Chats lassen sich nahtlos fortsetzen.") }
                            Checkbox { checked: o.extras, onchange: move |v| opts.write().extras = v, label: tr("Bots, Skills, Notizen, Canvas, KI-Ketten, Sprachsteuerung") }
                            Checkbox { checked: o.tools, onchange: move |v| opts.write().tools = v, label: tr("Werkzeuge und Sprachmodelle"), hint: tr("Die in nrzCode installierten Agent-CLIs und Whisper-Modelle – sonst lädt luCode sie bei Bedarf neu.") }
                            Checkbox { checked: o.skip_build_dirs, onchange: move |v| opts.write().skip_build_dirs = v, label: tr("Build-Ordner auslassen"), hint: tr("node_modules, target von Cargo und ähnliche Zwischenstände bleiben in nrzCode – das spart viel Platz und Zeit, der nächste Build legt sie neu an.") }
                        }
                        div { class: "flex flex-wrap items-center gap-3",
                            button {
                                class: "inline-flex h-10 items-center gap-2 rounded-lg bg-primary px-5 text-sm font-semibold text-primary-foreground disabled:opacity-50 [&_svg]:size-4",
                                disabled: running,
                                onclick: start,
                                if running {
                                    icons::LoaderCircle { class: "animate-spin" }
                                } else {
                                    icons::Download {}
                                }
                                if s.last_import_at > 0 { {tr("Neues nachholen")} } else { {tr("Alles übernehmen")} }
                            }
                            p { class: "text-xs text-muted-foreground", {tr("Am besten läuft in nrzCode gerade kein Agent – was dort noch arbeitet, kommt als angehalten an.")} }
                        }
                    },
                }
                if st.state != "idle" && !st.state.is_empty() {
                    Progress { status: st.clone(), percent }
                }
            }
        }
    }
}

/// Fortschritt, Ergebnis und Hinweise eines Imports.
#[component]
fn Progress(status: ImportStatus, percent: i64) -> Element {
    let running = status.is_running();
    let done = status.state == "done";
    rsx! {
        div { class: "grid gap-3 rounded-xl border bg-surface p-4",
            div { class: "flex flex-wrap items-center justify-between gap-2 text-sm",
                span { class: "inline-flex min-w-0 items-center gap-2 font-medium [&_svg]:size-4",
                    if running {
                        icons::LoaderCircle { class: "animate-spin text-primary" }
                    } else if done {
                        icons::CircleCheck { class: "text-success" }
                    } else {
                        icons::CircleAlert { class: "text-destructive" }
                    }
                    span { class: "truncate",
                        if done { {tr("Umzug abgeschlossen")} } else if running { {tr(&status.step)} } else { {tr("Umzug abgebrochen")} }
                        if running && !status.detail.is_empty() {
                            span { class: "font-normal text-muted-foreground", " · {status.detail}" }
                        }
                    }
                }
                span { class: "text-xs tabular-nums text-muted-foreground",
                    {trf("{0} Dateien · {1}", &[&util::number(status.files), &util::size(status.bytes)])}
                }
            }
            div { class: "h-2 overflow-hidden rounded-full bg-muted",
                div { class: if done { "h-full rounded-full bg-success transition-[width] duration-300" } else { "h-full rounded-full bg-primary transition-[width] duration-300" }, style: "width: {percent}%" }
            }
            if !status.error.is_empty() {
                Callout { text: tr(&status.error), error: true }
            }
            if done {
                if status.imported.is_empty() {
                    p { class: "text-sm text-muted-foreground", {tr("Es gab nichts Neues – alles war schon hier.")} }
                } else {
                    p { class: "text-sm text-muted-foreground", {tr("Neu in luCode:")} }
                    Counts { counts: status.imported, only_set: true }
                }
                div {
                    button {
                        class: "inline-flex h-9 items-center gap-1.5 rounded-md border px-3 text-sm hover:bg-accent [&_svg]:size-4",
                        // Projekte und Chats neu laden: die Listen der Oberfläche kennen das Übernommene noch nicht
                        onclick: move |_| {
                            if let Some(w) = web_sys::window() {
                                let _ = w.location().set_href("/");
                            }
                        },
                        icons::House {}
                        {tr("Zur Startseite – alles ist da")}
                    }
                }
            }
            if !status.notes.is_empty() {
                details { class: "text-xs text-muted-foreground",
                    summary { class: "cursor-pointer select-none", {trf("{0} Hinweise", &[&status.notes.len()])} }
                    ul { class: "mt-2 grid list-disc gap-1 pl-5",
                        for n in status.notes.iter() {
                            li { class: "break-words", "{n}" }
                        }
                    }
                }
            }
        }
    }
}

/// Startseite: einmaliger Hinweis für Administratoren, wenn ein nrzCode gefunden wurde und noch
/// nichts übernommen ist. „Später“ blendet ihn in diesem Browser aus.
#[component]
pub fn ImportBanner() -> Element {
    let admin = ME.read().as_ref().is_some_and(|m| m.is_admin);
    let mut source = use_signal(|| None::<ImportSource>);
    let mut hidden = use_signal(|| util::storage_get(DISMISSED).is_some());
    use_future(move || async move {
        if admin && !*hidden.peek() {
            source.set(load_source("").await);
        }
    });
    let Some(s) = source().filter(|s| admin && !hidden() && s.found && s.last_import_at == 0 && s.counts.content() > 0) else {
        return rsx! {};
    };
    rsx! {
        div { class: "lu-glass-panel flex flex-wrap items-center gap-4 rounded-2xl border border-ring/40 px-5 py-4",
            div { class: "grid size-10 shrink-0 place-items-center rounded-xl bg-primary text-primary-foreground [&_svg]:size-5", icons::Sparkles {} }
            div { class: "min-w-0 flex-1",
                p { class: "font-semibold", {tr("nrzCode gefunden – alles nach luCode holen?")} }
                p { class: "mt-0.5 text-sm text-muted-foreground",
                    {trf("{0} Projekte, {1} Chats und {2} Agent-Konten warten – samt Anmeldungen, Checkouts und Verlauf. nrzCode bleibt, wie es ist.", &[&s.counts.projects, &s.counts.threads, &s.counts.accounts])}
                }
            }
            div { class: "flex shrink-0 items-center gap-2",
                button {
                    class: "inline-flex h-9 items-center rounded-md px-3 text-sm text-muted-foreground hover:bg-accent hover:text-foreground",
                    onclick: move |_| {
                        util::storage_set(DISMISSED, "1");
                        hidden.set(true);
                    },
                    {tr("Später")}
                }
                Link { to: Route::Settings { section: "import".to_string() }, class: "inline-flex h-9 items-center gap-1.5 rounded-md bg-primary px-4 text-sm font-semibold text-primary-foreground [&_svg]:size-4",
                    icons::Download {}
                    {tr("Jetzt umziehen")}
                }
            }
        }
    }
}

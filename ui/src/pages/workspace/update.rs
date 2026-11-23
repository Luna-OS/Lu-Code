//! Updates der App unten in der Seitenleiste (nur in der App). „Nach Updates suchen“ fragt die
//! App ohne Dialog (`update:check:quiet`), zeigt währenddessen „Suche nach Updates …“, danach
//! kurz „luCode ist aktuell“ und verschwindet dann (bis zum nächsten Start).
//!
//! Findet die App ein Update – auch bei ihrer eigenen, regelmässigen Prüfung –, steht dort je
//! nach Einstellung:
//! * automatisch (Einstellungen → App, ab Werk an): „Update … wird geladen …“ mit Prozent, dann
//!   „Update … bereit – neu starten“. Die App installiert es von selbst, sobald kein Agent
//!   arbeitet und niemand tippt, sonst beim Beenden oder beim nächsten Start; ein Klick
//!   installiert sofort (arbeiten noch Agenten, erst nach einer Rückfrage).
//! * sonst: „Update auf … installieren“.
//!
//! Die App meldet ihren Stand über `window.__luUpdateState(status, text)`: `available`,
//! `downloading`, `ready`, `installing` (je mit der Version), `progress` (Prozent, -1 =
//! unbekannt), `current` (eigene Version) oder `error` (Text); `update:state` fragt danach (nach
//! dem Laden der Oberfläche). `window.__luUpdateAuto(an, möglich)` meldet die Einstellung.
//! Vor der Installation (`installing`) merkt sich die Oberfläche die offene Seite (`lu-resume`)
//! und öffnet sie nach dem Neustart wieder ([`RESUME`], eingelöst im Rahmen der App).

use super::layout::FOOT_ROW;
use crate::components::overlay::{confirm, Ask};
use crate::i18n::{tr, trf};
use crate::state::THREADS;
use crate::util;
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Debug)]
pub enum UpdateUi {
    /// Noch nicht geprüft: „Nach Updates suchen“.
    Idle,
    Checking,
    /// Geprüft, nichts Neues: kurz zeigen, dann `Done`.
    Current,
    Available(String),
    /// Wird still geladen (automatische Updates): Version und Prozent (-1 = unbekannt).
    Downloading(String, i32),
    /// Geladen und geprüft: wird in einem ruhigen Moment von selbst installiert.
    Ready(String),
    /// Wird gerade eingesetzt – die App startet gleich neu.
    Installing(String),
    Error(String),
    /// Ausgeblendet.
    Done,
}

pub static UPDATE: GlobalSignal<UpdateUi> = Signal::global(|| UpdateUi::Idle);

/// Automatische Updates: (eingeschaltet, hier möglich). `None`, solange die App nichts gemeldet hat.
pub static AUTO: GlobalSignal<Option<(bool, bool)>> = Signal::global(|| None);

/// Seite, die nach einem Neustart durch ein Update wieder geöffnet werden soll (einmal).
pub static RESUME: GlobalSignal<Option<String>> = Signal::global(|| None);

const RESUME_KEY: &str = "lu-resume";
/// So lange nach der Installation gilt die gemerkte Seite noch (Sekunden).
const RESUME_WITHIN: i64 = 180;

/// Rückmeldungen der App annehmen, nach ihrem Stand fragen und – nach einem Neustart durch ein
/// Update – die Seite von vorhin vormerken.
pub fn install() {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    if !util::in_app() {
        return;
    }
    let Some(w) = web_sys::window() else { return };
    if let Some(saved) = util::storage_get(RESUME_KEY) {
        util::storage_remove(RESUME_KEY);
        if let Some((at, path)) = saved.split_once('|') {
            if at.parse::<i64>().is_ok_and(|t| util::now() - t < RESUME_WITHIN) && path.starts_with('/') {
                *RESUME.write() = Some(path.to_string());
            }
        }
    }
    let cb = Closure::<dyn FnMut(String, String)>::new(|status: String, text: String| {
        let current = UPDATE.peek().clone();
        let next = match status.as_str() {
            "available" => UpdateUi::Available(text),
            // Prozent behalten, wenn die App denselben Download noch einmal meldet
            "downloading" => match current {
                UpdateUi::Downloading(ref v, p) if *v == text => UpdateUi::Downloading(text, p),
                _ => UpdateUi::Downloading(text, -1),
            },
            "progress" => match current {
                UpdateUi::Downloading(ref v, _) => UpdateUi::Downloading(v.clone(), text.parse().unwrap_or(-1)),
                _ => return,
            },
            "ready" => UpdateUi::Ready(text),
            "installing" => {
                // Gleich startet die App neu: die offene Seite soll danach wieder dastehen
                util::storage_set(RESUME_KEY, &format!("{}|{}", util::now(), util::current_path()));
                UpdateUi::Installing(text)
            }
            "current" => UpdateUi::Current,
            "error" => UpdateUi::Error(text),
            _ => return,
        };
        if current != next {
            *UPDATE.write() = next;
        }
    });
    let _ = js_sys::Reflect::set(&w, &"__luUpdateState".into(), cb.as_ref().unchecked_ref());
    cb.forget();
    let auto = Closure::<dyn FnMut(bool, bool)>::new(|on: bool, possible: bool| {
        if *AUTO.peek() != Some((on, possible)) {
            *AUTO.write() = Some((on, possible));
        }
    });
    let _ = js_sys::Reflect::set(&w, &"__luUpdateAuto".into(), auto.as_ref().unchecked_ref());
    auto.forget();
    util::native("update:state");
}

/// Jetzt prüfen (ohne Dialog der App); ohne Antwort nach 40 s als Fehler.
fn check() {
    *UPDATE.write() = UpdateUi::Checking;
    if !util::native("update:check:quiet") {
        *UPDATE.write() = UpdateUi::Error(tr("Die App antwortet nicht."));
        return;
    }
    spawn(async {
        gloo_timers::future::TimeoutFuture::new(40_000).await;
        if *UPDATE.peek() == UpdateUi::Checking {
            *UPDATE.write() = UpdateUi::Error(tr("Keine Antwort vom Update-Server."));
        }
    });
}

/// Jetzt installieren und neu starten. Arbeiten noch Agenten, erst nachfragen – der Neustart
/// stoppt sie.
fn install_now() {
    let working = THREADS.peek().values().filter(|t| t.status.is_active()).count();
    spawn(async move {
        if working > 0 {
            let text = if working == 1 {
                tr("Ein Agent arbeitet noch – der Neustart stoppt ihn. Ohne Klick installiert luCode das Update, sobald er fertig ist.")
            } else {
                trf("{0} Agenten arbeiten noch – der Neustart stoppt sie. Ohne Klick installiert luCode das Update, sobald sie fertig sind.", &[&working])
            };
            if !confirm(Ask::new(tr("Jetzt aktualisieren?"), text).confirm(tr("Aktualisieren und neu starten"))).await {
                return;
            }
        }
        util::native("update:install");
    });
}

/// Zeile über „Einstellungen“ – nur in der App und bis nach der Prüfung.
#[component]
pub fn UpdateRow() -> Element {
    // „Aktuell“ kurz zeigen, dann ausblenden
    use_effect(move || {
        if UPDATE() == UpdateUi::Current {
            spawn(async {
                gloo_timers::future::TimeoutFuture::new(2_500).await;
                if *UPDATE.peek() == UpdateUi::Current {
                    *UPDATE.write() = UpdateUi::Done;
                }
            });
        }
    });
    if !util::in_app() {
        return rsx! {};
    }
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let side = "shrink-0 text-xs tabular-nums text-muted-foreground/70";
    let still = "cursor-default hover:bg-transparent";
    match UPDATE() {
        UpdateUi::Done => rsx! {},
        UpdateUi::Idle => rsx! {
            button { class: FOOT_ROW, onclick: move |_| check(),
                icons::RefreshCw {}
                span { class: "min-w-0 flex-1 truncate", {tr("Nach Updates suchen")} }
                span { class: side, "{version}" }
            }
        },
        UpdateUi::Checking => rsx! {
            div { class: "{FOOT_ROW} {still} hover:text-muted-foreground",
                icons::RefreshCw { class: "animate-spin" }
                span { class: "min-w-0 flex-1 truncate", {tr("Suche nach Updates …")} }
                span { class: side, "{version}" }
            }
        },
        UpdateUi::Current => rsx! {
            div { class: "{FOOT_ROW} {still} lu-fade-out hover:text-muted-foreground",
                icons::Check { class: "text-success" }
                span { class: "min-w-0 flex-1 truncate", {tr("luCode ist aktuell")} }
                span { class: side, "{version}" }
            }
        },
        UpdateUi::Available(v) => rsx! {
            button {
                class: "{FOOT_ROW} text-info hover:text-info",
                title: tr("Lädt das Update und startet luCode neu."),
                onclick: move |_| install_now(),
                icons::CircleArrowDown {}
                span { class: "min-w-0 flex-1 truncate font-medium", {trf("Update auf {0} installieren", &[&v])} }
            }
        },
        UpdateUi::Downloading(v, percent) => rsx! {
            div {
                class: "{FOOT_ROW} {still} hover:text-muted-foreground",
                title: tr("Wird im Hintergrund geladen und von selbst installiert, sobald kein Agent arbeitet."),
                icons::CircleArrowDown { class: "animate-pulse text-info" }
                span { class: "min-w-0 flex-1 truncate", {trf("Update {0} wird geladen …", &[&v])} }
                if percent >= 0 {
                    span { class: side, "{percent} %" }
                }
            }
        },
        UpdateUi::Ready(v) => rsx! {
            button {
                class: "{FOOT_ROW} text-info hover:text-info",
                title: tr("Geladen und geprüft. luCode installiert es von selbst, sobald kein Agent arbeitet und du gerade nichts eingibst – oder jetzt mit einem Klick."),
                onclick: move |_| install_now(),
                icons::CircleArrowDown {}
                span { class: "min-w-0 flex-1 truncate font-medium", {trf("Update {0} bereit – neu starten", &[&v])} }
            }
        },
        UpdateUi::Installing(v) => rsx! {
            div { class: "{FOOT_ROW} {still} text-info hover:text-info",
                icons::LoaderCircle { class: "animate-spin" }
                span { class: "min-w-0 flex-1 truncate", {trf("Update {0} wird installiert …", &[&v])} }
            }
        },
        UpdateUi::Error(e) => rsx! {
            button { class: "{FOOT_ROW} text-destructive hover:text-destructive", title: "{e}", onclick: move |_| check(),
                icons::CircleAlert {}
                span { class: "min-w-0 flex-1 truncate", {tr("Prüfung fehlgeschlagen – erneut")} }
                span { class: side, "{version}" }
            }
        },
    }
}

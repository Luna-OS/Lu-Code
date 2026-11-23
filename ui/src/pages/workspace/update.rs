//! „Nach Updates suchen“ unten in der Seitenleiste (nur in der App): fragt die App ohne Dialog
//! (`update:check:quiet`), zeigt währenddessen „Suche nach Updates …“, danach kurz „luCode ist
//! aktuell“ und verschwindet dann (bis zum nächsten Start). Findet die App ein Update – auch bei
//! ihrer eigenen, regelmässigen Prüfung –, steht dort „Update auf … installieren“.
//!
//! Die App meldet Ergebnisse über `window.__luUpdateState(status, text)`: `available` (Version),
//! `current` (eigene Version) oder `error` (Text); `update:state` fragt nach einem schon
//! gefundenen Update (nach dem Neuladen der Oberfläche).

use super::layout::FOOT_ROW;
use crate::i18n::{tr, trf};
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
    Error(String),
    /// Ausgeblendet.
    Done,
}

pub static UPDATE: GlobalSignal<UpdateUi> = Signal::global(|| UpdateUi::Idle);

/// Rückmeldungen der App annehmen und nach einem schon gefundenen Update fragen.
pub fn install() {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    if !util::in_app() {
        return;
    }
    let Some(w) = web_sys::window() else { return };
    let cb = Closure::<dyn FnMut(String, String)>::new(|status: String, text: String| {
        let next = match status.as_str() {
            "available" => UpdateUi::Available(text),
            "current" => UpdateUi::Current,
            "error" => UpdateUi::Error(text),
            _ => return,
        };
        *UPDATE.write() = next;
    });
    let _ = js_sys::Reflect::set(&w, &"__luUpdateState".into(), cb.as_ref().unchecked_ref());
    cb.forget();
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
            div { class: "{FOOT_ROW} cursor-default hover:bg-transparent hover:text-muted-foreground",
                icons::RefreshCw { class: "animate-spin" }
                span { class: "min-w-0 flex-1 truncate", {tr("Suche nach Updates …")} }
                span { class: side, "{version}" }
            }
        },
        UpdateUi::Current => rsx! {
            div { class: "{FOOT_ROW} lu-fade-out cursor-default hover:bg-transparent hover:text-muted-foreground",
                icons::Check { class: "text-success" }
                span { class: "min-w-0 flex-1 truncate", {tr("luCode ist aktuell")} }
                span { class: side, "{version}" }
            }
        },
        UpdateUi::Available(v) => rsx! {
            button {
                class: "{FOOT_ROW} text-info hover:text-info",
                title: tr("Lädt das Update und startet luCode neu."),
                onclick: move |_| {
                    util::native("update:install");
                },
                icons::CircleArrowDown {}
                span { class: "min-w-0 flex-1 truncate font-medium", {trf("Update auf {0} installieren", &[&v])} }
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

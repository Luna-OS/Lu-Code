//! Eigene Anweisungen an die Agenten (Einstellungen → Anweisungen, und je Projekt auf der
//! Projektseite) und Sicherungen der Datenbank (Einstellungen → Instanz).

use crate::api;
use crate::components::common::*;
use crate::components::overlay::*;
use crate::i18n::{tr, trf};
use crate::util;
use dioxus::prelude::*;
use lucode_shared::extras::{BackupList, BackupSettings, Instructions, MAX_INSTRUCTIONS};

const AREA: &str = "min-h-40 w-full resize-y rounded-lg border border-input bg-transparent px-3 py-2.5 text-sm leading-relaxed outline-none placeholder:text-muted-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50";

/// Textfeld mit Speichern für eine Anweisung unter `path` (`/instructions` bzw.
/// `/projects/<id>/instructions`).
#[component]
fn InstructionsEditor(path: String, #[props(into)] placeholder: String) -> Element {
    let mut text = use_signal(String::new);
    let mut saved = use_signal(String::new);
    let mut loaded = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let load_path = path.clone();
    use_future(move || {
        let path = load_path.clone();
        async move {
            if let Ok(i) = api::get::<Instructions>(&path).await {
                text.set(i.text.clone());
                saved.set(i.text);
            }
            loaded.set(true);
        }
    });
    let save = move |_| {
        let path = path.clone();
        busy.set(true);
        spawn(async move {
            let body = Instructions { text: text.peek().clone() };
            match api::put::<Instructions, _>(&path, &body).await {
                Ok(i) => {
                    text.set(i.text.clone());
                    saved.set(i.text);
                    toast(tr("Gespeichert – gilt ab dem nächsten Lauf."));
                }
                Err(e) => toast_error(e),
            }
            busy.set(false);
        });
    };
    let len = text().chars().count();
    let changed = text() != saved();
    rsx! {
        div { class: "grid gap-3",
            textarea {
                class: AREA,
                placeholder: "{placeholder}",
                disabled: !loaded(),
                maxlength: "{MAX_INSTRUCTIONS}",
                value: "{text}",
                oninput: move |e| text.set(e.value()),
            }
            div { class: "flex items-center gap-3",
                button {
                    class: "inline-flex h-9 items-center gap-1.5 rounded-md bg-primary px-4 text-sm font-medium text-primary-foreground disabled:opacity-50 [&_svg]:size-4",
                    disabled: busy() || !changed,
                    onclick: save,
                    icons::Check {}
                    {tr("Speichern")}
                }
                if changed {
                    span { class: "text-xs text-warning", {tr("Noch nicht gespeichert")} }
                }
                span { class: "flex-1" }
                span { class: "text-xs tabular-nums text-muted-foreground", "{len} / {MAX_INSTRUCTIONS}" }
            }
        }
    }
}

/// Einstellungen → Anweisungen: gelten für jeden Agenten in jedem Chat.
#[component]
pub fn InstructionsSection() -> Element {
    rsx! {
        Section {
            title: tr("Eigene Anweisungen"),
            description: tr("Was jeder Agent bei jedem Lauf von dir wissen soll – egal ob Claude Code, Codex oder Gemini: Sprache, Stil, bevorzugte Werkzeuge, was er nie tun soll. Steht im Systemhinweis jedes Laufs, auch bei Subagenten und Bots."),
            InstructionsEditor {
                path: "/instructions",
                placeholder: tr("Zum Beispiel:\nAntworte auf Deutsch und duze mich.\nNutze pnpm statt npm.\nSchreibe Commit-Nachrichten auf Englisch.\nFrage nach, bevor du etwas löschst."),
            }
        }
        Section {
            title: tr("Je Projekt"),
            description: tr("Anweisungen, die nur in einem Projekt gelten (Architektur, Befehle zum Bauen und Testen, Stolpersteine), trägst du auf der Seite des Projekts ein. Sie gehen den allgemeinen vor."),
            p { class: "text-sm text-muted-foreground",
                {tr("Öffne ein Projekt (Seitenleiste → Projekt → Projektseite) und fülle dort „Anweisungen für dieses Projekt“ aus.")}
            }
        }
    }
}

/// Karte auf der Projektseite: Anweisungen für die Chats dieses Projekts.
#[component]
pub fn ProjectInstructions(id: i64) -> Element {
    rsx! {
        section { class: "rounded-xl border bg-card p-4 text-sm shadow-sm",
            p { class: "font-medium", {tr("Anweisungen für dieses Projekt")} }
            p { class: "mt-1 mb-3 text-xs text-muted-foreground", {tr("Bekommt jeder Agent in den Chats dieses Projekts mit – zusätzlich zu deinen eigenen Anweisungen.")} }
            InstructionsEditor {
                key: "{id}",
                path: format!("/projects/{id}/instructions"),
                placeholder: tr("Zum Beispiel:\nBauen: cargo build, Tests: cargo test.\nDie Oberfläche liegt in ui/ (Dioxus).\nNie direkt auf main pushen."),
            }
        }
    }
}

/// Einstellungen → Instanz: Sicherungen der Datenbank.
#[component]
pub fn BackupSection() -> Element {
    let mut list = use_signal(|| None::<BackupList>);
    let mut busy = use_signal(|| false);
    use_future(move || async move {
        if let Ok(l) = api::get::<BackupList>("/backups").await {
            list.set(Some(l));
        }
    });
    let create = move |_| {
        busy.set(true);
        spawn(async move {
            match api::action::<BackupList>("/backups").await {
                Ok(l) => {
                    list.set(Some(l));
                    toast(tr("Sicherung angelegt."));
                }
                Err(e) => toast_error(e),
            }
            busy.set(false);
        });
    };
    let set_auto = move |on: bool| {
        spawn(async move {
            match api::put::<BackupList, _>("/backups/settings", &BackupSettings { auto: on }).await {
                Ok(l) => list.set(Some(l)),
                Err(e) => toast_error(e),
            }
        });
    };
    let l = list();
    rsx! {
        Section {
            title: tr("Sicherungen"),
            description: tr("Ein Abbild der Datenbank mit allen Chats, Projekten, Konten und Einstellungen – in sich stimmig, auch während Agenten arbeiten. Die Dateien der Projekte liegen in ihren Repositories und gehören nicht dazu."),
            actions: rsx! {
                button {
                    class: "inline-flex h-9 items-center gap-1.5 rounded-md border px-3 text-sm hover:bg-accent disabled:opacity-50 [&_svg]:size-4",
                    disabled: busy(),
                    onclick: create,
                    if busy() {
                        icons::LoaderCircle { class: "animate-spin" }
                    } else {
                        icons::Save {}
                    }
                    {tr("Jetzt sichern")}
                }
            },
            match l {
                None => rsx! { Loading {} },
                Some(l) => rsx! {
                    div { class: "grid gap-4",
                        Toggle { checked: l.auto, onchange: set_auto, label: tr("Täglich von selbst sichern (die letzten sieben bleiben)") }
                        if l.backups.is_empty() {
                            p { class: "text-sm text-muted-foreground", {tr("Noch keine Sicherung.")} }
                        } else {
                            div { class: "divide-y rounded-lg border",
                                for b in l.backups.iter().cloned() {
                                    div { key: "{b.name}", class: "flex items-center gap-3 px-3 py-2 text-sm",
                                        icons::Database { class: "size-4 shrink-0 text-muted-foreground" }
                                        div { class: "min-w-0 flex-1",
                                            p { class: "truncate", {util::datetime(b.created_at)}
                                                if b.auto {
                                                    span { class: "ml-2 rounded-full bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground", {tr("automatisch")} }
                                                }
                                            }
                                            p { class: "truncate font-mono text-xs text-muted-foreground", "{b.name} · {util::size(b.size)}" }
                                        }
                                        a {
                                            class: "grid size-8 shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground [&_svg]:size-4",
                                            href: "/api/v1/backups/{b.name}/download",
                                            title: tr("Herunterladen"),
                                            icons::Download {}
                                        }
                                        button {
                                            class: "grid size-8 shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-destructive/15 hover:text-destructive [&_svg]:size-4",
                                            title: tr("Löschen"),
                                            onclick: {
                                                let name = b.name.clone();
                                                move |_| {
                                                    let name = name.clone();
                                                    spawn(async move {
                                                        match api::delete_json::<BackupList>(&format!("/backups/{name}")).await {
                                                            Ok(l) => list.set(Some(l)),
                                                            Err(e) => toast_error(e),
                                                        }
                                                    });
                                                }
                                            },
                                            icons::Trash2 {}
                                        }
                                    }
                                }
                            }
                        }
                        p { class: "text-xs text-muted-foreground", {trf("Ordner: {0}", &[&l.dir])} }
                    }
                },
            }
        }
    }
}

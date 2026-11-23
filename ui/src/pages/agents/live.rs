//! Live-Ansicht (`/live/<bereich>`): allen Agenten, die gerade arbeiten, nebeneinander zusehen –
//! je Agent eine Kachel mit seinem **Text** (die Antworten des laufenden Auftrags, wie sie
//! entstehen) und seinem **CLI** (Befehle mit Ausgabe, gelesene und bearbeitete Dateien, Suchen,
//! Werkzeuge – wie in einem Terminal). Umschalter Beides | Text | CLI (im Browser gemerkt).
//!
//! `bereich` = `all` (alle Threads des Benutzers) oder die Nummer eines Threads (er selbst und
//! seine Subagenten). Kacheln bleiben stehen, wenn ihr Agent fertig ist (bis „Fertige ausblenden“
//! oder zum Verlassen der Seite), und behalten ihre Reihenfolge – beim Zusehen springt nichts.
//! Arbeitet niemand, stehen die zuletzt aktiven Threads da.
//!
//! Die Daten kommen aus `state::THREADS` und `state::MESSAGES` (live über `events.rs`); jede
//! Kachel lädt den Verlauf ihres Threads bei Bedarf nach (`prefetch_messages`). Nur das Ende des
//! Verlaufs wird ausgewertet ([`cli_lines`], [`text_parts`]) und über `use_memo` verglichen: eine
//! Kachel zeichnet sich nur neu, wenn sich bei ihrem Agenten etwas getan hat.
//! [`ActivityLine`] zeigt dieselbe Auskunft einzeilig (Zeilen der Subagenten in Chat und Lineage),
//! [`LiveButton`] ist der Knopf in der Kopfzeile mit der Zahl der arbeitenden Agenten.

use super::chat::prefetch_messages;
use super::icons::AgentIcon;
use crate::api;
use crate::components::common::{ChangeStat, Loading};
use crate::components::overlay::toast_error;
use crate::components::widgets::Markdown;
use crate::i18n::{tr, trf};
use crate::pages::workspace::parts::{thread_title, ProjectBadge, ThreadState};
use crate::state::{self, MESSAGES, THREADS};
use crate::util;
use crate::Route;
use dioxus::prelude::*;
use lucode_shared::agents::{EditDiff, QuestionData, SubagentsData};
use serde::Deserialize;
use lucode_shared::{Message, ThreadStatus, ThreadSummary};
use std::collections::BTreeMap;

// ------------------------------------------------------------------ Ansicht

const VIEW_KEY: &str = "lu-live-view";

/// Was eine Kachel zeigt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Both,
    Text,
    Cli,
}

impl View {
    fn key(self) -> &'static str {
        match self {
            View::Both => "both",
            View::Text => "text",
            View::Cli => "cli",
        }
    }
}

static VIEW: GlobalSignal<View> = Signal::global(|| match util::storage_get(VIEW_KEY).as_deref() {
    Some("text") => View::Text,
    Some("cli") => View::Cli,
    _ => View::Both,
});

fn set_view(v: View) {
    util::storage_set(VIEW_KEY, v.key());
    if *VIEW.peek() != v {
        *VIEW.write() = v;
    }
}

// ------------------------------------------------------------------ Auswertung des Verlaufs

/// So viele Nachrichten vom Ende des Verlaufs werden ausgewertet.
const SCAN: usize = 80;
/// Höchstens so viele Zeilen im CLI einer Kachel.
const MAX_LINES: usize = 48;
/// Höchstens so viele Antworten im Text einer Kachel.
const MAX_TEXTS: usize = 6;

/// Art einer Zeile im CLI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    /// Auftrag des Benutzers (`›`)
    Prompt,
    /// Befehl (`$`) mit dem Ende seiner Ausgabe
    Command,
    /// Werkzeug: `read`, `edit`, `grep` …
    Tool,
    /// Antwort des Agenten (`●`) – nur wenn die Kachel allein das CLI zeigt
    Say,
    /// Hinweis: Subagenten gestartet, Rückfrage, Systemmeldung
    Note,
    /// Fehler
    Fail,
}

/// Eine Zeile im CLI eines Agenten.
#[derive(Clone, PartialEq, Debug)]
pub struct CliLine {
    /// Nachricht, aus der die Zeile stammt (Schlüssel)
    pub id: i64,
    pub kind: LineKind,
    /// Wort vorn bei Werkzeugen (`read`, `edit` …)
    pub verb: &'static str,
    pub text: String,
    /// Bearbeitungen: Zeilen hinzugefügt / entfernt
    pub adds: i64,
    pub dels: i64,
    pub running: bool,
    pub failed: bool,
    /// Ende der Ausgabe (Befehle) bzw. die Fehlermeldung eines Werkzeugs
    pub output: String,
    /// So viele Zeilen der Ausgabe stehen davor und werden nicht gezeigt
    pub hidden: usize,
}

impl CliLine {
    fn new(id: i64, kind: LineKind, text: String) -> Self {
        CliLine { id, kind, verb: "", text, adds: 0, dels: 0, running: false, failed: false, output: String::new(), hidden: 0 }
    }
}

/// Die ersten Zeilen eines Textes (ohne leere), auf `chars` Zeichen gekürzt.
fn first_lines(text: &str, lines: usize, chars: usize) -> String {
    let mut all = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty());
    let mut out = all.by_ref().take(lines).collect::<Vec<_>>().join("\n");
    let mut cut = all.next().is_some();
    if out.chars().count() > chars {
        out = out.chars().take(chars).collect();
        cut = true;
    }
    if cut {
        out.push('…');
    }
    out
}

/// Antwort als schlichter Text fürs CLI: ohne die Zeichen von Markdown (Überschriften, fett, Code).
fn plain(text: &str) -> String {
    text.lines().map(|l| l.trim_start_matches('#').trim_start()).collect::<Vec<_>>().join("\n").replace("**", "").replace('`', "")
}

/// Die letzten `n` Zeilen einer Ausgabe und wie viele davor stehen.
fn tail_lines(text: &str, n: usize) -> (String, usize) {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let hidden = lines.len().saturating_sub(n);
    let tail: Vec<String> = lines[hidden..].iter().map(|l| if l.chars().count() > 400 { l.chars().take(400).chain(std::iter::once('…')).collect() } else { l.to_string() }).collect();
    (tail.join("\n"), hidden)
}

/// Zeilen hinzugefügt und entfernt über alle Bearbeitungen eines Schritts.
fn diff_counts(diffs: &serde_json::Value) -> (i64, i64) {
    let Some(list) = diffs.as_array() else { return (0, 0) };
    list.iter().fold((0, 0), |(adds, dels), d| {
        let (a, r) = (d["adds"].as_i64().unwrap_or(0), d["dels"].as_i64().unwrap_or(0));
        // Ältere Nachrichten ohne Zahlen: aus dem alten und neuen Text zählen
        let (a, r) = if a == 0 && r == 0 { EditDiff::deserialize(d).map(|e| e.counts()).unwrap_or((0, 0)) } else { (a, r) };
        (adds + a, dels + r)
    })
}

/// Zeile eines Arbeitsschritts. Liest die Felder direkt aus `data` (Aufbau: `ToolCall`), ohne
/// sie zu kopieren – das läuft bei jedem gestreamten Stück für jede Kachel.
fn tool_line(m: &Message) -> CliLine {
    let d = &m.data;
    let field = |k: &str| d[k].as_str().unwrap_or_default();
    let target = if field("target").is_empty() { field("name") } else { field("target") };
    let failed = field("status") == "error";
    let mut l = match field("kind") {
        "command" => {
            let full = d["input"]["command"].as_str().or(d["input"]["cmd"].as_str()).filter(|c| !c.trim().is_empty()).unwrap_or(target);
            let mut l = CliLine::new(m.id, LineKind::Command, first_lines(full, 3, 400));
            (l.output, l.hidden) = tail_lines(field("output"), if failed { 10 } else { 6 });
            l
        }
        "think" => {
            let thought = first_lines(field("output"), 1, 160);
            let mut l = CliLine::new(m.id, LineKind::Tool, if thought.is_empty() { "…".into() } else { thought });
            l.verb = "think";
            l
        }
        kind => {
            let verb = match kind {
                "read" => "read",
                "edit" => "edit",
                "write" => "write",
                "search" => "grep",
                "web" => "web",
                "mcp" => "mcp",
                "task" => "task",
                "todo" => "todo",
                _ => "tool",
            };
            let text = if verb == "tool" && !field("summary").is_empty() { field("summary") } else { target };
            let mut l = CliLine::new(m.id, LineKind::Tool, first_lines(text, 1, 300));
            l.verb = verb;
            (l.adds, l.dels) = diff_counts(&d["diffs"]);
            if failed {
                l.output = first_lines(field("output"), 2, 240);
            }
            l
        }
    };
    l.running = field("status") == "running";
    l.failed = failed;
    l
}

/// Zeile einer Nachricht im CLI (`with_text`: auch die Antworten des Agenten).
fn cli_line(m: &Message, with_text: bool) -> Option<CliLine> {
    match m.role.as_str() {
        "tool" => Some(tool_line(m)),
        "user" => {
            let text = first_lines(&m.content, 2, 240);
            Some(CliLine::new(m.id, LineKind::Prompt, if text.is_empty() { tr("(Anhang)") } else { text }))
        }
        "assistant" => (with_text && !m.content.trim().is_empty()).then(|| CliLine::new(m.id, LineKind::Say, first_lines(&plain(&m.content), 4, 420))),
        "subagents" => {
            let d: SubagentsData = serde_json::from_value(m.data.clone()).unwrap_or_default();
            let n = d.children.len();
            let head = if n == 1 { tr("1 Subagent gestartet") } else { trf("{0} Subagenten gestartet", &[&n]) };
            let titles = d.titles.join(" · ");
            Some(CliLine::new(m.id, LineKind::Note, if titles.is_empty() { head } else { format!("{head}: {titles}") }))
        }
        "question" => Some(CliLine::new(m.id, LineKind::Note, format!("? {}", first_lines(&m.content, 2, 240)))),
        "error" => Some(CliLine::new(m.id, LineKind::Fail, first_lines(&m.content, 3, 300))),
        _ => {
            // Systemmeldungen: Text des Servers (deutsch) übersetzen; Rohausgaben des CLIs nicht zeigen
            if m.data.get("kind").and_then(|k| k.as_str()) == Some("raw") {
                return None;
            }
            let text = first_lines(&tr(&m.content), 2, 240);
            (!text.is_empty()).then(|| CliLine::new(m.id, LineKind::Note, text))
        }
    }
}

/// CLI eines Agenten: die letzten Schritte seines Verlaufs als Zeilen.
pub fn cli_lines(msgs: &[Message], with_text: bool) -> Vec<CliLine> {
    let from = msgs.len().saturating_sub(SCAN);
    let mut out: Vec<CliLine> = msgs[from..].iter().filter_map(|m| cli_line(m, with_text)).collect();
    let cut = out.len().saturating_sub(MAX_LINES);
    out.drain(..cut);
    out
}

/// Teil des Textes einer Kachel: der Auftrag (einfacher Text) oder eine Antwort (Markdown).
#[derive(Clone, PartialEq, Debug)]
pub struct TextPart {
    pub id: i64,
    pub user: bool,
    pub html: String,
    pub plain: String,
}

/// Text eines Agenten: der letzte Auftrag und die Antworten seither (höchstens [`MAX_TEXTS`]).
pub fn text_parts(msgs: &[Message]) -> Vec<TextPart> {
    let start = msgs.iter().rposition(|m| m.role == "user");
    let mut out = Vec::new();
    if let Some(m) = start.map(|i| &msgs[i]) {
        let plain = first_lines(&m.content, 4, 400);
        if !plain.is_empty() {
            out.push(TextPart { id: m.id, user: true, html: String::new(), plain });
        }
    }
    let texts: Vec<&Message> = msgs[start.unwrap_or(0)..].iter().filter(|m| m.role == "assistant" && !m.content.trim().is_empty()).collect();
    let skip = texts.len().saturating_sub(MAX_TEXTS);
    out.extend(texts.into_iter().skip(skip).map(|m| TextPart { id: m.id, user: false, html: m.html.clone(), plain: String::new() }));
    out
}

/// Was ein Agent gerade tut, in einer Zeile: sein laufender (sonst der letzte) Schritt bzw. die
/// letzte Zeile seiner Antwort.
pub fn activity(msgs: &[Message]) -> Option<String> {
    let m = msgs.iter().rev().take(12).find(|m| m.role == "tool" || (m.role == "assistant" && !m.content.trim().is_empty()))?;
    if m.role == "assistant" {
        let line = m.content.lines().rev().map(|l| l.trim().trim_start_matches(['#', '-', '*', '>', ' ']).trim()).find(|l| !l.is_empty())?;
        return Some(line.replace(['`', '*'], "").chars().take(160).collect());
    }
    let l = tool_line(m);
    let first = l.text.lines().next().unwrap_or_default();
    Some(match l.kind {
        LineKind::Command => format!("$ {first}"),
        _ => format!("{} {first}", l.verb),
    })
}

/// Offene Rückfrage am Ende des Verlaufs?
fn open_question(msgs: &[Message]) -> bool {
    msgs.iter().rev().take(30).any(|m| m.role == "question" && serde_json::from_value::<QuestionData>(m.data.clone()).is_ok_and(|d| d.answer.is_none() && !d.expired))
}

/// Was eine Kachel aus dem Verlauf ihres Threads zeigt.
#[derive(Clone, PartialEq, Default)]
struct Feed {
    loaded: bool,
    question: bool,
    parts: Vec<TextPart>,
    lines: Vec<CliLine>,
    /// Ändert sich, sobald Text bzw. CLI wachsen (dann scrollt die Fläche mit)
    text_sig: u64,
    cli_sig: u64,
}

impl Feed {
    fn of(msgs: &[Message], with_text: bool, loaded: bool) -> Feed {
        let from = msgs.len().saturating_sub(SCAN);
        let parts = text_parts(msgs);
        let lines = cli_lines(msgs, with_text);
        let text_sig = parts.iter().fold(parts.len() as u64, |s, p| s.wrapping_mul(31).wrapping_add((p.html.len() + p.plain.len()) as u64));
        let cli_sig = lines.iter().fold(lines.len() as u64, |s, l| s.wrapping_mul(31).wrapping_add((l.text.len() + l.output.len() + l.running as usize) as u64).wrapping_add(l.id as u64));
        Feed { loaded, question: open_question(&msgs[from..]), parts, lines, text_sig, cli_sig }
    }
}

/// Billige Kennung des Verlaufsendes: ändert sich, sobald dort etwas dazukommt oder sich ein
/// Schritt ändert (Text wächst, Ausgabe trifft ein, Zustand wechselt). Erst dann wertet eine
/// Kachel den Verlauf neu aus – nicht bei jedem Ereignis irgendeines anderen Threads.
fn stamp(msgs: &[Message]) -> u64 {
    let from = msgs.len().saturating_sub(16);
    msgs[from..].iter().fold(msgs.len() as u64, |h, m| {
        let d = &m.data;
        let len = |k: &str| d[k].as_str().map(str::len).unwrap_or(0) as u64;
        let parts = [m.id as u64, m.updated_at as u64, m.content.len() as u64, m.html.len() as u64, len("status"), len("output"), len("target"), d["diffs"].as_array().map(Vec::len).unwrap_or(0) as u64];
        parts.iter().fold(h, |h, p| (h ^ p).wrapping_mul(0x0100_0000_01b3))
    })
}

// ------------------------------------------------------------------ Bereich

/// Threads des Bereichs: alle, oder ein Thread mit seinen Subagenten (auch deren Subagenten).
fn scoped(all: &BTreeMap<i64, ThreadSummary>, root: Option<i64>) -> Vec<&ThreadSummary> {
    let Some(root) = root else { return all.values().collect() };
    let mut ids = vec![root];
    let mut i = 0;
    while i < ids.len() {
        let parent = ids[i];
        let kids: Vec<i64> = all.values().filter(|t| t.parent_id == Some(parent) && !ids.contains(&t.id)).map(|t| t.id).collect();
        ids.extend(kids);
        i += 1;
    }
    ids.iter().filter_map(|id| all.get(id)).collect()
}

/// Raster der Kacheln je nach ihrer Zahl. Passen sie in ein bis zwei Reihen, füllen sie die Höhe
/// (`flex-1`, gleich hohe Reihen); sonst – und auf schmalen Bildschirmen – haben sie eine feste
/// Höhe und die Seite rollt.
fn layout(n: usize) -> &'static str {
    match n {
        0 | 1 => "min-h-[20rem] flex-1 auto-rows-fr grid-cols-1",
        2 => "shrink-0 auto-rows-[24rem] grid-cols-1 lg:min-h-[20rem] lg:flex-1 lg:shrink lg:auto-rows-fr lg:grid-cols-2",
        3 => "shrink-0 auto-rows-[24rem] grid-cols-1 lg:min-h-[36rem] lg:flex-1 lg:shrink lg:auto-rows-fr lg:grid-cols-2 2xl:min-h-[20rem] 2xl:grid-cols-3",
        4 => "shrink-0 auto-rows-[24rem] grid-cols-1 lg:min-h-[36rem] lg:flex-1 lg:shrink lg:auto-rows-fr lg:grid-cols-2",
        5 | 6 => "shrink-0 auto-rows-[24rem] grid-cols-1 lg:grid-cols-2 2xl:min-h-[36rem] 2xl:flex-1 2xl:shrink 2xl:auto-rows-fr 2xl:grid-cols-3",
        _ => "shrink-0 auto-rows-[24rem] grid-cols-1 lg:grid-cols-2 2xl:grid-cols-3",
    }
}

// ------------------------------------------------------------------ Seite

/// Live-Ansicht. `scope`: `all` oder die Nummer eines Threads (er und seine Subagenten).
#[component]
pub fn Live(scope: String) -> Element {
    rsx! {
        // In einer Schleife, damit der Schlüssel greift: anderer Bereich → frische Kacheln
        for s in std::iter::once(scope) {
            LiveBody { key: "{s}", scope: s.clone() }
        }
    }
}

#[component]
fn LiveBody(scope: String) -> Element {
    let root: Option<i64> = scope.parse().ok();
    // Gezeigte Threads in der Reihenfolge, in der sie zu arbeiten begannen (bleiben stehen)
    let mut seen = use_signal(Vec::<i64>::new);
    use_effect(move || {
        let all = THREADS.read();
        let mut fresh: Vec<&ThreadSummary> = scoped(&all, root).into_iter().filter(|t| t.status.is_active() && !seen.peek().contains(&t.id)).collect();
        fresh.sort_by_key(|t| (t.status != ThreadStatus::Working, t.run_started_at.unwrap_or(t.last_activity_at), t.id));
        let ids: Vec<i64> = fresh.iter().map(|t| t.id).collect();
        drop(all);
        if !ids.is_empty() {
            seen.write().extend(ids);
        }
    });

    let view = VIEW();
    let all = THREADS.read();
    let in_scope = scoped(&all, root);
    let working = in_scope.iter().filter(|t| t.status == ThreadStatus::Working).count();
    let mut shown: Vec<i64> = seen().into_iter().filter(|id| all.contains_key(id)).collect();
    // Vor dem ersten Lauf des Effekts: die aktiven gleich zeigen
    let mut rest: Vec<&&ThreadSummary> = in_scope.iter().filter(|t| t.status.is_active() && !shown.contains(&t.id)).collect();
    rest.sort_by_key(|t| (t.status != ThreadStatus::Working, t.run_started_at.unwrap_or(t.last_activity_at), t.id));
    shown.extend(rest.iter().map(|t| t.id));
    let finished = shown.iter().filter(|id| all.get(id).is_some_and(|t| !t.status.is_active())).count();
    // Niemand arbeitet: die zuletzt aktiven Threads (damit die Seite nicht leer ist)
    let recent: Vec<i64> = if shown.is_empty() {
        let mut list: Vec<&&ThreadSummary> = in_scope.iter().filter(|t| t.status != ThreadStatus::Settled && !t.hidden).collect();
        list.sort_by_key(|t| std::cmp::Reverse(t.last_activity_at));
        list.iter().take(4).map(|t| t.id).collect()
    } else {
        Vec::new()
    };
    let root_title = root.and_then(|r| all.get(&r).map(thread_title));
    drop(all);

    let tiles = if shown.is_empty() { recent.clone() } else { shown.clone() };
    let n = tiles.len();
    let grid_ids = crate::pages::workspace::grid::join(&tiles.iter().copied().take(crate::pages::workspace::grid::MAX).collect::<Vec<_>>());
    let seg = |on: bool| if on { "inline-flex h-7 items-center gap-1.5 rounded-md bg-accent px-2.5 text-xs font-medium text-foreground shadow-xs [&_svg]:size-3.5" } else { "inline-flex h-7 items-center gap-1.5 rounded-md px-2.5 text-xs text-muted-foreground transition-colors hover:text-foreground [&_svg]:size-3.5" };
    let btn = "inline-flex h-8 items-center gap-1.5 rounded-md border bg-card px-2.5 text-sm hover:bg-accent [&_svg]:size-3.5";

    rsx! {
        div { class: "flex h-full min-h-0 flex-col",
            div { class: "flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 px-4 py-2.5",
                div { class: "flex min-w-0 items-center gap-2.5",
                    span { class: if working > 0 { "lu-live-dot lu-live-dot-on" } else { "lu-live-dot" } }
                    h1 { class: "truncate text-[15px] font-semibold",
                        match working {
                            0 => tr("Gerade arbeitet kein Agent"),
                            1 => tr("1 Agent arbeitet"),
                            k => trf("{0} Agenten arbeiten", &[&k]),
                        }
                    }
                }
                if let Some(title) = root_title {
                    Link {
                        to: Route::Live { scope: "all".into() },
                        class: "inline-flex h-7 min-w-0 max-w-72 items-center gap-1.5 rounded-full border bg-card pl-2.5 pr-1.5 text-xs text-muted-foreground hover:text-foreground",
                        title: tr("Alle Agenten zeigen"),
                        icons::Workflow { class: "size-3.5 shrink-0" }
                        span { class: "truncate", "{title}" }
                        icons::X { class: "size-3.5 shrink-0" }
                    }
                }
                div { class: "ml-auto flex flex-wrap items-center gap-2",
                    if finished > 0 {
                        button {
                            class: btn,
                            title: tr("Kacheln fertiger Agenten entfernen"),
                            onclick: move |_| {
                                let all = THREADS.peek();
                                seen.write().retain(|id| all.get(id).is_some_and(|t| t.status.is_active()));
                            },
                            icons::BrushCleaning {}
                            {tr("Fertige ausblenden")}
                        }
                    }
                    if n > 1 {
                        Link {
                            to: Route::Grid { ids: grid_ids },
                            class: btn,
                            title: tr("Dieselben Chats im Raster öffnen – dort kannst du jedem schreiben"),
                            icons::LayoutGrid {}
                            span { class: "hidden sm:inline", {tr("Im Raster öffnen")} }
                        }
                    }
                    div { class: "flex items-center gap-0.5 rounded-lg border bg-card p-0.5", role: "tablist",
                        button { class: seg(view == View::Both), title: tr("Text und CLI"), onclick: move |_| set_view(View::Both),
                            icons::Rows2 {}
                            {tr("Beides")}
                        }
                        button { class: seg(view == View::Text), title: tr("Nur die Antworten der Agenten"), onclick: move |_| set_view(View::Text),
                            icons::MessageSquareText {}
                            {tr("Text")}
                        }
                        button { class: seg(view == View::Cli), title: tr("Nur das CLI: Befehle, Dateien, Werkzeuge"), onclick: move |_| set_view(View::Cli),
                            icons::SquareTerminal {}
                            "CLI"
                        }
                    }
                }
            }
            if n == 0 {
                div { class: "flex min-h-0 flex-1 flex-col items-center justify-center gap-3 px-6 pb-16 text-center",
                    div { class: "grid size-14 place-items-center rounded-2xl border bg-card text-muted-foreground",
                        icons::Activity { class: "size-6" }
                    }
                    p { class: "text-base font-medium", {tr("Gerade arbeitet kein Agent")} }
                    p { class: "max-w-md text-sm text-muted-foreground",
                        {tr("Sobald Agenten oder Subagenten arbeiten, siehst du ihnen hier zu – Text und CLI nebeneinander.")}
                    }
                    Link { to: Route::Home {}, class: "mt-1 inline-flex h-9 items-center gap-2 rounded-md bg-primary px-4 text-sm font-medium text-primary-foreground",
                        icons::Plus { class: "size-4" }
                        {tr("Neuer Thread")}
                    }
                }
            } else {
                div { class: "flex min-h-0 flex-1 flex-col overflow-y-auto px-4 pb-4",
                    if shown.is_empty() {
                        p { class: "shrink-0 pb-2.5 text-xs text-muted-foreground", {tr("Zuletzt aktiv – sobald ein Agent arbeitet, erscheint er hier von selbst.")} }
                    }
                    div { class: "grid gap-3 {layout(n)}",
                        for id in tiles {
                            Tile { key: "{id}", thread_id: id }
                        }
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ Kachel

/// Kachel eines Agenten: Kopf (Agent, Titel, Zustand, Änderungen, Stopp, Öffnen), darunter Text
/// und/oder CLI.
#[component]
fn Tile(thread_id: i64) -> Element {
    use_hook(move || prefetch_messages(vec![thread_id]));
    // Zwei Stufen: Die Kennung ist billig und läuft bei jeder Nachricht (irgendeines Threads);
    // ausgewertet wird der Verlauf nur, wenn sie sich für diesen Thread geändert hat
    let changed = use_memo(move || {
        let loaded = state::LOADED.read().contains(&thread_id);
        let all = MESSAGES.read();
        (loaded, stamp(all.get(&thread_id).map(Vec::as_slice).unwrap_or(&[])))
    });
    let feed = use_memo(move || {
        let (loaded, _) = changed();
        let with_text = VIEW() == View::Cli;
        let all = MESSAGES.peek();
        Feed::of(all.get(&thread_id).map(Vec::as_slice).unwrap_or(&[]), with_text, loaded)
    });
    let view = VIEW();
    let Some(t) = THREADS.read().get(&thread_id).cloned() else { return rsx! {} };
    let parent = t.parent_id.and_then(|p| THREADS.read().get(&p).map(thread_title));
    let f = feed();
    let working = t.status == ThreadStatus::Working;
    let active = t.status.is_active();
    let model = if t.model.is_empty() { String::new() } else { super::data::model_label(t.agent, &t.model) };
    let frame = match t.status {
        ThreadStatus::Working => "lu-live-tile lu-live-tile-on",
        ThreadStatus::Error => "lu-live-tile border-destructive/40",
        ThreadStatus::Done => "lu-live-tile border-success/30",
        _ if f.question => "lu-live-tile border-warning/50",
        _ => "lu-live-tile",
    };
    let stop = move |_| {
        spawn(async move {
            match api::action::<ThreadSummary>(&format!("/threads/{thread_id}/stop")).await {
                Ok(s) => state::upsert_thread(s),
                Err(e) => toast_error(e),
            }
        });
    };
    let icon_btn = "grid size-7 shrink-0 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground [&_svg]:size-3.5";
    rsx! {
        section { class: "{frame} flex min-h-0 min-w-0 flex-col overflow-hidden rounded-xl border bg-card",
            header { class: "flex shrink-0 items-center gap-2.5 border-b px-3 py-2",
                AgentIcon { kind: t.agent, size: 18 }
                div { class: "min-w-0 flex-1",
                    Link { to: Route::ThreadPage { id: thread_id }, class: "block truncate text-sm font-medium hover:underline", title: tr("Chat öffnen"), {thread_title(&t)} }
                    div { class: "flex min-w-0 items-center gap-1.5 text-[11.5px] leading-4 text-muted-foreground",
                        if let Some(p) = parent {
                            icons::CornerLeftUp { class: "size-3 shrink-0" }
                            span { class: "truncate", "{p}" }
                        } else {
                            ProjectBadge { name: t.project_name.clone(), size: 13 }
                            span { class: "truncate", {t.project_name.clone().unwrap_or_else(|| tr("Ohne Projekt"))} }
                        }
                        if !model.is_empty() {
                            span { class: "shrink-0 text-muted-foreground/50", "·" }
                            span { class: "shrink-0", "{model}" }
                        }
                    }
                }
                ChangeStat { thread: t.clone() }
                ThreadState { thread: t.clone() }
                if active {
                    button { class: "{icon_btn} hover:text-destructive", title: tr("Stopp"), onclick: stop,
                        icons::CircleStop {}
                    }
                }
                Link { to: Route::ThreadPage { id: thread_id }, class: icon_btn, title: tr("Chat öffnen"),
                    icons::ArrowUpRight {}
                }
            }
            if view != View::Cli {
                TextPane { thread_id, parts: f.parts.clone(), sig: f.text_sig, working, loaded: f.loaded, grow: view == View::Text }
            }
            if view == View::Both {
                div { class: "flex h-6 shrink-0 select-none items-center gap-1.5 border-y bg-surface/60 px-3 font-mono text-[10.5px] uppercase tracking-wider text-muted-foreground/80",
                    icons::SquareTerminal { class: "size-3" }
                    "CLI"
                }
            }
            if view != View::Text {
                CliPane { thread_id, lines: f.lines.clone(), sig: f.cli_sig, working, loaded: f.loaded }
            }
            if f.question {
                Link { to: Route::ThreadPage { id: thread_id }, class: "flex shrink-0 items-center gap-2 border-t bg-warning/10 px-3 py-1.5 text-xs font-medium text-warning hover:bg-warning/15",
                    icons::MessageCircleQuestion { class: "size-3.5" }
                    {tr("Wartet auf deine Antwort – Chat öffnen")}
                }
            }
        }
    }
}

/// Fläche mit eigenem Bildlauf, die am Ende bleibt, solange man unten ist (`sig` ändert sich,
/// wenn der Inhalt wächst).
#[component]
fn Stick(id: String, sig: u64, #[props(into)] class: String, children: Element) -> Element {
    let mut at_bottom = use_signal(|| true);
    let sid = id.clone();
    use_effect(use_reactive!(|sig| {
        let _ = sig;
        if !*at_bottom.peek() {
            return;
        }
        let sid = sid.clone();
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(0).await;
            if let Some(el) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id(&sid)) {
                el.set_scroll_top(el.scroll_height());
            }
        });
    }));
    rsx! {
        div {
            id: "{id}",
            class: "{class}",
            onscroll: move |e| {
                let bottom = e.scroll_top() + e.client_height() as f64 >= e.scroll_height() as f64 - 48.0;
                if bottom != at_bottom() {
                    at_bottom.set(bottom);
                }
            },
            {children}
        }
    }
}

/// Text eines Agenten: Auftrag und Antworten.
#[component]
fn TextPane(thread_id: i64, parts: Vec<TextPart>, sig: u64, working: bool, loaded: bool, grow: bool) -> Element {
    let empty = !parts.iter().any(|p| !p.user);
    rsx! {
        Stick {
            id: format!("live-text-{thread_id}"),
            sig,
            class: if grow { "min-h-0 flex-1 overflow-y-auto px-3.5 py-3" } else { "min-h-0 flex-[5] overflow-y-auto px-3.5 py-3" },
            div { class: "flex flex-col gap-2.5",
                for p in parts {
                    if p.user {
                        div { key: "{p.id}", class: "flex items-start gap-2 rounded-lg bg-muted px-2.5 py-1.5 text-[12.5px] leading-snug text-muted-foreground",
                            icons::CornerDownRight { class: "mt-0.5 size-3.5 shrink-0" }
                            span { class: "min-w-0 whitespace-pre-wrap break-words", "{p.plain}" }
                        }
                    } else {
                        Markdown { key: "{p.id}", html: p.html.clone(), class: "lu-live-md" }
                    }
                }
                if !loaded && empty {
                    Loading {}
                } else if empty {
                    p { class: "text-[13px] text-muted-foreground",
                        if working { {tr("Noch keine Antwort – der Agent arbeitet …")} } else { {tr("Noch keine Antwort.")} }
                    }
                }
            }
        }
    }
}

/// CLI eines Agenten: seine Schritte wie in einem Terminal.
#[component]
fn CliPane(thread_id: i64, lines: Vec<CliLine>, sig: u64, working: bool, loaded: bool) -> Element {
    // Zwischen zwei Schritten (der Agent denkt oder schreibt): blinkender Cursor
    let cursor = working && !lines.iter().any(|l| l.running);
    rsx! {
        Stick { id: format!("live-cli-{thread_id}"), sig, class: "lu-term min-h-0 flex-[4] overflow-y-auto px-3.5 py-2.5",
            if lines.is_empty() {
                div { class: "text-muted-foreground/70",
                    if loaded { {tr("# noch keine Schritte")} } else { "…" }
                }
            }
            for l in lines {
                CliRow { key: "{l.id}", l }
            }
            if cursor {
                div { class: "lu-term-row",
                    span { class: "lu-term-mark text-success", "$" }
                    span { class: "lu-term-cursor" }
                }
            }
        }
    }
}

/// Zeile im CLI.
#[component]
fn CliRow(l: CliLine) -> Element {
    let mark = rsx! {
        if l.running {
            icons::LoaderCircle { class: "mt-[3px] size-3 shrink-0 animate-spin text-info" }
        } else if l.failed {
            span { class: "shrink-0 select-none text-destructive", "✗" }
        }
    };
    match l.kind {
        LineKind::Prompt => rsx! {
            div { class: "lu-term-row lu-term-gap",
                span { class: "lu-term-mark text-primary", "›" }
                span { class: "min-w-0 flex-1 whitespace-pre-wrap break-words font-medium text-foreground", "{l.text}" }
            }
        },
        LineKind::Command => rsx! {
            div { class: "lu-term-gap",
                div { class: "lu-term-row",
                    span { class: "lu-term-mark text-success", "$" }
                    span { class: "min-w-0 flex-1 whitespace-pre-wrap break-all text-foreground", "{l.text}" }
                    {mark}
                }
                if l.hidden > 0 {
                    div { class: "lu-term-out text-muted-foreground/60",
                        if l.hidden == 1 { {tr("… 1 Zeile davor")} } else { {trf("… {0} Zeilen davor", &[&l.hidden])} }
                    }
                }
                if !l.output.is_empty() {
                    pre { class: if l.failed { "lu-term-out text-destructive/85" } else { "lu-term-out text-muted-foreground" }, "{l.output}" }
                }
            }
        },
        LineKind::Tool => {
            // Dateien: der Name bleibt stehen, gekürzt wird der Ordner davor
            let (dir, name) = match l.text.rfind(['/', '\\']) {
                Some(i) if matches!(l.verb, "read" | "edit" | "write") && i + 1 < l.text.len() => (&l.text[..=i], &l.text[i + 1..]),
                _ => ("", l.text.as_str()),
            };
            rsx! {
            div {
                div { class: "lu-term-row",
                    span { class: "lu-term-verb", "{l.verb}" }
                    span { class: if l.verb == "think" { "flex min-w-0 flex-1 italic text-muted-foreground" } else { "flex min-w-0 flex-1 text-foreground/85" }, title: "{l.text}",
                        if !dir.is_empty() {
                            span { class: "min-w-0 shrink truncate text-muted-foreground", "{dir}" }
                        }
                        span { class: "max-w-full shrink-0 truncate", "{name}" }
                    }
                    if l.adds > 0 {
                        span { class: "shrink-0 text-success", "+{l.adds}" }
                    }
                    if l.dels > 0 {
                        span { class: "shrink-0 text-destructive", "−{l.dels}" }
                    }
                    {mark}
                }
                if !l.output.is_empty() {
                    pre { class: "lu-term-out text-destructive/85", "{l.output}" }
                }
            }
            }
        }
        LineKind::Say => rsx! {
            div { class: "lu-term-row lu-term-gap",
                span { class: "lu-term-mark text-primary", "●" }
                span { class: "min-w-0 flex-1 whitespace-pre-wrap break-words font-sans text-[12.5px] text-foreground/90", "{l.text}" }
            }
        },
        LineKind::Note => rsx! {
            div { class: "lu-term-row lu-term-gap text-info",
                span { class: "lu-term-mark", "»" }
                span { class: "min-w-0 flex-1 whitespace-pre-wrap break-words", "{l.text}" }
            }
        },
        LineKind::Fail => rsx! {
            div { class: "lu-term-row lu-term-gap text-destructive",
                span { class: "lu-term-mark", "✗" }
                span { class: "min-w-0 flex-1 whitespace-pre-wrap break-words", "{l.text}" }
            }
        },
    }
}

// ------------------------------------------------------------------ Einzeiler und Knopf

/// Was ein Agent gerade tut, als eine gedämpfte Zeile (unter dem Titel eines Subagenten). Lädt
/// den Verlauf des Threads bei Bedarf.
#[component]
pub fn ActivityLine(thread_id: i64) -> Element {
    use_hook(move || prefetch_messages(vec![thread_id]));
    let line = use_memo(move || MESSAGES.read().get(&thread_id).and_then(|l| activity(l)));
    match line() {
        Some(text) => rsx! {
            div { class: "truncate font-mono text-[11px] leading-4 text-muted-foreground", title: "{text}", "{text}" }
        },
        None => rsx! {},
    }
}

/// Knopf der Kopfzeile: Live-Ansicht öffnen (auf der Seite selbst: zurück). Zeigt die Zahl der
/// arbeitenden Agenten.
#[component]
pub fn LiveButton(#[props(into)] class: String) -> Element {
    let nav = use_navigator();
    let route = use_route::<Route>();
    let n = THREADS.read().values().filter(|t| t.status == ThreadStatus::Working).count();
    let on = matches!(route, Route::Live { .. });
    let title = match n {
        0 => tr("Live: Agenten bei der Arbeit zusehen"),
        1 => tr("Live: 1 Agent arbeitet – zusehen (Text und CLI)"),
        k => trf("Live: {0} Agenten arbeiten – zusehen (Text und CLI)", &[&k]),
    };
    rsx! {
        button {
            class: if on { "{class} relative bg-accent text-foreground" } else { "{class} relative" },
            title: "{title}",
            onclick: move |_| {
                if on {
                    nav.go_back();
                } else {
                    nav.push(Route::Live { scope: "all".into() });
                }
            },
            icons::Activity { class: if n > 0 { "text-info" } else { "" } }
            if n > 0 {
                span { class: "pointer-events-none absolute -right-1 -top-1 grid h-4 min-w-4 place-items-center rounded-full bg-info px-1 text-[10px] font-semibold leading-none tabular-nums text-primary-foreground", "{n}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(id: i64, role: &str, content: &str, data: serde_json::Value) -> Message {
        Message { id, thread_id: 1, role: role.into(), content: content.into(), html: format!("<p>{content}</p>"), data, ..Default::default() }
    }

    #[test]
    fn cli_lines_from_steps() {
        let msgs = vec![
            msg(1, "user", "Bitte die Tests reparieren\n\nDanke", json!({})),
            msg(2, "assistant", "Ich sehe nach.", json!({})),
            msg(3, "tool", "", json!({"kind": "read", "target": "src/lib.rs", "status": "ok"})),
            msg(4, "tool", "", json!({"kind": "command", "target": "cargo test", "input": {"command": "cargo test --all"}, "output": "a\nb\nc\nd\ne\nf\ng\nh", "status": "running"})),
            msg(5, "tool", "", json!({"kind": "edit", "target": "src/a.rs", "status": "ok", "diffs": [{"path": "src/a.rs", "before": "x", "after": "y\nz", "adds": 2, "dels": 1}]})),
            msg(6, "system", "roh", json!({"kind": "raw"})),
        ];
        let lines = cli_lines(&msgs, false);
        assert_eq!(lines.iter().map(|l| l.kind).collect::<Vec<_>>(), vec![LineKind::Prompt, LineKind::Tool, LineKind::Command, LineKind::Tool]);
        assert_eq!(lines[0].text, "Bitte die Tests reparieren\nDanke");
        assert_eq!((lines[1].verb, lines[1].text.as_str()), ("read", "src/lib.rs"));
        // Befehl: voller Befehl, die letzten sechs Zeilen der Ausgabe, zwei davor
        assert_eq!(lines[2].text, "cargo test --all");
        assert!(lines[2].running);
        assert_eq!((lines[2].output.as_str(), lines[2].hidden), ("c\nd\ne\nf\ng\nh", 2));
        assert_eq!((lines[3].verb, lines[3].adds, lines[3].dels), ("edit", 2, 1));
        // Allein das CLI: auch die Antworten – als schlichter Text
        assert_eq!(cli_lines(&msgs, true)[1].kind, LineKind::Say);
        assert_eq!(plain("## Fertig\n**Zwei** Dateien, `cargo test` ist grün."), "Fertig\nZwei Dateien, cargo test ist grün.");
    }

    #[test]
    fn text_since_last_prompt() {
        let msgs = vec![
            msg(1, "user", "alt", json!({})),
            msg(2, "assistant", "alte Antwort", json!({})),
            msg(3, "user", "neu", json!({})),
            msg(4, "assistant", "eins", json!({})),
            msg(5, "tool", "", json!({"kind": "read"})),
            msg(6, "assistant", "zwei", json!({})),
        ];
        let parts = text_parts(&msgs);
        assert_eq!(parts.iter().map(|p| (p.id, p.user)).collect::<Vec<_>>(), vec![(3, true), (4, false), (6, false)]);
        assert_eq!(parts[0].plain, "neu");
        assert_eq!(parts[2].html, "<p>zwei</p>");
    }

    #[test]
    fn activity_is_latest_step_or_text() {
        let mut msgs = vec![msg(1, "user", "los", json!({})), msg(2, "assistant", "## Plan\n- **erst** lesen\n", json!({}))];
        assert_eq!(activity(&msgs).as_deref(), Some("erst lesen"));
        msgs.push(msg(3, "tool", "", json!({"kind": "command", "target": "cargo build", "status": "running"})));
        assert_eq!(activity(&msgs).as_deref(), Some("$ cargo build"));
        msgs.push(msg(4, "tool", "", json!({"kind": "search", "target": "TODO", "status": "ok"})));
        assert_eq!(activity(&msgs).as_deref(), Some("grep TODO"));
        assert_eq!(activity(&[]), None);
    }

    #[test]
    fn stamp_follows_the_end_of_the_history() {
        let mut msgs = vec![msg(1, "user", "los", json!({})), msg(2, "tool", "", json!({"kind": "command", "target": "ls", "status": "running", "output": ""}))];
        let a = stamp(&msgs);
        assert_eq!(a, stamp(&msgs));
        // Ausgabe trifft ein, Zustand wechselt
        msgs[1].data = json!({"kind": "command", "target": "ls", "status": "ok", "output": "a.rs"});
        let b = stamp(&msgs);
        assert_ne!(a, b);
        // Text wächst beim Streamen (gleiche Nachricht, gleiche Sekunde)
        msgs.push(msg(3, "assistant", "Hal", json!({})));
        let c = stamp(&msgs);
        msgs[2].content.push_str("lo");
        assert_ne!(c, stamp(&msgs));
        assert_ne!(b, c);
    }

    #[test]
    fn scope_is_thread_with_descendants() {
        let t = |id: i64, parent: Option<i64>| ThreadSummary { id, parent_id: parent, ..Default::default() };
        let all: BTreeMap<i64, ThreadSummary> = [t(1, None), t(2, Some(1)), t(3, Some(2)), t(4, None), t(5, Some(4))].into_iter().map(|t| (t.id, t)).collect();
        assert_eq!(scoped(&all, Some(1)).iter().map(|t| t.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(scoped(&all, None).len(), 5);
        assert!(scoped(&all, Some(9)).is_empty());
    }
}

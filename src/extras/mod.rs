//! Was luCode über nrzCode hinaus kann – bewusst in einem eigenen Bereich, damit sich Änderungen
//! aus nrzCode weiter leicht übernehmen lassen:
//!
//! * **Suche** – `GET /search?q=`: Chats (Titel) und Nachrichten des Benutzers; die Oberfläche zeigt
//!   sie in der Befehlspalette (Strg+K / ⌘K).
//! * **Eigene Anweisungen** – `GET/PUT /instructions` (je Benutzer) und
//!   `GET/PUT /projects/<id>/instructions` (je Projekt). [`instructions_hint`] hängt sie bei jedem
//!   Lauf an den Systemhinweis.
//! * **Export** – `GET /threads/<id>/export`: der Chat als Markdown-Datei.
//! * **Sicherungen** – `GET/POST /backups`, `DELETE /backups/<name>`, `GET /backups/<name>/download`,
//!   `PUT /backups/settings` (Administratoren): Abbild der Datenbank unter `<data>/backups`
//!   (`VACUUM INTO`, auch im laufenden Betrieb in sich stimmig); täglich von selbst, die letzten
//!   sieben bleiben.

use crate::auth::{AdminUser, CurrentUser};
use crate::db::{self, now};
use crate::error::{AppError, AppResult};
use crate::AppState;
use lucode_shared::extras::{Backup, BackupList, BackupSettings, Instructions, SearchHit, SearchResults, MAX_INSTRUCTIONS};
use rocket::fs::NamedFile;
use rocket::http::{ContentType, Header};
use rocket::request::Request;
use rocket::response::{self, Responder, Response};
use rocket::serde::json::Json;
use rocket::{delete, get, post, put, routes, Route, State};
use std::path::PathBuf;
use std::time::Duration;

pub fn api_routes() -> Vec<Route> {
    routes![
        search,
        get_instructions,
        put_instructions,
        get_project_instructions,
        put_project_instructions,
        export_thread,
        list_backups,
        create_backup,
        delete_backup,
        download_backup,
        put_backup_settings
    ]
}

// ------------------------------------------------------------------ Suche

/// `%`, `_` und `\` in einem LIKE-Muster wörtlich nehmen.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Ausschnitt um die erste Fundstelle (ohne Zeilenumbrüche, mit … an abgeschnittenen Enden).
fn snippet(text: &str, query: &str) -> String {
    let lower = text.to_lowercase();
    // Beim Kleinschreiben können sich Längen ändern – dann zählt die Stelle nur ungefähr
    let found = lower.find(&query.to_lowercase()).filter(|_| lower.len() == text.len()).unwrap_or(0);
    let mut start = found.saturating_sub(70);
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (found + query.len() + 130).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    let mut out = text[start..end].split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        out.insert(0, '…');
    }
    if end < text.len() {
        out.push('…');
    }
    out
}

/// `GET /search?q=&limit=` – Chats, deren Titel passt, und Nachrichten (von dir und den Agenten),
/// in denen der Begriff steht. Ab zwei Zeichen.
#[get("/search?<q>&<limit>")]
async fn search(state: &State<AppState>, user: CurrentUser, q: &str, limit: Option<i64>) -> AppResult<Json<SearchResults>> {
    let q = q.trim();
    if q.chars().count() < 2 {
        return Ok(Json(SearchResults::default()));
    }
    let pattern = format!("%{}%", like_escape(q));
    let limit = limit.unwrap_or(30).clamp(1, 100);
    let threads: Vec<(i64, String, String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT t.id, t.title, COALESCE(p.name, ''), t.last_activity_at, t.bot_id FROM threads t LEFT JOIN projects p ON p.id = t.project_id \
         WHERE t.user_id = ? AND t.title LIKE ? ESCAPE '\\' ORDER BY t.last_activity_at DESC LIMIT 12",
    )
    .bind(user.id())
    .bind(&pattern)
    .fetch_all(&state.db)
    .await?;
    let messages: Vec<(i64, i64, String, String, i64, String, String, Option<i64>)> = sqlx::query_as(
        "SELECT m.id, m.thread_id, m.role, m.content, m.created_at, t.title, COALESCE(p.name, ''), t.bot_id FROM messages m \
         JOIN threads t ON t.id = m.thread_id LEFT JOIN projects p ON p.id = t.project_id \
         WHERE t.user_id = ? AND m.role IN ('user', 'assistant') AND m.content LIKE ? ESCAPE '\\' ORDER BY m.id DESC LIMIT ?",
    )
    .bind(user.id())
    .bind(&pattern)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(SearchResults {
        threads: threads
            .into_iter()
            .map(|(thread_id, title, project, at, bot_id)| SearchHit { thread_id, title, project, bot_id, at, ..Default::default() })
            .collect(),
        messages: messages
            .into_iter()
            .map(|(id, thread_id, role, content, at, title, project, bot_id)| SearchHit {
                thread_id,
                title,
                project,
                bot_id,
                message_id: Some(id),
                role,
                snippet: snippet(&content, q),
                at,
            })
            .collect(),
    }))
}

// ------------------------------------------------------------------ Eigene Anweisungen

fn clean_instructions(text: &str) -> String {
    text.trim().chars().take(MAX_INSTRUCTIONS).collect()
}

/// `GET /instructions` – eigene Anweisungen des Benutzers.
#[get("/instructions")]
async fn get_instructions(state: &State<AppState>, user: CurrentUser) -> AppResult<Json<Instructions>> {
    let text: String = sqlx::query_scalar("SELECT instructions FROM users WHERE id = ?").bind(user.id()).fetch_one(&state.db).await?;
    Ok(Json(Instructions { text }))
}

/// `PUT /instructions`
#[put("/instructions", data = "<body>")]
async fn put_instructions(state: &State<AppState>, user: CurrentUser, body: Json<Instructions>) -> AppResult<Json<Instructions>> {
    let text = clean_instructions(&body.text);
    sqlx::query("UPDATE users SET instructions = ?, updated_at = ? WHERE id = ?").bind(&text).bind(now()).bind(user.id()).execute(&state.db).await?;
    Ok(Json(Instructions { text }))
}

/// Projekt des Benutzers, sonst 404.
async fn own_project(state: &AppState, user_id: i64, project_id: i64) -> AppResult<()> {
    let owner: Option<(i64,)> = sqlx::query_as("SELECT user_id FROM projects WHERE id = ?").bind(project_id).fetch_optional(&state.db).await?;
    match owner {
        Some((o,)) if o == user_id => Ok(()),
        _ => Err(AppError::NotFound),
    }
}

/// `GET /projects/<id>/instructions` – Anweisungen für die Chats dieses Projekts.
#[get("/projects/<id>/instructions")]
async fn get_project_instructions(state: &State<AppState>, user: CurrentUser, id: i64) -> AppResult<Json<Instructions>> {
    own_project(state, user.id(), id).await?;
    let text: String = sqlx::query_scalar("SELECT instructions FROM projects WHERE id = ?").bind(id).fetch_one(&state.db).await?;
    Ok(Json(Instructions { text }))
}

/// `PUT /projects/<id>/instructions`
#[put("/projects/<id>/instructions", data = "<body>")]
async fn put_project_instructions(state: &State<AppState>, user: CurrentUser, id: i64, body: Json<Instructions>) -> AppResult<Json<Instructions>> {
    own_project(state, user.id(), id).await?;
    let text = clean_instructions(&body.text);
    sqlx::query("UPDATE projects SET instructions = ?, updated_at = ? WHERE id = ?").bind(&text).bind(now()).bind(id).execute(&state.db).await?;
    Ok(Json(Instructions { text }))
}

/// Abschnitt für den Systemhinweis eines Laufs: die Anweisungen des Benutzers und – in einem
/// Projekt – die des Projekts. Leer, wenn es keine gibt.
pub async fn instructions_hint(state: &AppState, user_id: i64, project_id: Option<i64>) -> String {
    let user: String = sqlx::query_scalar("SELECT instructions FROM users WHERE id = ?").bind(user_id).fetch_optional(&state.db).await.ok().flatten().unwrap_or_default();
    let project: String = match project_id {
        Some(p) => sqlx::query_scalar("SELECT instructions FROM projects WHERE id = ?").bind(p).fetch_optional(&state.db).await.ok().flatten().unwrap_or_default(),
        None => String::new(),
    };
    let mut h = String::new();
    if !user.trim().is_empty() {
        h.push_str("\n\nThe user's standing instructions (follow them unless the current request says otherwise):\n");
        h.push_str(user.trim());
    }
    if !project.trim().is_empty() {
        h.push_str("\n\nInstructions for this project (they take precedence over the standing ones):\n");
        h.push_str(project.trim());
    }
    h
}

// ------------------------------------------------------------------ Export

/// Datei zum Herunterladen.
struct Download {
    body: Vec<u8>,
    content_type: ContentType,
    name: String,
}

impl<'r> Responder<'r, 'static> for Download {
    fn respond_to(self, _: &'r Request<'_>) -> response::Result<'static> {
        Response::build()
            .header(self.content_type)
            .header(Header::new("Content-Disposition", format!("attachment; filename*=UTF-8''{}", urlencoding::encode(&self.name))))
            .header(Header::new("Cache-Control", "no-store"))
            .sized_body(self.body.len(), std::io::Cursor::new(self.body))
            .ok()
    }
}

/// Unix-Sekunden als `JJJJ-MM-TT HH:MM` (UTC).
fn stamp(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    // Tage seit 1970 → Datum (Algorithmus von Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// Dateiname aus einem Titel: Buchstaben, Ziffern, Bindestriche.
fn file_slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.chars().count() >= 60 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "chat".into()
    } else {
        out
    }
}

/// `GET /threads/<id>/export` – der Chat als Markdown: was du geschrieben hast und was der Agent
/// geantwortet hat; Arbeitsschritte (Werkzeuge) nur als Zahl dazwischen.
#[get("/threads/<id>/export")]
async fn export_thread(state: &State<AppState>, user: CurrentUser, id: i64) -> AppResult<Download> {
    let head: Option<(String, String, String, i64, String)> = sqlx::query_as(
        "SELECT t.title, t.agent_kind, t.model, t.created_at, COALESCE(p.name, '') FROM threads t LEFT JOIN projects p ON p.id = t.project_id WHERE t.id = ? AND t.user_id = ?",
    )
    .bind(id)
    .bind(user.id())
    .fetch_optional(&state.db)
    .await?;
    let Some((title, agent, model, created_at, project)) = head else {
        return Err(AppError::NotFound);
    };
    let title = if title.trim().is_empty() { format!("Chat {id}") } else { title };
    let rows: Vec<(String, String, i64)> = sqlx::query_as("SELECT role, content, created_at FROM messages WHERE thread_id = ? ORDER BY id").bind(id).fetch_all(&state.db).await?;
    let mut md = format!("# {title}\n\n");
    let mut meta = vec!["luCode".to_string()];
    if !project.is_empty() {
        meta.push(project);
    }
    meta.push(if model.is_empty() { agent.clone() } else { format!("{agent} ({model})") });
    meta.push(format!("{} UTC", stamp(created_at)));
    md.push_str(&format!("*{}*\n", meta.join(" · ")));
    let mut steps = 0;
    for (role, content, at) in rows {
        match role.as_str() {
            "tool" => steps += 1,
            "user" | "assistant" | "error" if !content.trim().is_empty() => {
                if steps > 0 {
                    md.push_str(&format!("\n*… {steps} {} …*\n", if steps == 1 { "Arbeitsschritt" } else { "Arbeitsschritte" }));
                    steps = 0;
                }
                let who = match role.as_str() {
                    "user" => "Du".to_string(),
                    "error" => "Fehler".to_string(),
                    _ => agent.clone(),
                };
                md.push_str(&format!("\n---\n\n**{who}** · {}\n\n{}\n", stamp(at), content.trim()));
            }
            _ => {}
        }
    }
    Ok(Download { body: md.into_bytes(), content_type: ContentType::new("text", "markdown").with_params(("charset", "utf-8")), name: format!("{}.md", file_slug(&title)) })
}

// ------------------------------------------------------------------ Sicherungen

/// So viele von selbst angelegte Sicherungen bleiben.
const KEEP_AUTO: usize = 7;
/// Abstand der selbst angelegten Sicherungen (Sekunden).
const AUTO_EVERY: i64 = 24 * 3600;

fn backups_dir(state: &AppState) -> PathBuf {
    state.config.data_dir.join("backups")
}

/// `lucode-<unix>.db` bzw. `lucode-auto-<unix>.db` → (Zeitpunkt, von selbst angelegt).
fn parse_name(name: &str) -> Option<(i64, bool)> {
    let rest = name.strip_prefix("lucode-")?.strip_suffix(".db")?;
    let (auto, digits) = match rest.strip_prefix("auto-") {
        Some(d) => (true, d),
        None => (false, rest),
    };
    if digits.is_empty() || digits.len() > 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((digits.parse().ok()?, auto))
}

async fn backups(state: &AppState) -> Vec<Backup> {
    let mut out = Vec::new();
    if let Ok(mut dir) = tokio::fs::read_dir(backups_dir(state)).await {
        while let Ok(Some(e)) = dir.next_entry().await {
            let name = e.file_name().to_string_lossy().to_string();
            let Some((created_at, auto)) = parse_name(&name) else { continue };
            let size = e.metadata().await.map(|m| m.len() as i64).unwrap_or(0);
            out.push(Backup { name, size, created_at, auto });
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    out
}

/// Sicherung anlegen: ein in sich stimmiges Abbild der Datenbank (auch während Agenten arbeiten).
pub async fn backup(state: &AppState, auto: bool) -> AppResult<Backup> {
    let dir = backups_dir(state);
    tokio::fs::create_dir_all(&dir).await?;
    let ts = now();
    let name = if auto { format!("lucode-auto-{ts}.db") } else { format!("lucode-{ts}.db") };
    let path = dir.join(&name);
    // VACUUM INTO schreibt nur in eine Datei, die es noch nicht gibt
    let _ = tokio::fs::remove_file(&path).await;
    sqlx::query("VACUUM INTO ?").bind(path.to_string_lossy().to_string()).execute(&state.db).await?;
    let size = tokio::fs::metadata(&path).await.map(|m| m.len() as i64).unwrap_or(0);
    Ok(Backup { name, size, created_at: ts, auto })
}

async fn auto_on(state: &AppState) -> bool {
    db::setting(&state.db, "backup_auto", "on").await.map(|v| v != "off").unwrap_or(true)
}

/// Täglich von selbst sichern und nur die letzten behalten.
pub fn start(state: AppState) {
    tokio::spawn(async move {
        // Nicht gleich beim Start: erst wenn die App eine Weile läuft
        tokio::time::sleep(Duration::from_secs(120)).await;
        loop {
            if auto_on(&state).await {
                let list = backups(&state).await;
                let newest = list.iter().filter(|b| b.auto).map(|b| b.created_at).max().unwrap_or(0);
                if now() - newest >= AUTO_EVERY {
                    match backup(&state, true).await {
                        Ok(b) => tracing::info!("Sicherung angelegt: {} ({} Bytes)", b.name, b.size),
                        Err(e) => tracing::warn!("Sicherung: {e}"),
                    }
                    for old in backups(&state).await.into_iter().filter(|b| b.auto).skip(KEEP_AUTO) {
                        let _ = tokio::fs::remove_file(backups_dir(&state).join(&old.name)).await;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
}

async fn backup_list(state: &AppState) -> BackupList {
    BackupList { backups: backups(state).await, auto: auto_on(state).await, dir: backups_dir(state).to_string_lossy().to_string() }
}

/// `GET /backups`
#[get("/backups")]
async fn list_backups(state: &State<AppState>, _admin: AdminUser) -> Json<BackupList> {
    Json(backup_list(state).await)
}

/// `POST /backups` – jetzt sichern.
#[post("/backups")]
async fn create_backup(state: &State<AppState>, _admin: AdminUser) -> AppResult<Json<BackupList>> {
    backup(state, false).await?;
    Ok(Json(backup_list(state).await))
}

/// `PUT /backups/settings` – tägliche Sicherung ein/aus.
#[put("/backups/settings", data = "<body>")]
async fn put_backup_settings(state: &State<AppState>, _admin: AdminUser, body: Json<BackupSettings>) -> AppResult<Json<BackupList>> {
    db::set_setting(&state.db, "backup_auto", if body.auto { "on" } else { "off" }).await?;
    Ok(Json(backup_list(state).await))
}

/// `DELETE /backups/<name>`
#[delete("/backups/<name>")]
async fn delete_backup(state: &State<AppState>, _admin: AdminUser, name: &str) -> AppResult<Json<BackupList>> {
    if parse_name(name).is_none() {
        return Err(AppError::NotFound);
    }
    tokio::fs::remove_file(backups_dir(state).join(name)).await.map_err(|_| AppError::NotFound)?;
    Ok(Json(backup_list(state).await))
}

/// Datei mit `Content-Disposition: attachment`.
struct Attachment(NamedFile, String);

impl<'r> Responder<'r, 'static> for Attachment {
    fn respond_to(self, req: &'r Request<'_>) -> response::Result<'static> {
        let mut res = self.0.respond_to(req)?;
        res.set_header(Header::new("Content-Disposition", format!("attachment; filename=\"{}\"", self.1)));
        res.set_header(Header::new("Cache-Control", "no-store"));
        Ok(res)
    }
}

/// `GET /backups/<name>/download`
#[get("/backups/<name>/download")]
async fn download_backup(state: &State<AppState>, _admin: AdminUser, name: &str) -> AppResult<Attachment> {
    if parse_name(name).is_none() {
        return Err(AppError::NotFound);
    }
    let file = NamedFile::open(backups_dir(state).join(name)).await.map_err(|_| AppError::NotFound)?;
    Ok(Attachment(file, name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_around_the_match() {
        let text = format!("{} Die Anmeldung über OAuth schlägt fehl, wenn der Token abläuft. {}", "Vorher. ".repeat(30), "Danach. ".repeat(40));
        let s = snippet(&text, "oauth");
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("über OAuth schlägt"), "{s}");
        assert!(s.chars().count() < 260, "{s}");
        assert_eq!(snippet("Kurz und gut", "gut"), "Kurz und gut");
        // Umlaute an der Schnittkante dürfen nicht stören
        let s = snippet(&"ä".repeat(200), "ää");
        assert!(!s.is_empty());
    }

    #[test]
    fn like_pattern_is_literal() {
        assert_eq!(like_escape("100%_fertig\\"), "100\\%\\_fertig\\\\");
    }

    #[test]
    fn stamps_and_names() {
        assert_eq!(stamp(0), "1970-01-01 00:00");
        assert_eq!(stamp(1_795_446_190), "2026-11-23 15:03");
        assert_eq!(stamp(951_782_400), "2000-02-29 00:00");
        assert_eq!(file_slug("Login-Seite bauen: ÄÖÜ & mehr!"), "login-seite-bauen-äöü-mehr");
        assert_eq!(file_slug("   "), "chat");
        assert_eq!(parse_name("lucode-1795446190.db"), Some((1_795_446_190, false)));
        assert_eq!(parse_name("lucode-auto-1795446190.db"), Some((1_795_446_190, true)));
        assert_eq!(parse_name("lucode-../x.db"), None);
        assert_eq!(parse_name("lucode.db"), None);
    }
}

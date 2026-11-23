//! Umzug von nrzCode: luCode übernimmt den Datenordner einer nrzCode-Instanz.
//!
//! Ablauf ([`run`]):
//! 1. **Datenbank** – in einer Transaktion: Die Quelle (`nrzcode.db`) wird nur gelesen, jede Zeile
//!    bekommt hier eine Nummer (die alte, wenn sie frei ist) und alle Verweise – auch die in JSON
//!    (Subagenten, Canvas, KI-Ketten) – folgen ihr. `import_map` merkt sich die Zuordnung: Ein
//!    wiederholter Import legt nichts doppelt an und holt nur Neues nach.
//! 2. **Dateien** – Homes der Agent-Konten (Anmeldungen der CLIs), Projekte mit Worktrees,
//!    Arbeitsordner, Anhänge, Profilbilder, Werkzeuge, Sprachmodelle. Worktrees zeigen danach auf
//!    das Repository hier, die Sitzungen von Claude Code liegen unter den neuen Arbeitsordnern.
//! 3. **Abschluss** – Projekte freigeben, Sitzungen prüfen (ohne Sitzung setzt der Agent mit dem
//!    Verlauf fort).
//!
//! nrzCode selbst bleibt unberührt. Benutzer werden über ihren Namen zugeordnet; hat die Quelle
//! nur einen, gehört alles dem, der importiert.
//!
//! Routen (Administratoren): `GET /import/nrzcode?path=`, `POST /import/nrzcode`,
//! `GET /import/nrzcode/status`.

mod files;
mod rows;

use crate::auth::AdminUser;
use crate::db::now;
use crate::error::{AppError, AppResult};
use crate::AppState;
use files::{CopyReport, IdMaps, Progress};
use lucode_shared::import::{ImportCounts, ImportOptions, ImportSource, ImportStatus};
use rocket::serde::json::Json;
use rocket::{get, post, routes, Route, State};
use rows::{Rec, RecExt, Val};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::ConnectOptions;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

pub fn api_routes() -> Vec<Route> {
    routes![source, start, status]
}

/// Datenbank von nrzCode im Datenordner.
const SOURCE_DB: &str = "nrzcode.db";
/// Tabellen, zu deren Zeilen Ordner gehören (für `import_map.files_done`).
const FILE_TABLES: &[&str] = &["users", "agent_accounts", "projects", "threads", "bots"];
/// Hinweis an Projekten, solange ihre Dateien kopiert werden.
const COPYING: &str = "Wird aus nrzCode übernommen …";

/// Stand des laufenden bzw. letzten Imports (einer je Instanz).
static STATUS: LazyLock<Mutex<ImportStatus>> = LazyLock::new(|| Mutex::new(ImportStatus { state: "idle".into(), ..Default::default() }));
/// Zähler der kopierten Dateien des laufenden Imports.
static PROGRESS: LazyLock<Mutex<Arc<Progress>>> = LazyLock::new(|| Mutex::new(Arc::new(Progress::default())));

/// Läuft gerade ein Umzug? (Die App installiert Updates erst danach – siehe [`crate::busy`].)
pub fn running() -> bool {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).is_running()
}

fn status_mut(f: impl FnOnce(&mut ImportStatus)) {
    f(&mut STATUS.lock().unwrap_or_else(|e| e.into_inner()));
}

fn current_status() -> ImportStatus {
    let mut s = STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let p = PROGRESS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    s.files = p.files.load(Ordering::Relaxed);
    s.bytes = p.bytes.load(Ordering::Relaxed);
    s
}

fn step(text: &str, detail: &str) {
    status_mut(|s| {
        s.step = text.to_string();
        s.detail = detail.to_string();
    });
}

fn note(text: String) {
    status_mut(|s| {
        if s.notes.len() < 40 {
            s.notes.push(text);
        }
    });
}

// ------------------------------------------------------------------ Quelle

/// Übliche Orte des Datenordners von nrzCode (App unter Windows/macOS, daneben liegende Ordner).
fn default_dirs(state: &AppState) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    if let Some(p) = env("LUCODE_IMPORT_DIR") {
        v.push(p);
    }
    if let Some(p) = env("APPDATA") {
        v.push(p.join("nrzCode"));
    }
    if let Some(home) = env("HOME").or_else(|| env("USERPROFILE")) {
        v.push(home.join("Library/Application Support/nrzCode"));
        v.push(home.join(".local/share/nrzCode"));
        v.push(home.join("nrzCode"));
    }
    // Neben dem eigenen Datenordner (z. B. zwei Volumes im selben Docker-Host)
    if let Some(parent) = files::absolute(&state.config.data_dir).parent() {
        for name in ["nrzCode", "nrzcode", "nrzcode-data", "nrz-code"] {
            v.push(parent.join(name));
        }
    }
    v
}

/// Datenordner der Quelle: der angegebene, sonst der erste übliche Ort mit einer `nrzcode.db`.
fn source_dir(state: &AppState, path: &str) -> Option<PathBuf> {
    let path = path.trim().trim_matches('"');
    if !path.is_empty() {
        let p = PathBuf::from(path);
        // Auch die Datei selbst darf angegeben sein
        return Some(if p.file_name().is_some_and(|n| n == SOURCE_DB) { p.parent().map(Path::to_path_buf).unwrap_or(p) } else { p });
    }
    default_dirs(state).into_iter().find(|d| d.join(SOURCE_DB).is_file())
}

/// Quelle nur lesend öffnen. Ein Lesevorgang über alles (`BEGIN`) hält einen gemeinsamen Stand
/// fest, auch wenn nrzCode gerade läuft und schreibt.
async fn open_source(state: &AppState, dir: &Path) -> AppResult<SqliteConnection> {
    let db = dir.join(SOURCE_DB);
    if !db.is_file() {
        return Err(AppError::bad(format!("In {} liegt keine {SOURCE_DB} – das ist kein Datenordner von nrzCode.", dir.display())));
    }
    let own = files::absolute(&state.config.data_dir);
    let theirs = files::absolute(dir);
    if own == theirs || theirs.starts_with(&own) || own.starts_with(&theirs) {
        return Err(AppError::bad("Der Ordner gehört zu luCode selbst."));
    }
    let direct = SqliteConnectOptions::new().filename(&db).read_only(true).busy_timeout(Duration::from_secs(10));
    let mut conn = match try_open(direct).await {
        Ok(c) => c,
        Err(first) => {
            // Ohne Schreibrecht im Ordner lässt sich eine WAL-Datenbank nicht lesen: Kopie nehmen
            let copy = state.config.tmp_dir().join("import-source");
            let _ = tokio::fs::remove_dir_all(&copy).await;
            tokio::fs::create_dir_all(&copy).await?;
            for ext in ["", "-wal", "-shm"] {
                let from = dir.join(format!("{SOURCE_DB}{ext}"));
                if from.is_file() {
                    tokio::fs::copy(&from, copy.join(format!("{SOURCE_DB}{ext}"))).await?;
                }
            }
            try_open(SqliteConnectOptions::new().filename(copy.join(SOURCE_DB)).busy_timeout(Duration::from_secs(10)))
                .await
                .map_err(|_| AppError::bad(format!("Die Datenbank von nrzCode lässt sich nicht lesen: {first}")))?
        }
    };
    let tables = rows::tables(&mut conn).await?;
    if ["users", "agent_accounts", "projects", "threads", "messages"].iter().any(|t| !tables.contains(*t)) {
        return Err(AppError::bad(format!("{} ist keine Datenbank von nrzCode.", db.display())));
    }
    Ok(conn)
}

async fn try_open(opts: SqliteConnectOptions) -> Result<SqliteConnection, sqlx::Error> {
    let mut conn = opts.connect().await?;
    sqlx::query("BEGIN").execute(&mut conn).await?;
    sqlx::query("SELECT COUNT(*) FROM sqlite_master").execute(&mut conn).await?;
    Ok(conn)
}

/// Kennung der Quelle: aus ihrem Cookie-Schlüssel (je Instanz einmalig), sonst aus dem Pfad.
async fn source_key(conn: &mut SqliteConnection, dir: &Path) -> String {
    let secret: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = 'cookie_secret'").fetch_optional(&mut *conn).await.ok().flatten();
    let basis = secret.map(|(s,)| s).filter(|s| !s.is_empty()).unwrap_or_else(|| files::absolute(dir).to_string_lossy().to_lowercase());
    hex::encode(&Sha256::digest(format!("nrzcode:{basis}").as_bytes())[..8])
}

async fn count(conn: &mut SqliteConnection, tables: &HashSet<String>, table: &str) -> i64 {
    if !tables.contains(table) {
        return 0;
    }
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM \"{table}\""))).fetch_one(&mut *conn).await.unwrap_or(0)
}

/// Tabellen der Zahlen: (Tabelle der Quelle, Feld).
const COUNTED: &[&str] =
    &["users", "agent_accounts", "nrzgit_connections", "projects", "threads", "messages", "bots", "agent_skills", "prompt_notes", "attachments", "canvas_boards"];

fn counts_from(values: &HashMap<&str, i64>) -> ImportCounts {
    let g = |k: &str| values.get(k).copied().unwrap_or(0);
    ImportCounts {
        users: g("users"),
        accounts: g("agent_accounts"),
        connections: g("nrzgit_connections"),
        projects: g("projects"),
        threads: g("threads"),
        messages: g("messages"),
        bots: g("bots"),
        skills: g("agent_skills"),
        notes: g("prompt_notes"),
        attachments: g("attachments"),
        boards: g("canvas_boards"),
    }
}

/// Quelle beschreiben: was liegt dort, was davon ist noch nicht hier.
async fn describe(state: &AppState, path: &str) -> ImportSource {
    let Some(dir) = source_dir(state, path) else {
        return ImportSource::default();
    };
    let mut out = ImportSource { path: dir.to_string_lossy().to_string(), ..Default::default() };
    let mut conn = match open_source(state, &dir).await {
        Ok(c) => c,
        Err(e) => {
            out.error = e.public_message();
            return out;
        }
    };
    out.found = true;
    let tables = rows::tables(&mut conn).await.unwrap_or_default();
    let mut have = HashMap::new();
    for t in COUNTED {
        have.insert(*t, count(&mut conn, &tables, t).await);
    }
    out.counts = counts_from(&have);
    let users: Vec<(String,)> = sqlx::query_as("SELECT username FROM users ORDER BY id").fetch_all(&mut conn).await.unwrap_or_default();
    out.users = users.into_iter().map(|(u,)| u).collect();
    let key = source_key(&mut conn, &dir).await;
    out.last_import_at = sqlx::query_scalar("SELECT COALESCE(MAX(finished_at), 0) FROM import_runs WHERE source = ? AND error = ''")
        .bind(&key)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    // Noch nicht übernommen: Zeilen der Quelle ohne Zuordnung. Für Tabellen ohne eigene Nummer
    // (Verbindungen, Anhänge, Canvas) zählt, ob es schon einen Import gab.
    let mapped: Vec<(String, i64)> = sqlx::query_as("SELECT tbl, COUNT(*) FROM import_map WHERE source = ? GROUP BY tbl").bind(&key).fetch_all(&state.db).await.unwrap_or_default();
    let mapped: HashMap<String, i64> = mapped.into_iter().collect();
    let mut pending = HashMap::new();
    for t in COUNTED {
        let total = have.get(t).copied().unwrap_or(0);
        let left = match *t {
            "nrzgit_connections" | "attachments" | "canvas_boards" => {
                if out.last_import_at == 0 {
                    total
                } else {
                    0
                }
            }
            _ => (total - mapped.get(*t).copied().unwrap_or(0)).max(0),
        };
        pending.insert(*t, left);
    }
    out.pending = counts_from(&pending);
    out
}

// ------------------------------------------------------------------ Datenbank

/// Der Datenbank-Teil des Imports (läuft in einer Transaktion des Ziels).
struct Job<'a> {
    src: SqliteConnection,
    dst: &'a mut SqliteConnection,
    /// Kennung der Quelle
    key: String,
    opts: ImportOptions,
    /// Wer importiert (ihm gehört alles, wenn die Quelle nur einen Benutzer hat)
    importer: i64,
    ts: i64,
    /// Datenordner hier (für `worktree_path`)
    dst_root: PathBuf,
    src_tables: HashSet<String>,
    /// Zuordnung alt → neu je Tabelle (frühere Importe und dieser)
    maps: HashMap<&'static str, HashMap<i64, i64>>,
    /// In diesem Lauf dazugekommen: (Tabelle, alt, neu, Dateien erledigt)
    fresh: Vec<(&'static str, i64, i64, bool)>,
    /// Vorhandene Nummern im Ziel je Tabelle
    exist: HashMap<&'static str, HashSet<i64>>,
    cols: HashMap<&'static str, HashSet<String>>,
    /// Bots, deren aktuelles Gespräch noch zu setzen ist: (Bot neu, Thread alt)
    bot_threads: Vec<(i64, i64)>,
    counts: ImportCounts,
}

impl Job<'_> {
    async fn read(&mut self, table: &str, order: &str) -> AppResult<Vec<Rec>> {
        if !self.src_tables.contains(table) {
            return Ok(Vec::new());
        }
        Ok(rows::read_all(&mut self.src, table, order).await?)
    }

    /// Vorhandene Nummern einer Tabelle des Ziels laden (einmal).
    async fn load_ids(&mut self, table: &'static str) -> AppResult<()> {
        if !self.exist.contains_key(table) {
            let ids: Vec<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT id FROM \"{table}\""))).fetch_all(&mut *self.dst).await?;
            self.exist.insert(table, ids.into_iter().map(|(i,)| i).collect());
        }
        Ok(())
    }

    /// Neue Nummer zu einer alten – nur wenn die Zeile hier (noch) existiert. Wer etwas
    /// Übernommenes in luCode gelöscht hat, bekommt es nicht zurück.
    fn resolve(&self, table: &'static str, old: Option<i64>) -> Option<i64> {
        let new = *self.maps.get(table)?.get(&old?)?;
        match self.exist.get(table) {
            Some(ids) => ids.contains(&new).then_some(new),
            None => Some(new),
        }
    }

    /// Schon einmal zugeordnet (auch wenn die Zeile hier inzwischen gelöscht ist)?
    fn seen(&self, table: &'static str, old: i64) -> bool {
        self.maps.get(table).is_some_and(|m| m.contains_key(&old))
    }

    fn remember(&mut self, table: &'static str, old: i64, new: i64, files_done: bool) {
        self.maps.entry(table).or_default().insert(old, new);
        self.exist.entry(table).or_default().insert(new);
        self.fresh.push((table, old, new, files_done || !FILE_TABLES.contains(&table)));
    }

    /// Nummern für neue Zeilen vergeben: die alte, wenn sie hier frei ist, sonst die nächste freie.
    /// So behalten Projekte und Chats nach Möglichkeit ihre Nummern (und Ordner).
    fn plan(&self, table: &'static str, olds: &[i64]) -> HashMap<i64, i64> {
        let mut taken: HashSet<i64> = self.exist.get(table).cloned().unwrap_or_default();
        // Auch Nummern, die ein früherer Import vergeben hat, bleiben vergeben
        if let Some(m) = self.maps.get(table) {
            taken.extend(m.values().copied());
        }
        let mut out = HashMap::new();
        let mut rest = Vec::new();
        for old in olds {
            if taken.insert(*old) {
                out.insert(*old, *old);
            } else {
                rest.push(*old);
            }
        }
        let mut next = taken.iter().copied().max().unwrap_or(0) + 1;
        for old in rest {
            out.insert(old, next);
            next += 1;
        }
        out
    }

    async fn put(&mut self, table: &'static str, rec: &Rec, or_ignore: bool) -> AppResult<Option<i64>> {
        if !self.cols.contains_key(table) {
            let c = rows::columns(&mut *self.dst, table).await?;
            self.cols.insert(table, c);
        }
        let cols = &self.cols[table];
        Ok(rows::insert(&mut *self.dst, table, rec, cols, or_ignore).await?)
    }

    /// Zeile mit fester Nummer einfügen und die Zuordnung merken.
    async fn put_as(&mut self, table: &'static str, mut rec: Rec, old: i64, new: i64, files_done: bool) -> AppResult<()> {
        rec.insert("id".into(), Val::Int(new));
        self.put(table, &rec, false).await?;
        self.remember(table, old, new, files_done);
        Ok(())
    }

    /// Zeile mit neuer, vom Ziel vergebener Nummer einfügen und die Zuordnung merken.
    async fn put_new(&mut self, table: &'static str, mut rec: Rec, old: i64) -> AppResult<i64> {
        rec.remove("id");
        let new = self.put(table, &rec, false).await?.ok_or_else(|| AppError::internal(format!("{table}: Zeile nicht eingefügt")))?;
        self.remember(table, old, new, true);
        Ok(new)
    }

    async fn load_maps(&mut self) -> AppResult<()> {
        let known: Vec<(String, i64, i64)> = sqlx::query_as("SELECT tbl, old_id, new_id FROM import_map WHERE source = ?").bind(&self.key).fetch_all(&mut *self.dst).await?;
        for (tbl, old, new) in known {
            // Nur Tabellen, die dieses Programm kennt
            if let Some(name) = MAPPED.iter().find(|t| **t == tbl) {
                self.maps.entry(name).or_default().insert(old, new);
            }
        }
        for t in ["users", "agent_accounts", "projects", "threads", "runs", "bots"] {
            self.load_ids(t).await?;
        }
        Ok(())
    }

    async fn save_maps(&mut self) -> AppResult<()> {
        for (tbl, old, new, files_done) in std::mem::take(&mut self.fresh) {
            sqlx::query("INSERT OR REPLACE INTO import_map (source, tbl, old_id, new_id, files_done, created_at) VALUES (?, ?, ?, ?, ?, ?)")
                .bind(&self.key)
                .bind(tbl)
                .bind(old)
                .bind(new)
                .bind(files_done)
                .bind(self.ts)
                .execute(&mut *self.dst)
                .await?;
        }
        Ok(())
    }

    // -------------------------------------------------------------- Tabellen

    /// Benutzer: gleicher Name → derselbe Benutzer; hat die Quelle nur einen, ist es der
    /// Importierende; sonst wird das Konto angelegt (mit dem Passwort aus nrzCode).
    async fn users(&mut self) -> AppResult<()> {
        let list = self.read("users", "id").await?;
        let single = list.len() == 1;
        let here: Vec<(i64, String)> = sqlx::query_as("SELECT id, lower_name FROM users").fetch_all(&mut *self.dst).await?;
        let mut create = Vec::new();
        for u in list {
            let Some(old) = u.id("id") else { continue };
            if self.seen("users", old) {
                continue;
            }
            let same = here.iter().find(|(_, l)| l.as_str() == u.str("lower_name")).map(|(id, _)| *id);
            match same.or(single.then_some(self.importer)) {
                Some(id) => {
                    // Leere Angaben aus nrzCode ergänzen (Sprache, Name, Profilbild)
                    sqlx::query(
                        "UPDATE users SET language = CASE WHEN language = '' THEN ? ELSE language END, \
                         full_name = CASE WHEN full_name = '' THEN ? ELSE full_name END, \
                         email = CASE WHEN email = '' THEN ? ELSE email END, \
                         avatar_at = CASE WHEN avatar_at = 0 THEN ? ELSE avatar_at END WHERE id = ?",
                    )
                    .bind(u.str("language"))
                    .bind(u.str("full_name"))
                    .bind(u.str("email"))
                    .bind(u.id("avatar_at").unwrap_or(0))
                    .bind(id)
                    .execute(&mut *self.dst)
                    .await?;
                    self.remember("users", old, id, false);
                }
                None => create.push((old, u)),
            }
        }
        let plan = self.plan("users", &create.iter().map(|(o, _)| *o).collect::<Vec<_>>());
        for (old, mut u) in create {
            // Das Thema „Mondnacht“ von nrzCode ist hier das dunkle Thema selbst
            if u.str("theme") == "mondnacht" {
                u.put("theme", "dark");
            }
            self.put_as("users", u, old, plan[&old], false).await?;
            self.counts.users += 1;
        }
        Ok(())
    }

    /// Verbindungen zu nrzGit, luGit und GitHub – je Benutzer und Plattform höchstens eine;
    /// eine hier schon eingerichtete bleibt.
    async fn connections(&mut self) -> AppResult<()> {
        for mut c in self.read("nrzgit_connections", "user_id").await? {
            let Some(user) = self.resolve("users", c.id("user_id")) else { continue };
            let kind = if c.contains_key("kind") { c.str("kind").to_string() } else { "nrzgit".to_string() };
            let has: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM nrzgit_connections WHERE user_id = ? AND kind = ?").bind(user).bind(&kind).fetch_optional(&mut *self.dst).await?;
            if has.is_some() {
                continue;
            }
            c.put("user_id", user);
            if self.put("nrzgit_connections", &c, true).await?.is_some() {
                self.counts.connections += 1;
            }
        }
        Ok(())
    }

    /// Agent-Konten (die Homes mit den Anmeldungen folgen im Datei-Teil).
    async fn accounts(&mut self) -> AppResult<()> {
        let mut list = Vec::new();
        for a in self.read("agent_accounts", "id").await? {
            let Some(old) = a.id("id") else { continue };
            if self.seen("agent_accounts", old) {
                continue;
            }
            let Some(user) = self.resolve("users", a.id("user_id")) else { continue };
            list.push((old, user, a));
        }
        let plan = self.plan("agent_accounts", &list.iter().map(|(o, _, _)| *o).collect::<Vec<_>>());
        for (old, user, mut a) in list {
            a.put("user_id", user);
            // Vorgabe-Konto nur, wenn es hier für diese Art noch keines gibt
            if a.id("is_default") == Some(1) {
                let has: Option<(i64,)> = sqlx::query_as("SELECT id FROM agent_accounts WHERE user_id = ? AND kind = ? AND is_default = 1 LIMIT 1")
                    .bind(user)
                    .bind(a.str("kind"))
                    .fetch_optional(&mut *self.dst)
                    .await?;
                if has.is_some() {
                    a.put("is_default", 0);
                }
            }
            self.put_as("agent_accounts", a, old, plan[&old], false).await?;
            self.counts.accounts += 1;
        }
        Ok(())
    }

    /// Skills: je Benutzer ist der Name eindeutig – ein gleichnamiger hier bleibt.
    async fn skills(&mut self) -> AppResult<()> {
        for mut s in self.read("agent_skills", "id").await? {
            let Some(old) = s.id("id") else { continue };
            if self.seen("agent_skills", old) {
                continue;
            }
            let Some(user) = self.resolve("users", s.id("user_id")) else { continue };
            let has: Option<(i64,)> = sqlx::query_as("SELECT id FROM agent_skills WHERE user_id = ? AND name = ?").bind(user).bind(s.str("name")).fetch_optional(&mut *self.dst).await?;
            if let Some((id,)) = has {
                self.remember("agent_skills", old, id, true);
                continue;
            }
            s.put("user_id", user);
            self.put_new("agent_skills", s, old).await?;
            self.counts.skills += 1;
        }
        Ok(())
    }

    /// Projekte. Bis ihre Dateien kopiert sind, gelten sie als „wird geholt“.
    async fn projects(&mut self) -> AppResult<()> {
        let mut list = Vec::new();
        for p in self.read("projects", "id").await? {
            let Some(old) = p.id("id") else { continue };
            if self.seen("projects", old) {
                continue;
            }
            let Some(user) = self.resolve("users", p.id("user_id")) else { continue };
            list.push((old, user, p));
        }
        let plan = self.plan("projects", &list.iter().map(|(o, _, _)| *o).collect::<Vec<_>>());
        for (old, user, mut p) in list {
            p.put("user_id", user);
            p.put("status", "cloning");
            p.put("status_message", COPYING);
            self.put_as("projects", p, old, plan[&old], false).await?;
            self.counts.projects += 1;
        }
        Ok(())
    }

    /// Bots (ihr aktuelles Gespräch wird gesetzt, sobald die Threads da sind).
    async fn bots(&mut self) -> AppResult<()> {
        let mut list = Vec::new();
        for b in self.read("bots", "id").await? {
            let Some(old) = b.id("id") else { continue };
            if self.seen("bots", old) {
                continue;
            }
            let Some(user) = self.resolve("users", b.id("user_id")) else { continue };
            list.push((old, user, b));
        }
        let plan = self.plan("bots", &list.iter().map(|(o, _, _)| *o).collect::<Vec<_>>());
        for (old, user, mut b) in list {
            let new = plan[&old];
            if let Some(thread) = b.id("thread_id") {
                self.bot_threads.push((new, thread));
            }
            b.put("user_id", user);
            b.put("account_id", self.resolve("agent_accounts", b.id("account_id")));
            b.put("thread_id", Val::Null);
            self.put_as("bots", b, old, new, false).await?;
            self.counts.bots += 1;
        }
        Ok(())
    }

    /// Threads: Chats in Projekten und ohne Projekt (Projekte & Chats) bzw. Gespräche mit Bots
    /// (Extras). Ein Thread kommt nur mit, wenn sein Projekt, sein Bot und sein Eltern-Thread
    /// hier sind.
    async fn threads(&mut self) -> AppResult<()> {
        struct Take {
            old: i64,
            parent: Option<i64>,
            rec: Rec,
        }
        let mut take: Vec<Take> = Vec::new();
        let mut coming: HashSet<i64> = HashSet::new();
        for mut t in self.read("threads", "id").await? {
            let Some(old) = t.id("id") else { continue };
            if self.seen("threads", old) {
                continue;
            }
            let bot = t.id("bot_id");
            if (bot.is_some() && !self.opts.extras) || (bot.is_none() && !self.opts.workspaces) {
                continue;
            }
            let Some(user) = self.resolve("users", t.id("user_id")) else { continue };
            let project = t.id("project_id");
            let new_project = self.resolve("projects", project);
            let new_bot = self.resolve("bots", bot);
            if (project.is_some() && new_project.is_none()) || (bot.is_some() && new_bot.is_none()) {
                continue;
            }
            let parent = t.id("parent_id");
            if let Some(p) = parent {
                if !coming.contains(&p) && self.resolve("threads", Some(p)).is_none() {
                    continue;
                }
            }
            t.put("user_id", user);
            t.put("project_id", new_project);
            t.put("bot_id", new_bot);
            t.put("parent_id", Val::Null);
            t.put("account_id", self.resolve("agent_accounts", t.id("account_id")));
            // Was in nrzCode gerade lief, läuft hier nicht: ruhig stellen, nichts von selbst fortsetzen
            if matches!(t.str("status"), "working" | "waiting") {
                t.put("status", "idle");
            }
            t.put("resume_at", Val::Null);
            t.put("resume_attempts", 0);
            t.put("queued", "[]");
            coming.insert(old);
            take.push(Take { old, parent, rec: t });
        }
        let plan = self.plan("threads", &take.iter().map(|t| t.old).collect::<Vec<_>>());
        let mut parents = Vec::new();
        for Take { old, parent, mut rec } in take {
            let new = plan[&old];
            let path = match (rec.id("use_worktree"), rec.id("project_id")) {
                (Some(1), Some(p)) if !rec.str("worktree_path").is_empty() => {
                    self.dst_root.join("workspaces").join(p.to_string()).join("worktrees").join(new.to_string()).to_string_lossy().to_string()
                }
                _ => String::new(),
            };
            rec.put("worktree_path", path);
            // Nur Chats ohne Projekt und ohne Bot haben einen eigenen Ordner (scratch)
            let own_dir = rec.id("project_id").is_none() && rec.id("bot_id").is_none();
            self.put_as("threads", rec, old, new, !own_dir).await?;
            self.counts.threads += 1;
            if let Some(p) = parent {
                parents.push((new, p));
            }
        }
        for (new, old_parent) in parents {
            if let Some(parent) = self.resolve("threads", Some(old_parent)) {
                sqlx::query("UPDATE threads SET parent_id = ? WHERE id = ?").bind(parent).bind(new).execute(&mut *self.dst).await?;
            }
        }
        Ok(())
    }

    /// Läufe. Der Trigger an `runs` schreibt je Lauf eine Zeile in `usage_log` – die kommt
    /// gleich aus der Quelle, darum werden die vom Trigger angelegten wieder entfernt.
    async fn runs(&mut self) -> AppResult<()> {
        let usage_before: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM usage_log").fetch_one(&mut *self.dst).await?;
        let mut after = 0;
        loop {
            if !self.src_tables.contains("runs") {
                break;
            }
            let chunk = rows::read_chunk(&mut self.src, "runs", after, 500).await?;
            let Some(last) = chunk.last().and_then(|r| r.id("id")) else { break };
            after = last;
            for mut r in chunk {
                let Some(old) = r.id("id") else { continue };
                if self.seen("runs", old) {
                    continue;
                }
                let Some(thread) = self.resolve("threads", r.id("thread_id")) else { continue };
                r.put("thread_id", thread);
                r.put("account_id", self.resolve("agent_accounts", r.id("account_id")));
                if matches!(r.str("status"), "queued" | "running") {
                    r.put("status", "error");
                    r.put("error", "Lief beim Umzug aus nrzCode noch");
                    r.put("finished_at", self.ts);
                }
                self.put_new("runs", r, old).await?;
            }
        }
        if self.src_tables.contains("usage_log") {
            sqlx::query("DELETE FROM usage_log WHERE id > ?").bind(usage_before).execute(&mut *self.dst).await?;
        }
        Ok(())
    }

    /// Nachrichten, blockweise. In „Subagenten“-Nachrichten folgen die Nummern der Kinder.
    async fn messages(&mut self) -> AppResult<()> {
        let mut after = 0;
        loop {
            let chunk = rows::read_chunk(&mut self.src, "messages", after, 400).await?;
            let Some(last) = chunk.last().and_then(|r| r.id("id")) else { break };
            after = last;
            for mut m in chunk {
                let Some(old) = m.id("id") else { continue };
                if self.seen("messages", old) {
                    continue;
                }
                let Some(thread) = self.resolve("threads", m.id("thread_id")) else { continue };
                m.put("thread_id", thread);
                m.put("run_id", self.resolve("runs", m.id("run_id")));
                if m.str("role") == "subagents" {
                    if let Ok(mut data) = serde_json::from_str::<serde_json::Value>(m.str("data")) {
                        if let Some(children) = data.get("children").and_then(|c| c.as_array()).cloned() {
                            // Titel stehen an derselben Stelle wie die Kinder – beide zusammen filtern
                            let titles = data.get("titles").and_then(|t| t.as_array()).cloned().unwrap_or_default();
                            let (mut ids, mut names) = (Vec::new(), Vec::new());
                            for (i, c) in children.iter().enumerate() {
                                if let Some(new) = self.resolve("threads", c.as_i64()) {
                                    ids.push(serde_json::json!(new));
                                    if let Some(t) = titles.get(i) {
                                        names.push(t.clone());
                                    }
                                }
                            }
                            data["children"] = serde_json::Value::Array(ids);
                            if !titles.is_empty() {
                                data["titles"] = serde_json::Value::Array(names);
                            }
                            m.put("data", data.to_string());
                        }
                    }
                }
                self.put_new("messages", m, old).await?;
                self.counts.messages += 1;
            }
        }
        Ok(())
    }

    /// Was sonst an Threads hängt: Seiten-Chats (/btw) und Pull Requests.
    async fn thread_children(&mut self) -> AppResult<()> {
        for table in ["btw_entries", "workspace_pull_requests"] {
            for mut r in self.read(table, "id").await? {
                let Some(old) = r.id("id") else { continue };
                if self.seen(table, old) {
                    continue;
                }
                let Some(thread) = self.resolve("threads", r.id("thread_id")) else { continue };
                r.put("thread_id", thread);
                self.put_new(table, r, old).await?;
            }
        }
        Ok(())
    }

    /// Anhänge (die Dateien folgen im Datei-Teil).
    async fn attachments(&mut self) -> AppResult<()> {
        for mut a in self.read("attachments", "created_at").await? {
            let Some(user) = self.resolve("users", a.id("user_id")) else { continue };
            a.put("user_id", user);
            if self.put("attachments", &a, true).await?.is_some() {
                self.counts.attachments += 1;
            }
        }
        Ok(())
    }

    /// Nutzung (Tokens, Kosten, Limits) – auch die von inzwischen gelöschten Chats.
    async fn usage(&mut self) -> AppResult<()> {
        for mut u in self.read("usage_log", "id").await? {
            let Some(old) = u.id("id") else { continue };
            if self.seen("usage_log", old) {
                continue;
            }
            let Some(user) = self.resolve("users", u.id("user_id")) else { continue };
            u.put("user_id", user);
            u.put("run_id", self.resolve("runs", u.id("run_id")));
            u.put("thread_id", self.resolve("threads", u.id("thread_id")));
            u.put("account_id", self.resolve("agent_accounts", u.id("account_id")));
            // Sitzungs-Schlüssel sind nur je Instanz eindeutig
            if !u.str("thread_key").is_empty() {
                u.put("thread_key", format!("n{}:{}", &self.key[..6], u.str("thread_key")));
            }
            if matches!(u.str("status"), "queued" | "running") {
                u.put("status", "error");
                u.put("finished_at", self.ts);
            }
            self.put_new("usage_log", u, old).await?;
        }
        Ok(())
    }

    /// Team: ob es für ein Projekt eingeschaltet ist und was die Agenten schon gelesen haben.
    /// Den Stand selbst holt der nächste Abgleich aus dem Repository.
    async fn team(&mut self) -> AppResult<()> {
        for mut t in self.read("team_projects", "project_id").await? {
            let Some(project) = self.resolve("projects", t.id("project_id")) else { continue };
            t.put("project_id", project);
            t.put("remote_sha", "");
            t.put("doc", "{}");
            t.put("presence_at", 0);
            t.put("synced_at", 0);
            t.put("hot_until", 0);
            t.put("error", "");
            self.put("team_projects", &t, true).await?;
        }
        for mut s in self.read("team_seen", "created_at").await? {
            let Some(project) = self.resolve("projects", s.id("project_id")) else { continue };
            let thread = match s.id("thread_id") {
                Some(0) | None => 0,
                Some(t) => match self.resolve("threads", Some(t)) {
                    Some(n) => n,
                    None => continue,
                },
            };
            let enabled: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM team_projects WHERE project_id = ?").bind(project).fetch_optional(&mut *self.dst).await?;
            if enabled.is_none() {
                continue;
            }
            s.put("project_id", project);
            s.put("thread_id", thread);
            self.put("team_seen", &s, true).await?;
        }
        Ok(())
    }

    /// Notizblock.
    async fn notes(&mut self) -> AppResult<()> {
        for mut n in self.read("prompt_notes", "id").await? {
            let Some(old) = n.id("id") else { continue };
            if self.seen("prompt_notes", old) {
                continue;
            }
            let Some(user) = self.resolve("users", n.id("user_id")) else { continue };
            n.put("user_id", user);
            n.put("project_id", self.resolve("projects", n.id("project_id")));
            self.put_new("prompt_notes", n, old).await?;
            self.counts.notes += 1;
        }
        Ok(())
    }

    /// Routinen der Bots.
    async fn routines(&mut self) -> AppResult<()> {
        for mut r in self.read("bot_routines", "id").await? {
            let Some(old) = r.id("id") else { continue };
            if self.seen("bot_routines", old) {
                continue;
            }
            let Some(bot) = self.resolve("bots", r.id("bot_id")) else { continue };
            r.put("bot_id", bot);
            self.put_new("bot_routines", r, old).await?;
        }
        Ok(())
    }

    /// Canvas: Fenster (mit den neuen Nummern der Chats) und die Zeichenebene. Eine Fläche,
    /// die es hier für den Arbeitsbereich schon gibt, bleibt.
    async fn boards(&mut self) -> AppResult<()> {
        for mut b in self.read("canvas_boards", "user_id").await? {
            let Some(user) = self.resolve("users", b.id("user_id")) else { continue };
            let project = match b.id("project_key") {
                Some(0) | None => 0,
                Some(p) => match self.resolve("projects", Some(p)) {
                    Some(n) => n,
                    None => continue,
                },
            };
            let has: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM canvas_boards WHERE user_id = ? AND project_key = ?").bind(user).bind(project).fetch_optional(&mut *self.dst).await?;
            if has.is_some() {
                continue;
            }
            b.put("user_id", user);
            b.put("project_key", project);
            if let Ok(mut layout) = serde_json::from_str::<serde_json::Value>(b.str("layout")) {
                if let Some(windows) = layout.get_mut("windows").and_then(|w| w.as_array_mut()) {
                    let mut keep = Vec::new();
                    for mut w in std::mem::take(windows) {
                        match w.get("threadId").and_then(|t| t.as_i64()) {
                            // Fenster eines Chats: nur mit dem Chat
                            Some(old) => {
                                let Some(new) = self.resolve("threads", Some(old)) else { continue };
                                w["threadId"] = serde_json::json!(new);
                                w["id"] = serde_json::json!(format!("t{new}"));
                            }
                            // Terminals gehören zur laufenden Instanz – sie kommen nicht mit
                            None if w.get("kind").and_then(|k| k.as_str()) == Some("terminal") => continue,
                            None => {}
                        }
                        keep.push(w);
                    }
                    *windows = keep;
                }
                if let Some(closed) = layout.get_mut("closed").and_then(|c| c.as_array_mut()) {
                    let ids: Vec<serde_json::Value> = closed.iter().filter_map(|c| self.resolve("threads", c.as_i64())).map(|n| serde_json::json!(n)).collect();
                    *closed = ids;
                }
                b.put("layout", layout.to_string());
            }
            if self.put("canvas_boards", &b, true).await?.is_some() {
                self.counts.boards += 1;
            }
        }
        Ok(())
    }

    async fn dest_setting(&mut self, key: &str) -> AppResult<String> {
        let v: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(&mut *self.dst).await?;
        Ok(v.map(|(v,)| v).unwrap_or_default())
    }

    async fn set_dest_setting(&mut self, key: &str, value: &str) -> AppResult<()> {
        sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
            .bind(key)
            .bind(value)
            .execute(&mut *self.dst)
            .await?;
        Ok(())
    }

    /// Einstellungen je Benutzer: KI-Ketten und Sprachsteuerung (mit den neuen Nummern der Konten).
    /// Was hier schon eingestellt ist, bleibt. Die Kopplung mit dem Handy kommt bewusst nicht mit:
    /// Zwei Programme dürfen sich nicht als derselbe PC melden.
    async fn user_settings(&mut self) -> AppResult<()> {
        let settings: HashMap<String, String> = self.read("settings", "key").await?.into_iter().map(|r| (r.str("key").to_string(), r.str("value").to_string())).collect();
        let users: Vec<(i64, i64)> = self.maps.get("users").map(|m| m.iter().map(|(o, n)| (*o, *n)).collect()).unwrap_or_default();
        // Vorgabe-Adresse von nrzGit der Instanz, wenn hier noch keine steht
        if let Some(url) = settings.get("nrzgit_url").filter(|u| !u.is_empty()) {
            if self.dest_setting("nrzgit_url").await?.is_empty() {
                self.set_dest_setting("nrzgit_url", url).await?;
            }
        }
        for (old, new) in users {
            if let Some(raw) = settings.get(&format!("chains:{old}")) {
                let key = format!("chains:{new}");
                let have = self.dest_setting(&key).await?;
                if have.is_empty() || have == "[]" {
                    if let Ok(mut chains) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
                        for chain in chains.iter_mut() {
                            let steps: Vec<serde_json::Value> = chain
                                .get("steps")
                                .and_then(|s| s.as_array())
                                .map(|steps| {
                                    steps
                                        .iter()
                                        .filter_map(|s| {
                                            let account = self.resolve("agent_accounts", s.get("accountId").and_then(|a| a.as_i64()))?;
                                            let mut s = s.clone();
                                            s["accountId"] = serde_json::json!(account);
                                            Some(s)
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            chain["steps"] = serde_json::Value::Array(steps);
                        }
                        // Eine Kette braucht mindestens zwei Stufen
                        chains.retain(|c| c.get("steps").and_then(|s| s.as_array()).is_some_and(|s| s.len() >= 2));
                        if !chains.is_empty() {
                            self.set_dest_setting(&key, &serde_json::Value::Array(chains).to_string()).await?;
                        }
                    }
                }
            }
            if let Some(raw) = settings.get(&format!("voice:{old}")) {
                let key = format!("voice:{new}");
                if self.dest_setting(&key).await?.is_empty() {
                    if let Ok(mut voice) = serde_json::from_str::<serde_json::Value>(raw) {
                        if voice.is_object() {
                            let account = self.resolve("agent_accounts", voice.get("accountId").and_then(|a| a.as_i64()));
                            voice["accountId"] = account.map(|a| serde_json::json!(a)).unwrap_or(serde_json::Value::Null);
                            self.set_dest_setting(&key, &voice.to_string()).await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Bots zeigen auf ihr aktuelles Gespräch.
    async fn finish_bots(&mut self) -> AppResult<()> {
        for (bot, old_thread) in std::mem::take(&mut self.bot_threads) {
            if let Some(thread) = self.resolve("threads", Some(old_thread)) {
                sqlx::query("UPDATE bots SET thread_id = ? WHERE id = ?").bind(thread).bind(bot).execute(&mut *self.dst).await?;
            }
        }
        Ok(())
    }

    async fn run(&mut self) -> AppResult<()> {
        self.load_maps().await?;
        self.users().await?;
        if self.opts.connections {
            self.connections().await?;
        }
        if self.opts.accounts {
            self.accounts().await?;
        }
        if self.opts.extras {
            self.skills().await?;
            self.bots().await?;
        }
        if self.opts.workspaces {
            self.projects().await?;
        }
        self.threads().await?;
        self.runs().await?;
        self.messages().await?;
        self.thread_children().await?;
        self.attachments().await?;
        if self.opts.workspaces {
            self.usage().await?;
            self.team().await?;
        }
        if self.opts.extras {
            self.notes().await?;
            self.routines().await?;
            self.boards().await?;
            self.user_settings().await?;
        }
        self.finish_bots().await?;
        self.save_maps().await
    }
}

/// Tabellen mit Zuordnung alt → neu.
const MAPPED: &[&str] = &[
    "users",
    "agent_accounts",
    "agent_skills",
    "projects",
    "bots",
    "threads",
    "runs",
    "messages",
    "btw_entries",
    "workspace_pull_requests",
    "usage_log",
    "prompt_notes",
    "bot_routines",
];

// ------------------------------------------------------------------ Dateien

/// Ordner kopieren (blockierend, mit Fortschritt). Fehlt die Quelle, geschieht nichts.
async fn copy(from: PathBuf, to: PathBuf, skip_build: bool, progress: Arc<Progress>) -> CopyReport {
    if !from.exists() {
        return CopyReport::default();
    }
    let label = from.display().to_string();
    tokio::task::spawn_blocking(move || {
        let mut report = CopyReport::default();
        if let Err(e) = files::copy_tree(&from, &to, skip_build, &progress, &mut report) {
            report.failed_count += 1;
            report.failed.push(format!("{}: {e}", to.display()));
        }
        report
    })
    .await
    .unwrap_or_else(|e| CopyReport { failed_count: 1, failed: vec![format!("{label}: {e}")], ..Default::default() })
}

fn report_notes(what: &str, report: &CopyReport) {
    if report.skipped_dirs > 0 {
        note(format!("{what}: {} Build-Ordner ausgelassen", report.skipped_dirs));
    }
    if report.failed_count > 0 {
        note(format!("{what}: {} Einträge liessen sich nicht kopieren ({})", report.failed_count, report.failed.join("; ")));
    }
}

/// Worktrees eines kopierten Projekts auf die neuen Nummern der Chats bringen und ihre Verweise
/// auf das Repository hier stellen.
fn fix_worktrees(project_dir: &Path, threads: &HashMap<i64, i64>) -> Vec<String> {
    let mut problems = Vec::new();
    let dir = project_dir.join("worktrees");
    let Ok(entries) = std::fs::read_dir(&dir) else { return problems };
    // Erst alle beiseite, dann an den neuen Namen – zwei Nummern können getauscht sein
    let mut moves = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(old) = name.parse::<i64>() else { continue };
        match threads.get(&old) {
            Some(new) if *new != old => {
                let parked = dir.join(format!(".umzug-{old}"));
                if std::fs::rename(e.path(), &parked).is_ok() {
                    moves.push((parked, dir.join(new.to_string())));
                }
            }
            _ => {}
        }
    }
    for (from, to) in moves {
        if to.exists() || std::fs::rename(&from, &to).is_err() {
            problems.push(format!("Worktree {} liess sich nicht umbenennen", to.display()));
        }
    }
    let repo = project_dir.join("repo");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                if let Err(err) = files::relink_worktree(&repo, &e.path()) {
                    problems.push(format!("Worktree {}: {err}", e.path().display()));
                }
            }
        }
    }
    problems
}

async fn mark_files_done(state: &AppState, key: &str, table: &str, old: i64) {
    let _ = sqlx::query("UPDATE import_map SET files_done = 1 WHERE source = ? AND tbl = ? AND old_id = ?").bind(key).bind(table).bind(old).execute(&state.db).await;
}

/// Zuordnungen dieser Quelle für die Ordner (alle Importe).
async fn id_maps(state: &AppState, key: &str) -> AppResult<IdMaps> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as("SELECT tbl, old_id, new_id FROM import_map WHERE source = ? AND tbl IN ('projects', 'threads', 'bots')").bind(key).fetch_all(&state.db).await?;
    let mut maps = IdMaps::default();
    for (tbl, old, new) in rows {
        match tbl.as_str() {
            "projects" => maps.projects.insert(old, new),
            "threads" => maps.threads.insert(old, new),
            _ => maps.bots.insert(old, new),
        };
    }
    Ok(maps)
}

/// Der Datei-Teil: alles, dessen Ordner noch nicht kopiert sind (auch aus einem früheren,
/// abgebrochenen Import).
async fn copy_files(state: &AppState, src_root: &Path, key: &str, opts: &ImportOptions, mut src: SqliteConnection) -> AppResult<()> {
    let dst_root = files::absolute(&state.config.data_dir);
    let progress = PROGRESS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let pending: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT tbl, old_id, new_id FROM import_map WHERE source = ? AND files_done = 0 \
         ORDER BY CASE tbl WHEN 'users' THEN 0 WHEN 'agent_accounts' THEN 1 WHEN 'projects' THEN 2 WHEN 'threads' THEN 3 ELSE 4 END, old_id",
    )
    .bind(key)
    .fetch_all(&state.db)
    .await?;
    status_mut(|s| s.total = 1 + pending.len() as i64 + 3);
    let maps = id_maps(state, key).await?;
    // Stand der Projekte in der Quelle (Name für die Anzeige, Status zum Freigeben)
    let projects: HashMap<i64, (String, String, String)> = if rows::tables(&mut src).await?.contains("projects") {
        rows::read_all(&mut src, "projects", "id")
            .await?
            .into_iter()
            .filter_map(|p| Some((p.id("id")?, (p.str("name").to_string(), p.str("status").to_string(), p.str("status_message").to_string()))))
            .collect()
    } else {
        HashMap::new()
    };
    // Quelle gleich wieder schliessen: Ein offener Lesevorgang hielte in nrzCode das Aufräumen des
    // Journals (WAL) auf, solange hier kopiert wird
    drop(src);
    let mut new_homes: Vec<PathBuf> = Vec::new();

    for (table, old, new) in pending {
        match table.as_str() {
            "users" => {
                step("Profilbilder und Anmeldungen an Registries", "");
                let avatar = src_root.join("avatars").join(old.to_string());
                let target = dst_root.join("avatars").join(new.to_string());
                if avatar.is_file() && !target.exists() {
                    tokio::fs::create_dir_all(dst_root.join("avatars")).await?;
                    let _ = tokio::fs::copy(&avatar, &target).await;
                }
                let docker = dst_root.join("docker").join(new.to_string());
                if !docker.exists() {
                    let r = copy(src_root.join("docker").join(old.to_string()), docker, false, progress.clone()).await;
                    report_notes("Docker-Anmeldung", &r);
                }
            }
            "agent_accounts" => {
                let label: Option<(String, String)> = sqlx::query_as("SELECT kind, label FROM agent_accounts WHERE id = ?").bind(new).fetch_optional(&state.db).await?;
                let Some((kind, label)) = label else {
                    mark_files_done(state, key, &table, old).await;
                    continue;
                };
                step("Agent-Konto mit Anmeldung", &format!("{label} ({kind})"));
                let target = dst_root.join("accounts").join(new.to_string());
                let r = copy(src_root.join("accounts").join(old.to_string()), target.clone(), false, progress.clone()).await;
                report_notes(&format!("Konto {label}"), &r);
                new_homes.push(target.join("home"));
            }
            "projects" => {
                let (name, src_status, src_message) = projects.get(&old).cloned().unwrap_or_default();
                step("Projekt", &name);
                let from = src_root.join("workspaces").join(old.to_string());
                let target = dst_root.join("workspaces").join(new.to_string());
                let existed = from.is_dir();
                let r = copy(from, target.clone(), opts.skip_build_dirs, progress.clone()).await;
                report_notes(&format!("Projekt {name}"), &r);
                let threads = maps.threads.clone();
                let dir = target.clone();
                for problem in tokio::task::spawn_blocking(move || fix_worktrees(&dir, &threads)).await.unwrap_or_default() {
                    note(problem);
                }
                // Freigeben – mit dem Stand aus nrzCode, sofern das Repository wirklich da ist
                let (status, message) = if !existed {
                    ("error".to_string(), "Der Ordner des Projekts fehlte in nrzCode.".to_string())
                } else if target.join("repo").is_dir() && (src_status == "ready" || src_status == "cloning" || src_status.is_empty()) {
                    ("ready".to_string(), String::new())
                } else {
                    (src_status, src_message)
                };
                sqlx::query("UPDATE projects SET status = ?, status_message = ?, updated_at = ? WHERE id = ? AND status = 'cloning' AND status_message = ?")
                    .bind(&status)
                    .bind(&message)
                    .bind(now())
                    .bind(new)
                    .bind(COPYING)
                    .execute(&state.db)
                    .await?;
            }
            "threads" => {
                step("Arbeitsordner der Chats ohne Projekt", "");
                let r = copy(src_root.join("scratch").join(old.to_string()), dst_root.join("scratch").join(new.to_string()), opts.skip_build_dirs, progress.clone()).await;
                report_notes("Arbeitsordner", &r);
            }
            "bots" => {
                step("Arbeitsordner der Bots", "");
                let r = copy(src_root.join("bots").join(old.to_string()), dst_root.join("bots").join(new.to_string()), opts.skip_build_dirs, progress.clone()).await;
                report_notes("Bot", &r);
            }
            _ => {}
        }
        mark_files_done(state, key, &table, old).await;
        status_mut(|s| s.done += 1);
    }

    // Anhänge: jede Datei der Quelle, deren Eintrag es hier gibt
    step("Anhänge", "");
    let uuids: Vec<(String,)> = sqlx::query_as("SELECT uuid FROM attachments").fetch_all(&state.db).await?;
    let from_dir = src_root.join("attachments");
    let to_dir = state.config.attachments_dir();
    tokio::fs::create_dir_all(&to_dir).await?;
    for (uuid,) in uuids {
        let (from, to) = (from_dir.join(&uuid), to_dir.join(&uuid));
        if from.is_file() && !to.exists() {
            if let Ok(n) = tokio::fs::copy(&from, &to).await {
                progress.files.fetch_add(1, Ordering::Relaxed);
                progress.bytes.fetch_add(n as i64, Ordering::Relaxed);
            }
        }
    }
    status_mut(|s| s.done += 1);

    // Werkzeuge (Agent-CLIs) und Sprachmodelle – nur, was hier fehlt
    if opts.tools {
        step("Werkzeuge und Sprachmodelle", "");
        let tools = state.config.tools_dir();
        let empty = std::fs::read_dir(&tools).map(|mut d| d.next().is_none()).unwrap_or(true);
        if empty {
            let r = copy(src_root.join("tools"), tools, false, progress.clone()).await;
            report_notes("Werkzeuge", &r);
        } else if src_root.join("tools").is_dir() {
            note("Werkzeuge: hier sind schon welche installiert – die aus nrzCode wurden nicht kopiert".into());
        }
        if let Ok(mut models) = tokio::fs::read_dir(src_root.join("models")).await {
            while let Ok(Some(m)) = models.next_entry().await {
                let target = state.config.models_dir().join(m.file_name());
                if m.path().is_dir() && !target.exists() {
                    let r = copy(m.path(), target, false, progress.clone()).await;
                    report_notes("Sprachmodell", &r);
                }
            }
        }
    }
    status_mut(|s| s.done += 1);

    // Sitzungen von Claude Code in den neu kopierten Homes unter die neuen Arbeitsordner legen
    step("Sitzungen der Agenten", "");
    for home in new_homes {
        let (s, d, m) = (src_root.to_path_buf(), dst_root.clone(), maps.clone());
        let _ = tokio::task::spawn_blocking(move || files::move_claude_sessions(&home, &files::absolute(&s), &d, &m)).await;
    }
    check_sessions(state, key).await?;
    status_mut(|s| s.done += 1);
    Ok(())
}

/// Arbeitsordner eines Threads, ohne etwas anzulegen: eigener Worktree (wenn vorhanden), sonst der
/// Hauptcheckout des Projekts, der Ordner des Bots oder `scratch/<thread>`.
async fn workdir_of(state: &AppState, thread: i64) -> AppResult<Option<PathBuf>> {
    let row: Option<(Option<i64>, bool, Option<i64>)> = sqlx::query_as("SELECT project_id, use_worktree, bot_id FROM threads WHERE id = ?").bind(thread).fetch_optional(&state.db).await?;
    let Some((project, use_worktree, bot)) = row else { return Ok(None) };
    let data = &state.config.data_dir;
    Ok(Some(match (project, bot) {
        (Some(p), _) => {
            let worktree = data.join("workspaces").join(p.to_string()).join("worktrees").join(thread.to_string());
            if use_worktree && worktree.join(".git").exists() {
                worktree
            } else {
                data.join("workspaces").join(p.to_string()).join("repo")
            }
        }
        (None, Some(b)) => data.join("bots").join(b.to_string()),
        (None, None) => data.join("scratch").join(thread.to_string()),
    }))
}

/// Sitzungen der übernommenen Chats prüfen: Eine Sitzung bleibt nur stehen, wenn das CLI sie hier
/// auch findet (Claude Code: Datei unter dem neuen Arbeitsordner; Codex: sein Home wurde kopiert).
/// Sonst beginnt der nächste Lauf eine neue Sitzung und bekommt den bisherigen Verlauf mit.
async fn check_sessions(state: &AppState, key: &str) -> AppResult<()> {
    let threads: Vec<(i64, String, Option<i64>, String)> = sqlx::query_as(
        "SELECT t.id, t.agent_kind, t.account_id, t.provider_session_id FROM threads t \
         JOIN import_map m ON m.tbl = 'threads' AND m.new_id = t.id AND m.source = ? WHERE t.provider_session_id <> ''",
    )
    .bind(key)
    .fetch_all(&state.db)
    .await?;
    let mut cleared = 0;
    for (id, kind, account, session) in threads {
        let home = account.map(|a| state.config.accounts_dir().join(a.to_string()).join("home"));
        let keep = match (kind.as_str(), &home) {
            ("claude", Some(home)) => match workdir_of(state, id).await? {
                Some(dir) => files::claude_session_exists(home, &files::absolute(&dir), &session),
                None => false,
            },
            ("codex", Some(home)) => home.join(".codex").is_dir(),
            _ => false,
        };
        if !keep {
            sqlx::query("UPDATE threads SET provider_session_id = '' WHERE id = ?").bind(id).execute(&state.db).await?;
            cleared += 1;
        }
    }
    if cleared > 0 {
        note(format!("{cleared} Chats setzen mit dem bisherigen Verlauf fort (ihre Sitzung liess sich nicht übernehmen)"));
    }
    Ok(())
}

// ------------------------------------------------------------------ Ablauf

/// Import ausführen: Datenbank (eine Transaktion), dann Dateien.
async fn run(state: &AppState, importer: i64, opts: ImportOptions) -> AppResult<ImportCounts> {
    let dir = source_dir(state, &opts.path).ok_or_else(|| AppError::bad("Kein Datenordner von nrzCode gefunden – bitte den Pfad angeben."))?;
    step("Datenbank von nrzCode lesen", "");
    let mut src = open_source(state, &dir).await?;
    let key = source_key(&mut src, &dir).await;
    let src_tables = rows::tables(&mut src).await?;
    let ts = now();
    let run_id: i64 = sqlx::query_scalar("INSERT INTO import_runs (source, path, user_id, started_at) VALUES (?, ?, ?, ?) RETURNING id")
        .bind(&key)
        .bind(dir.to_string_lossy().to_string())
        .bind(importer)
        .bind(ts)
        .fetch_one(&state.db)
        .await?;

    let result = async {
        step("Konten, Projekte und Chats übernehmen", "");
        let mut tx = state.db.begin().await?;
        let counts = {
            let mut job = Job {
                src,
                dst: &mut *tx,
                key: key.clone(),
                opts: opts.clone(),
                importer,
                ts,
                dst_root: files::absolute(&state.config.data_dir),
                src_tables,
                maps: HashMap::new(),
                fresh: Vec::new(),
                exist: HashMap::new(),
                cols: HashMap::new(),
                bot_threads: Vec::new(),
                counts: ImportCounts::default(),
            };
            job.run().await?;
            job.counts
        };
        tx.commit().await?;
        status_mut(|s| {
            s.imported = counts;
            s.done = 1;
        });
        // Für den Datei-Teil neu lesen (der Stand der Quelle ist derselbe oder neuer)
        let src = open_source(state, &dir).await?;
        copy_files(state, &dir, &key, &opts, src).await?;
        Ok::<ImportCounts, AppError>(counts)
    }
    .await;

    let (summary, error) = match &result {
        Ok(c) => (serde_json::to_string(c).unwrap_or_default(), String::new()),
        Err(e) => ("{}".to_string(), e.to_string()),
    };
    let _ = sqlx::query("UPDATE import_runs SET summary = ?, error = ?, finished_at = ? WHERE id = ?").bind(summary).bind(error).bind(now()).bind(run_id).execute(&state.db).await;
    result
}

// ------------------------------------------------------------------ Routen

/// `GET /import/nrzcode?path=` – nrzCode finden (ohne `path`: an den üblichen Orten) und zählen.
#[get("/import/nrzcode?<path>")]
async fn source(state: &State<AppState>, _admin: AdminUser, path: Option<String>) -> Json<ImportSource> {
    Json(describe(state, path.as_deref().unwrap_or("")).await)
}

/// `GET /import/nrzcode/status` – Stand des laufenden bzw. letzten Imports.
#[get("/import/nrzcode/status")]
fn status(_admin: AdminUser) -> Json<ImportStatus> {
    Json(current_status())
}

/// `POST /import/nrzcode` – Import starten (läuft im Hintergrund, Stand über `…/status`).
#[post("/import/nrzcode", data = "<body>")]
async fn start(state: &State<AppState>, admin: AdminUser, body: Json<ImportOptions>) -> AppResult<Json<ImportStatus>> {
    let opts = body.into_inner();
    // Vorab prüfen, damit Fehler gleich als Antwort kommen
    let dir = source_dir(state, &opts.path).ok_or_else(|| AppError::bad("Kein Datenordner von nrzCode gefunden – bitte den Pfad angeben."))?;
    drop(open_source(state, &dir).await?);
    {
        let mut s = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        if s.is_running() {
            return Err(AppError::conflict("Es läuft schon ein Import."));
        }
        *s = ImportStatus { state: "running".into(), step: "Vorbereiten".into(), total: 1, started_at: now(), ..Default::default() };
    }
    *PROGRESS.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(Progress::default());
    let state: AppState = state.inner().clone();
    let importer = admin.0.id;
    tokio::spawn(async move {
        let result = run(&state, importer, opts).await;
        status_mut(|s| {
            s.finished_at = now();
            match result {
                Ok(counts) => {
                    s.state = "done".into();
                    s.imported = counts;
                    s.step = "Fertig".into();
                    s.detail.clear();
                    s.done = s.total;
                }
                Err(e) => {
                    tracing::error!("Import aus nrzCode: {e}");
                    s.state = "error".into();
                    s.error = e.public_message();
                }
            }
        });
    });
    Ok(Json(current_status()))
}

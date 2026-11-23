//! Umzug von nrzCode: Quelle erkennen, alles übernehmen (Zeilen mit neuen Nummern, Verweise in
//! JSON, Dateien, Worktrees, Sitzungen von Claude Code) und wiederholen, ohne etwas doppelt
//! anzulegen.
//!
//! Die „nrzCode-Instanz“ ist ein zweiter Datenordner mit demselben Schema (die Migrationen
//! 0001–0019 von luCode sind die von nrzCode), dessen Datenbank als `nrzcode.db` daneben liegt.

mod common;

use common::{json, TestApp, TestUser};
use lucode_shared::import::{ImportCounts, ImportOptions, ImportSource, ImportStatus};
use rocket::http::Status;
use serde_json::{json as j, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Der Import kennt je Prozess nur einen Stand – die Tests laufen darum nacheinander.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Ordnername der Sitzungen von Claude Code für einen Arbeitsordner.
fn slug(p: &Path) -> String {
    p.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Stand der Datenbank der Quelle als `nrzcode.db` ablegen (wie eine laufende nrzCode-Instanz
/// sie hinterlässt).
async fn snapshot(src: &TestApp) {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(src.db()).await.unwrap();
    std::fs::copy(src.dir.path().join("lucode.db"), src.dir.path().join("nrzcode.db")).unwrap();
}

async fn one<T>(db: &sqlx::SqlitePool, sql: &'static str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
{
    sqlx::query_scalar(sql).fetch_one(db).await.unwrap_or_else(|e| panic!("{sql}: {e}"))
}

struct Source {
    app: TestApp,
    project: i64,
    main: i64,
    child: i64,
    scratch: i64,
    bot: i64,
    bot_thread: i64,
    claude: i64,
    codex: i64,
}

/// Eine nrzCode-Instanz mit allem, was umziehen soll.
async fn nrzcode() -> Source {
    let app = TestApp::new().await;
    let db = app.db().clone();
    let now = lucode::db::now();
    let luna = app.user("Luna", true).await;
    let mia = app.user("Mia", false).await;
    sqlx::query("UPDATE users SET theme = 'mondnacht', language = 'de', avatar_at = 77 WHERE id = ?").bind(luna.id).execute(&db).await.unwrap();
    let data = app.dir.path().to_path_buf();

    // Zwei Agent-Konten mit Home
    let mut accounts = Vec::new();
    for (kind, label) in [("claude", "Ultra"), ("codex", "Pro")] {
        let id: i64 = sqlx::query_scalar("INSERT INTO agent_accounts (user_id, kind, label, is_default, status, created_at, updated_at) VALUES (?, ?, ?, 1, 'ok', ?, ?) RETURNING id")
            .bind(luna.id)
            .bind(kind)
            .bind(label)
            .bind(now)
            .bind(now)
            .fetch_one(&db)
            .await
            .unwrap();
        let home = data.join("accounts").join(id.to_string()).join("home");
        std::fs::create_dir_all(home.join(format!(".{kind}"))).unwrap();
        std::fs::write(home.join(format!(".{kind}")).join("credentials.json"), format!("{{\"login\":\"{label}\"}}")).unwrap();
        accounts.push(id);
    }
    let (claude, codex) = (accounts[0], accounts[1]);

    // Verbindungen zu zwei Plattformen
    for (kind, url) in [("lugit", "http://lugit.local:8430"), ("github", "https://github.com")] {
        sqlx::query("INSERT INTO nrzgit_connections (user_id, kind, url, token, username, created_at, updated_at) VALUES (?, ?, ?, 'tok', 'luna', ?, ?)")
            .bind(luna.id)
            .bind(kind)
            .bind(url)
            .bind(now)
            .bind(now)
            .execute(&db)
            .await
            .unwrap();
    }

    // Projekt mit Hauptcheckout, Build-Ordner und einem Subagenten im eigenen Worktree
    let (project, repo) = app.project(luna.id, "Demo").await;
    std::fs::create_dir_all(repo.join("node_modules/dep")).unwrap();
    std::fs::write(repo.join("node_modules/dep/index.js"), "module.exports = 1").unwrap();
    std::fs::write(repo.join("notizen.txt"), "nicht eingecheckt").unwrap();
    let main = app.thread(luna.id, Some(project), "Login bauen").await;
    let child = app.thread(luna.id, Some(project), "Teil A").await;
    let worktree = data.join("workspaces").join(project.to_string()).join("worktrees").join(child.to_string());
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    let branch = format!("nrz/teil-a-{child}");
    git(&repo, &["worktree", "add", "-q", "-b", &branch, &worktree.to_string_lossy()]);
    std::fs::write(worktree.join("halbfertig.rs"), "// in Arbeit").unwrap();
    sqlx::query("UPDATE threads SET parent_id = ?, use_worktree = 1, worktree_path = ?, branch = ?, account_id = ?, status = 'settled' WHERE id = ?")
        .bind(main)
        .bind(worktree.to_string_lossy().to_string())
        .bind(&branch)
        .bind(claude)
        .bind(child)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE threads SET use_worktree = 0, account_id = ?, provider_session_id = 'sess-main', status = 'working', queued = '[{\"text\":\"später\"}]', resume_at = 99 WHERE id = ?")
        .bind(claude)
        .bind(main)
        .execute(&db)
        .await
        .unwrap();
    // Sitzung von Claude Code für den Hauptcheckout; der Subagent hat eine Sitzung ohne Datei
    let sessions = data.join("accounts").join(claude.to_string()).join("home/.claude/projects").join(slug(&repo));
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("sess-main.jsonl"), "{}\n").unwrap();
    sqlx::query("UPDATE threads SET provider_session_id = 'sess-weg' WHERE id = ?").bind(child).execute(&db).await.unwrap();

    // Läufe (einer lief noch) und Nachrichten
    let run: i64 = sqlx::query_scalar("INSERT INTO runs (thread_id, agent_kind, account_id, status, input_tokens, started_at, finished_at) VALUES (?, 'claude', ?, 'done', 1200, ?, ?) RETURNING id")
        .bind(main)
        .bind(claude)
        .bind(now)
        .bind(now)
        .fetch_one(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO runs (thread_id, agent_kind, account_id, status, started_at) VALUES (?, 'claude', ?, 'running', ?)").bind(main).bind(claude).bind(now).execute(&db).await.unwrap();
    let uuid = "0a0a0a0a-1111-2222-3333-444444444444";
    sqlx::query("INSERT INTO attachments (uuid, user_id, name, size, content_type, created_at) VALUES (?, ?, 'bild.png', 4, 'image/png', ?)").bind(uuid).bind(luna.id).bind(now).execute(&db).await.unwrap();
    std::fs::write(data.join("attachments").join(uuid), b"\x89PNG").unwrap();
    for (role, content, data_json, run_id) in [
        ("user", "Bau die Login-Seite", j!({ "attachments": [{ "uuid": uuid, "name": "bild.png", "url": format!("/attachments/{uuid}") }] }).to_string(), Some(run)),
        ("assistant", "Mache ich.", "{}".to_string(), Some(run)),
        ("subagents", "", j!({ "children": [child, 9999], "titles": ["Teil A", "Gelöscht"], "byUser": false }).to_string(), Some(run)),
    ] {
        sqlx::query("INSERT INTO messages (thread_id, run_id, role, content, data, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(main)
            .bind(run_id)
            .bind(role)
            .bind(content)
            .bind(data_json)
            .bind(now)
            .bind(now)
            .execute(&db)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO messages (thread_id, role, content, created_at, updated_at) VALUES (?, 'assistant', 'Teil A ist fertig.', ?, ?)").bind(child).bind(now).bind(now).execute(&db).await.unwrap();

    // Chat ohne Projekt mit eigenem Arbeitsordner
    let scratch = app.thread(mia.id, None, "Frage ohne Projekt").await;
    std::fs::create_dir_all(data.join("scratch").join(scratch.to_string())).unwrap();
    std::fs::write(data.join("scratch").join(scratch.to_string()).join("skizze.md"), "# Skizze").unwrap();

    // Bot mit Gespräch, Routine und Arbeitsordner
    let bot: i64 = sqlx::query_scalar("INSERT INTO bots (user_id, name, emoji, memory, account_id, created_at, updated_at) VALUES (?, 'Rechercheur', '🔎', 'mag Tee', ?, ?, ?) RETURNING id")
        .bind(luna.id)
        .bind(claude)
        .bind(now)
        .bind(now)
        .fetch_one(&db)
        .await
        .unwrap();
    let bot_thread = app.thread(luna.id, None, "Gespräch").await;
    sqlx::query("UPDATE threads SET bot_id = ? WHERE id = ?").bind(bot).bind(bot_thread).execute(&db).await.unwrap();
    sqlx::query("UPDATE bots SET thread_id = ? WHERE id = ?").bind(bot_thread).bind(bot).execute(&db).await.unwrap();
    sqlx::query("INSERT INTO bot_routines (bot_id, title, prompt, created_at) VALUES (?, 'Morgens', 'Was gibt es Neues?', ?)").bind(bot).bind(now).execute(&db).await.unwrap();
    std::fs::create_dir_all(data.join("bots").join(bot.to_string())).unwrap();
    std::fs::write(data.join("bots").join(bot.to_string()).join("merkzettel.txt"), "Tee").unwrap();

    // Skill, Notiz, Canvas, KI-Kette, Sprachsteuerung, Team, Profilbild
    sqlx::query("INSERT INTO agent_skills (user_id, name, prompt, created_at, updated_at) VALUES (?, 'review', 'Prüfe den Code', ?, ?)").bind(luna.id).bind(now).bind(now).execute(&db).await.unwrap();
    sqlx::query("INSERT INTO prompt_notes (user_id, project_id, text, created_at, updated_at) VALUES (?, ?, 'Später: Tests', ?, ?)").bind(luna.id).bind(project).bind(now).bind(now).execute(&db).await.unwrap();
    let layout = j!({
        "windows": [
            { "id": format!("t{main}"), "kind": "chat", "threadId": main, "name": "Lena", "x": 1.0, "y": 2.0, "w": 540.0, "h": 680.0, "z": 1 },
            { "id": "term-1", "kind": "terminal", "terminalId": "abc", "name": "Tom", "x": 0.0, "y": 0.0, "w": 10.0, "h": 10.0, "z": 2 },
            { "id": "web-1", "kind": "browser", "url": "http://localhost:3000", "name": "Web", "x": 0.0, "y": 0.0, "w": 10.0, "h": 10.0, "z": 3 }
        ],
        "closed": [child, 4242]
    });
    sqlx::query("INSERT INTO canvas_boards (user_id, project_key, layout, shapes, rev, updated_at) VALUES (?, ?, ?, '[{\"id\":\"s1\",\"kind\":\"note\",\"text\":\"Idee\"}]', 3, ?)")
        .bind(luna.id)
        .bind(project)
        .bind(layout.to_string())
        .bind(now)
        .execute(&db)
        .await
        .unwrap();
    let chains = j!([{ "id": "c1", "name": "Erst Claude", "enabled": true, "steps": [{ "accountId": claude, "model": "" }, { "accountId": codex, "model": "" }, { "accountId": 555, "model": "" }] }]);
    lucode::db::set_setting(&db, &format!("chains:{}", luna.id), &chains.to_string()).await.unwrap();
    lucode::db::set_setting(&db, &format!("voice:{}", luna.id), &j!({ "sttModel": "small", "accountId": claude, "speak": false }).to_string()).await.unwrap();
    lucode::db::set_setting(&db, &format!("mobile:{}", luna.id), "{\"token\":\"geheim\"}").await.unwrap();
    sqlx::query("INSERT INTO team_projects (project_id, enabled, auto_reply, remote_sha, doc) VALUES (?, 1, 1, 'abc', '{\"v\":1}')").bind(project).execute(&db).await.unwrap();
    std::fs::create_dir_all(data.join("avatars")).unwrap();
    std::fs::write(data.join("avatars").join(luna.id.to_string()), b"bild").unwrap();
    std::fs::create_dir_all(data.join("tools/npm/bin")).unwrap();
    std::fs::write(data.join("tools/npm/bin/claude"), "#!/bin/sh").unwrap();
    std::fs::create_dir_all(data.join("models/whisper-base")).unwrap();
    std::fs::write(data.join("models/whisper-base/config.json"), "{}").unwrap();

    snapshot(&app).await;
    Source { app, project, main, child, scratch, bot, bot_thread, claude, codex }
}

async fn get<T: serde::de::DeserializeOwned + Send + 'static>(t: &TestApp, u: &TestUser, path: &str) -> T {
    let res = t.client.get(path).private_cookie(u.cookie.clone()).dispatch().await;
    assert_eq!(res.status(), Status::Ok, "GET {path}");
    res.into_json().await.expect("json")
}

/// Import starten und warten, bis er fertig ist.
async fn import(t: &TestApp, u: &TestUser, opts: &ImportOptions) -> ImportStatus {
    let res = t.client.post("/api/v1/import/nrzcode").private_cookie(u.cookie.clone()).header(json()).body(serde_json::to_string(opts).unwrap()).dispatch().await;
    let status = res.status();
    let text = res.into_string().await.unwrap_or_default();
    assert_eq!(status, Status::Ok, "POST /import/nrzcode: {text}");
    for _ in 0..600 {
        let s: ImportStatus = get(t, u, "/api/v1/import/nrzcode/status").await;
        if !s.is_running() {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("Import wird nicht fertig");
}

#[rocket::async_test]
async fn umzug_von_nrzcode() {
    let _one = ONE_AT_A_TIME.lock().await;
    let src = nrzcode().await;
    let src_dir = src.app.dir.path().to_path_buf();

    // Ziel: luCode mit eigenem Konto „Luna“ und schon vorhandenen Daten – die Nummern 1 sind
    // vergeben, alles aus nrzCode bekommt also neue
    let t = TestApp::new().await;
    let luna = t.user("Luna", true).await;
    let (own_project, _) = t.project(luna.id, "Eigenes").await;
    let own_thread = t.thread(luna.id, Some(own_project), "Eigener Chat").await;
    let now = lucode::db::now();
    let own_account: i64 = sqlx::query_scalar("INSERT INTO agent_accounts (user_id, kind, label, is_default, created_at, updated_at) VALUES (?, 'claude', 'Hier', 1, ?, ?) RETURNING id")
        .bind(luna.id)
        .bind(now)
        .bind(now)
        .fetch_one(t.db())
        .await
        .unwrap();
    assert_eq!((own_project, own_thread, own_account), (src.project, src.main, src.claude), "die Nummern der Quelle sind hier schon vergeben");
    let db = t.db().clone();
    let data = t.dir.path().to_path_buf();

    // Nur Administratoren
    let gast = t.user("Gast", false).await;
    let res = t.client.get("/api/v1/import/nrzcode").private_cookie(gast.cookie.clone()).dispatch().await;
    assert_eq!(res.status(), Status::Forbidden);

    // Quelle erkennen und zählen
    let path = urlencoding::encode(&src_dir.to_string_lossy()).to_string();
    let found: ImportSource = get(&t, &luna, &format!("/api/v1/import/nrzcode?path={path}")).await;
    assert!(found.found, "{}", found.error);
    assert_eq!(found.users, vec!["Luna", "Mia"]);
    assert_eq!((found.counts.accounts, found.counts.projects, found.counts.threads, found.counts.messages, found.counts.bots), (2, 1, 4, 4, 1));
    assert_eq!(found.pending.threads, 4);
    assert_eq!(found.last_import_at, 0);
    let missing: ImportSource = get(&t, &luna, &format!("/api/v1/import/nrzcode?path={}", urlencoding::encode(&data.join("gibtsnicht").to_string_lossy()))).await;
    assert!(!missing.found && !missing.error.is_empty());
    // Der eigene Datenordner ist keine Quelle
    let own: ImportSource = get(&t, &luna, &format!("/api/v1/import/nrzcode?path={}", urlencoding::encode(&data.to_string_lossy()))).await;
    assert!(!own.found);

    // ---------------------------------------------------------------- Alles übernehmen
    let opts = ImportOptions { path: src_dir.to_string_lossy().to_string(), ..Default::default() };
    let done = import(&t, &luna, &opts).await;
    assert_eq!(done.state, "done", "{} {:?}", done.error, done.notes);
    assert_eq!(
        done.imported,
        ImportCounts { users: 1, accounts: 2, connections: 2, projects: 1, threads: 4, messages: 4, bots: 1, skills: 1, notes: 1, attachments: 1, boards: 1 }
    );
    assert!(done.files > 5 && done.bytes > 0);

    // Benutzer: Luna ist dieselbe (Sprache und Profilbild ergänzt), Mia wurde angelegt
    let users: Vec<(i64, String, String, String, i64)> = sqlx::query_as("SELECT id, username, language, theme, avatar_at FROM users ORDER BY id").fetch_all(&db).await.unwrap();
    assert_eq!(users.len(), 3);
    assert_eq!((users[0].1.as_str(), users[0].2.as_str(), users[0].4), ("Luna", "de", 77));
    let mia = users.iter().find(|u| u.1 == "Mia").expect("Mia").0;
    assert!(data.join("avatars").join(luna.id.to_string()).is_file());

    // Konten: neue Nummern, Home kopiert, kein zweites Vorgabe-Konto für Claude
    let accounts: Vec<(i64, String, String, bool)> = sqlx::query_as("SELECT id, kind, label, is_default FROM agent_accounts ORDER BY id").fetch_all(&db).await.unwrap();
    assert_eq!(accounts.len(), 3);
    let claude = accounts.iter().find(|a| a.2 == "Ultra").unwrap();
    let codex = accounts.iter().find(|a| a.2 == "Pro").unwrap();
    assert_ne!(claude.0, src.claude, "die alte Nummer war vergeben");
    assert!(!claude.3 && codex.3);
    let home = data.join("accounts").join(claude.0.to_string()).join("home");
    assert_eq!(std::fs::read_to_string(home.join(".claude/credentials.json")).unwrap(), "{\"login\":\"Ultra\"}");
    assert_eq!(one::<i64>(&db, "SELECT COUNT(*) FROM nrzgit_connections").await, 2);

    // Projekt: neue Nummer, freigegeben, Checkout ohne Build-Ordner, Uneingechecktes dabei
    let (project, status): (i64, String) = sqlx::query_as("SELECT id, status FROM projects WHERE name = 'Demo'").fetch_one(&db).await.unwrap();
    assert_ne!(project, src.project);
    assert_eq!(status, "ready");
    let repo = data.join("workspaces").join(project.to_string()).join("repo");
    assert!(repo.join("README.md").is_file() && repo.join("notizen.txt").is_file());
    assert!(!repo.join("node_modules").exists(), "Build-Ordner bleiben in nrzCode");
    assert_eq!(done.notes.iter().filter(|n| n.contains("Build-Ordner")).count(), 1, "{:?}", done.notes);

    // Threads: Eltern, Konto, Status, Warteschlange
    let threads: Vec<(i64, String, Option<i64>, Option<i64>, String, String, Option<i64>, String, String, i64)> =
        sqlx::query_as("SELECT id, title, parent_id, account_id, status, queued, resume_at, worktree_path, provider_session_id, user_id FROM threads ORDER BY id").fetch_all(&db).await.unwrap();
    assert_eq!(threads.len(), 5);
    let main = threads.iter().find(|t| t.1 == "Login bauen").unwrap();
    let child = threads.iter().find(|t| t.1 == "Teil A").unwrap();
    let scratch = threads.iter().find(|t| t.1 == "Frage ohne Projekt").unwrap();
    assert_eq!((main.2, main.3, main.4.as_str(), main.5.as_str(), main.6), (None, Some(claude.0), "idle", "[]", None));
    assert_eq!((child.2, child.3), (Some(main.0), Some(claude.0)));
    assert_eq!(scratch.9, mia);
    // Worktree: Ordner trägt die neue Nummer, git arbeitet im Repository hier
    let worktree = data.join("workspaces").join(project.to_string()).join("worktrees").join(child.0.to_string());
    assert_eq!(PathBuf::from(&child.7), worktree);
    assert!(worktree.join("halbfertig.rs").is_file());
    assert_eq!(git(&worktree, &["branch", "--show-current"]), format!("nrz/teil-a-{}", src.child));
    let common = PathBuf::from(git(&worktree, &["rev-parse", "--path-format=absolute", "--git-common-dir"]));
    assert_eq!(std::fs::canonicalize(common).unwrap(), std::fs::canonicalize(repo.join(".git")).unwrap(), "der Worktree gehört zum Repository in luCode");
    assert!(git(&worktree, &["status", "--porcelain"]).contains("halbfertig.rs"));
    assert_eq!(git(&repo, &["worktree", "list"]).lines().count(), 2);
    // Sitzung von Claude Code: liegt unter dem neuen Arbeitsordner und bleibt; ohne Datei wird neu begonnen
    assert!(home.join(".claude/projects").join(slug(&repo)).join("sess-main.jsonl").is_file());
    assert_eq!((main.8.as_str(), child.8.as_str()), ("sess-main", ""));
    assert!(done.notes.iter().any(|n| n.contains("Verlauf")), "{:?}", done.notes);

    // Nachrichten: Verweise auf Subagenten folgen den neuen Nummern
    assert_eq!(one::<i64>(&db, "SELECT COUNT(*) FROM messages").await, 4);
    let sub: String = one(&db, "SELECT data FROM messages WHERE role = 'subagents'").await;
    let sub: Value = serde_json::from_str(&sub).unwrap();
    assert_eq!((sub["children"].clone(), sub["titles"].clone()), (j!([child.0]), j!(["Teil A"])));
    assert!(data.join("attachments/0a0a0a0a-1111-2222-3333-444444444444").is_file());

    // Läufe und Nutzung: je Lauf genau eine Zeile, was lief, gilt als abgebrochen
    let runs: Vec<(i64, String)> = sqlx::query_as("SELECT id, status FROM runs ORDER BY id").fetch_all(&db).await.unwrap();
    assert_eq!(runs.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(), ["done", "error"]);
    let usage: Vec<(Option<i64>, Option<i64>, Option<i64>, String, String)> = sqlx::query_as("SELECT run_id, thread_id, account_id, status, thread_key FROM usage_log ORDER BY id").fetch_all(&db).await.unwrap();
    assert_eq!(usage.len(), 2, "{usage:?}");
    assert_eq!((usage[0].0, usage[0].1, usage[0].2, usage[0].3.as_str()), (Some(runs[0].0), Some(main.0), Some(claude.0), "done"));
    assert_eq!(usage[1].3, "error");
    assert!(usage[0].4.starts_with('n') && usage[0].4 == usage[1].4, "{usage:?}");

    // Chat ohne Projekt, Bot mit Gespräch, Routine und Ordner
    assert!(data.join("scratch").join(scratch.0.to_string()).join("skizze.md").is_file());
    let (bot, bot_thread, bot_account, memory): (i64, Option<i64>, Option<i64>, String) = sqlx::query_as("SELECT id, thread_id, account_id, memory FROM bots").fetch_one(&db).await.unwrap();
    let talk = threads.iter().find(|t| t.1 == "Gespräch").unwrap();
    assert_eq!((bot_thread, bot_account, memory.as_str()), (Some(talk.0), Some(claude.0), "mag Tee"));
    assert_eq!(one::<i64>(&db, "SELECT bot_id FROM threads WHERE title = 'Gespräch'").await, bot);
    assert_eq!(one::<i64>(&db, "SELECT bot_id FROM bot_routines").await, bot);
    assert!(data.join("bots").join(bot.to_string()).join("merkzettel.txt").is_file());

    // Skill, Notiz, Canvas, Kette, Sprachsteuerung, Team
    assert_eq!(one::<i64>(&db, "SELECT COUNT(*) FROM agent_skills").await, 1);
    assert_eq!(one::<i64>(&db, "SELECT project_id FROM prompt_notes").await, project);
    let (key, layout, shapes): (i64, String, String) = sqlx::query_as("SELECT project_key, layout, shapes FROM canvas_boards").fetch_one(&db).await.unwrap();
    let layout: Value = serde_json::from_str(&layout).unwrap();
    assert_eq!(key, project);
    assert_eq!(layout["windows"].as_array().unwrap().len(), 2, "Chat und Browser, das Terminal nicht");
    assert_eq!((layout["windows"][0]["threadId"].clone(), layout["windows"][0]["id"].clone()), (j!(main.0), j!(format!("t{}", main.0))));
    assert_eq!(layout["closed"], j!([child.0]));
    assert!(shapes.contains("Idee"));
    let chains: Value = serde_json::from_str(&lucode::db::setting(&db, &format!("chains:{}", luna.id), "").await.unwrap()).unwrap();
    assert_eq!(chains[0]["steps"], j!([{ "accountId": claude.0, "model": "" }, { "accountId": codex.0, "model": "" }]));
    let voice: Value = serde_json::from_str(&lucode::db::setting(&db, &format!("voice:{}", luna.id), "").await.unwrap()).unwrap();
    assert_eq!((voice["accountId"].clone(), voice["sttModel"].clone()), (j!(claude.0), j!("small")));
    assert_eq!(lucode::db::setting(&db, &format!("mobile:{}", luna.id), "").await.unwrap(), "", "die Kopplung mit dem Handy zieht nicht um");
    let (team_on, team_doc): (bool, String) = sqlx::query_as("SELECT enabled, doc FROM team_projects WHERE project_id = ?").bind(project).fetch_one(&db).await.unwrap();
    assert!(team_on && team_doc == "{}");
    assert!(data.join("tools/npm/bin/claude").is_file() && data.join("models/whisper-base/config.json").is_file());

    // Die Quelle ist unberührt
    assert_eq!(one::<String>(src.app.db(), "SELECT status FROM threads WHERE title = 'Login bauen'").await, "working");
    assert!(src_dir.join("workspaces").join(src.project.to_string()).join("worktrees").join(src.child.to_string()).join(".git").is_file());
    assert!(git(&src_dir.join("workspaces").join(src.project.to_string()).join("worktrees").join(src.child.to_string()), &["rev-parse", "--path-format=absolute", "--git-common-dir"]).replace('\\', "/").contains(&src_dir.file_name().unwrap().to_string_lossy().to_string()));

    // ---------------------------------------------------------------- Wiederholen: nichts doppelt
    let again = import(&t, &luna, &opts).await;
    assert_eq!(again.state, "done", "{}", again.error);
    assert_eq!(again.imported, ImportCounts::default());
    assert_eq!((one::<i64>(&db, "SELECT COUNT(*) FROM threads").await, one::<i64>(&db, "SELECT COUNT(*) FROM messages").await, one::<i64>(&db, "SELECT COUNT(*) FROM usage_log").await), (5, 4, 2));
    let found: ImportSource = get(&t, &luna, &format!("/api/v1/import/nrzcode?path={path}")).await;
    assert!(found.last_import_at > 0);
    assert_eq!(found.pending.content(), 0, "{:?}", found.pending);

    // ---------------------------------------------------------------- Neues in nrzCode nachholen
    let now = lucode::db::now();
    sqlx::query("INSERT INTO messages (thread_id, role, content, created_at, updated_at) VALUES (?, 'user', 'Noch etwas', ?, ?)").bind(src.main).bind(now).bind(now).execute(src.app.db()).await.unwrap();
    let neu = src.app.thread(1, Some(src.project), "Neuer Chat").await;
    sqlx::query("INSERT INTO messages (thread_id, role, content, created_at, updated_at) VALUES (?, 'user', 'Hallo', ?, ?)").bind(neu).bind(now).bind(now).execute(src.app.db()).await.unwrap();
    snapshot(&src.app).await;
    // In luCode Gelöschtes kommt nicht zurück – auch nicht seine neuen Nachrichten
    sqlx::query("DELETE FROM threads WHERE id = ?").bind(scratch.0).execute(&db).await.unwrap();
    sqlx::query("INSERT INTO messages (thread_id, role, content, created_at, updated_at) VALUES (?, 'user', 'Für den Gelöschten', ?, ?)").bind(src.scratch).bind(now).bind(now).execute(src.app.db()).await.unwrap();
    snapshot(&src.app).await;
    let found: ImportSource = get(&t, &luna, &format!("/api/v1/import/nrzcode?path={path}")).await;
    assert_eq!((found.pending.threads, found.pending.messages), (1, 3));
    let more = import(&t, &luna, &opts).await;
    assert_eq!(more.state, "done", "{}", more.error);
    assert_eq!((more.imported.threads, more.imported.messages), (1, 2));
    assert_eq!(one::<i64>(&db, "SELECT COUNT(*) FROM messages WHERE content = 'Noch etwas' AND thread_id = (SELECT id FROM threads WHERE title = 'Login bauen')").await, 1);
    assert_eq!(one::<i64>(&db, "SELECT COUNT(*) FROM threads WHERE title = 'Frage ohne Projekt'").await, 0);
    let _ = (src.bot, src.bot_thread, src.codex);
}

/// Nur Ausgewähltes: ohne „Projekte & Chats“ kommen Konten und Extras, aber keine Projekte.
#[rocket::async_test]
async fn auswahl_ohne_projekte() {
    let _one = ONE_AT_A_TIME.lock().await;
    let src = nrzcode().await;
    let t = TestApp::new().await;
    let luna = t.user("Luna", true).await;
    let opts = ImportOptions { path: src.app.dir.path().to_string_lossy().to_string(), workspaces: false, tools: false, ..Default::default() };
    let done = import(&t, &luna, &opts).await;
    assert_eq!(done.state, "done", "{}", done.error);
    let db = t.db();
    assert_eq!(one::<i64>(db, "SELECT COUNT(*) FROM projects").await, 0);
    // Nur das Gespräch mit dem Bot (Extras), keine Chats aus Projekten
    assert_eq!(one::<i64>(db, "SELECT COUNT(*) FROM threads").await, 1);
    assert_eq!(one::<i64>(db, "SELECT COUNT(*) FROM agent_accounts").await, 2);
    assert_eq!(one::<i64>(db, "SELECT COUNT(*) FROM bots").await, 1);
    // Ohne Vorbelegung behalten die Konten ihre Nummern
    assert_eq!(one::<i64>(db, "SELECT id FROM agent_accounts WHERE label = 'Ultra'").await, src.claude);
    assert!(!t.dir.path().join("tools/npm").exists());
    // Die Notiz verliert nur den Bezug zum (nicht übernommenen) Projekt
    assert_eq!(one::<Option<i64>>(db, "SELECT project_id FROM prompt_notes").await, None);
}

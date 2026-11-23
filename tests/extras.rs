//! Was luCode über nrzCode hinaus kann: Suche über alle Chats, eigene Anweisungen (Benutzer und
//! Projekt, im Systemhinweis der Läufe), Export als Markdown, Sicherungen der Datenbank.

mod common;

use common::{json, TestApp, TestUser};
use lucode_shared::extras::{BackupList, Instructions, SearchResults};
use rocket::http::Status;
use serde_json::json as j;

async fn get<T: serde::de::DeserializeOwned + Send + 'static>(t: &TestApp, u: &TestUser, path: &str) -> T {
    let res = t.client.get(path).private_cookie(u.cookie.clone()).dispatch().await;
    assert_eq!(res.status(), Status::Ok, "GET {path}");
    res.into_json().await.expect("json")
}

async fn message(t: &TestApp, thread: i64, role: &str, content: &str) -> i64 {
    let now = lucode::db::now();
    sqlx::query_scalar("INSERT INTO messages (thread_id, role, content, created_at, updated_at) VALUES (?, ?, ?, ?, ?) RETURNING id")
        .bind(thread)
        .bind(role)
        .bind(content)
        .bind(now)
        .bind(now)
        .fetch_one(t.db())
        .await
        .unwrap()
}

#[rocket::async_test]
async fn suche_ueber_alle_chats() {
    let t = TestApp::new().await;
    let anna = t.user("Anna", false).await;
    let ben = t.user("Ben", false).await;
    let (project, _) = t.project(anna.id, "Shop").await;
    let login = t.thread(anna.id, Some(project), "Login mit OAuth").await;
    let other = t.thread(anna.id, None, "Kleinkram").await;
    let fremd = t.thread(ben.id, None, "OAuth bei Ben").await;
    let hit = message(&t, other, "assistant", &format!("{} Der OAuth-Token läuft nach einer Stunde ab. {}", "Davor. ".repeat(40), "Danach. ".repeat(40))).await;
    message(&t, other, "tool", "oauth im Werkzeug zählt nicht").await;
    message(&t, fremd, "user", "oauth geheim").await;
    message(&t, login, "user", "100%_fertig?").await;

    let r: SearchResults = get(&t, &anna, "/api/v1/search?q=oauth").await;
    assert_eq!(r.threads.iter().map(|h| h.thread_id).collect::<Vec<_>>(), vec![login], "nur eigene Chats, Titel ohne Gross/Klein");
    assert_eq!(r.threads[0].project, "Shop");
    assert_eq!(r.messages.len(), 1, "{:?}", r.messages);
    let m = &r.messages[0];
    assert_eq!((m.thread_id, m.message_id, m.role.as_str(), m.title.as_str()), (other, Some(hit), "assistant", "Kleinkram"));
    assert!(m.snippet.contains("OAuth-Token läuft") && m.snippet.starts_with('…'), "{}", m.snippet);
    // Zeichen mit Sonderbedeutung in LIKE sind wörtlich gemeint
    let r: SearchResults = get(&t, &anna, "/api/v1/search?q=100%25_fertig").await;
    assert_eq!(r.messages.len(), 1);
    let r: SearchResults = get(&t, &anna, "/api/v1/search?q=1_0").await;
    assert!(r.messages.is_empty() && r.threads.is_empty());
    // Zu kurz: nichts
    let r: SearchResults = get(&t, &anna, "/api/v1/search?q=o").await;
    assert!(r.threads.is_empty() && r.messages.is_empty());
    assert_eq!(t.client.get("/api/v1/search?q=oauth").dispatch().await.status(), Status::Unauthorized);
}

#[rocket::async_test]
async fn eigene_anweisungen() {
    let t = TestApp::new().await;
    let anna = t.user("Anna", false).await;
    let ben = t.user("Ben", false).await;
    let (project, _) = t.project(anna.id, "Shop").await;

    let i: Instructions = get(&t, &anna, "/api/v1/instructions").await;
    assert_eq!(i.text, "");
    assert_eq!(lucode::extras::instructions_hint(&t.state, anna.id, Some(project)).await, "");
    let put = |path: String, user: TestUser, text: &'static str| {
        let client = &t.client;
        async move { client.put(path).private_cookie(user.cookie.clone()).header(json()).body(j!({ "text": text }).to_string()).dispatch().await.status() }
    };
    assert_eq!(put("/api/v1/instructions".into(), anna.clone(), "  Antworte auf Deutsch. Nutze pnpm.  ").await, Status::Ok);
    assert_eq!(put(format!("/api/v1/projects/{project}/instructions"), anna.clone(), "Tests mit vitest.").await, Status::Ok);
    // Fremdes Projekt: nicht lesbar, nicht änderbar
    assert_eq!(put(format!("/api/v1/projects/{project}/instructions"), ben.clone(), "Böse").await, Status::NotFound);
    let res = t.client.get(format!("/api/v1/projects/{project}/instructions")).private_cookie(ben.cookie.clone()).dispatch().await;
    assert_eq!(res.status(), Status::NotFound);

    let i: Instructions = get(&t, &anna, "/api/v1/instructions").await;
    assert_eq!(i.text, "Antworte auf Deutsch. Nutze pnpm.");
    let p: Instructions = get(&t, &anna, &format!("/api/v1/projects/{project}/instructions")).await;
    assert_eq!(p.text, "Tests mit vitest.");
    // Im Systemhinweis: erst die des Benutzers, im Projekt zusätzlich die des Projekts
    let hint = lucode::extras::instructions_hint(&t.state, anna.id, Some(project)).await;
    let (user_at, project_at) = (hint.find("Antworte auf Deutsch").expect("Benutzer"), hint.find("Tests mit vitest.").expect("Projekt"));
    assert!(user_at < project_at && hint.contains("standing instructions") && hint.contains("this project"), "{hint}");
    let hint = lucode::extras::instructions_hint(&t.state, anna.id, None).await;
    assert!(hint.contains("pnpm") && !hint.contains("vitest"));
    assert_eq!(lucode::extras::instructions_hint(&t.state, ben.id, None).await, "");
}

#[rocket::async_test]
async fn chat_als_markdown() {
    let t = TestApp::new().await;
    let anna = t.user("Anna", false).await;
    let ben = t.user("Ben", false).await;
    let (project, _) = t.project(anna.id, "Shop").await;
    let thread = t.thread(anna.id, Some(project), "Login: OAuth & mehr").await;
    message(&t, thread, "user", "Bau die Anmeldung.").await;
    message(&t, thread, "tool", "").await;
    message(&t, thread, "tool", "").await;
    message(&t, thread, "assistant", "Fertig – `login.rs` ist neu.").await;

    let res = t.client.get(format!("/api/v1/threads/{thread}/export")).private_cookie(anna.cookie.clone()).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    let disposition = res.headers().get_one("Content-Disposition").unwrap_or_default().to_string();
    assert!(disposition.starts_with("attachment") && disposition.contains("login-oauth-mehr.md"), "{disposition}");
    assert!(res.content_type().is_some_and(|c| c.sub() == "markdown"));
    let md = res.into_string().await.unwrap();
    assert!(md.starts_with("# Login: OAuth & mehr\n"), "{md}");
    assert!(md.contains("luCode · Shop · claude"), "{md}");
    let (user_at, steps_at, answer_at) = (md.find("**Du**").unwrap(), md.find("2 Arbeitsschritte").unwrap(), md.find("**claude**").unwrap());
    assert!(user_at < steps_at && steps_at < answer_at, "{md}");
    assert!(md.contains("Fertig – `login.rs` ist neu."));
    // Fremde Chats gibt es nicht
    let res = t.client.get(format!("/api/v1/threads/{thread}/export")).private_cookie(ben.cookie.clone()).dispatch().await;
    assert_eq!(res.status(), Status::NotFound);
}

#[rocket::async_test]
async fn sicherungen() {
    let t = TestApp::new().await;
    let admin = t.user("Chefin", true).await;
    let anna = t.user("Anna", false).await;
    t.thread(anna.id, None, "Wichtiger Chat").await;

    let res = t.client.get("/api/v1/backups").private_cookie(anna.cookie.clone()).dispatch().await;
    assert_eq!(res.status(), Status::Forbidden);
    let list: BackupList = get(&t, &admin, "/api/v1/backups").await;
    assert!(list.backups.is_empty() && list.auto);

    let res = t.client.post("/api/v1/backups").private_cookie(admin.cookie.clone()).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    let list: BackupList = res.into_json().await.unwrap();
    assert_eq!(list.backups.len(), 1);
    let b = list.backups[0].clone();
    assert!(b.size > 0 && !b.auto && b.name.starts_with("lucode-") && b.name.ends_with(".db"));
    // Die Sicherung ist eine vollständige Datenbank
    let file = t.dir.path().join("backups").join(&b.name);
    let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(&file).read_only(true);
    let pool = sqlx::SqlitePool::connect_with(opts).await.unwrap();
    let title: String = sqlx::query_scalar("SELECT title FROM threads").fetch_one(&pool).await.unwrap();
    assert_eq!(title, "Wichtiger Chat");
    pool.close().await;

    // Herunterladen
    let res = t.client.get(format!("/api/v1/backups/{}/download", b.name)).private_cookie(admin.cookie.clone()).dispatch().await;
    assert_eq!(res.status(), Status::Ok);
    assert!(res.headers().get_one("Content-Disposition").is_some_and(|d| d.contains(&b.name)));
    assert_eq!(res.into_bytes().await.unwrap().len() as i64, b.size);
    // Keine fremden Dateien über den Namen
    for bad in ["lucode.db", "..%2Flucode.db", "lucode-abc.db"] {
        let res = t.client.get(format!("/api/v1/backups/{bad}/download")).private_cookie(admin.cookie.clone()).dispatch().await;
        assert_eq!(res.status(), Status::NotFound, "{bad}");
    }

    // Tägliche Sicherung abschalten, von selbst angelegte erkennen, löschen
    let res = t.client.put("/api/v1/backups/settings").private_cookie(admin.cookie.clone()).header(json()).body(j!({ "auto": false }).to_string()).dispatch().await;
    let list: BackupList = res.into_json().await.unwrap();
    assert!(!list.auto);
    let auto = lucode::extras::backup(&t.state, true).await.unwrap();
    assert!(auto.auto && auto.name.contains("auto"));
    let res = t.client.delete(format!("/api/v1/backups/{}", b.name)).private_cookie(admin.cookie.clone()).dispatch().await;
    let list: BackupList = res.into_json().await.unwrap();
    assert_eq!(list.backups.iter().map(|x| x.name.clone()).collect::<Vec<_>>(), vec![auto.name]);
    assert!(!file.exists());
}

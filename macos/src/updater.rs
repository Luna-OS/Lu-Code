//! Automatische Updates aus den Releases von GitHub (Luna-OS/Lu-Code). Die Antwort eines nrzGit
//! oder luGit (`LUCODE_UPDATE_REPO`) wird ebenso verstanden.
//!
//! Windows: Tag `windows-<version>` mit `luCode-<version>-x64.exe`. Das laufende Programm wird
//! umbenannt (das erlaubt Windows), die neue Datei an seine Stelle kopiert und neu gestartet.
//!
//! Seit 1.0.12 gibt es ein gemeinsames Release `v<version>` mit DMG und EXE.
//! Seit 1.0.29 liegt für Windows zusätzlich ein Installer bei (`luCode-<version>-Setup.exe`, ohne
//! `x64` im Namen – ältere Versionen suchen die EXE mit `x64` und nähmen sonst den Installer).
//!
//! Releases der macOS-App haben den Tag `macos-<version>` (z. B. `macos-1.0.6`) und eine
//! angehängte DMG (`luCode-<version>-<arch>.dmg`), optional dazu `SHA256SUMS`. Die App fragt
//! beim Start und danach alle paar Stunden die Release-Liste ab, meldet eine neuere Version
//! (Hinweis mit Knopf im Fenster und Eintrag im Menü), lädt die DMG, prüft die Prüfsumme,
//! ersetzt das Programm-Bundle und startet neu.
//!
//! Automatisch (Einstellungen → App, ab Werk an): Die neue Version wird still geladen, gegen die
//! Prüfsumme des Releases geprüft und vorgemerkt (`pending.json` im Cache-Ordner). Installiert
//! wird in einem ruhigen Moment – kein Agent arbeitet, niemand tippt (`main.rs`) –, beim Beenden
//! oder, falls es dazu nicht kam, beim nächsten Start ([`install_pending`]). Ohne Prüfsumme oder
//! wenn sich das Programm nicht selbst ersetzen kann ([`can_self_replace`]), bleibt es beim
//! Hinweis mit Knopf.

use futures::StreamExt;
use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Repository, aus dem die Updates kommen (GitHub). Mit `LUCODE_UPDATE_REPO` überschreibbar
/// (volle API-Adresse des Repositorys, auch die eines nrzGit/luGit).
pub const DEFAULT_REPO_API: &str = "https://api.github.com/repos/Luna-OS/Lu-Code";
#[cfg(target_os = "macos")]
pub const TAG_PREFIX: &str = "macos-";
#[cfg(not(target_os = "macos"))]
pub const TAG_PREFIX: &str = "windows-";
/// Endung der Datei im Release.
const ASSET_EXT: &str = if cfg!(target_os = "macos") { ".dmg" } else { ".exe" };
/// Abstand zwischen zwei automatischen Prüfungen.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone)]
pub struct Update {
    pub version: Version,
    pub title: String,
    pub file_name: String,
    pub file_url: String,
    pub sha256: Option<String>,
    pub page_url: String,
}

pub fn repo_api() -> String {
    std::env::var("LUCODE_UPDATE_REPO").unwrap_or_else(|_| DEFAULT_REPO_API.into()).trim_end_matches('/').to_string()
}

/// Server-Adresse aus der API-Adresse (`http://host:port`) – für relative Links.
fn origin(api: &str) -> String {
    match api.find("/api/") {
        Some(i) => api[..i].to_string(),
        None => api.to_string(),
    }
}

/// Laufende Version. `LUCODE_APP_VERSION` überschreibt sie (zum Testen des Updaters).
pub fn current_version() -> Version {
    std::env::var("LUCODE_APP_VERSION")
        .ok()
        .and_then(|v| Version::parse(v.trim()).ok())
        .unwrap_or_else(|| Version::parse(env!("CARGO_PKG_VERSION")).expect("Version"))
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("luCode-desktop/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())
}

/// Architektur-Kennung im Namen der DMG.
fn arch_tag() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" if cfg!(windows) => "x64",
        "x86_64" => "x86_64",
        other => other,
    }
}

/// Neueste veröffentlichte Version, wenn sie neuer als `current` ist.
pub async fn check(current: &Version) -> Result<Option<Update>, String> {
    let api = repo_api();
    let http = client()?;
    let resp = http
        .get(format!("{api}/releases?limit=100&per_page=100"))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Update-Server nicht erreichbar: {e}"))?;
    // Noch keine Releases (oder ein privates Repository): nichts Neues, kein Fehler
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("Update-Server antwortet mit {}", resp.status()));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    let items = body.get("items").and_then(Value::as_array).or_else(|| body.as_array()).cloned().unwrap_or_default();
    let Some((version, release)) = newest(&items) else {
        return Ok(None);
    };
    if &version <= current {
        return Ok(None);
    }
    let base = origin(&api);
    let files = attachments(&release, &base);
    let Some((file_name, file_url)) = pick_dmg(&files) else {
        return Err(format!("Release {version} hat keine Datei ({ASSET_EXT}) für {}", arch_tag()));
    };
    let sha256 = match files.iter().find(|(n, _)| n.eq_ignore_ascii_case("SHA256SUMS")) {
        Some((_, url)) => {
            let text = http.get(url).timeout(Duration::from_secs(20)).send().await.map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
            sum_for(&text, &file_name)
        }
        None => None,
    };
    let s = |k: &str| release.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let page = Some(s("htmlUrl")).filter(|p| !p.is_empty()).unwrap_or_else(|| s("html_url"));
    Ok(Some(Update {
        title: Some(s("title")).filter(|t| !t.is_empty()).or_else(|| Some(s("name")).filter(|t| !t.is_empty())).unwrap_or_else(|| format!("luCode {version}")),
        page_url: if page.starts_with('/') { format!("{base}{page}") } else { page },
        version,
        file_name,
        file_url,
        sha256,
    }))
}

/// Höchste Version unter den veröffentlichten `macos-*`-Releases.
fn newest(items: &[Value]) -> Option<(Version, Value)> {
    items
        .iter()
        .filter(|r| !r.get("isDraft").or_else(|| r.get("draft")).and_then(Value::as_bool).unwrap_or(false))
        .filter(|r| !r.get("isPrerelease").or_else(|| r.get("prerelease")).and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|r| {
            let tag = r.get("tagName").or_else(|| r.get("tag_name")).and_then(Value::as_str)?;
            // Gemeinsames Release für beide Plattformen (`v<version>`) oder das alte je Plattform
            let v = tag.strip_prefix(TAG_PREFIX).or_else(|| tag.strip_prefix('v'))?.trim_start_matches('v');
            Some((Version::parse(v).ok()?, r.clone()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
}

/// Angehängte Dateien als (Name, absolute Adresse).
fn attachments(release: &Value, base: &str) -> Vec<(String, String)> {
    let list = release
        .get("attachments")
        .or_else(|| release.get("assets"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    list.iter()
        .filter_map(|a| {
            let name = a.get("name").and_then(Value::as_str)?.to_string();
            let url = ["browserDownloadUrl", "downloadUrl", "url", "browser_download_url"]
                .iter()
                .find_map(|k| a.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))?;
            let url = if url.starts_with('/') { format!("{base}{url}") } else { url.to_string() };
            Some((name, url))
        })
        .collect()
}

/// Datei (DMG bzw. EXE) für diese Architektur, sonst eine universelle, sonst irgendeine. Der
/// Windows-Installer (`luCode-<version>-Setup.exe`) zählt nicht – die App ersetzt nur ihr Programm.
fn pick_dmg(files: &[(String, String)]) -> Option<(String, String)> {
    let dmgs: Vec<&(String, String)> = files
        .iter()
        .filter(|(n, _)| n.to_lowercase().ends_with(ASSET_EXT) && !n.to_lowercase().contains("setup"))
        .collect();
    let lower = |n: &str| n.to_lowercase();
    dmgs.iter()
        .find(|(n, _)| lower(n).contains(arch_tag()))
        .or_else(|| dmgs.iter().find(|(n, _)| lower(n).contains("universal")))
        .or_else(|| dmgs.first())
        .map(|f| (*f).clone())
}

/// Zeile `<sha256>  <name>` aus einer SHA256SUMS-Datei.
fn sum_for(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let sum = it.next()?;
        let file = it.next()?.trim_start_matches('*');
        (file == name && sum.len() == 64).then(|| sum.to_lowercase())
    })
}

/// Lädt die DMG in den Cache-Ordner; `progress` bekommt 0.0 … 1.0 (oder -1, wenn die Grösse
/// unbekannt ist).
pub async fn download(update: &Update, progress: impl Fn(f64)) -> Result<PathBuf, String> {
    let dir = cache_dir();
    tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    let target = dir.join(&update.file_name);
    let part = dir.join(format!("{}.part", update.file_name));
    let resp = client()?.get(&update.file_url).send().await.map_err(|e| format!("Download fehlgeschlagen: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Download fehlgeschlagen: {}", resp.status()));
    }
    let total = resp.content_length();
    let mut file = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download abgebrochen: {e}"))?;
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        done += chunk.len() as u64;
        progress(total.map(|t| done as f64 / t.max(1) as f64).unwrap_or(-1.0));
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    if let Some(expected) = &update.sha256 {
        let got = hex::encode(hasher.finalize());
        if &got != expected {
            let _ = tokio::fs::remove_file(&part).await;
            return Err("Prüfsumme stimmt nicht – Update abgebrochen".into());
        }
    }
    tokio::fs::rename(&part, &target).await.map_err(|e| e.to_string())?;
    Ok(target)
}

fn cache_dir() -> PathBuf {
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("luCode").join("updates");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join("Library/Caches/luCode/updates")
}

/// Bundle der laufenden App (`…/luCode.app`), wenn sie aus einem Bundle läuft.
#[cfg(target_os = "macos")]
pub fn current_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?.to_path_buf();
    (bundle.extension().is_some_and(|e| e == "app") && exe.parent()?.ends_with("Contents/MacOS")).then_some(bundle)
}

/// Ergebnis der Installation.
pub enum Installed {
    /// Bundle ersetzt – App neu starten.
    Replaced(PathBuf),
    /// Kein Bundle oder keine Schreibrechte: DMG im Finder geöffnet, der Benutzer zieht die App selbst.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    OpenedDmg(String),
}

/// Windows: laufendes Programm umbenennen, neue EXE an seine Stelle.
#[cfg(not(target_os = "macos"))]
pub fn install(file: &Path) -> Result<Installed, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = exe.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("Programm lässt sich nicht ersetzen: {e}"))?;
    if let Err(e) = std::fs::copy(file, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("Neue Version lässt sich nicht einsetzen: {e}"));
    }
    let _ = std::fs::remove_file(file);
    Ok(Installed::Replaced(exe))
}

// ------------------------------------------------------------------ Vorgemerkte Updates

/// Geladenes, geprüftes Update, das auf einen ruhigen Moment wartet. Steht als `pending.json`
/// neben der Datei im Cache-Ordner und überlebt so das Beenden der App.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pending {
    version: String,
    file: PathBuf,
    sha256: String,
}

fn pending_path() -> PathBuf {
    cache_dir().join("pending.json")
}

fn file_sha256(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Geladenes Update vormerken. Nur mit der Prüfsumme des Releases – und die Datei muss ihr
/// (noch) entsprechen.
pub fn stage(update: &Update, file: &Path) -> Result<(), String> {
    let sha256 = file_sha256(file)?;
    if update.sha256.as_deref() != Some(sha256.as_str()) {
        return Err("Prüfsumme fehlt oder stimmt nicht – das Update wird nicht vorgemerkt".into());
    }
    let pending = Pending { version: update.version.to_string(), file: file.to_path_buf(), sha256 };
    let data = serde_json::to_vec_pretty(&pending).map_err(|e| e.to_string())?;
    std::fs::write(pending_path(), data).map_err(|e| e.to_string())
}

/// Vorgemerktes Update, wenn es neuer ist als `current` und seine Datei unverändert im
/// Cache-Ordner liegt; ein überholter oder beschädigter Eintrag wird samt Datei entfernt.
pub fn pending(current: &Version) -> Option<(Version, PathBuf)> {
    let p: Pending = serde_json::from_slice(&std::fs::read(pending_path()).ok()?).ok()?;
    let in_cache = p.file.parent() == Some(cache_dir().as_path());
    let valid = Version::parse(&p.version).ok().filter(|v| v > current && in_cache && file_sha256(&p.file).is_ok_and(|s| s == p.sha256));
    match valid {
        Some(version) => Some((version, p.file)),
        None => {
            clear_pending();
            if in_cache {
                let _ = std::fs::remove_file(&p.file);
            }
            None
        }
    }
}

pub fn clear_pending() {
    let _ = std::fs::remove_file(pending_path());
}

/// Kann sich das Programm ohne Zutun ersetzen? Windows: sein Ordner ist beschreibbar. macOS: die
/// App läuft aus einem Bundle in einem beschreibbaren Ordner (sonst zieht der Benutzer sie selbst
/// aus der DMG). Nur dann gibt es automatische Updates.
pub fn can_self_replace() -> bool {
    #[cfg(target_os = "macos")]
    let dir = current_bundle().and_then(|b| b.parent().map(Path::to_path_buf));
    #[cfg(not(target_os = "macos"))]
    let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    dir.is_some_and(|d| writable(&d))
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".lucode-probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Beim Start der App, bevor irgendetwas läuft: ein vorgemerktes Update installieren. `true` =
/// die neue Version startet gleich, dieser Prozess soll enden.
pub fn install_pending() -> bool {
    let Some((version, file)) = pending(&current_version()) else { return false };
    // Eintrag gleich entfernen: scheitert die Installation, gibt es keine Schleife aus Neustarts
    clear_pending();
    if !can_self_replace() {
        return false;
    }
    tracing::info!("Vorgemerktes Update {version} wird installiert");
    match install(&file) {
        Ok(Installed::Replaced(target)) => {
            relaunch_after_exit(&target);
            true
        }
        Ok(Installed::OpenedDmg(text)) => {
            tracing::warn!("Update {version}: {text}");
            false
        }
        Err(e) => {
            tracing::warn!("Update {version}: {e}");
            false
        }
    }
}

/// Windows: Rest eines Updates (umbenanntes altes Programm) wegräumen.
pub fn cleanup() {
    #[cfg(not(target_os = "macos"))]
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old.exe"));
    }
}

/// Windows: neue Version starten; sie wartet, bis diese beendet ist (`--relaunched`).
#[cfg(not(target_os = "macos"))]
pub fn relaunch_after_exit(exe: &Path) {
    let _ = Command::new(exe).arg("--relaunched").spawn();
}

/// DMG einhängen, App darin ins laufende Bundle kopieren, aushängen.
#[cfg(target_os = "macos")]
pub fn install(dmg: &Path) -> Result<Installed, String> {
    let Some(bundle) = current_bundle() else {
        open_dmg(dmg)?;
        return Ok(Installed::OpenedDmg("Die App läuft nicht aus einem Programm-Bundle – die DMG wurde geöffnet.".into()));
    };
    let parent = bundle.parent().ok_or("Bundle ohne Ordner")?.to_path_buf();

    let mount = std::env::temp_dir().join(format!("lucode-update-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&mount);
    run(Command::new("/usr/bin/hdiutil").arg("attach").arg(dmg).args(["-nobrowse", "-readonly", "-noautoopen", "-mountpoint"]).arg(&mount))?;
    let result = (|| {
        let app = std::fs::read_dir(&mount)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or("In der DMG ist keine App")?;
        let staged = parent.join(".luCode-update.app");
        let old = parent.join(".luCode-old.app");
        let _ = std::fs::remove_dir_all(&staged);
        let _ = std::fs::remove_dir_all(&old);
        if let Err(e) = run(Command::new("/usr/bin/ditto").arg(&app).arg(&staged)) {
            let _ = std::fs::remove_dir_all(&staged);
            return Err(format!("Keine Schreibrechte in {}: {e}", parent.display()));
        }
        let _ = Command::new("/usr/bin/xattr").args(["-dr", "com.apple.quarantine"]).arg(&staged).status();
        std::fs::rename(&bundle, &old).map_err(|e| format!("Altes Programm lässt sich nicht verschieben: {e}"))?;
        if let Err(e) = std::fs::rename(&staged, &bundle) {
            let _ = std::fs::rename(&old, &bundle);
            return Err(format!("Neues Programm lässt sich nicht einsetzen: {e}"));
        }
        let _ = std::fs::remove_dir_all(&old);
        Ok(bundle.clone())
    })();
    let _ = Command::new("/usr/bin/hdiutil").arg("detach").arg(&mount).arg("-quiet").status();
    let _ = std::fs::remove_dir(&mount);
    match result {
        Ok(b) => {
            let _ = std::fs::remove_file(dmg);
            Ok(Installed::Replaced(b))
        }
        Err(e) => {
            open_dmg(dmg)?;
            Ok(Installed::OpenedDmg(format!("{e}\n\nDie DMG wurde geöffnet – ziehe luCode in den Ordner Programme.")))
        }
    }
}

#[cfg(target_os = "macos")]
fn open_dmg(dmg: &Path) -> Result<(), String> {
    run(Command::new("/usr/bin/open").arg(dmg))
}

/// Startet das Bundle neu, sobald dieser Prozess beendet ist.
#[cfg(target_os = "macos")]
pub fn relaunch_after_exit(bundle: &Path) {
    let pid = std::process::id().to_string();
    let _ = Command::new("/bin/sh")
        .args(["-c", "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; /usr/bin/open \"$0\"", &bundle.to_string_lossy(), &pid])
        .spawn();
}

#[cfg(target_os = "macos")]
fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[cfg(target_os = "macos")]
    fn picks_newest_macos_release() {
        let items = vec![
            json!({"tagName": "umbrel-9.9.9", "isDraft": false, "isPrerelease": false}),
            json!({"tagName": "macos-1.0.6", "isDraft": false, "isPrerelease": false}),
            json!({"tagName": "macos-1.1.0", "isDraft": true, "isPrerelease": false}),
            json!({"tagName": "macos-1.0.10", "isDraft": false, "isPrerelease": false}),
            json!({"tagName": "macos-2.0.0", "isDraft": false, "isPrerelease": true}),
        ];
        assert_eq!(newest(&items).unwrap().0, Version::new(1, 0, 10));
        let mut items = items;
        items.push(json!({"tagName": "v1.0.12", "isDraft": false, "isPrerelease": false}));
        assert_eq!(newest(&items).unwrap().0, Version::new(1, 0, 12));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn picks_dmg_and_sum() {
        let files = vec![
            ("SHA256SUMS".to_string(), "u0".to_string()),
            ("luCode-1.0.6-x86_64.dmg".to_string(), "u1".to_string()),
            ("luCode-1.0.6-arm64.dmg".to_string(), "u2".to_string()),
        ];
        let (name, _) = pick_dmg(&files).unwrap();
        assert!(name.contains(arch_tag()));
        let sum = "a".repeat(64);
        assert_eq!(sum_for(&format!("{sum}  {name}\n"), &name), Some(sum));
    }

    #[test]
    fn staged_update_needs_the_release_checksum() {
        let dir = std::env::temp_dir().join(format!("lucode-updater-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("luCode-9.9.9-x64.exe");
        std::fs::write(&file, b"abc").unwrap();
        // SHA-256 von "abc"
        assert_eq!(file_sha256(&file).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let update = |sha: Option<&str>| Update {
            version: Version::new(9, 9, 9),
            title: String::new(),
            file_name: "luCode-9.9.9-x64.exe".into(),
            file_url: String::new(),
            sha256: sha.map(str::to_string),
            page_url: String::new(),
        };
        // Ohne Prüfsumme des Releases oder mit einer anderen: nicht vormerken
        assert!(stage(&update(None), &file).is_err());
        assert!(stage(&update(Some(&"0".repeat(64))), &file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_attachment_urls() {
        let r = json!({"attachments": [{"name": "a.dmg", "downloadUrl": "/x/a.dmg"}]});
        assert_eq!(attachments(&r, "http://h:1"), vec![("a.dmg".to_string(), "http://h:1/x/a.dmg".to_string())]);
    }
}

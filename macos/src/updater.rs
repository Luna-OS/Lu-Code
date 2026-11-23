//! Automatische Updates aus den Releases von GitHub (Luna-OS/Lu-Code). Die Antwort eines nrzGit
//! oder luGit (`LUCODE_UPDATE_REPO`) wird ebenso verstanden.
//!
//! Windows: Release `v<version>` mit `luCode-<version>-x64.zip` (darin `luCode/luCode.exe`). Die
//! App lädt die ZIP, prüft sie gegen `SHA256SUMS`, packt `luCode.exe` neben das laufende Programm
//! aus (`luCode.new.exe`) und prüft, dass es ein vollständiges Windows-Programm für diese
//! Architektur ist. Erst dann wird getauscht: das laufende Programm umbenannt (`luCode.old.exe`,
//! das erlaubt Windows), das neue an seine Stelle und gestartet. Startet es nicht, kommt das alte
//! zurück ([`relaunch_or_rollback`]).
//!
//! **Keine einzelne EXE im Release.** Bis 1.1.1 nahm die App bei GitHub statt der Download-Adresse
//! einer Datei (`browser_download_url`) deren API-Adresse (`url`) – die liefert JSON mit der
//! Beschreibung der Datei, nicht die Datei. `SHA256SUMS` kam ebenso als JSON, also ohne
//! Prüfsumme, und so setzte „Aktualisieren“ dieses JSON als `luCode.exe` ein: Windows meldete
//! danach „Nicht unterstützte 16-Bit-Anwendung“, luCode musste neu installiert werden. Diese
//! Versionen suchen fürs Update eine `.exe` ohne „Setup“ im Namen – liegt keine bei, melden sie
//! nur, dass das Release keine passende Datei hat, statt sich zu zerstören. `scripts/windows.sh`
//! baut darum keine `luCode-<version>-x64.exe` mehr, `scripts/release.sh` lehnt sie ab. Von bis
//! 1.1.1 wird einmal von Hand aktualisiert (`luCode-<version>-Setup.exe`). Eine einzelne EXE
//! (ältere Releases, eigene Plattformen) versteht die App weiterhin.
//!
//! Den Installer (`luCode-<version>-Setup.exe`) nimmt die App nie – sie ersetzt nur ihr Programm.
//!
//! macOS: Release `v<version>` (früher `macos-<version>`) mit `luCode-<version>-<arch>.dmg`. Die
//! App lädt die DMG, prüft die Prüfsumme, ersetzt das Programm-Bundle und startet neu.
//!
//! Die App fragt beim Start und danach alle paar Stunden die Release-Liste ab und meldet eine
//! neuere Version (Hinweis mit Knopf im Fenster und Eintrag im Menü). Eingesetzt wird nur, was zur
//! Prüfsumme des Releases passt – ohne `SHA256SUMS` öffnet „Aktualisieren“ die Release-Seite.
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
/// Endungen der Datei im Release, die erste passende gewinnt. Windows: die ZIP, sonst (ältere
/// Releases, eigene Plattformen) eine einzelne EXE.
const ASSET_EXTS: &[&str] = if cfg!(target_os = "macos") { &[".dmg"] } else { &[".zip", ".exe"] };
/// Name des Programms in der ZIP (`luCode/luCode.exe`).
#[cfg(not(target_os = "macos"))]
const PROGRAM_NAME: &str = "luCode.exe";
/// Kleiner ist kein luCode (das Programm hat rund 30 MB) – fängt abgebrochene Downloads ab.
const MIN_PROGRAM_SIZE: u64 = 1024 * 1024;
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

/// Anfrage nach einer Datei des Releases. `Accept: application/octet-stream`: Landet doch einmal
/// eine API-Adresse von GitHub hier (`…/releases/assets/<id>`), kommt so die Datei statt JSON.
fn get_file(http: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
    http.get(url).header(reqwest::header::ACCEPT, "application/octet-stream")
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
    let Some((file_name, file_url)) = pick_file(&files) else {
        return Err(format!("Release {version} hat keine Datei ({}) für {}", ASSET_EXTS.join(", "), arch_tag()));
    };
    let sha256 = match files.iter().find(|(n, _)| n.eq_ignore_ascii_case("SHA256SUMS")) {
        Some((_, url)) => {
            let text = get_file(&http, url)
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|e| format!("Prüfsummen nicht abrufbar: {e}"))?
                .text()
                .await
                .map_err(|e| e.to_string())?;
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

/// Angehängte Dateien als (Name, absolute Adresse zum Herunterladen).
///
/// GitHub nennt je Datei zwei Adressen: `browser_download_url` liefert die Datei, `url` (API)
/// ohne besonderen `Accept`-Kopf nur ihre Beschreibung als JSON. Bis 1.1.1 stand `url` vorn – die
/// App lud JSON statt des Programms (siehe Moduldoku). nrzGit und luGit nennen
/// `browserDownloadUrl` bzw. `downloadUrl`.
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
            let url = ["browserDownloadUrl", "browser_download_url", "downloadUrl", "url"]
                .iter()
                .find_map(|k| a.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))?;
            let url = if url.starts_with('/') { format!("{base}{url}") } else { url.to_string() };
            Some((name, url))
        })
        .collect()
}

/// Datei für diese Architektur (je Endung aus [`ASSET_EXTS`], in dieser Reihenfolge): mit der
/// Architektur im Namen, sonst eine universelle, sonst irgendeine – eine ZIP aber nur mit der
/// Architektur im Namen (sonst wäre es vielleicht die einer anderen Plattform). Der
/// Windows-Installer (`luCode-<version>-Setup.exe`) zählt nicht – die App ersetzt nur ihr Programm.
fn pick_file(files: &[(String, String)]) -> Option<(String, String)> {
    let lower = |n: &str| n.to_lowercase();
    ASSET_EXTS.iter().find_map(|ext| {
        let found: Vec<&(String, String)> = files.iter().filter(|(n, _)| lower(n).ends_with(ext) && !lower(n).contains("setup")).collect();
        found
            .iter()
            .find(|(n, _)| lower(n).contains(arch_tag()))
            .or_else(|| found.iter().find(|(n, _)| lower(n).contains("universal")))
            .or_else(|| if *ext == ".zip" { None } else { found.first() })
            .map(|f| (*f).clone())
    })
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

/// Lädt die Datei des Releases (ZIP, EXE bzw. DMG) in den Cache-Ordner; `progress` bekommt
/// 0.0 … 1.0 (oder -1, wenn die Grösse unbekannt ist). Nur mit der Prüfsumme des Releases – und
/// die Datei muss ihr entsprechen und sein, was sie sein soll ([`verify_asset`]).
pub async fn download(update: &Update, progress: impl Fn(f64)) -> Result<PathBuf, String> {
    let Some(expected) = update.sha256.clone() else {
        return Err(format!(
            "Das Release {} nennt keine Prüfsumme (SHA256SUMS) – ohne sie setzt luCode nichts ein. Bitte von der Release-Seite installieren.",
            update.version
        ));
    };
    let dir = cache_dir();
    tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    let target = dir.join(&update.file_name);
    let part = dir.join(format!("{}.part", update.file_name));
    let resp = get_file(&client()?, &update.file_url).send().await.map_err(|e| format!("Download fehlgeschlagen: {e}"))?;
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
    let got = hex::encode(hasher.finalize());
    if got != expected {
        let _ = tokio::fs::remove_file(&part).await;
        return Err("Prüfsumme stimmt nicht – Update abgebrochen".into());
    }
    let checked = part.clone();
    let name = update.file_name.clone();
    let verified = tokio::task::spawn_blocking(move || verify_asset(&checked, &name)).await.map_err(|e| e.to_string())?;
    if let Err(e) = verified {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!("{e} – Update abgebrochen"));
    }
    tokio::fs::rename(&part, &target).await.map_err(|e| e.to_string())?;
    Ok(target)
}

// ------------------------------------------------------------------ Ist es das Programm?

/// Ist die Datei des Releases, was sie sein soll? Windows: eine ZIP mit `luCode.exe` darin bzw.
/// eine EXE – jeweils ein Windows-Programm für diese Architektur. macOS: eine DMG. `name` ist der
/// Name der Datei im Release (die geladene Datei heisst im Cache vorübergehend `….part`).
pub fn verify_asset(path: &Path, name: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = name;
        check_dmg(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        if is_zip(name) {
            let (head, size) = with_program_in_zip(path, |size, entry| {
                use std::io::Read;
                let mut head = Vec::with_capacity(4096);
                entry.take(4096).read_to_end(&mut head).map_err(|e| e.to_string())?;
                Ok((head, size))
            })?;
            check_program_head(&head, size)
        } else {
            check_program(path)
        }
    }
}

/// Endet der Name auf `.zip`?
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn is_zip(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".zip")
}

/// `luCode.exe` in der ZIP suchen und `f` mit seiner Grösse und seinem Inhalt aufrufen. Wer bis
/// zum Ende liest, bekommt einen Fehler, falls der Inhalt nicht zur Prüfsumme (CRC-32) in der ZIP
/// passt.
#[cfg(not(target_os = "macos"))]
fn with_program_in_zip<T>(zip: &Path, f: impl FnOnce(u64, &mut dyn std::io::Read) -> Result<T, String>) -> Result<T, String> {
    let file = std::fs::File::open(zip).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Keine gültige ZIP: {e}"))?;
    let index = (0..archive.len())
        .find(|&i| {
            archive.by_index(i).is_ok_and(|e| {
                let name = e.name().map(|n| n.replace('\\', "/")).unwrap_or_default();
                e.is_file() && name.rsplit('/').next().is_some_and(|n| n.eq_ignore_ascii_case(PROGRAM_NAME))
            })
        })
        .ok_or_else(|| format!("In der ZIP ist kein {PROGRAM_NAME}"))?;
    let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
    f(entry.size(), &mut entry)
}

/// Maschinen-Kennung im PE-Kopf für diese Architektur.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn pe_machine() -> u16 {
    match std::env::consts::ARCH {
        "aarch64" => 0xAA64,
        "x86" => 0x014C,
        _ => 0x8664,
    }
}

/// Ist das ein Windows-Programm (PE) für diese Architektur? `head` sind die ersten Bytes der
/// Datei (4 KB genügen), `size` ihre Grösse. Ein Programm beginnt mit `MZ`, bei `0x3C` steht, wo
/// der PE-Kopf liegt (`PE\0\0`, danach die Maschine). Beginnt eine Datei nicht mit `MZ`, meldet
/// Windows „Nicht unterstützte 16-Bit-Anwendung“.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn check_program_head(head: &[u8], size: u64) -> Result<(), String> {
    const NOT: &str = "Die neue Version ist kein Windows-Programm";
    if size < MIN_PROGRAM_SIZE {
        return Err(format!("{NOT} (nur {size} Bytes)"));
    }
    if !head.starts_with(b"MZ") {
        return Err(NOT.into());
    }
    let at = head.get(0x3C..0x40).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or(NOT)?;
    let pe = head.get(at..at + 6).ok_or(NOT)?;
    if &pe[..4] != b"PE\0\0" {
        return Err(NOT.into());
    }
    let machine = u16::from_le_bytes([pe[4], pe[5]]);
    if machine != pe_machine() {
        return Err(format!("Die neue Version ist für eine andere Architektur (0x{machine:04X}) gebaut"));
    }
    Ok(())
}

/// [`check_program_head`] für eine Datei.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn check_program(path: &Path) -> Result<(), String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let mut head = Vec::with_capacity(4096);
    file.by_ref().take(4096).read_to_end(&mut head).map_err(|e| e.to_string())?;
    check_program_head(&head, size)
}

/// Ist das eine DMG? Eine UDIF-Abbilddatei endet mit einem 512 Byte grossen Block, der mit
/// `koly` beginnt.
#[cfg(target_os = "macos")]
fn check_dmg(path: &Path) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    const NOT: &str = "Die geladene Datei ist keine DMG";
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if size < MIN_PROGRAM_SIZE {
        return Err(format!("{NOT} (nur {size} Bytes)"));
    }
    let mut tail = [0u8; 512];
    file.seek(SeekFrom::End(-512)).map_err(|e| e.to_string())?;
    file.read_exact(&mut tail).map_err(|e| e.to_string())?;
    if &tail[..4] != b"koly" {
        return Err(NOT.into());
    }
    Ok(())
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

/// Windows: neue Version neben das laufende Programm schreiben (`luCode.new.exe`, aus der ZIP bzw.
/// als Kopie der EXE) und prüfen; erst wenn sie ein vollständiges Windows-Programm ist, das
/// laufende umbenennen (`luCode.old.exe`) und die neue an seine Stelle. Scheitert ein Schritt,
/// bleibt `luCode.exe`, wie es war.
#[cfg(not(target_os = "macos"))]
pub fn install(file: &Path) -> Result<Installed, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = exe.with_extension("old.exe");
    let new = exe.with_extension("new.exe");
    let _ = std::fs::remove_file(&new);
    if let Err(e) = write_program(file, &new).and_then(|()| check_program(&new)) {
        let _ = std::fs::remove_file(&new);
        return Err(format!("Neue Version lässt sich nicht einsetzen: {e}"));
    }
    let _ = std::fs::remove_file(&old);
    if let Err(e) = rename_retry(&exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(format!("Programm lässt sich nicht ersetzen: {e}"));
    }
    if let Err(e) = rename_retry(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        let _ = std::fs::remove_file(&new);
        return Err(format!("Neue Version lässt sich nicht einsetzen: {e}"));
    }
    let _ = std::fs::remove_file(file);
    Ok(Installed::Replaced(exe))
}

/// Programm aus der Datei des Releases nach `to` schreiben: aus der ZIP auspacken (prüft dabei
/// CRC-32 und Grösse) bzw. die EXE kopieren.
#[cfg(not(target_os = "macos"))]
fn write_program(file: &Path, to: &Path) -> Result<(), String> {
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if is_zip(&name) {
        with_program_in_zip(file, |size, entry| {
            let mut out = std::fs::File::create(to).map_err(|e| e.to_string())?;
            let written = std::io::copy(entry, &mut out).map_err(|e| format!("Auspacken: {e}"))?;
            if written != size {
                return Err(format!("Auspacken: {written} statt {size} Bytes"));
            }
            out.sync_all().map_err(|e| e.to_string())
        })
    } else {
        std::fs::copy(file, to).map(drop).map_err(|e| e.to_string())
    }
}

/// Umbenennen, bei Bedarf mehrmals: Gleich nach dem Schreiben hält oft der Virenscanner die neue
/// Datei kurz offen.
#[cfg(not(target_os = "macos"))]
fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(_) if tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            r => return r,
        }
    }
}

/// Windows: Update zurücknehmen – das bisherige Programm (`luCode.old.exe`) wieder an seinen
/// Platz, die neue Version weg. Für den Fall, dass sie sich nicht starten lässt.
#[cfg(not(target_os = "macos"))]
fn rollback(exe: &Path) -> Result<(), String> {
    let old = exe.with_extension("old.exe");
    if !old.is_file() {
        return Err("Die bisherige Version ist nicht mehr da".into());
    }
    let failed = exe.with_extension("new.exe");
    let _ = std::fs::remove_file(&failed);
    rename_retry(exe, &failed).map_err(|e| e.to_string())?;
    if let Err(e) = rename_retry(&old, exe) {
        let _ = std::fs::rename(&failed, exe);
        return Err(e.to_string());
    }
    let _ = std::fs::remove_file(&failed);
    Ok(())
}

/// Nach dem Einsetzen: neue Version starten (sie wartet, bis dieser Prozess endet). `Ok` = dieser
/// Prozess soll jetzt enden. Lässt sie sich nicht starten, kommt unter Windows das bisherige
/// Programm zurück – luCode läuft dann einfach weiter.
pub fn relaunch_or_rollback(target: &Path) -> Result<(), String> {
    let Err(e) = relaunch_after_exit(target) else { return Ok(()) };
    #[cfg(not(target_os = "macos"))]
    if let Err(r) = rollback(target) {
        return Err(format!("Die neue Version startet nicht ({e}), und die bisherige liess sich nicht zurückholen ({r}). Bitte luCode neu installieren."));
    }
    Err(format!("Die neue Version startet nicht ({e}) – die bisherige bleibt."))
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
    verify_asset(file, &update.file_name)?;
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
        Ok(Installed::Replaced(target)) => match relaunch_or_rollback(&target) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Update {version}: {e}");
                false
            }
        },
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

/// Windows: Reste eines Updates (umbenanntes altes Programm, halb geschriebenes neues) wegräumen.
pub fn cleanup() {
    #[cfg(not(target_os = "macos"))]
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old.exe"));
        let _ = std::fs::remove_file(exe.with_extension("new.exe"));
    }
}

/// Windows: neue Version starten; sie wartet, bis diese beendet ist (`--relaunched`).
#[cfg(not(target_os = "macos"))]
fn relaunch_after_exit(exe: &Path) -> Result<(), String> {
    Command::new(exe).arg("--relaunched").spawn().map(drop).map_err(|e| e.to_string())
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
fn relaunch_after_exit(bundle: &Path) -> Result<(), String> {
    let pid = std::process::id().to_string();
    Command::new("/bin/sh")
        .args(["-c", "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; /usr/bin/open \"$0\"", &bundle.to_string_lossy(), &pid])
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
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
        let (name, _) = pick_file(&files).unwrap();
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
        // Auch mit passender Prüfsumme: Was kein Programm ist, wird nicht vorgemerkt
        assert!(stage(&update(Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")), &file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_attachment_urls() {
        let r = json!({"attachments": [{"name": "a.dmg", "downloadUrl": "/x/a.dmg"}]});
        assert_eq!(attachments(&r, "http://h:1"), vec![("a.dmg".to_string(), "http://h:1/x/a.dmg".to_string())]);
    }

    #[test]
    fn github_assets_load_the_file_not_its_description() {
        // So beschreibt GitHub eine Datei: `url` (API) liefert JSON, `browser_download_url` die Datei.
        // Bis 1.1.1 stand `url` vorn – geladen wurde JSON und als luCode.exe eingesetzt.
        let r = json!({"assets": [
            {
                "name": "luCode-1.1.2-x64.zip",
                "url": "https://api.github.com/repos/Luna-OS/Lu-Code/releases/assets/1",
                "browser_download_url": "https://github.com/Luna-OS/Lu-Code/releases/download/v1.1.2/luCode-1.1.2-x64.zip"
            },
            {
                "name": "SHA256SUMS",
                "url": "https://api.github.com/repos/Luna-OS/Lu-Code/releases/assets/2",
                "browser_download_url": "https://github.com/Luna-OS/Lu-Code/releases/download/v1.1.2/SHA256SUMS"
            }
        ]});
        let files = attachments(&r, "https://api.github.com");
        assert_eq!(files[0].1, "https://github.com/Luna-OS/Lu-Code/releases/download/v1.1.2/luCode-1.1.2-x64.zip");
        assert_eq!(files[1].1, "https://github.com/Luna-OS/Lu-Code/releases/download/v1.1.2/SHA256SUMS");
        // Ohne Download-Adresse bleibt die API-Adresse (dann mit `Accept: application/octet-stream`)
        let r = json!({"assets": [{"name": "a.zip", "url": "https://api.github.com/x/releases/assets/3"}]});
        assert_eq!(attachments(&r, "")[0].1, "https://api.github.com/x/releases/assets/3");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn windows_takes_the_zip_never_the_installer() {
        // Unter Windows x64 heissen die Dateien `luCode-<version>-x64.zip` usw.
        let a = arch_tag();
        let pick = |names: &[String]| pick_file(&names.iter().map(|n| (n.clone(), format!("u/{n}"))).collect::<Vec<_>>()).map(|(n, _)| n);
        let n = |s: &str| s.replace("ARCH", a);
        let v = |names: &[&str]| names.iter().map(|s| n(s)).collect::<Vec<_>>();
        // Release ab 1.1.2: ZIP, Installer, Prüfsummen
        assert_eq!(pick(&v(&["luCode-1.1.2-Setup.exe", "luCode-1.1.2-ARCH.zip", "SHA256SUMS"])), Some(n("luCode-1.1.2-ARCH.zip")));
        // Ältere Releases mit einzelner EXE: die ZIP bleibt vorn, sonst die EXE
        assert_eq!(pick(&v(&["luCode-1.1.1-ARCH.exe", "luCode-1.1.1-ARCH.zip"])), Some(n("luCode-1.1.1-ARCH.zip")));
        assert_eq!(pick(&v(&["luCode-1.0.9-ARCH.exe", "luCode-1.0.9-Setup.exe"])), Some(n("luCode-1.0.9-ARCH.exe")));
        // Nur der Installer oder eine ZIP ohne Architektur im Namen: nichts
        assert_eq!(pick(&v(&["luCode-1.1.2-Setup.exe", "SHA256SUMS"])), None);
        assert_eq!(pick(&v(&["luCode-1.1.2-source.zip"])), None);
    }

    /// Kopf eines Windows-Programms (MZ, PE-Kopf bei 0x80) für `machine`, aufgefüllt auf `size`.
    #[cfg(not(target_os = "macos"))]
    fn fake_program(machine: u16, size: usize) -> Vec<u8> {
        let mut b = vec![0u8; size];
        b[..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        b
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn only_a_whole_windows_program_is_installed() {
        let big = 2 * 1024 * 1024;
        let ok = fake_program(pe_machine(), big);
        assert!(check_program_head(&ok[..4096], big as u64).is_ok());
        // Was bis 1.1.1 als luCode.exe landete: die JSON-Beschreibung der Datei von GitHub
        let json = br#"{"url":"https://api.github.com/repos/Luna-OS/Lu-Code/releases/assets/1","name":"luCode-1.1.1-x64.exe"}"#;
        assert!(check_program_head(json, big as u64).is_err());
        // Abgebrochener Download, falsche Architektur, kaputter PE-Kopf
        assert!(check_program_head(&ok[..4096], 4096).is_err());
        let other = if pe_machine() == 0x8664 { 0xAA64 } else { 0x8664 };
        assert!(check_program_head(&fake_program(other, big)[..4096], big as u64).is_err());
        let mut broken = ok.clone();
        broken[0x80] = b'X';
        assert!(check_program_head(&broken[..4096], big as u64).is_err());
        let mut far = ok.clone();
        far[0x3C..0x40].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert!(check_program_head(&far[..4096], big as u64).is_err());
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn program_comes_out_of_the_zip() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("lucode-updater-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip_with = |name: &str, files: &[(&str, Vec<u8>)]| {
            let path = dir.join(name);
            let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            for (n, data) in files {
                z.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
                z.write_all(data).unwrap();
            }
            z.finish().unwrap();
            path
        };
        let program = fake_program(pe_machine(), 2 * 1024 * 1024 + 17);
        // So packt scripts/windows.sh: Ordner luCode mit luCode.exe und LIESMICH.txt
        let good = zip_with("luCode-9.9.9-x64.zip", &[("luCode/LIESMICH.txt", b"hallo".to_vec()), ("luCode/luCode.exe", program.clone())]);
        assert!(verify_asset(&good, "luCode-9.9.9-x64.zip").is_ok());
        // Im Cache heisst sie während des Downloads `….part` – geprüft wird nach dem Namen im Release
        let part = dir.join("luCode-9.9.9-x64.zip.part");
        std::fs::copy(&good, &part).unwrap();
        assert!(verify_asset(&part, "luCode-9.9.9-x64.zip").is_ok());
        let out = dir.join("luCode.new.exe");
        write_program(&good, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), program);
        assert!(check_program(&out).is_ok());
        // Ohne Programm, mit etwas anderem als Programm, oder gar keine ZIP
        let empty = zip_with("leer.zip", &[("luCode/LIESMICH.txt", b"hallo".to_vec())]);
        assert!(verify_asset(&empty, "luCode-9.9.9-x64.zip").is_err());
        let fake = zip_with("falsch.zip", &[("luCode/luCode.exe", vec![b'{'; 2 * 1024 * 1024])]);
        assert!(verify_asset(&fake, "luCode-9.9.9-x64.zip").is_err());
        let json = dir.join("json.zip");
        std::fs::write(&json, br#"{"url":"https://api.github.com/x"}"#).unwrap();
        assert!(verify_asset(&json, "luCode-9.9.9-x64.zip").is_err());
        // Eine einzelne EXE wird direkt geprüft
        let exe = dir.join("luCode-9.9.9-x64.exe");
        std::fs::write(&exe, &program).unwrap();
        assert!(verify_asset(&exe, "luCode-9.9.9-x64.exe").is_ok());
        std::fs::write(&exe, br#"{"url":"https://api.github.com/x"}"#).unwrap();
        assert!(verify_asset(&exe, "luCode-9.9.9-x64.exe").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

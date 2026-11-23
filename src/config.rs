//! Einstellungen. Quelle ist Rockets Figment: `Rocket.toml` und Umgebungsvariablen mit
//! dem Präfix `LUCODE_` (z. B. `LUCODE_DATA_DIR`, `LUCODE_PREVIEW_PORT`).

use rocket::figment::providers::{Env, Format, Toml};
use rocket::figment::Figment;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Datenordner: Datenbank, Agent-Konten, Arbeitsbereiche, Werkzeuge.
    pub data_dir: PathBuf,
    /// Öffentliche Adresse, z. B. "http://umbrel.local:8510" (leer = aus der Anfrage).
    pub root_url: String,
    /// Port des Vorschau-Proxys (zeigt Server der Agenten im Browser), 0 = aus.
    pub preview_port: u16,
    /// Cookies nur über HTTPS senden – einschalten, sobald luCode hinter HTTPS läuft.
    pub cookie_secure: bool,
    /// Hintergrundarbeiten starten. Tests schalten sie aus.
    pub background: bool,
    /// Programm `git` (Pfad oder Name im PATH).
    pub git_binary: String,
    /// Wie viele Agenten gleichzeitig laufen dürfen (je Instanz).
    pub max_parallel_runs: usize,
    /// Vorgabe für die nrzGit-Adresse (z. B. im Umbrel-Netz), wenn der Benutzer keine einträgt.
    pub nrzgit_url: String,
    /// Selbst gehostete Git-Plattformen (luGit, nrzGit) jedem anbieten. Ohne das zeigt eine
    /// frische Instanz nur GitHub; luGit und nrzGit erscheinen erst, wenn sie verbunden sind.
    /// Die Verwaltung kann es unter Einstellungen → Instanz umstellen.
    pub private_forges: bool,
    /// Desktop-Modus (macOS-App): Token für die Anmeldung ohne Passwort (`/__desktop/login`).
    /// Leer = aus.
    pub desktop_token: String,
    /// Desktop-Modus: Benutzername und voller Name für das lokale Konto beim ersten Start.
    pub desktop_user: String,
    pub desktop_full_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            root_url: String::new(),
            preview_port: 8511,
            cookie_secure: false,
            background: true,
            git_binary: "git".into(),
            max_parallel_runs: 8,
            nrzgit_url: String::new(),
            private_forges: false,
            desktop_token: String::new(),
            desktop_user: String::new(),
            desktop_full_name: String::new(),
        }
    }
}

impl Config {
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("lucode.db")
    }

    /// Läuft der Server in der Desktop-App (Windows, macOS)? Dann sind Server, Agenten und der
    /// Browser des Benutzers auf demselben Rechner.
    pub fn is_desktop(&self) -> bool {
        !self.desktop_token.is_empty()
    }

    /// `<data>/accounts/<id>` – je Agent-Konto ein eigenes Home (`home/`).
    pub fn accounts_dir(&self) -> PathBuf {
        self.data_dir.join("accounts")
    }

    /// `<data>/workspaces/<projekt-id>` – Repository (`repo/`) und Worktrees (`worktrees/<thread>`).
    pub fn workspaces_dir(&self) -> PathBuf {
        self.data_dir.join("workspaces")
    }

    /// `<data>/scratch/<thread-id>` – Arbeitsordner für Threads ohne Projekt.
    pub fn scratch_dir(&self) -> PathBuf {
        self.data_dir.join("scratch")
    }

    /// `<data>/tools` – installierte Agent-CLIs (npm-Präfix `tools/npm`) und weitere Werkzeuge.
    pub fn tools_dir(&self) -> PathBuf {
        self.data_dir.join("tools")
    }

    /// `<data>/attachments` – angehängte Dateien (Bilder im Composer).
    pub fn attachments_dir(&self) -> PathBuf {
        self.data_dir.join("attachments")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.data_dir.join("tmp")
    }

    /// Heruntergeladene Modelle (Spracherkennung): `models/whisper-<grösse>/`.
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }
}

/// Rocket-Konfiguration plus `LUCODE_*`-Umgebung (statt `ROCKET_*`).
pub fn figment() -> Figment {
    Figment::from(rocket::Config::default())
        .merge(Toml::file(Env::var_or("LUCODE_CONFIG", "Rocket.toml")).nested())
        // LUCODE_LOG ist der tracing-Filter (main.rs), keine Rocket-Einstellung
        .merge(Env::prefixed("LUCODE_").ignore(&["PROFILE", "LOG"]).global())
        .select(rocket::figment::Profile::from_env_or(
            "LUCODE_PROFILE",
            rocket::Config::DEFAULT_PROFILE,
        ))
}

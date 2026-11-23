//! Umzug von nrzCode: luCode übernimmt den Datenordner einer nrzCode-Instanz – Konten,
//! Git-Verbindungen, Projekte samt Checkouts, Chats, Bots, Skills, Notizen, Canvas, Werkzeuge.
//! Der Import lässt sich wiederholen: schon Übernommenes wird erkannt und nicht doppelt angelegt.
//!
//! `GET /api/v1/import/nrzcode` (Quelle finden und zählen), `POST /api/v1/import/nrzcode`
//! (starten), `GET /api/v1/import/nrzcode/status` (Fortschritt) – nur für Administratoren.

use serde::{Deserialize, Serialize};

/// Zahlen einer Quelle bzw. eines Imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportCounts {
    pub users: i64,
    pub accounts: i64,
    pub connections: i64,
    pub projects: i64,
    pub threads: i64,
    pub messages: i64,
    pub bots: i64,
    pub skills: i64,
    pub notes: i64,
    pub attachments: i64,
    pub boards: i64,
}

impl ImportCounts {
    /// Gibt es überhaupt etwas?
    pub fn is_empty(&self) -> bool {
        *self == ImportCounts::default()
    }

    /// Alles ausser Benutzern (die gibt es meist schon).
    pub fn content(&self) -> i64 {
        self.accounts + self.connections + self.projects + self.threads + self.messages + self.bots + self.skills + self.notes + self.attachments + self.boards
    }
}

/// Gefundene nrzCode-Instanz (`GET /import/nrzcode?path=`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportSource {
    /// Datenordner von nrzCode (dort liegt `nrzcode.db`)
    pub path: String,
    pub found: bool,
    /// Warum die Quelle nicht lesbar ist (leer = in Ordnung)
    pub error: String,
    /// Was in der Quelle liegt
    pub counts: ImportCounts,
    /// Was davon noch nicht übernommen ist
    pub pending: ImportCounts,
    /// Benutzernamen der Quelle
    pub users: Vec<String>,
    /// Letzter Import von dieser Quelle (Unix-Sekunden, 0 = noch nie)
    pub last_import_at: i64,
}

/// Was übernommen wird (`POST /import/nrzcode`). Ohne Angaben: alles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportOptions {
    /// Datenordner von nrzCode (leer = der automatisch gefundene)
    pub path: String,
    /// Agent-Konten samt Anmeldung (Home-Ordner der CLIs)
    pub accounts: bool,
    /// Verbindungen zu nrzGit, luGit und GitHub
    pub connections: bool,
    /// Projekte samt Checkouts und Worktrees, Chats mit Verlauf, Anhänge, Nutzung
    pub workspaces: bool,
    /// Bots, Skills, Notizen, Canvas, KI-Ketten, Sprachsteuerung
    pub extras: bool,
    /// Installierte Agent-CLIs und Sprachmodelle
    pub tools: bool,
    /// Build-Zwischenstände nicht kopieren (`node_modules`, Cargo-`target` …) – sie entstehen
    /// beim nächsten Build neu
    pub skip_build_dirs: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions { path: String::new(), accounts: true, connections: true, workspaces: true, extras: true, tools: true, skip_build_dirs: true }
    }
}

/// Stand des Imports (`GET /import/nrzcode/status`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportStatus {
    /// `idle` | `running` | `done` | `error`
    pub state: String,
    /// Was gerade geschieht (deutscher Text, die Oberfläche übersetzt)
    pub step: String,
    /// Einzelheit zum Schritt, z. B. der Name des Projekts
    pub detail: String,
    /// Schritte erledigt / insgesamt
    pub done: i64,
    pub total: i64,
    /// Kopierte Dateien und Bytes
    pub files: i64,
    pub bytes: i64,
    pub error: String,
    /// Hinweise (z. B. übersprungene Ordner)
    pub notes: Vec<String>,
    /// Was neu dazugekommen ist
    pub imported: ImportCounts,
    pub started_at: i64,
    pub finished_at: i64,
}

impl ImportStatus {
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }
}

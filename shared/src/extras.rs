//! Was luCode über nrzCode hinaus kann: Suche über alle Chats (Befehlspalette), eigene
//! Anweisungen an die Agenten (je Benutzer und je Projekt), Sicherungen der Datenbank.

use serde::{Deserialize, Serialize};

/// Treffer der Suche: ein Chat (Titel passt) oder eine Nachricht darin.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchHit {
    pub thread_id: i64,
    /// Titel des Chats
    pub title: String,
    /// Name des Projekts (leer = ohne Projekt)
    pub project: String,
    /// Gespräch mit einem Bot (führt nach `/b/<bot>`)
    pub bot_id: Option<i64>,
    /// Nachricht, in der der Begriff steht (`None`: der Titel passt)
    pub message_id: Option<i64>,
    /// `user` | `assistant` (bei Nachrichten)
    pub role: String,
    /// Ausschnitt um die Fundstelle
    pub snippet: String,
    /// Zeitpunkt der Nachricht bzw. der letzten Aktivität (Unix-Sekunden)
    pub at: i64,
}

/// `GET /api/v1/search?q=` – Chats und Nachrichten des Benutzers, neueste zuerst.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchResults {
    pub threads: Vec<SearchHit>,
    pub messages: Vec<SearchHit>,
}

/// `GET/PUT /api/v1/instructions` bzw. `/api/v1/projects/<id>/instructions` – eigene Anweisungen,
/// die jeder Agent bei jedem Lauf mitbekommt.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Instructions {
    pub text: String,
}

/// Längste Anweisung (Zeichen).
pub const MAX_INSTRUCTIONS: usize = 6000;

/// Eine Sicherung der Datenbank (`<data>/backups/<name>`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Backup {
    pub name: String,
    pub size: i64,
    pub created_at: i64,
    /// Von selbst angelegt (täglich)
    pub auto: bool,
}

/// `GET /api/v1/backups` (Administratoren).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BackupList {
    pub backups: Vec<Backup>,
    /// Täglich von selbst sichern
    pub auto: bool,
    /// Ordner der Sicherungen
    pub dir: String,
}

/// `PUT /api/v1/backups/settings`
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BackupSettings {
    pub auto: bool,
}

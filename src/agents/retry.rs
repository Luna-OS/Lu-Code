//! Neuer Versuch nach einem Verbindungsfehler: Bricht ein Lauf ab, weil die Anfrage an das Modell
//! nicht durchkam („Request timed out“, „Connection error“, „fetch failed“, 5xx …), startet luCode
//! das CLI im selben Lauf noch einmal – nach einer kurzen Pause und höchstens [`DELAYS`]-mal.
//! Hat das CLI schon eine Sitzung angelegt, setzt der neue Versuch sie fort (die Nachricht des
//! Benutzers steht dort bereits, Claude Code speichert sie vor der Anfrage) und bekommt nur
//! [`PROMPT`]; sonst geht die ursprüngliche Nachricht mit ihren Anhängen noch einmal raus.
//!
//! Rate- und Nutzungslimits sind kein Verbindungsfehler: dort plant [`super::resume`] das
//! Weitermachen zum Zurücksetzen des Limits.

/// Pausen vor den neuen Versuchen (Sekunden). Claude Code hat dann meist selbst schon mehrmals
/// wiederholt – ein frischer Prozess hilft vor allem, wenn ein laufender Stream abgerissen ist.
pub const DELAYS: [u64; 3] = [5, 30, 90];

/// Prompt eines neuen Versuchs, der die Sitzung fortsetzt.
pub const PROMPT: &str = "The connection to the model was interrupted (the request timed out or the connection dropped), \
so your previous turn did not finish. Please continue exactly where you left off and finish the task.";

/// Sieht die Meldung nach einem vorübergehenden Verbindungs- oder Serverfehler aus?
pub fn is_transient(text: &str) -> bool {
    let t = text.to_lowercase();
    if super::resume::is_rate_limit(&t)
        || ["certificate", "context window", "prompt is too long", "authenticat", "api key", "/login"].iter().any(|k| t.contains(k))
    {
        return false;
    }
    [
        "request timed out",
        "connection error",
        "unable to connect to api",
        "fetch failed",
        "socket hang up",
        "econnreset",
        "etimedout",
        "eai_again",
        "network error",
        "network is unreachable",
        "connection reset",
        "connection closed",
        "stream disconnected",
        "error sending request",
        "upstream connect error",
        "internal server error",
        "bad gateway",
        "service unavailable",
        "gateway timeout",
    ]
    .iter()
    .any(|k| t.contains(k))
        || regex::Regex::new(r"\b(?:api error|status|error|code)[: ]+50[0234]\b").is_ok_and(|re| re.is_match(&t))
}

/// Kurzform der Meldung für den Hinweis im Chat (erste Zeile, ohne die Ratschläge des CLIs).
pub fn short(text: &str) -> String {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    super::messages::clip(super::parse::strip_ansi(first).trim(), 300)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects() {
        assert!(is_transient("Request timed out"));
        assert!(is_transient("Request timed out. If the API is slow to answer, or a proxy or gateway holds responses, set API_TIMEOUT_MS above 600000."));
        assert!(is_transient("API Error: Connection error."));
        assert!(is_transient("Unable to connect to API. Check your internet connection"));
        assert!(is_transient(r#"API Error: 500 {"type":"error","error":{"type":"api_error","message":"Internal server error"}}"#));
        assert!(is_transient("API Error: 503 upstream connect error or disconnect/reset before headers"));
        assert!(is_transient("stream disconnected before completion: error sending request for url"));
        assert!(is_transient("TypeError: fetch failed"));
    }

    #[test]
    fn not_transient() {
        assert!(!is_transient("Datei nicht gefunden"));
        assert!(!is_transient("Prompt is too long"));
        assert!(!is_transient("Unable to connect to API: SSL certificate verification failed. Check your proxy or corporate SSL certificates"));
        assert!(!is_transient("Invalid API key · Fix external API key"));
        // Limits plant `resume`
        assert!(!is_transient("API Error: 529 overloaded_error"));
        assert!(!is_transient("Claude AI usage limit reached|1793534400"));
        assert!(!is_transient("Ich habe 500 Zeilen geändert"));
    }

    #[test]
    fn short_form() {
        assert_eq!(short("\nRequest timed out. If the API is slow\nmore"), "Request timed out. If the API is slow");
    }
}

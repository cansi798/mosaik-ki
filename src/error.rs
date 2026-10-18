use std::fmt;

/// Fehler der KI. `user_message` liefert den Text für die Oberfläche (ohne Fachbegriffe).
#[derive(Debug, Clone, PartialEq)]
pub enum KiError {
    NotInstalled,
    NothingSelected,
    Unavailable(String),
    Timeout,
    TooLong { chars: usize, max: usize },
    ToolFailed(String),
    BadResponse(String),
    AnswerTooLong,
}

impl KiError {
    pub fn user_message(&self) -> String {
        match self {
            Self::NotInstalled => ("KI ist nicht installiert \u{2013} mit \u{201e}KI-Stufe ändern \u{2026}\u{201c} nachrüsten.").into(),
            Self::NothingSelected => "Bitte zuerst Text markieren oder eine Anweisung eingeben.".into(),
            Self::Unavailable(_) => {
                ("Die KI konnte nicht gestartet werden. Bitte die App neu starten oder \u{201e}KI-Stufe ändern \u{2026}\u{201c} erneut ausführen.").into()
            }
            Self::Timeout => "Die KI hat zu lange gebraucht. Bitte noch einmal versuchen.".into(),
            Self::AnswerTooLong => "Die Antwort wäre zu lang geworden. Bitte einen kürzeren Abschnitt markieren.".into(),
            Self::TooLong { .. } => "Bitte einen kürzeren Abschnitt markieren.".into(),
            Self::ToolFailed(why) => format!("Das hat nicht geklappt: {why}"),
            Self::BadResponse(_) => "Die KI hat keine verwertbare Antwort geliefert. Bitte noch einmal versuchen.".into(),
        }
    }
}

impl fmt::Display for KiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "KI nicht installiert"),
            Self::NothingSelected => write!(f, "keine Eingabe"),
            Self::Unavailable(why) => write!(f, "KI-Dienst nicht verfügbar: {why}"),
            Self::Timeout => write!(f, "Zeitüberschreitung"),
            Self::TooLong { chars, max } => write!(f, "Eingabe zu lang: {chars} > {max} Zeichen"),
            Self::ToolFailed(why) => write!(f, "Werkzeug fehlgeschlagen: {why}"),
            Self::BadResponse(why) => write!(f, "ungültige Antwort: {why}"),
            Self::AnswerTooLong => write!(f, "Antwort abgeschnitten"),
        }
    }
}

impl std::error::Error for KiError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_message_makes_no_claim_about_the_document() {
        let m = KiError::Timeout.user_message();
        assert!(!m.contains("unverändert"), "{m}");
        assert!(m.contains("noch einmal versuchen"));
    }

    #[test]
    fn user_messages_avoid_jargon() {
        let all = [
            KiError::NotInstalled,
            KiError::NothingSelected,
            KiError::Unavailable("x".into()),
            KiError::Timeout,
            KiError::TooLong { chars: 2, max: 1 },
            KiError::ToolFailed("x".into()),
            KiError::BadResponse("x".into()),
            KiError::AnswerTooLong,
        ];
        for e in all {
            let m = e.user_message();
            for word in ["RAM", "Modell", "Server", "Port", "Release", "HTTP"] {
                assert!(!m.contains(word), "{m}");
            }
        }
    }
}

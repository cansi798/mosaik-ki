//! Feste Schnellaktionen der Seitenleiste: ohne freie Werkzeugwahl, daher auch mit dem kleinen Modell zuverlässig.
use serde_json::{Value, json};

use crate::KiError;

pub const MAX_INPUT_CHARS: usize = 12_000;

pub const SYSTEM_PROMPT: &str = "Du bist die KI in Mosaik, einer Software für Büro und Gestaltung. \
Antworte in der Sprache des Textes (meist Deutsch). Gib nur das Ergebnis aus, ohne Einleitung, \
ohne Erklärung und ohne Markdown.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    ReplaceSelection,
    InsertFormula,
    ShowOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuickAction {
    pub id: &'static str,
    pub label: &'static str,
    pub instruction: &'static str,
    pub apply: Apply,
}

pub const TEXT_ACTIONS: [QuickAction; 4] = [
    QuickAction {
        id: "korrigieren",
        label: "Korrigieren",
        instruction: "Korrigiere Rechtschreibung, Grammatik und Zeichensetzung. Ändere nichts an Inhalt und Stil.",
        apply: Apply::ReplaceSelection,
    },
    QuickAction {
        id: "umformulieren",
        label: "Umformulieren",
        instruction: "Formuliere den Text klarer und flüssiger. Behalte Bedeutung, Sprache und ungefähre Länge bei.",
        apply: Apply::ReplaceSelection,
    },
    QuickAction {
        id: "zusammenfassen",
        label: "Zusammenfassen",
        instruction: "Fasse den Text in wenigen Sätzen zusammen.",
        apply: Apply::ShowOnly,
    },
    QuickAction {
        id: "entwurf",
        label: "Entwurf aus Stichpunkten",
        instruction: "Schreibe aus den Stichpunkten einen zusammenhängenden, gut lesbaren Text.",
        apply: Apply::ReplaceSelection,
    },
];

pub const SHEET_ACTIONS: [QuickAction; 2] = [
    QuickAction {
        id: "formel",
        label: "Formel aus Beschreibung",
        instruction: "Erzeuge genau eine Tabellenformel für die Beschreibung. Verwende englische Funktionsnamen \
(z. B. SUM, AVERAGE, IF) und Kommas als Trennzeichen. Antworte nur mit der Formel, beginnend mit =.",
        apply: Apply::InsertFormula,
    },
    QuickAction {
        id: "erklaeren",
        label: "Bereich erklären",
        instruction: "Erkläre kurz und verständlich, was diese Daten zeigen und was auffällt.",
        apply: Apply::ShowOnly,
    },
];

pub fn build_messages(action: &QuickAction, input: &str) -> Result<Vec<Value>, KiError> {
    if input.trim().is_empty() {
        return Err(KiError::NothingSelected);
    }
    let chars = input.chars().count();
    if chars > MAX_INPUT_CHARS {
        return Err(KiError::TooLong {
            chars,
            max: MAX_INPUT_CHARS,
        });
    }
    Ok(vec![
        json!({"role": "system", "content": SYSTEM_PROMPT}),
        json!({"role": "user", "content": format!("{}\n\n<<<\n{input}\n>>>", action.instruction)}),
    ])
}

pub fn postprocess(apply: Apply, answer: &str) -> String {
    let text = strip_fences(answer.trim());
    match apply {
        Apply::InsertFormula => formula_line(&text),
        Apply::ReplaceSelection | Apply::ShowOnly => text,
    }
}

fn strip_fences(s: &str) -> String {
    let Some(rest) = s.strip_prefix("```") else {
        return s.to_string();
    };
    let body = rest.split_once('\n').map_or("", |(_, b)| b);
    body.trim_end()
        .strip_suffix("```")
        .unwrap_or(body)
        .trim()
        .to_string()
}

fn formula_line(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim().trim_matches('`').trim())
        .find(|l| l.starts_with('='))
        .or_else(|| text.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    if line.starts_with('=') {
        line.to_string()
    } else {
        format!("={line}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_system_and_user_message() {
        let m = build_messages(&TEXT_ACTIONS[0], "Das ist ein Tset.").unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0]["role"], "system");
        let user = m[1]["content"].as_str().unwrap();
        assert!(user.starts_with(TEXT_ACTIONS[0].instruction));
        assert!(user.contains("Das ist ein Tset."));
    }

    #[test]
    fn rejects_empty_and_too_long_input() {
        assert_eq!(
            build_messages(&TEXT_ACTIONS[0], "  \n"),
            Err(KiError::NothingSelected)
        );
        let long = "ä".repeat(MAX_INPUT_CHARS + 1);
        assert_eq!(
            build_messages(&TEXT_ACTIONS[0], &long),
            Err(KiError::TooLong {
                chars: MAX_INPUT_CHARS + 1,
                max: MAX_INPUT_CHARS
            })
        );
        assert!(build_messages(&TEXT_ACTIONS[0], &"ä".repeat(MAX_INPUT_CHARS)).is_ok());
    }

    #[test]
    fn strips_code_fences_and_whitespace() {
        assert_eq!(
            postprocess(Apply::ReplaceSelection, "```\nHallo Welt\n```"),
            "Hallo Welt"
        );
        assert_eq!(postprocess(Apply::ShowOnly, "```text\nA\nB\n```\n"), "A\nB");
        assert_eq!(postprocess(Apply::ReplaceSelection, "  Satz.  "), "Satz.");
    }

    #[test]
    fn formula_is_single_line_with_equals_sign() {
        assert_eq!(
            postprocess(Apply::InsertFormula, "```excel\n=SUM(A1:A5)\n```"),
            "=SUM(A1:A5)"
        );
        assert_eq!(
            postprocess(Apply::InsertFormula, "SUM(A1:A5)"),
            "=SUM(A1:A5)"
        );
        assert_eq!(
            postprocess(
                Apply::InsertFormula,
                "Die Formel lautet:\n`=AVERAGE(B2:B9)`"
            ),
            "=AVERAGE(B2:B9)"
        );
    }

    #[test]
    fn action_ids_are_unique() {
        let mut ids: Vec<&str> = TEXT_ACTIONS
            .iter()
            .chain(SHEET_ACTIONS.iter())
            .map(|a| a.id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TEXT_ACTIONS.len() + SHEET_ACTIONS.len());
    }
}

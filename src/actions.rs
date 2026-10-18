//! Feste Schnellaktionen der Seitenleiste: ohne freie Werkzeugwahl, daher auch mit dem kleinen Modell zuverlässig.
use serde_json::{Value, json};

use crate::KiError;
use crate::server::CONTEXT_TOKENS;

pub const MAX_INPUT_CHARS: usize = 12_000;
/// Grobe Schätzung für Systemtext und Anweisung.
const PROMPT_TOKENS: u32 = 300;

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

/// Inhalt des ersten Codeblocks (auch nach einer Einleitung oder einzeilig); ohne Codeblock der Text selbst.
fn strip_fences(s: &str) -> String {
    let Some(start) = s.find("```") else {
        return s.to_string();
    };
    let rest = &s[start + 3..];
    let (body, closed) = match rest.find("```") {
        Some(end) => (&rest[..end], true),
        None => (rest, false),
    };
    // Mit Zeilenumbruch ist die erste Zeile die Sprachangabe; einzeilig ist alles Inhalt.
    let content = match body.split_once('\n') {
        Some((_lang, text)) => text,
        None if closed => body,
        None => "",
    };
    content.trim().to_string()
}

/// Wie `SUM(` oder `STDEV.S(`: Großbuchstabe, dann Großbuchstaben/Ziffern/Punkte, dann `(`.
fn looks_like_function_call(line: &str) -> bool {
    let mut chars = line.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_uppercase()) {
        return false;
    }
    for c in chars {
        if c == '(' {
            return true;
        }
        if !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '.') {
            return false;
        }
    }
    false
}

fn formula_line(text: &str) -> String {
    let lines = || text.lines().map(|l| l.trim().trim_matches('`').trim());
    if let Some(line) = lines().find(|l| l.starts_with('=')) {
        return line.to_string();
    }
    match lines().find(|l| looks_like_function_call(l)) {
        Some(line) => format!("={line}"),
        None => String::new(),
    }
}

/// Maximale Antwortlänge (Tokens) passend zur Eingabe: der Rest des Kontexts, begrenzt auf 256..=4096.
pub fn max_tokens_for(input: &str) -> u32 {
    let est_input = u32::try_from(input.chars().count() / 3).unwrap_or(u32::MAX);
    CONTEXT_TOKENS
        .saturating_sub(est_input)
        .saturating_sub(PROMPT_TOKENS)
        .clamp(256, 4096)
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
    fn intro_before_fence_is_dropped() {
        assert_eq!(
            postprocess(
                Apply::ReplaceSelection,
                "Hier ist der Text:\n```text\nA\nB\n```\nViel Erfolg!"
            ),
            "A\nB"
        );
        assert_eq!(
            postprocess(
                Apply::ShowOnly,
                "Intro:\n```\nErster\n```\nund\n```\nZweiter\n```"
            ),
            "Erster"
        );
    }

    #[test]
    fn one_line_fence_gives_content() {
        assert_eq!(postprocess(Apply::ReplaceSelection, "```Hallo```"), "Hallo");
        assert_eq!(
            postprocess(Apply::InsertFormula, "```=SUM(A1)```"),
            "=SUM(A1)"
        );
        assert_eq!(postprocess(Apply::ReplaceSelection, "```\n```"), "");
    }

    #[test]
    fn formula_from_prose_and_function_call() {
        assert_eq!(
            postprocess(Apply::InsertFormula, "Die Formel lautet:\nSUM(A1)"),
            "=SUM(A1)"
        );
        assert_eq!(
            postprocess(Apply::InsertFormula, "x\nSTDEV.S(B1:B9)"),
            "=STDEV.S(B1:B9)"
        );
    }

    #[test]
    fn pure_prose_or_empty_is_no_formula() {
        assert_eq!(
            postprocess(Apply::InsertFormula, "Das kann ich leider nicht berechnen."),
            ""
        );
        assert_eq!(postprocess(Apply::InsertFormula, ""), "");
        assert_eq!(postprocess(Apply::ReplaceSelection, "  "), "");
    }

    #[test]
    fn max_tokens_stays_within_bounds_and_uses_context() {
        assert_eq!(max_tokens_for(""), 4096);
        assert!(max_tokens_for(&"a".repeat(12_000)) >= 3000);
        for n in [0, 1, 300, 6000, 12_000, 100_000, 10_000_000] {
            let t = max_tokens_for(&"a".repeat(n));
            assert!((256..=4096).contains(&t), "{n}: {t}");
        }
        assert_eq!(max_tokens_for(&"a".repeat(60_000)), 256);
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

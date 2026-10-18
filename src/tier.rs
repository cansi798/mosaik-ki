use serde::{Deserialize, Serialize};

/// Installationsstufe, die der Nutzer in „Mosaik einrichten" wählt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Standard,
    Kompakt,
    Pro,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Standard, Tier::Kompakt, Tier::Pro];

    pub fn has_ki(self) -> bool {
        !matches!(self, Tier::Standard)
    }

    pub fn label(self) -> &'static str {
        match self {
            Tier::Standard => "Standard",
            Tier::Kompakt => "KI Kompakt",
            Tier::Pro => "KI Pro",
        }
    }

    /// Mindest-Arbeitsspeicher in GB (aus `mosaik-branding/requirements.json`).
    pub fn ram_gb(self) -> u32 {
        match self {
            Tier::Standard => 4,
            Tier::Kompakt => 8,
            Tier::Pro => 12,
        }
    }

    /// Zusätzlicher Speicherplatz in GB gegenüber Standard (aus `requirements.json`).
    pub fn extra_disk_gb(self) -> u32 {
        match self {
            Tier::Standard => 0,
            Tier::Kompakt => 5,
            Tier::Pro => 7,
        }
    }

    /// Alltagsbeispiele für die Auswahlkarte (aus `requirements.json`).
    pub fn examples(self) -> &'static [&'static str] {
        match self {
            Tier::Standard => &["Alle Programme", "Ohne KI"],
            Tier::Kompakt => &[
                "Texte korrigieren lassen",
                "Zusammenfassungen",
                "Formeln bauen",
            ],
            Tier::Pro => &["Bessere Texte und Analysen", "Bilder verstehen"],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Tier::Kompakt).unwrap(),
            "\"kompakt\""
        );
        assert_eq!(serde_json::from_str::<Tier>("\"pro\"").unwrap(), Tier::Pro);
    }

    #[test]
    fn requirements_grow_with_tier() {
        assert!(Tier::Standard.ram_gb() < Tier::Kompakt.ram_gb());
        assert!(Tier::Kompakt.ram_gb() < Tier::Pro.ram_gb());
        assert_eq!(Tier::Standard.extra_disk_gb(), 0);
        assert!(Tier::ALL.iter().all(|t| !t.examples().is_empty()));
    }

    #[test]
    fn only_standard_has_no_ki() {
        assert!(!Tier::Standard.has_ki());
        assert!(Tier::Kompakt.has_ki() && Tier::Pro.has_ki());
    }
}

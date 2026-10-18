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
    fn only_standard_has_no_ki() {
        assert!(!Tier::Standard.has_ki());
        assert!(Tier::Kompakt.has_ki() && Tier::Pro.has_ki());
    }
}

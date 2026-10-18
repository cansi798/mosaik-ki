//! Prüfung des Computers: Arbeitsspeicher, freier Speicherplatz, passende Stufe.
use std::path::{Path, PathBuf};

use super::{Plan, format_gb};
use crate::{Tier, models};

/// Arbeitsspeicher wird mit 10 % Toleranz verglichen: ein „12-GB-Rechner“ meldet oft nur 11,x GB.
const RAM_TOLERANCE: f64 = 0.9;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Werte des eigenen Computers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInfo {
    /// Gesamter Arbeitsspeicher in Bytes.
    pub ram_bytes: u64,
    /// Freier Platz am Installationsort; `None`, wenn unbekannt.
    pub free_bytes: Option<u64>,
    /// Noch benötigter Speicherplatz je Stufe in der Reihenfolge von [`Tier::ALL`].
    pub needed_bytes: [u64; 3],
    /// Für dieses Betriebssystem gibt es die KI.
    pub ki_supported: bool,
}

impl SystemInfo {
    /// Arbeitsspeicher in GB (1 GB = 2³⁰ Bytes, wie ihn das Betriebssystem anzeigt).
    pub fn ram_gb(&self) -> f64 {
        self.ram_bytes as f64 / GIB
    }

    /// Gerundet für die Anzeige, z. B. „8 GB“.
    pub fn ram_text(&self) -> String {
        format!("{} GB", self.ram_gb().round() as u64)
    }

    pub fn free_text(&self) -> Option<String> {
        self.free_bytes.map(format_gb)
    }

    pub fn needed(&self, tier: Tier) -> u64 {
        Tier::ALL
            .iter()
            .position(|t| *t == tier)
            .map_or(0, |i| self.needed_bytes[i])
    }
}

/// Liest Arbeitsspeicher und freien Platz und berechnet, wie viel jede Stufe noch braucht.
pub fn system_check(install_dir: &Path) -> SystemInfo {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let needed_bytes = Tier::ALL.map(|tier| {
        Plan::new(tier, install_dir.to_path_buf(), PathBuf::new())
            .map(|p| p.space_needed())
            .unwrap_or(0)
    });
    SystemInfo {
        ram_bytes: sys.total_memory(),
        free_bytes: free_space(install_dir),
        needed_bytes,
        ki_supported: models::llama_asset(std::env::consts::OS, std::env::consts::ARCH).is_some(),
    }
}

/// Freier Platz auf dem Laufwerk von `dir`; `dir` muss noch nicht existieren.
pub fn free_space(dir: &Path) -> Option<u64> {
    let existing = dir.ancestors().find(|p| p.is_dir())?;
    fs4::available_space(existing).ok()
}

/// `Ok`, wenn die Stufe auf diesem Computer läuft; sonst ein Satz mit dem Grund, z. B.
/// „Braucht 12 GB Arbeitsspeicher – Ihr Computer hat 8 GB“.
pub fn fits(tier: Tier, info: &SystemInfo) -> Result<(), String> {
    if tier.has_ki() && !info.ki_supported {
        return Err("Für dieses Betriebssystem leider nicht verfügbar".into());
    }
    let need = f64::from(tier.ram_gb());
    if info.ram_gb() < need * RAM_TOLERANCE {
        return Err(format!(
            "Braucht {} GB Arbeitsspeicher \u{2013} Ihr Computer hat {}",
            tier.ram_gb(),
            info.ram_text()
        ));
    }
    let needed = info.needed(tier);
    if let Some(free) = info.free_bytes
        && free < needed
    {
        return Err(format!(
            "Braucht noch {} freien Speicherplatz \u{2013} Ihr Computer hat {} frei",
            format_gb(needed),
            format_gb(free)
        ));
    }
    Ok(())
}

/// Höchste Stufe, die zu diesem Computer passt.
pub fn recommended_tier(info: &SystemInfo) -> Tier {
    [Tier::Pro, Tier::Kompakt]
        .into_iter()
        .find(|t| fits(*t, info).is_ok())
        .unwrap_or(Tier::Standard)
}

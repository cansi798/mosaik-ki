use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::{Tier, models};

pub const DEFAULT_PORT: u16 = 11535;

fn default_port() -> u16 {
    DEFAULT_PORT
}

/// Inhalt von `mosaik-ki.json`. Wird von „Mosaik einrichten“ geschrieben und von jeder App gelesen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KiConfig {
    pub tier: Tier,
    pub install_dir: PathBuf,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl KiConfig {
    pub fn new(tier: Tier, install_dir: PathBuf) -> Self {
        Self {
            tier,
            install_dir,
            port: DEFAULT_PORT,
        }
    }

    pub fn server_binary(&self) -> PathBuf {
        let name = if cfg!(windows) {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        self.install_dir.join("llama").join(name)
    }

    pub fn model_dir(&self) -> PathBuf {
        self.install_dir.join("models")
    }

    pub fn model_path(&self) -> Option<PathBuf> {
        models::main_model(self.tier).map(|m| self.model_dir().join(m.file))
    }

    pub fn mmproj_path(&self) -> Option<PathBuf> {
        models::mmproj(self.tier).map(|m| self.model_dir().join(m.file))
    }

    /// Stufe mit KI und alle benötigten Dateien vorhanden.
    pub fn is_ready(&self) -> bool {
        if !self.tier.has_ki() || !self.server_binary().is_file() {
            return false;
        }
        let model_ok = self.model_path().is_some_and(|p| p.is_file());
        let mmproj_ok = self.mmproj_path().is_none_or(|p| p.is_file());
        model_ok && mmproj_ok
    }

    /// `None`, wenn die Datei fehlt oder nicht lesbar/gültig ist.
    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Atomar: erst in `<datei>.<pid>.<n>.tmp` schreiben, dann umbenennen. Pid und Zähler verhindern,
    /// dass gleichzeitige Speichervorgänge (andere Apps, andere Threads) dieselbe Zwischendatei benutzen.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut name = path.file_name().map(OsString::from).unwrap_or_default();
        name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = path.with_file_name(name);
        let result = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}

/// z. B. `~/.config/Mosaik/mosaik-ki.json`, `%APPDATA%\Mosaik\mosaik-ki.json`.
pub fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Mosaik").join("mosaik-ki.json"))
}

/// z. B. `~/.local/share/Mosaik/ki`, `%LOCALAPPDATA%\Mosaik\ki`.
pub fn default_install_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("Mosaik").join("ki"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_empty_or_broken_file_means_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mosaik-ki.json");
        assert_eq!(KiConfig::load(&p), None);
        std::fs::write(&p, "").unwrap();
        assert_eq!(KiConfig::load(&p), None);
        std::fs::write(&p, "{\"tier\":\"kompakt\",\"install").unwrap();
        assert_eq!(KiConfig::load(&p), None);
    }

    #[test]
    fn save_and_load_roundtrip_with_umlaut_path() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Jürgen Müller").join("ki");
        let cfg = KiConfig::new(Tier::Pro, install.clone());
        let p = dir.path().join("sub").join("mosaik-ki.json");
        cfg.save(&p).unwrap();
        assert_eq!(KiConfig::load(&p), Some(cfg));
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn concurrent_saves_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mosaik-ki.json");
        let handles: Vec<_> = [Tier::Kompakt, Tier::Pro]
            .into_iter()
            .map(|tier| {
                let p = p.clone();
                let cfg = KiConfig::new(tier, dir.path().to_path_buf());
                std::thread::spawn(move || {
                    for _ in 0..200 {
                        cfg.save(&p).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(KiConfig::load(&p).is_some());
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "tmp-Dateien übrig");
    }

    #[test]
    fn failed_save_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        // Ziel ist ein Verzeichnis: rename schlägt fehl.
        let p = dir.path().join("mosaik-ki.json");
        std::fs::create_dir(&p).unwrap();
        let cfg = KiConfig::new(Tier::Kompakt, dir.path().to_path_buf());
        assert!(cfg.save(&p).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn port_defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        std::fs::write(&p, r#"{"tier":"kompakt","install_dir":"/x"}"#).unwrap();
        assert_eq!(KiConfig::load(&p).unwrap().port, DEFAULT_PORT);
    }

    #[test]
    fn ready_only_when_binary_and_all_model_files_exist() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = KiConfig::new(Tier::Pro, dir.path().to_path_buf());
        assert!(!cfg.is_ready());
        std::fs::create_dir_all(cfg.server_binary().parent().unwrap()).unwrap();
        std::fs::write(cfg.server_binary(), b"").unwrap();
        std::fs::create_dir_all(cfg.model_dir()).unwrap();
        std::fs::write(cfg.model_path().unwrap(), b"").unwrap();
        assert!(!cfg.is_ready(), "mmproj fehlt noch");
        std::fs::write(cfg.mmproj_path().unwrap(), b"").unwrap();
        assert!(cfg.is_ready());
        assert!(!KiConfig::new(Tier::Standard, dir.path().to_path_buf()).is_ready());
    }
}

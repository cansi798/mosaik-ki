use std::path::{Path, PathBuf};

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

    /// Atomar: erst in `<datei>.tmp` schreiben, dann umbenennen.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
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
        assert!(!p.with_extension("json.tmp").exists());
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

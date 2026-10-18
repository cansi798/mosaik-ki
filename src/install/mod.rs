//! Installation der KI direkt aus den Apps (Feature `install`).
//!
//! Lädt das passende llama.cpp-Paket und die Gemma-Dateien, prüft sie per SHA-256, setzt
//! abgebrochene Downloads fort und schreibt `mosaik-ki.json`. Der spätere Installer
//! „Mosaik einrichten“ nutzt dieselben Funktionen.
//!
//! Alle Texte für die Oberfläche (`Progress::text`, [`InstallError::user_message`], [`fits`])
//! kommen ohne Fachbegriffe aus.
mod download;
mod system;
mod unpack;

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub use system::{SystemInfo, fits, free_space, recommended_tier, system_check};

use crate::models::{self, LlamaAsset, ModelFile};
use crate::{KiConfig, Tier, default_config_path, default_install_dir, server};

/// Name der Datei in `<install_dir>/llama`, die festhält, welches llama.cpp-Paket entpackt ist.
const LLAMA_MARKER: &str = ".mosaik-llama";
/// Ordner für Pakete, die noch entpackt werden müssen.
const DOWNLOAD_DIR: &str = "downloads";
/// Reserve zusätzlich zu den Dateigrößen.
const SPACE_MARGIN: u64 = 200_000_000;
/// Entpackt braucht das llama.cpp-Paket etwa das Dreifache seiner Größe; mit Archiv selbst also ×4, plus Luft.
const LLAMA_SPACE_FACTOR: u64 = 5;

/// Eine herunterzuladende Datei mit Prüfsumme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    pub url: String,
    /// SHA-256 als Hex-Text.
    pub sha256: String,
    pub size: u64,
    /// Dateiname am Zielort.
    pub name: String,
}

impl From<&ModelFile> for FileSpec {
    fn from(m: &ModelFile) -> Self {
        Self {
            url: m.url(),
            sha256: m.sha256.into(),
            size: m.size,
            name: m.file.into(),
        }
    }
}

impl From<&LlamaAsset> for FileSpec {
    fn from(a: &LlamaAsset) -> Self {
        Self {
            url: a.url(),
            sha256: a.sha256.into(),
            size: a.size,
            name: a.name.into(),
        }
    }
}

/// Abschnitt der Installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Checking,
    Downloading,
    Unpacking,
    Removing,
    Done,
}

/// Fortschritt für die Oberfläche.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub phase: Phase,
    /// Bereits vorhandene Bytes aller Downloads.
    pub done: u64,
    /// Gesamtgröße aller Downloads (0, wenn nichts zu laden ist).
    pub total: u64,
    /// Verständlicher Text, z. B. „KI wird heruntergeladen … 1,2 von 3,4 GB“.
    pub text: String,
}

impl Progress {
    fn new(phase: Phase, done: u64, total: u64) -> Self {
        let text = match phase {
            Phase::Preparing => "Installation wird vorbereitet …".to_string(),
            Phase::Checking => "KI wird geprüft …".into(),
            Phase::Downloading => format!(
                "KI wird heruntergeladen … {} von {} GB",
                gb_number(done),
                gb_number(total)
            ),
            Phase::Unpacking => "KI wird eingerichtet …".into(),
            Phase::Removing => "Nicht mehr benötigte Dateien werden entfernt …".into(),
            Phase::Done => "Fertig.".into(),
        };
        Self {
            phase,
            done,
            total,
            text,
        }
    }

    /// Anteil 0.0 … 1.0 für einen Fortschrittsbalken.
    pub fn fraction(&self) -> f32 {
        match self.phase {
            Phase::Done => 1.0,
            _ if self.total == 0 => 0.0,
            _ => (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32,
        }
    }
}

/// Fehler bei der Installation. [`InstallError::user_message`] liefert einen Satz ohne Fachbegriffe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// Der Nutzer hat abgebrochen. Ein erneuter Aufruf setzt an derselben Stelle fort.
    Cancelled,
    /// Keine Verbindung oder die Gegenstelle antwortet nicht (Detail für Protokolle).
    Offline(String),
    NoSpace {
        needed: u64,
        free: u64,
    },
    /// Eine Datei kam auch nach erneutem Laden beschädigt an.
    Corrupt(String),
    /// Für dieses Betriebssystem bzw. diesen Prozessor gibt es kein llama.cpp-Paket.
    Unsupported,
    /// Für diesen Ordner läuft schon eine Installation.
    Busy,
    /// Der laufende KI-Dienst ließ sich nicht beenden.
    InUse(String),
    /// Lesen oder Schreiben auf dem Datenträger ist fehlgeschlagen.
    Disk(String),
    /// Der Benutzerordner für Einstellungen bzw. Daten ist unbekannt.
    NoFolder,
}

impl InstallError {
    pub fn user_message(&self) -> String {
        match self {
            Self::Cancelled => "Die Installation wurde abgebrochen. Mit \u{201e}Nochmal versuchen\u{201c} geht es an derselben Stelle weiter.".into(),
            Self::Offline(_) => "Keine Verbindung zum Internet. Bitte die Verbindung prüfen und nochmal versuchen.".into(),
            Self::NoSpace { needed, free } => format!(
                "Zu wenig Speicherplatz: Es werden noch {} gebraucht, frei sind {}. Bitte Speicherplatz freigeben und nochmal versuchen.",
                format_gb(*needed),
                format_gb(*free)
            ),
            Self::Corrupt(_) => "Eine Datei kam beschädigt an. Bitte nochmal versuchen.".into(),
            Self::Unsupported => "Die KI ist für dieses Betriebssystem leider nicht verfügbar.".into(),
            Self::Busy => "Die KI wird gerade schon eingerichtet. Bitte kurz warten und nochmal versuchen.".into(),
            Self::InUse(_) => "Die KI wird gerade benutzt. Bitte alle Mosaik-Programme schließen und nochmal versuchen.".into(),
            Self::Disk(_) => "Die Dateien konnten nicht gespeichert werden. Bitte nochmal versuchen.".into(),
            Self::NoFolder => "Der Ordner für die KI wurde nicht gefunden. Bitte nochmal versuchen.".into(),
        }
    }
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "abgebrochen"),
            Self::Offline(why) => write!(f, "Download fehlgeschlagen: {why}"),
            Self::NoSpace { needed, free } => {
                write!(f, "zu wenig Platz: {needed} Bytes nötig, {free} frei")
            }
            Self::Corrupt(name) => write!(f, "Prüfsumme falsch: {name}"),
            Self::Unsupported => write!(f, "kein llama.cpp-Paket für diese Plattform"),
            Self::Busy => write!(f, "Installation läuft bereits"),
            Self::InUse(why) => write!(f, "Dienst läuft noch: {why}"),
            Self::Disk(why) => write!(f, "Dateifehler: {why}"),
            Self::NoFolder => write!(f, "Benutzerordner unbekannt"),
        }
    }
}

impl std::error::Error for InstallError {}

/// Was für eine Stufe zu tun ist. [`Plan::new`] füllt alles aus `models.rs`; Tests setzen eigene Dateien ein.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub tier: Tier,
    pub install_dir: PathBuf,
    pub config_path: PathBuf,
    /// llama.cpp-Paket (zip oder tar.gz); `None` bei Standard.
    pub llama: Option<FileSpec>,
    /// Name des Programms im Paket, z. B. `llama-server.exe`.
    pub server_name: String,
    pub models: Vec<FileSpec>,
}

impl Plan {
    /// Plan für dieses Betriebssystem.
    pub fn new(
        tier: Tier,
        install_dir: PathBuf,
        config_path: PathBuf,
    ) -> Result<Self, InstallError> {
        let llama = if tier.has_ki() {
            let asset = models::llama_asset(std::env::consts::OS, std::env::consts::ARCH)
                .ok_or(InstallError::Unsupported)?;
            Some(FileSpec::from(asset))
        } else {
            None
        };
        let server_name = KiConfig::new(tier, install_dir.clone())
            .server_binary()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Self {
            tier,
            install_dir,
            config_path,
            llama,
            server_name,
            models: models::model_files(tier)
                .iter()
                .map(FileSpec::from)
                .collect(),
        })
    }

    fn llama_dir(&self) -> PathBuf {
        self.install_dir.join("llama")
    }

    fn model_dir(&self) -> PathBuf {
        self.install_dir.join("models")
    }

    fn llama_ready(&self, spec: &FileSpec) -> bool {
        let dir = self.llama_dir();
        dir.join(&self.server_name).is_file()
            && std::fs::read_to_string(dir.join(LLAMA_MARKER))
                .is_ok_and(|m| m.trim().eq_ignore_ascii_case(&spec.sha256))
    }

    /// Alle noch zu ladenden Dateien mit Zielort.
    fn pending(&self) -> Vec<(&FileSpec, PathBuf)> {
        let mut out = Vec::new();
        if let Some(spec) = &self.llama
            && !self.llama_ready(spec)
        {
            out.push((spec, self.install_dir.join(DOWNLOAD_DIR).join(&spec.name)));
        }
        for m in &self.models {
            out.push((m, self.model_dir().join(&m.name)));
        }
        out.retain(|(spec, dest)| !download::is_complete(spec, dest));
        out
    }

    /// Wie viel Speicherplatz noch gebraucht wird (bereits geladene Teile sind abgezogen).
    pub fn space_needed(&self) -> u64 {
        let pending = self.pending();
        if pending.is_empty() {
            return 0;
        }
        let mut needed = SPACE_MARGIN;
        for (spec, dest) in pending {
            let have = download::part_len(dest.as_path());
            let factor = if Some(spec) == self.llama.as_ref() {
                LLAMA_SPACE_FACTOR
            } else {
                1
            };
            needed += spec.size.saturating_mul(factor).saturating_sub(have);
        }
        needed
    }

    /// Führt die Installation aus (blockierend, gehört in einen Hintergrund-Thread).
    ///
    /// `progress` wird regelmäßig aufgerufen. Ist `cancel` gesetzt, endet die Arbeit mit
    /// [`InstallError::Cancelled`]; angefangene Downloads bleiben als `.part` liegen und werden beim
    /// nächsten Aufruf fortgesetzt.
    pub fn run(
        &self,
        progress: &mut dyn FnMut(&Progress),
        cancel: &AtomicBool,
    ) -> Result<KiConfig, InstallError> {
        self.run_with(&download::Downloader::default(), progress, cancel)
    }

    fn run_with(
        &self,
        downloader: &download::Downloader,
        progress: &mut dyn FnMut(&Progress),
        cancel: &AtomicBool,
    ) -> Result<KiConfig, InstallError> {
        let _lock = DirLock::acquire(&self.install_dir)?;
        progress(&Progress::new(Phase::Preparing, 0, 0));
        let old = KiConfig::load(&self.config_path);
        let pending = self.pending();

        if !pending.is_empty() {
            let needed = self.space_needed();
            if let Some(free) = free_space(&self.install_dir)
                && free < needed
            {
                return Err(InstallError::NoSpace { needed, free });
            }
            for (_, dest) in &pending {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| disk_error(&e, self))?;
                }
            }
            let total: u64 = pending.iter().map(|(s, _)| s.size).sum();
            let mut finished = 0u64;
            let mut reporter = Throttle::default();
            for (spec, dest) in &pending {
                downloader.fetch(spec, dest, cancel, &mut |phase, have| {
                    let p = Progress::new(phase, finished + have, total);
                    if reporter.due(&p) {
                        progress(&p);
                    }
                })?;
                finished += spec.size;
            }
            progress(&Progress::new(Phase::Downloading, total, total));
        }
        check_cancel(cancel)?;

        // Ab hier werden Dateien ersetzt oder gelöscht: vorher den laufenden Dienst beenden.
        if let Some(old) = &old {
            server::stop(old).map_err(|e| InstallError::InUse(e.to_string()))?;
        }

        if let Some(spec) = &self.llama
            && !self.llama_ready(spec)
        {
            progress(&Progress::new(Phase::Unpacking, 0, 0));
            self.unpack_llama(spec)?;
        }

        progress(&Progress::new(Phase::Removing, 0, 0));
        self.remove_unneeded()?;

        let mut cfg = KiConfig::new(self.tier, self.install_dir.clone());
        if let Some(old) = &old {
            cfg.port = old.port;
        }
        cfg.save(&self.config_path)
            .map_err(|e| disk_error(&e, self))?;
        progress(&Progress::new(Phase::Done, 0, 0));
        Ok(cfg)
    }

    fn unpack_llama(&self, spec: &FileSpec) -> Result<(), InstallError> {
        let archive = self.install_dir.join(DOWNLOAD_DIR).join(&spec.name);
        let fresh = self.install_dir.join("llama.new");
        unpack::extract(&archive, &fresh, &self.server_name).map_err(|e| match e {
            InstallError::NoSpace { .. } => InstallError::NoSpace {
                needed: spec.size.saturating_mul(LLAMA_SPACE_FACTOR),
                free: free_space(&self.install_dir).unwrap_or(0),
            },
            other => other,
        })?;
        std::fs::write(fresh.join(LLAMA_MARKER), &spec.sha256).map_err(|e| disk_error(&e, self))?;
        let dir = self.llama_dir();
        remove_path(&dir).map_err(|e| InstallError::InUse(e.to_string()))?;
        std::fs::rename(&fresh, &dir).map_err(|e| disk_error(&e, self))?;
        let _ = std::fs::remove_file(&archive);
        Ok(())
    }

    /// Löscht Modelldateien, die die gewählte Stufe nicht braucht; bei Standard alles.
    fn remove_unneeded(&self) -> Result<(), InstallError> {
        let keep: HashSet<&str> = self.models.iter().map(|m| m.name.as_str()).collect();
        if let Ok(entries) = std::fs::read_dir(self.model_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if !keep.contains(name.to_string_lossy().as_ref()) {
                    remove_path(&entry.path()).map_err(|e| InstallError::InUse(e.to_string()))?;
                }
            }
        }
        let downloads = self.install_dir.join(DOWNLOAD_DIR);
        if self.llama.is_none() {
            for p in [
                self.llama_dir(),
                self.install_dir.join("llama.new"),
                self.model_dir(),
                downloads.clone(),
                self.install_dir.join("ki.log"),
                self.install_dir.join("llama-server.pid"),
            ] {
                remove_path(&p).map_err(|e| InstallError::InUse(e.to_string()))?;
            }
            // Nur leer entfernen: der Ordner könnte auch Fremdes enthalten.
            let _ = std::fs::remove_dir(&self.install_dir);
        } else {
            let _ = std::fs::remove_dir(&downloads);
        }
        Ok(())
    }
}

/// Installiert `tier` nach `install_dir` und schreibt `mosaik-ki.json` an den Standardort.
/// Standard entfernt die KI. Blockierend.
pub fn install(
    tier: Tier,
    install_dir: &Path,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<KiConfig, InstallError> {
    let config_path = default_config_path().ok_or(InstallError::NoFolder)?;
    Plan::new(tier, install_dir.to_path_buf(), config_path)?.run(progress, cancel)
}

/// Entfernt die KI (Stufe Standard): beendet den Dienst, löscht Programm und Modelldateien.
pub fn uninstall(install_dir: &Path) -> Result<KiConfig, InstallError> {
    install(
        Tier::Standard,
        install_dir,
        &mut |_| {},
        &AtomicBool::new(false),
    )
}

/// Installationsordner der vorhandenen Einrichtung, sonst der Standardordner.
pub fn current_install_dir() -> Option<PathBuf> {
    default_config_path()
        .and_then(|p| KiConfig::load(&p))
        .map(|c| c.install_dir)
        .or_else(default_install_dir)
}

/// Freier Speicherplatz in GB mit Komma, z. B. „3,4 GB“ (1 GB = 10⁹ Bytes).
pub fn format_gb(bytes: u64) -> String {
    format!("{} GB", gb_number(bytes))
}

fn gb_number(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1e9).replace('.', ",")
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), InstallError> {
    if cancel.load(Ordering::Relaxed) {
        Err(InstallError::Cancelled)
    } else {
        Ok(())
    }
}

fn disk_error(e: &std::io::Error, plan: &Plan) -> InstallError {
    if e.kind() == std::io::ErrorKind::StorageFull {
        InstallError::NoSpace {
            needed: plan.space_needed(),
            free: free_space(&plan.install_dir).unwrap_or(0),
        }
    } else {
        InstallError::Disk(e.to_string())
    }
}

fn remove_path(p: &Path) -> std::io::Result<()> {
    let result = match std::fs::symlink_metadata(p) {
        Err(_) => return Ok(()),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
    };
    match result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Meldet Fortschritt höchstens alle 100 ms, Phasenwechsel sofort.
#[derive(Default)]
struct Throttle {
    last: Option<(Instant, Phase)>,
}

impl Throttle {
    fn due(&mut self, p: &Progress) -> bool {
        let now = Instant::now();
        let due = match self.last {
            None => true,
            Some((at, phase)) => phase != p.phase || now - at >= Duration::from_millis(100),
        };
        if due {
            self.last = Some((now, p.phase));
        }
        due
    }
}

static RUNNING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Verhindert zwei gleichzeitige Installationen in denselben Ordner (innerhalb dieses Programms).
struct DirLock(PathBuf);

impl DirLock {
    fn acquire(dir: &Path) -> Result<Self, InstallError> {
        let mut running = RUNNING.lock().map_err(|_| InstallError::Busy)?;
        if running.iter().any(|d| d == dir) {
            return Err(InstallError::Busy);
        }
        running.push(dir.to_path_buf());
        Ok(Self(dir.to_path_buf()))
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        if let Ok(mut running) = RUNNING.lock() {
            running.retain(|d| d != &self.0);
        }
    }
}

#[cfg(test)]
mod tests;

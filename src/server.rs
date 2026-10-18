//! Startet `llama-server` bei Bedarf oder findet einen laufenden Mosaik-Dienst.
use std::ffi::OsString;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::client::ALIAS;
use crate::http::{self, HttpError};
use crate::{KiConfig, KiError};

pub const IDLE_SECONDS: u32 = 900;
pub const CONTEXT_TOKENS: u32 = 8192;

pub fn server_args(cfg: &KiConfig, port: u16) -> Result<Vec<OsString>, KiError> {
    let model = cfg.model_path().ok_or(KiError::NotInstalled)?;
    let mut args: Vec<OsString> = vec!["-m".into(), model.into_os_string()];
    if let Some(mmproj) = cfg.mmproj_path() {
        args.push("--mmproj".into());
        args.push(mmproj.into_os_string());
    }
    for a in [
        "--host".to_string(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--alias".into(),
        ALIAS.into(),
        "-c".into(),
        CONTEXT_TOKENS.to_string(),
        "--parallel".into(),
        "1".into(),
        "--sleep-idle-seconds".into(),
        IDLE_SECONDS.to_string(),
        "--reasoning".into(),
        "off".into(),
        "--no-webui".into(),
    ] {
        args.push(a.into());
    }
    Ok(args)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// Mosaik-Dienst läuft und ist bereit.
    Ours,
    /// Ein llama-server lädt gerade das Modell (503).
    Loading,
    /// Ein anderes Programm belegt den Port.
    Foreign,
    /// Niemand hört auf dem Port.
    Free,
}

pub fn probe(port: u16) -> Probe {
    match http::request(port, "GET", "/v1/models", None, Duration::from_secs(2)) {
        Err(HttpError::Refused) => Probe::Free,
        Err(_) => Probe::Foreign,
        Ok(r) if r.status == 503 => Probe::Loading,
        Ok(r) if r.status == 200 && r.body.contains(&format!("\"{ALIAS}\"")) => Probe::Ours,
        Ok(_) => Probe::Foreign,
    }
}

/// Sorgt dafür, dass ein Mosaik-Dienst bereit ist, und liefert dessen Port.
/// Belegt ein fremdes Programm den Port, wird ein freier gewählt und (falls `config_path`) gespeichert.
pub fn ensure_running(
    cfg: &mut KiConfig,
    config_path: Option<&Path>,
    timeout: Duration,
) -> Result<u16, KiError> {
    if !cfg.is_ready() {
        return Err(KiError::NotInstalled);
    }
    match probe(cfg.port) {
        Probe::Ours => return Ok(cfg.port),
        Probe::Loading => return wait_ready(cfg.port, timeout),
        Probe::Free => {}
        Probe::Foreign => {
            cfg.port = free_port()?;
            if let Some(path) = config_path {
                cfg.save(path)
                    .map_err(|e| KiError::Unavailable(e.to_string()))?;
            }
        }
    }
    spawn(cfg)?;
    wait_ready(cfg.port, timeout)
}

fn wait_ready(port: u16, timeout: Duration) -> Result<u16, KiError> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if probe(port) == Probe::Ours {
            return Ok(port);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(KiError::Unavailable("Start dauerte zu lange".into()))
}

fn free_port() -> Result<u16, KiError> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| KiError::Unavailable(e.to_string()))?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| KiError::Unavailable(e.to_string()))
}

fn spawn(cfg: &KiConfig) -> Result<(), KiError> {
    let unavailable = |e: std::io::Error| KiError::Unavailable(e.to_string());
    let args = server_args(cfg, cfg.port)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(cfg.install_dir.join("ki.log"))
        .map_err(unavailable)?;
    let log_err = log.try_clone().map_err(unavailable)?;
    let binary = cfg.server_binary();
    let mut cmd = Command::new(&binary);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    if let Some(dir) = binary.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().map(|_| ()).map_err(unavailable)
}

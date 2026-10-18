//! Startet `llama-server` bei Bedarf oder findet einen laufenden Mosaik-Dienst.
use std::ffi::OsString;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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
        // Ein beschäftigter eigener Dienst antwortet evtl. nicht rechtzeitig: nicht noch einen starten.
        Err(HttpError::Timeout) => Probe::Loading,
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
        Probe::Loading => return wait_ready(cfg, None, timeout),
        Probe::Free => {}
        Probe::Foreign => {
            cfg.port = free_port()?;
            if let Some(path) = config_path {
                // Nicht fatal: der neue Port gilt im Speicher weiter, nur die Datei bleibt alt.
                let _ = cfg.save(path);
            }
        }
    }
    let child = spawn(cfg)?;
    wait_ready(cfg, Some(child), timeout)
}

/// Wartet auf den Dienst. Mit `child` wird zusätzlich geprüft, ob der gestartete Prozess schon
/// beendet ist; dann steht der Grund am Ende von `ki.log`. Nach Erfolg wird `child` fallen gelassen,
/// der Prozess läuft eigenständig weiter.
fn wait_ready(cfg: &KiConfig, mut child: Option<Child>, timeout: Duration) -> Result<u16, KiError> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if probe(cfg.port) == Probe::Ours {
            return Ok(cfg.port);
        }
        if let Some(c) = child.as_mut()
            && matches!(c.try_wait(), Ok(Some(_)))
        {
            // Ein letzter Blick: vielleicht antwortet ein anderer eigener Dienst auf dem Port.
            if probe(cfg.port) == Probe::Ours {
                return Ok(cfg.port);
            }
            return Err(KiError::Unavailable(log_tail(cfg, 5)));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(KiError::Unavailable("Start dauerte zu lange".into()))
}

fn log_tail(cfg: &KiConfig, lines: usize) -> String {
    let text = std::fs::read(cfg.install_dir.join("ki.log"))
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    let tail = all[all.len().saturating_sub(lines)..].join("\n");
    if tail.trim().is_empty() {
        "Prozess sofort beendet".into()
    } else {
        tail
    }
}

fn pid_path(cfg: &KiConfig) -> PathBuf {
    cfg.install_dir.join("llama-server.pid")
}

fn write_pid(cfg: &KiConfig, pid: u32) -> std::io::Result<()> {
    let path = pid_path(cfg);
    let tmp = path.with_extension("pid.tmp");
    let result = std::fs::write(&tmp, pid.to_string()).and_then(|()| std::fs::rename(&tmp, &path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Beendet den Mosaik-Dienst (für Installer und Deinstallation vor dem Ersetzen von Dateien).
/// Beendet wird nur, wenn die PID-Datei existiert und der Port wirklich einen Mosaik-Dienst
/// beantwortet (Schutz gegen wiederverwendete PIDs). Sonst gibt es nichts zu tun: `Ok(())`.
pub fn stop(cfg: &KiConfig) -> Result<(), KiError> {
    let path = pid_path(cfg);
    let Some(pid) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
    else {
        return Ok(());
    };
    if probe(cfg.port) != Probe::Ours {
        return Ok(());
    }
    terminate(pid)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while probe(cfg.port) == Probe::Ours {
        if Instant::now() >= deadline {
            return Err(KiError::Unavailable(
                "Der KI-Dienst ließ sich nicht beenden".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn terminate(pid: u32) -> Result<(), KiError> {
    let mut cmd;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        cmd = Command::new("kill");
        cmd.arg(pid.to_string());
    }
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| KiError::Unavailable(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(KiError::Unavailable(
            "Der KI-Dienst ließ sich nicht beenden".into(),
        ))
    }
}

fn free_port() -> Result<u16, KiError> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| KiError::Unavailable(e.to_string()))?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| KiError::Unavailable(e.to_string()))
}

fn spawn(cfg: &KiConfig) -> Result<Child, KiError> {
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
    let child = cmd.spawn().map_err(unavailable)?;
    // Ohne PID-Datei lässt sich der Dienst später nicht gezielt beenden; der Start selbst gelingt trotzdem.
    let _ = write_pid(cfg, child.id());
    Ok(child)
}

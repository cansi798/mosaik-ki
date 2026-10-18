//! Download mit Fortsetzen (`.part` + HTTP-Range) und SHA-256-Prüfung.
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::{FileSpec, InstallError, Phase};

const CHUNK: usize = 64 * 1024;

pub(super) fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

pub(super) fn part_len(dest: &Path) -> u64 {
    std::fs::metadata(part_path(dest)).map_or(0, |m| m.len())
}

/// Die Datei liegt fertig am Ziel. Sie wird erst nach bestandener Prüfung umbenannt,
/// die passende Größe genügt deshalb.
pub(super) fn is_complete(spec: &FileSpec, dest: &Path) -> bool {
    std::fs::metadata(dest).is_ok_and(|m| m.is_file() && m.len() == spec.size)
}

pub(super) struct Downloader {
    agent: ureq::Agent,
    /// Versuche ohne jeden Fortschritt, bevor aufgegeben wird.
    pub(super) attempts: u32,
    pub(super) backoff: Duration,
}

impl Default for Downloader {
    fn default() -> Self {
        // Ein Abruf liest höchstens 60 s lang; danach wird ab der erreichten Stelle neu angefragt.
        // So bleibt ein hängender Download nie ewig stehen und ein Abbruch greift zügig.
        Self::with_body_timeout(Duration::from_secs(60))
    }
}

impl Downloader {
    pub(super) fn with_body_timeout(body: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_recv_body(Some(body))
            .user_agent(concat!("mosaik-ki/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            attempts: 4,
            backoff: Duration::from_secs(2),
        }
    }

    /// Lädt `spec` nach `dest`. `on_bytes(phase, vorhandene_bytes)` meldet den Stand dieser Datei.
    pub(super) fn fetch(
        &self,
        spec: &FileSpec,
        dest: &Path,
        cancel: &AtomicBool,
        on_bytes: &mut dyn FnMut(Phase, u64),
    ) -> Result<(), InstallError> {
        if is_complete(spec, dest) {
            on_bytes(Phase::Downloading, spec.size);
            return Ok(());
        }
        let part = part_path(dest);
        // Zweiter Durchgang nur nach falscher Prüfsumme: dann komplett neu laden.
        for _round in 0..2 {
            let mut state = PartFile::open(&part, spec.size, cancel, on_bytes)?;
            let mut failures = 0;
            while state.have < spec.size {
                check(cancel)?;
                match self.request(spec, &mut state, cancel, on_bytes) {
                    Ok(()) => failures = 0,
                    Err(Attempt::Fatal(e)) => return Err(e),
                    Err(Attempt::Retry(e)) => {
                        failures += 1;
                        if failures >= self.attempts {
                            return Err(e);
                        }
                        sleep_checked(self.backoff * failures, cancel)?;
                    }
                }
            }
            let digest = state.finish().map_err(|e| io_error(&e, &part))?;
            if digest.eq_ignore_ascii_case(&spec.sha256) {
                std::fs::rename(&part, dest).map_err(|e| io_error(&e, &part))?;
                on_bytes(Phase::Downloading, spec.size);
                return Ok(());
            }
            let _ = std::fs::remove_file(&part);
            on_bytes(Phase::Downloading, 0);
        }
        Err(InstallError::Corrupt(spec.name.clone()))
    }

    /// Ein Abruf ab `state.have`. `Ok` heißt: Datei vollständig oder zumindest Fortschritt gemacht.
    fn request(
        &self,
        spec: &FileSpec,
        state: &mut PartFile,
        cancel: &AtomicBool,
        on_bytes: &mut dyn FnMut(Phase, u64),
    ) -> Result<(), Attempt> {
        let mut req = self.agent.get(&spec.url);
        if state.have > 0 {
            req = req.header("Range", format!("bytes={}-", state.have));
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(416)) => {
                // Der Teil passt nicht zur Datei der Gegenstelle: von vorn beginnen.
                state
                    .reset()
                    .map_err(|e| Attempt::Fatal(io_error(&e, &state.path)))?;
                return Err(Attempt::Retry(InstallError::Offline("416".into())));
            }
            Err(ureq::Error::StatusCode(code)) if (400..500).contains(&code) && code != 429 => {
                return Err(Attempt::Fatal(InstallError::Offline(format!(
                    "{code} für {}",
                    spec.url
                ))));
            }
            Err(e) => return Err(Attempt::Retry(InstallError::Offline(e.to_string()))),
        };
        match resp.status().as_u16() {
            206 => {
                let start = resp
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(content_range_start);
                if start != Some(state.have) {
                    state
                        .reset()
                        .map_err(|e| Attempt::Fatal(io_error(&e, &state.path)))?;
                    return Err(Attempt::Retry(InstallError::Offline(
                        "unpassender Bereich".into(),
                    )));
                }
            }
            // Die Gegenstelle ignoriert den Bereich und schickt alles: von vorn.
            200 => {
                if state.have > 0 {
                    state
                        .reset()
                        .map_err(|e| Attempt::Fatal(io_error(&e, &state.path)))?;
                    on_bytes(Phase::Downloading, 0);
                }
            }
            other => {
                return Err(Attempt::Retry(InstallError::Offline(format!(
                    "Status {other}"
                ))));
            }
        }
        let before = state.have;
        let mut reader = resp.into_body().into_reader();
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Attempt::Fatal(InstallError::Cancelled));
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    if state.have > before {
                        return Ok(());
                    }
                    return Err(Attempt::Retry(InstallError::Offline(e.to_string())));
                }
            };
            // Mehr als erwartet: abschneiden, die Prüfsumme entscheidet.
            let room = usize::try_from(state.size - state.have).unwrap_or(usize::MAX);
            let n = n.min(room);
            if let Err(e) = state.append(&buf[..n]) {
                let mut err = io_error(&e, &state.path);
                if let InstallError::NoSpace { needed, .. } = &mut err {
                    // Halbe Dateien nicht liegen lassen, wenn der Platz voll ist.
                    *needed = state.size;
                    let _ = std::fs::remove_file(&state.path);
                }
                return Err(Attempt::Fatal(err));
            }
            on_bytes(Phase::Downloading, state.have);
            if state.have >= state.size {
                break;
            }
        }
        if state.have > before || state.have >= state.size {
            Ok(())
        } else {
            Err(Attempt::Retry(InstallError::Offline(
                "leere Antwort".into(),
            )))
        }
    }
}

enum Attempt {
    Retry(InstallError),
    Fatal(InstallError),
}

/// Die `.part`-Datei samt laufender Prüfsumme über ihren Inhalt.
struct PartFile {
    path: PathBuf,
    file: File,
    hasher: Sha256,
    have: u64,
    size: u64,
}

impl PartFile {
    /// Öffnet bzw. legt an und rechnet die Prüfsumme über schon vorhandene Bytes.
    fn open(
        path: &Path,
        size: u64,
        cancel: &AtomicBool,
        on_bytes: &mut dyn FnMut(Phase, u64),
    ) -> Result<Self, InstallError> {
        let io = |e: std::io::Error| io_error(&e, path);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(io)?;
        let mut len = file.metadata().map_err(io)?.len();
        if len > size {
            file.set_len(0).map_err(io)?;
            len = 0;
        }
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        let mut read = 0u64;
        file.seek(SeekFrom::Start(0)).map_err(io)?;
        while read < len {
            check(cancel)?;
            let n = file.read(&mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            read += n as u64;
            on_bytes(Phase::Checking, read);
        }
        file.set_len(read).map_err(io)?;
        file.seek(SeekFrom::Start(read)).map_err(io)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            hasher,
            have: read,
            size,
        })
    }

    fn reset(&mut self) -> std::io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.hasher = Sha256::new();
        self.have = 0;
        Ok(())
    }

    fn append(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.file.write_all(data)?;
        self.hasher.update(data);
        self.have += data.len() as u64;
        Ok(())
    }

    /// Schreibt alles auf den Datenträger und liefert die Prüfsumme als Hex-Text.
    fn finish(self) -> std::io::Result<String> {
        self.file.sync_all()?;
        drop(self.file);
        Ok(hex(&self.hasher.finalize()))
    }
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `bytes 100-199/200` → 100.
fn content_range_start(v: &str) -> Option<u64> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    rest.split('-').next()?.trim().parse().ok()
}

fn check(cancel: &AtomicBool) -> Result<(), InstallError> {
    super::check_cancel(cancel)
}

fn sleep_checked(total: Duration, cancel: &AtomicBool) -> Result<(), InstallError> {
    let step = Duration::from_millis(50);
    let mut slept = Duration::ZERO;
    while slept < total {
        check(cancel)?;
        std::thread::sleep(step);
        slept += step;
    }
    check(cancel)
}

fn io_error(e: &std::io::Error, path: &Path) -> InstallError {
    if e.kind() == ErrorKind::StorageFull {
        let dir = path.parent().unwrap_or(path);
        InstallError::NoSpace {
            needed: 0,
            free: super::free_space(dir).unwrap_or(0),
        }
    } else {
        InstallError::Disk(format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(content_range_start("bytes 100-199/200"), Some(100));
        assert_eq!(content_range_start("bytes 0-0/*"), Some(0));
        assert_eq!(content_range_start("items 1-2/3"), None);
    }

    #[test]
    fn part_path_appends_suffix() {
        assert_eq!(
            part_path(Path::new("/x/modell.gguf")),
            PathBuf::from("/x/modell.gguf.part")
        );
    }
}

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;

use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use mosaik_ki::client::{ChatBackend, HttpChat, Reply};
use mosaik_ki::server::{self, Probe};
use mosaik_ki::{KiConfig, KiError, Tier, http};
use serde_json::json;

/// Die Tests starten Prozesse und suchen freie Ports; parallel laufen sie sich in die Quere
/// (Port-Wettlauf, "Text file busy" beim Kopieren des Fake-Servers). Daher nacheinander.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn fake_install(dir: &Path) -> KiConfig {
    let mut cfg = KiConfig::new(Tier::Kompakt, dir.join("Mosaik Ü").join("ki"));
    cfg.port = common::free_port();
    let bin = cfg.server_binary();
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fake-llama-server.py"
        ),
        &bin,
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(cfg.model_dir()).unwrap();
    std::fs::write(cfg.model_path().unwrap(), b"gguf").unwrap();
    cfg
}

fn quit(port: u16) {
    let _ = http::request(port, "GET", "/quit", None, Duration::from_secs(2));
}

#[test]
fn args_contain_idle_sleep_alias_and_localhost() {
    let _guard = serial();
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_install(dir.path());
    let args: Vec<String> = server::server_args(&cfg, 4321)
        .unwrap()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let joined = args.join(" ");
    for needle in [
        "--host 127.0.0.1",
        "--port 4321",
        "--alias mosaik",
        "--sleep-idle-seconds 900",
        "--reasoning off",
        "--no-webui",
        "-c 8192",
    ] {
        assert!(joined.contains(needle), "{needle} fehlt in {joined}");
    }
    assert!(
        args.iter()
            .any(|a| a.ends_with("gemma-4-E2B_q4_0-it.gguf") && a.contains("Mosaik Ü"))
    );
}

#[test]
fn not_installed_without_files() {
    let _guard = serial();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = KiConfig::new(Tier::Kompakt, dir.path().to_path_buf());
    assert_eq!(
        server::ensure_running(&mut cfg, None, Duration::from_secs(1)),
        Err(KiError::NotInstalled)
    );
}

#[test]
fn starts_server_then_reuses_it() {
    let _guard = serial();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fake_install(dir.path());
    assert_eq!(server::probe(cfg.port), Probe::Free);
    let port = server::ensure_running(&mut cfg, None, Duration::from_secs(10)).unwrap();
    assert_eq!(server::probe(port), Probe::Ours);
    let again = server::ensure_running(&mut cfg, None, Duration::from_secs(10)).unwrap();
    assert_eq!(port, again);
    let reply = HttpChat::new(port)
        .chat(&[json!({"role":"user","content":"Hallo"})], &[])
        .unwrap();
    assert_eq!(reply, Reply::Text("ECHO: Hallo".into()));
    quit(port);
}

#[test]
fn foreign_program_on_port_moves_to_free_port_and_saves_it() {
    let _guard = serial();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fake_install(dir.path());
    let foreign = TcpListener::bind(("127.0.0.1", cfg.port)).unwrap();
    std::thread::spawn(move || {
        for s in foreign.incoming().flatten() {
            use std::io::Write;
            let mut s = s;
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        }
    });
    let config_path = dir.path().join("mosaik-ki.json");
    let old = cfg.port;
    let port =
        server::ensure_running(&mut cfg, Some(&config_path), Duration::from_secs(10)).unwrap();
    assert_ne!(port, old);
    assert_eq!(KiConfig::load(&config_path).unwrap().port, port);
    quit(port);
}

#[test]
fn waits_while_loading_and_times_out_with_clear_error() {
    let _guard = serial();
    let port = common::free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            use std::io::Write;
            let mut s = s;
            let _ = s.write_all(
                b"HTTP/1.1 503 Loading\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            );
        }
    });
    assert_eq!(server::probe(port), Probe::Loading);
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fake_install(dir.path());
    cfg.port = port;
    let e = server::ensure_running(&mut cfg, None, Duration::from_millis(800)).unwrap_err();
    assert!(matches!(e, KiError::Unavailable(_)), "{e:?}");
}

#[test]
fn peer_that_drops_connections_is_foreign_not_free() {
    let _guard = serial();
    let port = common::free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            drop(s);
        }
    });
    assert_eq!(server::probe(port), Probe::Foreign);
}

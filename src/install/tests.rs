use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::download::{Downloader, hex, part_path};
use super::*;

// ---------- Fake-Server ----------

#[derive(Clone, Default)]
struct Behavior {
    /// Antwortet immer mit 200 und allem, egal was angefragt wurde.
    ignore_range: bool,
    /// Beim ersten Abruf nach so vielen Bytes die Verbindung trennen.
    drop_first_after: Option<usize>,
}

struct Fake {
    port: u16,
    /// Range-Kopf jeder Anfrage ("" ohne Range), in Reihenfolge.
    log: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }
    fn ranges(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

fn serve(files: Vec<(&str, Vec<u8>)>, behavior: Behavior) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let files: Vec<(String, Vec<u8>)> = files.into_iter().map(|(n, d)| (n.into(), d)).collect();
    let log2 = log.clone();
    std::thread::spawn(move || {
        let mut first = true;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let path = line
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .trim_start_matches('/')
                .to_string();
            let mut range = String::new();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("range:") {
                    range = v.trim().to_string();
                }
            }
            log2.lock().unwrap().push(range.clone());
            let Some((_, data)) = files.iter().find(|(n, _)| *n == path) else {
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            };
            let start = range
                .strip_prefix("bytes=")
                .and_then(|r| r.split('-').next())
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|_| !behavior.ignore_range);
            let head = match start {
                Some(s) if s >= data.len() => {
                    let _ = stream.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                Some(s) => format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {s}-{}/{}\r\nConnection: close\r\n\r\n",
                    data.len() - s,
                    data.len() - 1,
                    data.len()
                ),
                None => format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    data.len()
                ),
            };
            let body = &data[start.unwrap_or(0)..];
            let _ = stream.write_all(head.as_bytes());
            let body = match behavior.drop_first_after {
                Some(n) if first => &body[..n.min(body.len())],
                _ => body,
            };
            first = false;
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    });
    Fake { port, log }
}

fn data(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn sha(d: &[u8]) -> String {
    hex(&Sha256::digest(d))
}

fn spec(fake: &Fake, name: &str, d: &[u8]) -> FileSpec {
    FileSpec {
        url: fake.url(name),
        sha256: sha(d),
        size: d.len() as u64,
        name: name.into(),
    }
}

fn fast() -> Downloader {
    let mut d = Downloader::with_body_timeout(Duration::from_secs(10));
    d.backoff = Duration::from_millis(10);
    d.attempts = 3;
    d
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

// ---------- Download ----------

#[test]
fn download_resumes_after_dropped_connection() {
    let d = data(400_000, 1);
    let fake = serve(
        vec![("a.bin", d.clone())],
        Behavior {
            drop_first_after: Some(150_000),
            ..Behavior::default()
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.bin");
    fast()
        .fetch(
            &spec(&fake, "a.bin", &d),
            &dest,
            &no_cancel(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), d);
    assert!(!part_path(&dest).exists());
    let ranges = fake.ranges();
    assert_eq!(ranges[0], "");
    assert!(
        ranges[1].starts_with("bytes=") && ranges[1] != "bytes=0-",
        "{ranges:?}"
    );
}

#[test]
fn existing_part_is_continued_with_range() {
    let d = data(50_000, 2);
    let fake = serve(vec![("b.bin", d.clone())], Behavior::default());
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("b.bin");
    std::fs::write(part_path(&dest), &d[..1000]).unwrap();
    fast()
        .fetch(
            &spec(&fake, "b.bin", &d),
            &dest,
            &no_cancel(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), d);
    assert_eq!(fake.ranges(), vec!["bytes=1000-".to_string()]);
}

#[test]
fn server_ignoring_range_restarts_from_zero() {
    let d = data(50_000, 3);
    let fake = serve(
        vec![("c.bin", d.clone())],
        Behavior {
            ignore_range: true,
            ..Behavior::default()
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("c.bin");
    std::fs::write(part_path(&dest), &d[..1000]).unwrap();
    fast()
        .fetch(
            &spec(&fake, "c.bin", &d),
            &dest,
            &no_cancel(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), d);
}

#[test]
fn damaged_part_is_discarded_and_loaded_again() {
    let d = data(50_000, 4);
    let fake = serve(vec![("d.bin", d.clone())], Behavior::default());
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("d.bin");
    std::fs::write(part_path(&dest), vec![0xAB; 1000]).unwrap();
    fast()
        .fetch(
            &spec(&fake, "d.bin", &d),
            &dest,
            &no_cancel(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), d);
    assert_eq!(
        fake.ranges(),
        vec!["bytes=1000-".to_string(), String::new()]
    );
}

#[test]
fn wrong_checksum_twice_fails_and_leaves_nothing() {
    let d = data(20_000, 5);
    let fake = serve(vec![("e.bin", d.clone())], Behavior::default());
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("e.bin");
    let mut s = spec(&fake, "e.bin", &d);
    s.sha256 = "0".repeat(64);
    let err = fast()
        .fetch(&s, &dest, &no_cancel(), &mut |_, _| {})
        .unwrap_err();
    assert_eq!(err, InstallError::Corrupt("e.bin".into()));
    assert!(!dest.exists() && !part_path(&dest).exists());
    assert_eq!(fake.ranges().len(), 2);
}

#[test]
fn cancel_keeps_part_and_next_run_continues() {
    let d = data(600_000, 6);
    let fake = serve(vec![("f.bin", d.clone())], Behavior::default());
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("f.bin");
    let s = spec(&fake, "f.bin", &d);
    let cancel = AtomicBool::new(false);
    let err = fast()
        .fetch(&s, &dest, &cancel, &mut |phase, have| {
            if phase == Phase::Downloading && have > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
    assert_eq!(err, InstallError::Cancelled);
    let kept = std::fs::metadata(part_path(&dest)).unwrap().len();
    assert!(kept > 0 && kept < d.len() as u64, "{kept}");
    fast()
        .fetch(&s, &dest, &no_cancel(), &mut |_, _| {})
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), d);
    assert_eq!(fake.ranges().last().unwrap(), &format!("bytes={kept}-"));
}

#[test]
fn unreachable_source_is_offline_and_missing_file_fails_fast() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("x.bin");
    let s = FileSpec {
        url: format!("http://127.0.0.1:{port}/x.bin"),
        sha256: "0".repeat(64),
        size: 10,
        name: "x.bin".into(),
    };
    let err = fast()
        .fetch(&s, &dest, &no_cancel(), &mut |_, _| {})
        .unwrap_err();
    assert!(matches!(err, InstallError::Offline(_)), "{err:?}");

    let fake = serve(vec![], Behavior::default());
    let s = FileSpec {
        url: fake.url("fehlt.bin"),
        ..s
    };
    let err = fast()
        .fetch(&s, &dest, &no_cancel(), &mut |_, _| {})
        .unwrap_err();
    assert!(matches!(err, InstallError::Offline(_)), "{err:?}");
    assert_eq!(fake.ranges().len(), 1, "404 wird nicht wiederholt");
}

// ---------- Entpacken ----------

fn server_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

fn test_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o755);
        for (name, content) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(content).unwrap();
        }
        zip.finish().unwrap();
    }
    buf.into_inner()
}

#[test]
fn zip_extracts_server_folder_only_and_rejects_escapes() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("llama.zip");
    let server = format!("llama-b1/{}", server_name());
    std::fs::write(
        &archive,
        test_zip(&[
            (&server, b"programm"),
            ("llama-b1/ggml.dll", b"bibliothek"),
            ("llama-b1/sub/extra.bin", b"x"),
            ("anderes/readme.txt", b"nein"),
            ("../boese.txt", b"nein"),
        ]),
    )
    .unwrap();
    let dest = dir.path().join("out").join("llama");
    unpack::extract(&archive, &dest, server_name()).unwrap();
    assert_eq!(
        std::fs::read(dest.join(server_name())).unwrap(),
        b"programm"
    );
    assert_eq!(std::fs::read(dest.join("ggml.dll")).unwrap(), b"bibliothek");
    assert!(dest.join("sub").join("extra.bin").is_file());
    assert!(!dest.join("readme.txt").exists());
    assert!(!dir.path().join("out").join("boese.txt").exists());
    assert!(!dir.path().join("boese.txt").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest.join(server_name()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
}

#[test]
fn zip_without_server_is_rejected_and_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("llama.zip");
    std::fs::write(&archive, test_zip(&[("ggml.dll", b"x")])).unwrap();
    let dest = dir.path().join("llama");
    let err = unpack::extract(&archive, &dest, server_name()).unwrap_err();
    assert!(matches!(err, InstallError::Corrupt(_)), "{err:?}");
    assert!(!dest.exists());
}

#[test]
fn tar_gz_extracts_files_and_links() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("llama.tar.gz");
    {
        let file = std::fs::File::create(&archive).unwrap();
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        let mut add = |path: &str, content: &[u8], mode: u32| {
            let mut h = tar::Header::new_gnu();
            h.set_size(content.len() as u64);
            h.set_mode(mode);
            h.set_cksum();
            tar.append_data(&mut h, path, content).unwrap();
        };
        // Verweis vor seinem Ziel, wie in den echten Paketen.
        add(&format!("llama-b1/{}", server_name()), b"programm", 0o755);
        add("llama-b1/libllama.so.0.7.0", b"bibliothek", 0o644);
        let mut link = |path: &str, target: &str| {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            tar.append_link(&mut h, path, target).unwrap();
        };
        link("llama-b1/libllama.so", "libllama.so.0");
        link("llama-b1/libllama.so.0", "libllama.so.0.7.0");
        link("llama-b1/boese.so", "../../etc/passwd");
        tar.into_inner().unwrap().finish().unwrap();
    }
    let dest = dir.path().join("llama");
    unpack::extract(&archive, &dest, server_name()).unwrap();
    assert_eq!(
        std::fs::read(dest.join(server_name())).unwrap(),
        b"programm"
    );
    assert_eq!(
        std::fs::read(dest.join("libllama.so")).unwrap(),
        b"bibliothek"
    );
    assert_eq!(
        std::fs::read(dest.join("libllama.so.0")).unwrap(),
        b"bibliothek"
    );
    assert!(std::fs::symlink_metadata(dest.join("boese.so")).is_err());
}

// ---------- Ganze Installation ----------

struct Setup {
    _dir: tempfile::TempDir,
    root: PathBuf,
    fake: Fake,
    llama: FileSpec,
    a: FileSpec,
    b: FileSpec,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let zip = test_zip(&[(server_name(), b"programm"), ("ggml.dll", b"bibliothek")]);
    let a = data(30_000, 7);
    let b = data(20_000, 8);
    let fake = serve(
        vec![
            ("llama.zip", zip.clone()),
            ("a.gguf", a.clone()),
            ("b.gguf", b.clone()),
        ],
        Behavior::default(),
    );
    Setup {
        llama: spec(&fake, "llama.zip", &zip),
        a: spec(&fake, "a.gguf", &a),
        b: spec(&fake, "b.gguf", &b),
        fake,
        root,
        _dir: dir,
    }
}

impl Setup {
    fn plan(&self, tier: Tier, models: Vec<FileSpec>) -> Plan {
        Plan {
            tier,
            install_dir: self.root.join("Jürgen").join("ki"),
            config_path: self.root.join("config").join("mosaik-ki.json"),
            llama: tier.has_ki().then(|| self.llama.clone()),
            server_name: server_name().into(),
            models,
        }
    }
}

fn run(plan: &Plan) -> (Result<KiConfig, InstallError>, Vec<Progress>) {
    let mut seen = Vec::new();
    let r = plan.run_with(&fast(), &mut |p| seen.push(p.clone()), &no_cancel());
    (r, seen)
}

#[test]
fn install_switch_to_smaller_tier_and_remove() {
    let s = setup();
    let pro = s.plan(Tier::Pro, vec![s.a.clone(), s.b.clone()]);
    let (cfg, seen) = run(&pro);
    let cfg = cfg.unwrap();
    assert_eq!(cfg.tier, Tier::Pro);
    assert_eq!(KiConfig::load(&pro.config_path), Some(cfg.clone()));
    let llama = pro.install_dir.join("llama");
    assert_eq!(
        std::fs::read(llama.join(server_name())).unwrap(),
        b"programm"
    );
    assert!(llama.join("ggml.dll").is_file());
    assert!(pro.install_dir.join("models").join("a.gguf").is_file());
    assert!(pro.install_dir.join("models").join("b.gguf").is_file());
    assert!(
        !pro.install_dir.join(DOWNLOAD_DIR).exists(),
        "Paket aufgeräumt"
    );
    assert_eq!(seen.last().unwrap().phase, Phase::Done);
    assert!(
        seen.iter()
            .any(|p| p.text.starts_with("KI wird heruntergeladen …"))
    );
    assert_eq!(s.fake.ranges().len(), 3);

    // Gleiche Stufe nochmal: nichts wird geladen.
    run(&pro).0.unwrap();
    assert_eq!(s.fake.ranges().len(), 3);

    // Kleinere Stufe: b wird gelöscht, a und das Programm bleiben, nichts wird geladen.
    let kompakt = s.plan(Tier::Kompakt, vec![s.a.clone()]);
    std::fs::write(
        kompakt.install_dir.join("models").join("alt.gguf.part"),
        b"x",
    )
    .unwrap();
    let cfg = run(&kompakt).0.unwrap();
    assert_eq!(cfg.tier, Tier::Kompakt);
    let models = kompakt.install_dir.join("models");
    assert!(models.join("a.gguf").is_file());
    assert!(!models.join("b.gguf").exists());
    assert!(!models.join("alt.gguf.part").exists());
    assert!(llama.join(server_name()).is_file());
    assert_eq!(s.fake.ranges().len(), 3);

    // Standard: alles weg, Einstellung bleibt mit Stufe Standard.
    let standard = s.plan(Tier::Standard, vec![]);
    let cfg = run(&standard).0.unwrap();
    assert_eq!(cfg.tier, Tier::Standard);
    assert!(!standard.install_dir.exists());
    assert_eq!(
        KiConfig::load(&standard.config_path).map(|c| c.tier),
        Some(Tier::Standard)
    );
}

#[test]
fn port_from_existing_config_is_kept() {
    let s = setup();
    let plan = s.plan(Tier::Kompakt, vec![s.a.clone()]);
    let mut old = KiConfig::new(Tier::Standard, plan.install_dir.clone());
    old.port = 12345;
    old.save(&plan.config_path).unwrap();
    assert_eq!(run(&plan).0.unwrap().port, 12345);
}

#[test]
fn space_needed_counts_only_what_is_missing() {
    let s = setup();
    let plan = s.plan(Tier::Kompakt, vec![s.a.clone()]);
    let full = SPACE_MARGIN + s.llama.size * LLAMA_SPACE_FACTOR + s.a.size;
    assert_eq!(plan.space_needed(), full);
    let models = plan.install_dir.join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(part_path(&models.join("a.gguf")), vec![0u8; 1000]).unwrap();
    assert_eq!(plan.space_needed(), full - 1000);
    run(&plan).0.unwrap();
    assert_eq!(plan.space_needed(), 0);
    assert_eq!(s.plan(Tier::Standard, vec![]).space_needed(), 0);
}

#[test]
fn second_install_into_same_folder_is_busy() {
    let s = setup();
    let plan = s.plan(Tier::Kompakt, vec![s.a.clone()]);
    let _held = DirLock::acquire(&plan.install_dir).unwrap();
    assert_eq!(run(&plan).0.unwrap_err(), InstallError::Busy);
}

#[test]
fn cancelled_install_writes_no_config() {
    let s = setup();
    let plan = s.plan(Tier::Kompakt, vec![s.a.clone()]);
    let cancel = AtomicBool::new(true);
    let r = plan.run_with(&fast(), &mut |_| {}, &cancel);
    assert_eq!(r.unwrap_err(), InstallError::Cancelled);
    assert_eq!(KiConfig::load(&plan.config_path), None);
}

// ---------- Prüfung des Computers ----------

const GIB: u64 = 1024 * 1024 * 1024;

fn info(ram_gib: f64, free: Option<u64>) -> SystemInfo {
    SystemInfo {
        ram_bytes: (ram_gib * GIB as f64) as u64,
        free_bytes: free,
        needed_bytes: [0, 4_000_000_000, 6_500_000_000],
        ki_supported: true,
    }
}

#[test]
fn ram_is_compared_with_ten_percent_tolerance() {
    assert_eq!(recommended_tier(&info(11.4, None)), Tier::Pro);
    assert_eq!(recommended_tier(&info(10.9, None)), Tier::Pro);
    assert_eq!(recommended_tier(&info(10.7, None)), Tier::Kompakt);
    assert_eq!(recommended_tier(&info(7.3, None)), Tier::Kompakt);
    assert_eq!(recommended_tier(&info(7.1, None)), Tier::Standard);
    assert_eq!(recommended_tier(&info(4.0, None)), Tier::Standard);
    assert_eq!(
        fits(Tier::Standard, &info(2.0, None)),
        Err("Braucht 4 GB Arbeitsspeicher \u{2013} Ihr Computer hat 2 GB".into())
    );
}

#[test]
fn reasons_name_requirement_and_own_value() {
    assert_eq!(
        fits(Tier::Pro, &info(7.8, None)),
        Err("Braucht 12 GB Arbeitsspeicher \u{2013} Ihr Computer hat 8 GB".into())
    );
    assert_eq!(
        fits(Tier::Pro, &info(16.0, Some(3_100_000_000))),
        Err(
            "Braucht noch 6,5 GB freien Speicherplatz \u{2013} Ihr Computer hat 3,1 GB frei".into()
        )
    );
    assert_eq!(
        recommended_tier(&info(16.0, Some(5_000_000_000))),
        Tier::Kompakt
    );
    let mut unsupported = info(32.0, None);
    unsupported.ki_supported = false;
    assert!(fits(Tier::Kompakt, &unsupported).is_err());
    assert_eq!(fits(Tier::Standard, &unsupported), Ok(()));
    assert_eq!(recommended_tier(&unsupported), Tier::Standard);
}

#[test]
fn system_check_reports_plausible_values() {
    let dir = tempfile::tempdir().unwrap();
    let i = system_check(&dir.path().join("noch").join("nicht").join("da"));
    assert!(i.ram_bytes > GIB / 2);
    assert!(i.free_bytes.is_some());
    assert_eq!(i.needed(Tier::Standard), 0);
    assert!(i.needed(Tier::Pro) > i.needed(Tier::Kompakt));
}

#[test]
fn texts_use_german_numbers_and_no_jargon() {
    let p = Progress::new(Phase::Downloading, 1_234_000_000, 3_400_000_000);
    assert_eq!(p.text, "KI wird heruntergeladen \u{2026} 1,2 von 3,4 GB");
    assert!((p.fraction() - 0.363).abs() < 0.01);
    let errors = [
        InstallError::Cancelled,
        InstallError::Offline("x".into()),
        InstallError::NoSpace {
            needed: 5_200_000_000,
            free: 1,
        },
        InstallError::Corrupt("x".into()),
        InstallError::Unsupported,
        InstallError::Busy,
        InstallError::InUse("x".into()),
        InstallError::Disk("x".into()),
        InstallError::NoFolder,
    ];
    let mut texts: Vec<String> = errors.iter().map(InstallError::user_message).collect();
    texts.extend(
        [
            Phase::Preparing,
            Phase::Checking,
            Phase::Unpacking,
            Phase::Removing,
            Phase::Done,
        ]
        .map(|ph| Progress::new(ph, 0, 0).text),
    );
    texts.push(fits(Tier::Pro, &info(4.0, None)).unwrap_err());
    for t in texts {
        for word in [
            "RAM", "Modell", "Server", "Port", "Release", "HTTP", "SHA", "Download",
        ] {
            assert!(!t.contains(word), "{t}");
        }
    }
    assert!(
        InstallError::NoSpace {
            needed: 5_200_000_000,
            free: 1
        }
        .user_message()
        .contains("5,2 GB")
    );
}

/// Lädt das echte llama.cpp-Paket für dieses System und prüft URL, Größe und SHA-256.
/// Danach wird die zweite Hälfte per Range nachgeladen (über die Weiterleitung von GitHub).
#[test]
#[ignore = "lädt rund 20 MB aus dem Internet"]
fn real_llama_download_matches_checksum() {
    let asset = models::llama_asset(std::env::consts::OS, std::env::consts::ARCH).unwrap();
    let s = FileSpec::from(asset);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join(&s.name);
    Downloader::default()
        .fetch(&s, &dest, &no_cancel(), &mut |_, _| {})
        .unwrap();
    assert_eq!(std::fs::metadata(&dest).unwrap().len(), asset.size);
    println!("{} ok ({} Bytes, {})", s.url, asset.size, asset.sha256);

    // Fortsetzen: erste Hälfte als .part, Rest per Range.
    let half = asset.size / 2;
    let bytes = std::fs::read(&dest).unwrap();
    std::fs::remove_file(&dest).unwrap();
    std::fs::write(part_path(&dest), &bytes[..half as usize]).unwrap();
    let mut min_download = u64::MAX;
    Downloader::default()
        .fetch(&s, &dest, &no_cancel(), &mut |phase, have| {
            if phase == Phase::Downloading {
                min_download = min_download.min(have);
            }
        })
        .unwrap();
    assert!(
        min_download >= half,
        "Range wurde nicht beachtet: {min_download}"
    );

    let llama = dir.path().join("llama");
    let name = KiConfig::new(Tier::Kompakt, dir.path().to_path_buf())
        .server_binary()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    unpack::extract(&dest, &llama, &name).unwrap();
    assert!(llama.join(&name).is_file());
    println!(
        "entpackt: {} Dateien",
        std::fs::read_dir(&llama).unwrap().count()
    );
}

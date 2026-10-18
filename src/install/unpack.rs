//! Entpackt das llama.cpp-Paket (zip unter Windows, tar.gz unter macOS/Linux).
//!
//! Übernommen wird der Ordner, in dem `llama-server` liegt, samt allen Bibliotheken daneben.
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};

use super::InstallError;

fn corrupt(why: impl std::fmt::Display) -> InstallError {
    InstallError::Corrupt(format!("Paket: {why}"))
}

fn disk(e: std::io::Error) -> InstallError {
    if e.kind() == std::io::ErrorKind::StorageFull {
        InstallError::NoSpace { needed: 0, free: 0 }
    } else {
        InstallError::Disk(e.to_string())
    }
}

/// Entpackt `archive` nach `dest` (wird vorher geleert). `server_name` muss im Paket vorkommen.
pub(super) fn extract(archive: &Path, dest: &Path, server_name: &str) -> Result<(), InstallError> {
    super::remove_path(dest).map_err(disk)?;
    std::fs::create_dir_all(dest).map_err(disk)?;
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let result = if name.ends_with(".zip") {
        extract_zip(archive, dest, server_name)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        extract_tar_gz(archive, dest, server_name)
    } else {
        Err(corrupt("unbekanntes Format"))
    };
    let result = result.and_then(|()| {
        let server = dest.join(server_name);
        if !server.is_file() {
            return Err(corrupt("Programm fehlt"));
        }
        make_executable(&server).map_err(disk)
    });
    if result.is_err() {
        let _ = super::remove_path(dest);
    }
    result
}

/// Nur gewöhnliche, relative Pfadteile; alles andere (`..`, absolute Pfade) wird abgelehnt.
fn safe_relative(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Pfad des Eintrags relativ zum Ordner von `llama-server`.
fn below(prefix: &Path, path: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix(prefix).ok()?;
    (!rel.as_os_str().is_empty()).then(|| rel.to_path_buf())
}

fn find_prefix<'a>(
    paths: impl Iterator<Item = &'a PathBuf>,
    server_name: &str,
) -> Result<PathBuf, InstallError> {
    paths
        .filter(|p| p.file_name().is_some_and(|n| n == server_name))
        .min_by_key(|p| p.components().count())
        .map(|p| p.parent().map(Path::to_path_buf).unwrap_or_default())
        .ok_or_else(|| corrupt("Programm fehlt"))
}

fn write_file(target: &Path, reader: &mut dyn Read, mode: Option<u32>) -> Result<(), InstallError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(disk)?;
    }
    let mut out = File::create(target).map_err(disk)?;
    std::io::copy(reader, &mut out).map_err(|e| {
        if e.kind() == std::io::ErrorKind::InvalidData {
            corrupt(e)
        } else {
            disk(e)
        }
    })?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode & 0o777))
            .map_err(disk)?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

fn extract_zip(archive: &Path, dest: &Path, server_name: &str) -> Result<(), InstallError> {
    let file = File::open(archive).map_err(disk)?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(corrupt)?;
    let names: Vec<PathBuf> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok()?.enclosed_name())
        .filter_map(|p| safe_relative(&p))
        .collect();
    let prefix = find_prefix(names.iter(), server_name)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(corrupt)?;
        let Some(path) = entry.enclosed_name().and_then(|p| safe_relative(&p)) else {
            continue;
        };
        let Some(rel) = below(&prefix, &path) else {
            continue;
        };
        let target = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(disk)?;
        } else if !entry.is_symlink() {
            let mode = entry.unix_mode();
            write_file(&target, &mut entry, mode)?;
        }
    }
    Ok(())
}

fn open_tar(
    archive: &Path,
) -> Result<tar::Archive<flate2::read::GzDecoder<BufReader<File>>>, InstallError> {
    let file = File::open(archive).map_err(disk)?;
    Ok(tar::Archive::new(flate2::read::GzDecoder::new(
        BufReader::new(file),
    )))
}

fn extract_tar_gz(archive: &Path, dest: &Path, server_name: &str) -> Result<(), InstallError> {
    // Erster Durchgang: wo liegt das Programm?
    let mut names = Vec::new();
    let mut tar = open_tar(archive)?;
    for entry in tar.entries().map_err(corrupt)? {
        let entry = entry.map_err(corrupt)?;
        if let Some(p) = entry.path().ok().and_then(|p| safe_relative(&p)) {
            names.push(p);
        }
    }
    let prefix = find_prefix(names.iter(), server_name)?;

    let mut links = Vec::new();
    let mut tar = open_tar(archive)?;
    for entry in tar.entries().map_err(corrupt)? {
        let mut entry = entry.map_err(corrupt)?;
        let Some(path) = entry.path().ok().and_then(|p| safe_relative(&p)) else {
            continue;
        };
        let Some(rel) = below(&prefix, &path) else {
            continue;
        };
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(dest.join(&rel)).map_err(disk)?;
        } else if kind.is_symlink() {
            // Verweise nur innerhalb desselben Ordners (z. B. libllama.so -> libllama.so.0).
            if let Some(target) = entry
                .link_name()
                .ok()
                .flatten()
                .and_then(|t| safe_relative(&t))
                .filter(|t| t.components().count() == 1)
            {
                links.push((rel, target));
            }
        } else if kind.is_file() {
            let mode = entry.header().mode().ok();
            write_file(&dest.join(&rel), &mut entry, mode)?;
        }
    }
    create_links(dest, links)
}

/// Unter Unix echte Verweise, sonst Kopien. Ketten (a -> b -> c) werden in mehreren Runden aufgelöst.
fn create_links(dest: &Path, links: Vec<(PathBuf, PathBuf)>) -> Result<(), InstallError> {
    #[cfg(unix)]
    {
        for (rel, target) in links {
            let link = dest.join(&rel);
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&target, &link).map_err(disk)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let mut links = links;
        while !links.is_empty() {
            let before = links.len();
            let mut rest = Vec::new();
            for (rel, target) in links {
                let link = dest.join(&rel);
                let source = link.with_file_name(&target);
                if source.is_file() {
                    std::fs::copy(&source, &link).map_err(disk)?;
                } else {
                    rest.push((rel, target));
                }
            }
            if rest.len() == before {
                // Verweise ins Leere: weglassen.
                break;
            }
            links = rest;
        }
        Ok(())
    }
}

fn make_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(path)?.permissions();
        perm.set_mode(perm.mode() | 0o755);
        std::fs::set_permissions(path, perm)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

//! Bottle extraction (the "pour" unpack step).
//!
//! A bottle tarball is gzip-compressed and contains a single top-level keg directory laid out as
//! `<name>/<pkg_version>/...`. Extracting "into the Cellar" therefore reproduces
//! `<cellar>/<name>/<pkg_version>/`. Unix permissions and symlinks are preserved (Homebrew relies
//! on this for executables and the many intra-keg symlinks). See `specs/install-flow.md` §6b/§9d
//! and `specs/download-ghcr.md` §11.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use tar::Archive;

use crate::error::{FerroError, Result};

/// Extract the bottle `tarball` (`.tar.gz`) into `into_cellar`, returning the absolute keg path
/// `<into_cellar>/<name>/<pkg_version>`.
///
/// Unix file permissions and symlinks are preserved. The `<name>/<pkg_version>` top-level prefix is
/// read from the archive's entries (it is identical across every member of a bottle tarball).
pub fn extract_bottle(tarball: &Path, into_cellar: &Path) -> Result<PathBuf> {
    let keg_relative = top_level_keg_dir(tarball)?;

    std::fs::create_dir_all(into_cellar).map_err(|e| FerroError::io(into_cellar, e))?;

    let file = File::open(tarball).map_err(|e| FerroError::io(tarball, e))?;
    let mut archive = Archive::new(GzDecoder::new(BufReader::new(file)));
    // Preserve permissions and modification times; do NOT preserve ownership (bottles are unpacked
    // as the invoking user, like Homebrew). Symlinks are unpacked as symlinks by default.
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    archive.set_overwrite(true);
    archive
        .unpack(into_cellar)
        .map_err(|e| FerroError::io(into_cellar, e))?;

    let keg = into_cellar.join(&keg_relative);
    if !keg.is_dir() {
        return Err(FerroError::Unsupported(format!(
            "bottle tarball {} did not extract the expected keg directory {}",
            tarball.display(),
            keg.display()
        )));
    }
    Ok(keg)
}

/// Read the bottle's top-level `<name>/<pkg_version>` directory from the archive entries.
///
/// Every member of a well-formed bottle tarball shares this two-component prefix. We require it to
/// be consistent across entries and reject anything that escapes it (e.g. `..` or an absolute path)
/// rather than guessing, since a wrong keg path could corrupt a real install.
fn top_level_keg_dir(tarball: &Path) -> Result<PathBuf> {
    let file = File::open(tarball).map_err(|e| FerroError::io(tarball, e))?;
    let mut archive = Archive::new(GzDecoder::new(BufReader::new(file)));
    let entries = archive.entries().map_err(|e| FerroError::io(tarball, e))?;

    let mut keg: Option<PathBuf> = None;
    for entry in entries {
        let entry = entry.map_err(|e| FerroError::io(tarball, e))?;
        let path = entry.path().map_err(|e| FerroError::io(tarball, e))?;
        let prefix = two_component_prefix(&path).ok_or_else(|| {
            FerroError::Unsupported(format!(
                "bottle tarball {} contains entry {:?} without a <name>/<version> prefix",
                tarball.display(),
                path
            ))
        })?;
        match &keg {
            None => keg = Some(prefix),
            Some(existing) if *existing != prefix => {
                return Err(FerroError::Unsupported(format!(
                    "bottle tarball {} has inconsistent top-level keg dirs: {} vs {}",
                    tarball.display(),
                    existing.display(),
                    prefix.display()
                )));
            }
            Some(_) => {}
        }
    }

    keg.ok_or_else(|| {
        FerroError::Unsupported(format!("bottle tarball {} is empty", tarball.display()))
    })
}

/// The first two `Normal` path components (`<name>/<version>`), or `None` if the path is absolute,
/// uses `..`/`.`/root components, or has fewer than two normal components.
fn two_component_prefix(path: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let mut out = PathBuf::new();
    let mut count = 0;
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                out.push(part);
                count += 1;
                if count == 2 {
                    return Some(out);
                }
            }
            // Reject absolute, parent-dir, or current-dir components: these are not valid bottle
            // layout and could escape the Cellar.
            _ => return None,
        }
    }
    // A bare `<name>/` entry (the keg's own directory record, e.g. `wget/` then `wget/1.2/`) has
    // only one component; that's fine as long as some entry establishes the two-component prefix.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ferrobrew-{label}-{}-{nanos}", std::process::id()))
    }

    /// Build a tiny gzip-compressed bottle tarball laid out as `<name>/<version>/...` containing a
    /// regular file, an executable, and a symlink. Returns the tarball path.
    fn build_bottle(dir: &Path, name: &str, version: &str) -> PathBuf {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let tarball = dir.join(format!("{name}--{version}.bottle.tar.gz"));
        let gz = GzEncoder::new(File::create(&tarball).unwrap(), Compression::fast());
        let mut builder = tar::Builder::new(gz);

        let keg = format!("{name}/{version}");

        // A plain text file.
        let readme = b"hello bottle";
        let mut header = tar::Header::new_gnu();
        header.set_size(readme.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("{keg}/README"), &readme[..])
            .unwrap();

        // An executable.
        let script = b"#!/bin/sh\necho hi\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(script.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("{keg}/bin/tool"), &script[..])
            .unwrap();

        // A relative symlink: bin/tool-link -> tool.
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder
            .append_link(&mut header, format!("{keg}/bin/tool-link"), "tool")
            .unwrap();

        builder.into_inner().unwrap().finish().unwrap();
        tarball
    }

    #[test]
    fn extract_bottle_returns_keg_path_and_preserves_contents() {
        let dir = unique_temp_dir("extract-basic");
        std::fs::create_dir_all(&dir).unwrap();
        let tarball = build_bottle(&dir, "wget", "1.21.4");
        let cellar = dir.join("Cellar");

        let keg = extract_bottle(&tarball, &cellar).unwrap();
        assert_eq!(keg, cellar.join("wget").join("1.21.4"));
        assert!(keg.is_dir());

        let readme = std::fs::read_to_string(keg.join("README")).unwrap();
        assert_eq!(readme, "hello bottle");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_bottle_preserves_executable_permissions() {
        let dir = unique_temp_dir("extract-perms");
        std::fs::create_dir_all(&dir).unwrap();
        let tarball = build_bottle(&dir, "wget", "1.21.4");
        let cellar = dir.join("Cellar");

        let keg = extract_bottle(&tarball, &cellar).unwrap();
        let mode = std::fs::metadata(keg.join("bin/tool"))
            .unwrap()
            .permissions()
            .mode();
        // The executable bit for the owner must survive extraction.
        assert_eq!(mode & 0o755, 0o755);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_bottle_preserves_symlinks() {
        let dir = unique_temp_dir("extract-symlink");
        std::fs::create_dir_all(&dir).unwrap();
        let tarball = build_bottle(&dir, "wget", "1.21.4");
        let cellar = dir.join("Cellar");

        let keg = extract_bottle(&tarball, &cellar).unwrap();
        let link = keg.join("bin/tool-link");
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("tool"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn top_level_keg_dir_reads_name_and_version() {
        let dir = unique_temp_dir("extract-toplevel");
        std::fs::create_dir_all(&dir).unwrap();
        let tarball = build_bottle(&dir, "foo", "2.0.0_1");
        assert_eq!(
            top_level_keg_dir(&tarball).unwrap(),
            PathBuf::from("foo/2.0.0_1")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_component_prefix_rejects_traversal_and_absolute() {
        assert_eq!(
            two_component_prefix(Path::new("name/version/file")),
            Some(PathBuf::from("name/version"))
        );
        assert_eq!(two_component_prefix(Path::new("/etc/passwd")), None);
        assert_eq!(two_component_prefix(Path::new("../escape/file")), None);
        assert_eq!(two_component_prefix(Path::new("name")), None);
    }

    /// Build a gzip tarball from `(path, contents)` entries, exactly as given (no `<name>/<version>`
    /// assumptions), so tests can exercise malformed layouts.
    fn build_raw_tar(dir: &Path, label: &str, entries: &[(&str, &[u8])]) -> PathBuf {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let tarball = dir.join(format!("{label}.tar.gz"));
        let gz = GzEncoder::new(File::create(&tarball).unwrap(), Compression::fast());
        let mut builder = tar::Builder::new(gz);
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        tarball
    }

    #[test]
    fn extract_bottle_rejects_missing_name_version_prefix() {
        // Entries without a `<name>/<version>` two-component prefix are not a valid bottle and must
        // be refused rather than guessed at.
        let dir = unique_temp_dir("extract-noprefix");
        std::fs::create_dir_all(&dir).unwrap();
        let tarball = build_raw_tar(&dir, "noprefix", &[("toplevelfile", b"x")]);
        let err = extract_bottle(&tarball, &dir.join("Cellar")).unwrap_err();
        assert!(matches!(err, FerroError::Unsupported(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_bottle_rejects_inconsistent_keg_dirs() {
        // Two different top-level keg dirs in one tarball is ambiguous; reject it.
        let dir = unique_temp_dir("extract-inconsistent");
        std::fs::create_dir_all(&dir).unwrap();
        let tarball = build_raw_tar(
            &dir,
            "inconsistent",
            &[("wget/1.0/a", b"x"), ("curl/2.0/b", b"y")],
        );
        let err = extract_bottle(&tarball, &dir.join("Cellar")).unwrap_err();
        assert!(matches!(err, FerroError::Unsupported(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_bottle_missing_tarball_is_io_error() {
        let err = extract_bottle(
            Path::new("/nonexistent/ferrobrew/bottle.tar.gz"),
            Path::new("/tmp/cellar"),
        )
        .unwrap_err();
        assert!(matches!(err, FerroError::Io { .. }));
    }
}

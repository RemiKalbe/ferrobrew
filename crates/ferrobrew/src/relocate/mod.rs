//! Bottle relocation: rewrite a poured keg's placeholder tokens into the target machine's concrete
//! paths.
//!
//! This is the **pour** direction of Homebrew's keg relocation (`specs/relocation.md`). A bottle
//! tarball stores portable `@@HOMEBREW_*@@` placeholders; when poured into the Cellar we must:
//!
//! 1. substitute those tokens back to real paths in every **text** file (and text strings inside
//!    classified binaries), recording each changed file (with its hardlink siblings) as a
//!    keg-relative path for the receipt's `changed_files`; and
//! 2. rewrite **dynamic linkage** in Mach-O binaries (macOS — `install_name_tool` + mandatory
//!    ad-hoc codesign) or ELF binaries (Linux — `patchelf`).
//!
//! The riskiest module in ferrobrew: a wrong edit corrupts a user's real install. It is therefore
//! conservative — files it cannot fully understand are left untouched, and binary fixups on a
//! non-native OS are skipped gracefully rather than guessed at.
//!
//! Reference: `Library/Homebrew/keg_relocate.rb` and its `extend/os/{mac,linux}/keg_relocate.rb`
//! overrides.

mod classify;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod text;

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::error::{FerroError, Result};

pub use classify::is_homebrew_created_file;
pub use text::{
    standard_relocation, Key, Matcher, Relocation, ReplacementPair, CELLAR_PLACEHOLDER,
    JAVA_PLACEHOLDER, LIBRARY_PLACEHOLDER, NULL_BYTE, PERL_PLACEHOLDER, PREFIX_PLACEHOLDER,
    REPOSITORY_PLACEHOLDER,
};

/// Relocate a poured keg in place, returning the keg-relative paths of every file changed by text
/// substitution (the receipt's `changed_files`).
///
/// Steps, mirroring `replace_placeholders_with_locations` (spec §6):
/// 1. dynamic-linkage rewrite over all binaries (Mach-O on macOS / ELF on Linux); skipped on the
///    non-native OS;
/// 2. text substitution over text/libtool files, grouped by inode so hardlinks are read/written
///    once and every sibling is recorded.
///
/// `keg_dir` is the installed-formula directory `<cellar>/<name>/<version>`; the formula name is
/// taken from its parent directory for the `.brew/<name>.rb` skip and the glibc/gcc gates.
pub fn relocate_keg(keg_dir: &Path, reloc: &Relocation) -> Result<Vec<String>> {
    if !keg_dir.is_dir() {
        return Err(FerroError::NotFound(format!(
            "keg directory {}",
            keg_dir.display()
        )));
    }
    let formula_name = formula_name_from_keg(keg_dir);

    // Collect every regular (non-symlink, non-dir) file once.
    let files = collect_files(keg_dir)?;

    // 1. Dynamic-linkage rewrite (binaries). Native-OS only; skipped gracefully elsewhere.
    relocate_dynamic_linkage(&files, reloc, &formula_name)?;

    // 2. Text substitution, grouped by inode.
    let changed = replace_text_in_files(keg_dir, &files, reloc, &formula_name)?;
    Ok(changed)
}

/// The formula name from the keg path `<cellar>/<name>/<version>` (the parent directory's name).
fn formula_name_from_keg(keg_dir: &Path) -> String {
    keg_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// Recursively collect regular files under `root`, skipping symlinks and directories. Returns
/// absolute paths.
fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| FerroError::io(&dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| FerroError::io(&dir, e))?;
            let path = entry.path();
            // symlink_metadata: do NOT follow symlinks (spec rejects symlinks everywhere).
            let meta = std::fs::symlink_metadata(&path).map_err(|e| FerroError::io(&path, e))?;
            let ft = meta.file_type();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Dynamic-linkage rewrite over binaries. On macOS this is Mach-O; on Linux, ELF; on any other
/// host it is a graceful no-op (the spec requires skipping binary fixups on the non-native OS).
#[allow(unused_variables)]
fn relocate_dynamic_linkage(
    files: &[PathBuf],
    reloc: &Relocation,
    formula_name: &str,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
        for file in files {
            let meta = match std::fs::symlink_metadata(file) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !seen.insert((meta.dev(), meta.ino())) {
                continue; // hardlink already processed
            }
            let head = read_head(file, 8);
            if !macos::is_macho(&head) {
                continue;
            }
            with_writable(file, &meta, || macos::relocate_macho(file, reloc))?;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
        for file in files {
            let meta = match std::fs::symlink_metadata(file) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !seen.insert((meta.dev(), meta.ino())) {
                continue;
            }
            let head = read_head(file, 64);
            if !linux::is_elf(&head) {
                continue;
            }
            // Pour-direction: skip_protodesc is false (only skipped during bottling, spec §8).
            with_writable(file, &meta, || {
                linux::relocate_elf(file, reloc, formula_name, false)
            })?;
        }
    }

    Ok(())
}

/// Read up to `n` bytes from the head of a file; empty on error (treated as "not a binary").
fn read_head(file: &Path, n: usize) -> Vec<u8> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    match std::fs::File::open(file).and_then(|mut f| f.read(&mut buf)) {
        Ok(read) => {
            buf.truncate(read);
            buf
        }
        Err(_) => Vec::new(),
    }
}

/// Run `op` with the file temporarily made user-writable, restoring the original mode afterwards
/// (Homebrew's `ensure_writable`). On Unix we toggle the owner-write bit.
#[cfg(unix)]
fn with_writable<F>(file: &Path, meta: &std::fs::Metadata, op: F) -> Result<bool>
where
    F: FnOnce() -> Result<bool>,
{
    use std::os::unix::fs::PermissionsExt;
    let original_mode = meta.permissions().mode();
    let writable = original_mode & 0o200 != 0;
    if !writable {
        let mut perms = meta.permissions();
        perms.set_mode(original_mode | 0o200);
        std::fs::set_permissions(file, perms).map_err(|e| FerroError::io(file, e))?;
    }
    let result = op();
    if !writable {
        let mut perms = std::fs::Permissions::from_mode(original_mode);
        perms.set_mode(original_mode);
        // Best-effort restore; do not mask the operation's own error.
        let _ = std::fs::set_permissions(file, perms);
    }
    result
}

/// Text substitution over text/libtool files, grouped by inode (spec §9 `replace_text_in_files`).
///
/// For each inode group: read the first file once; classify; if it is a text candidate, apply the
/// relocation; on change, atomic-write the new content, re-create the hardlink siblings, and record
/// every member of the group as a keg-relative path string.
fn replace_text_in_files(
    keg_dir: &Path,
    files: &[PathBuf],
    reloc: &Relocation,
    formula_name: &str,
) -> Result<Vec<String>> {
    // Group by (dev, ino) so hardlinks share one read/write. BTreeMap for deterministic ordering.
    let mut groups: BTreeMap<(u64, u64), Vec<PathBuf>> = BTreeMap::new();
    for file in files {
        let meta = std::fs::symlink_metadata(file).map_err(|e| FerroError::io(file, e))?;
        groups
            .entry((meta.dev(), meta.ino()))
            .or_default()
            .push(file.clone());
    }

    let mut changed_files: Vec<String> = Vec::new();
    for members in groups.values() {
        let Some(first) = members.first() else {
            continue;
        };
        let keg_relative = relative_to(keg_dir, first);

        let content = std::fs::read(first).map_err(|e| FerroError::io(first, e))?;
        if !classify::is_text_candidate(&keg_relative, formula_name, &content) {
            continue;
        }

        let mut buf = content;
        if !reloc.replace_text(&mut buf) {
            continue;
        }

        atomic_write(first, &buf)?;

        // atomic_write breaks hardlinks (rename installs a new inode); re-link the siblings.
        for sibling in &members[1..] {
            relink(first, sibling)?;
        }

        for member in members {
            changed_files.push(relative_to(keg_dir, member).to_string_lossy().into_owned());
        }
    }

    changed_files.sort();
    Ok(changed_files)
}

/// The path of `file` relative to `keg_dir` (keg-relative, as stored in the receipt).
fn relative_to(keg_dir: &Path, file: &Path) -> PathBuf {
    file.strip_prefix(keg_dir)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| file.to_path_buf())
}

/// Write `bytes` to `target` atomically: write a temp file in the same directory, copy the
/// original's mode/uid/gid, then rename over the target (Homebrew's `atomic_write`, pathname.rb).
fn atomic_write(target: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let original = std::fs::symlink_metadata(target).map_err(|e| FerroError::io(target, e))?;

    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = target.file_name().and_then(|n| n.to_str()).unwrap_or("tmp");
    let tmp = parent.join(format!(".{base}.ferrobrew.{pid}.{nanos}"));

    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(FerroError::io(&tmp, e));
    }

    // Preserve mode (and uid/gid where permitted) before the rename.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            &tmp,
            std::fs::Permissions::from_mode(original.permissions().mode()),
        );
        // chown is best-effort: only root can change owner; preserving it matters for shared kegs.
        let _ = chown_like(&tmp, original.uid(), original.gid());
    }

    if let Err(e) = std::fs::rename(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(FerroError::io(target, e));
    }
    Ok(())
}

/// Best-effort `chown` matching `uid`/`gid`, via libc. Failure (e.g. unprivileged) is ignored by
/// the caller; we only attempt to preserve ownership when we can.
#[cfg(unix)]
fn chown_like(path: &Path, uid: u32, gid: u32) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: `c` is a valid NUL-terminated path; chown takes ownership of nothing.
    let rc = unsafe { libc_chown(c.as_ptr(), uid, gid) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "chown"]
    fn libc_chown(path: *const std::os::raw::c_char, uid: u32, gid: u32) -> std::os::raw::c_int;
}

/// Re-create `link` as a hardlink to `target`, replacing whatever is currently there (Homebrew's
/// `FileUtils.ln(first, file, force: true)` after atomic_write broke the original hardlink).
fn relink(target: &Path, link: &Path) -> Result<()> {
    if link.exists() || std::fs::symlink_metadata(link).is_ok() {
        std::fs::remove_file(link).map_err(|e| FerroError::io(link, e))?;
    }
    std::fs::hard_link(target, link).map_err(|e| FerroError::io(link, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Platform};
    use std::fs;
    use std::io::Write;

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = base.join(format!("ferrobrew-reloc-test-{pid}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config_with(prefix: &str, cellar: &str) -> Config {
        let platform = Platform {
            is_macos: cfg!(target_os = "macos"),
            default_prefix: prefix.into(),
            default_repository: prefix.into(),
        };
        Config::resolve(
            |k| match k {
                "HOMEBREW_PREFIX" => Some(prefix.to_string()),
                "HOMEBREW_REPOSITORY" => Some(prefix.to_string()),
                "HOMEBREW_CELLAR" => Some(cellar.to_string()),
                "HOMEBREW_LIBRARY" => Some(format!("{prefix}/Library")),
                "HOME" => Some("/tmp".to_string()),
                _ => None,
            },
            &platform,
        )
    }

    /// Build a keg layout `<root>/Cellar/<name>/<version>` and return the keg dir.
    fn make_keg(root: &Path, name: &str, version: &str) -> PathBuf {
        let keg = root.join("Cellar").join(name).join(version);
        fs::create_dir_all(&keg).unwrap();
        keg
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    #[test]
    fn text_file_gets_substituted_and_recorded() {
        let root = tempdir();
        let prefix = root.join("opt/homebrew");
        let cellar = prefix.join("Cellar");
        let cfg = config_with(prefix.to_str().unwrap(), cellar.to_str().unwrap());
        let reloc = standard_relocation(&cfg);

        let keg = make_keg(&prefix, "wget", "1.0");
        let script = keg.join("bin/wget-config");
        write_file(
            &script,
            b"#!/bin/sh\nexport PREFIX=@@HOMEBREW_PREFIX@@\nexport CELLAR=@@HOMEBREW_CELLAR@@\n",
        );

        let changed = relocate_keg(&keg, &reloc).unwrap();
        assert_eq!(changed, vec!["bin/wget-config".to_string()]);

        let result = fs::read_to_string(&script).unwrap();
        assert!(result.contains(&format!("PREFIX={}", prefix.display())));
        assert!(result.contains(&format!("CELLAR={}", cellar.display())));
        assert!(!result.contains("@@HOMEBREW_"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn binary_text_strings_are_substituted_but_real_binary_skipped() {
        let root = tempdir();
        let prefix = root.join("opt/homebrew");
        let cellar = prefix.join("Cellar");
        let cfg = config_with(prefix.to_str().unwrap(), cellar.to_str().unwrap());
        let reloc = standard_relocation(&cfg);

        let keg = make_keg(&prefix, "foo", "2.0");

        // A "binary" containing a NUL but no shebang -> NOT a text candidate, left untouched.
        let bin = keg.join("bin/foo");
        let mut bin_bytes = Vec::new();
        bin_bytes.extend_from_slice(b"\x00\x01@@HOMEBREW_PREFIX@@/lib\x00");
        write_file(&bin, &bin_bytes);

        let changed = relocate_keg(&keg, &reloc).unwrap();
        assert!(changed.is_empty());
        // Untouched.
        assert_eq!(fs::read(&bin).unwrap(), bin_bytes);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn libtool_archive_with_nul_is_still_relocated() {
        let root = tempdir();
        let prefix = root.join("opt/homebrew");
        let cellar = prefix.join("Cellar");
        let cfg = config_with(prefix.to_str().unwrap(), cellar.to_str().unwrap());
        let reloc = standard_relocation(&cfg);

        let keg = make_keg(&prefix, "foo", "2.0");
        let la = keg.join("lib/libfoo.la");
        // .la files are always candidates even with a NUL byte present.
        let mut bytes = b"libdir='@@HOMEBREW_PREFIX@@/lib'".to_vec();
        bytes.push(0u8);
        write_file(&la, &bytes);

        let changed = relocate_keg(&keg, &reloc).unwrap();
        assert_eq!(changed, vec!["lib/libfoo.la".to_string()]);
        let result = fs::read(&la).unwrap();
        assert!(result.starts_with(format!("libdir='{}/lib'", prefix.display()).as_bytes()));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn metafile_extension_is_skipped() {
        let root = tempdir();
        let prefix = root.join("opt/homebrew");
        let cellar = prefix.join("Cellar");
        let cfg = config_with(prefix.to_str().unwrap(), cellar.to_str().unwrap());
        let reloc = standard_relocation(&cfg);

        let keg = make_keg(&prefix, "foo", "2.0");
        let readme = keg.join("share/doc/README.md");
        write_file(&readme, b"see @@HOMEBREW_PREFIX@@ for details\n");

        let changed = relocate_keg(&keg, &reloc).unwrap();
        assert!(changed.is_empty());
        // README left untouched (placeholder remains).
        assert!(fs::read_to_string(&readme)
            .unwrap()
            .contains("@@HOMEBREW_PREFIX@@"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn brew_formula_copy_is_skipped() {
        let root = tempdir();
        let prefix = root.join("opt/homebrew");
        let cellar = prefix.join("Cellar");
        let cfg = config_with(prefix.to_str().unwrap(), cellar.to_str().unwrap());
        let reloc = standard_relocation(&cfg);

        let keg = make_keg(&prefix, "wget", "1.0");
        let formula = keg.join(".brew/wget.rb");
        write_file(&formula, b"prefix \"@@HOMEBREW_PREFIX@@\"\n");

        let changed = relocate_keg(&keg, &reloc).unwrap();
        assert!(changed.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_change_means_empty_changed_files() {
        let root = tempdir();
        let prefix = root.join("opt/homebrew");
        let cellar = prefix.join("Cellar");
        let cfg = config_with(prefix.to_str().unwrap(), cellar.to_str().unwrap());
        let reloc = standard_relocation(&cfg);

        let keg = make_keg(&prefix, "foo", "1.0");
        write_file(&keg.join("share/notes.txt.keep"), b"no tokens here\n");
        write_file(&keg.join("bin/run"), b"#!/bin/sh\necho hi\n");

        let changed = relocate_keg(&keg, &reloc).unwrap();
        assert!(changed.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn hardlinks_share_substitution_and_both_recorded() {
        let root = tempdir();
        let prefix = root.join("opt/homebrew");
        let cellar = prefix.join("Cellar");
        let cfg = config_with(prefix.to_str().unwrap(), cellar.to_str().unwrap());
        let reloc = standard_relocation(&cfg);

        let keg = make_keg(&prefix, "foo", "1.0");
        let a = keg.join("bin/a");
        let b = keg.join("bin/b");
        write_file(&a, b"#!/bin/sh\nP=@@HOMEBREW_PREFIX@@\n");
        fs::hard_link(&a, &b).unwrap();

        let changed = relocate_keg(&keg, &reloc).unwrap();
        assert_eq!(changed, vec!["bin/a".to_string(), "bin/b".to_string()]);

        // Both files reflect the substitution...
        let want = format!("#!/bin/sh\nP={}\n", prefix.display());
        assert_eq!(fs::read_to_string(&a).unwrap(), want);
        assert_eq!(fs::read_to_string(&b).unwrap(), want);
        // ...and remain hardlinked (same inode).
        let ia = fs::metadata(&a).unwrap().ino();
        let ib = fs::metadata(&b).unwrap().ino();
        assert_eq!(ia, ib);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_keg_dir_is_not_found() {
        let root = tempdir();
        let missing = root.join("does/not/exist");
        let cfg = config_with("/opt/homebrew", "/opt/homebrew/Cellar");
        let reloc = standard_relocation(&cfg);
        let err = relocate_keg(&missing, &reloc).unwrap_err();
        assert!(matches!(err, FerroError::NotFound(_)));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn symlinks_are_not_relocated() {
        let root = tempdir();
        let prefix = root.join("opt/homebrew");
        let cellar = prefix.join("Cellar");
        let cfg = config_with(prefix.to_str().unwrap(), cellar.to_str().unwrap());
        let reloc = standard_relocation(&cfg);

        let keg = make_keg(&prefix, "foo", "1.0");
        let real = keg.join("bin/real");
        write_file(&real, b"#!/bin/sh\nP=@@HOMEBREW_PREFIX@@\n");
        let link = keg.join("bin/link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let changed = relocate_keg(&keg, &reloc).unwrap();
        // Only the real file is recorded; the symlink is ignored.
        assert_eq!(changed, vec!["bin/real".to_string()]);

        fs::remove_dir_all(&root).ok();
    }
}

//! The `link` / `optlink` / `unlink` algorithms.
//!
//! A faithful port of `Keg#link`, `Keg#optlink`, `Keg#unlink`, `Keg#link_dir`,
//! `make_relative_symlink`, and `resolve_any_conflicts` from `Library/Homebrew/keg.rb`. Symlinks
//! are created RELATIVE (Homebrew relies on relative links for relocatability), and `resolved_path`
//! is a one-level resolve relative to the link's own directory — matching Ruby so that
//! `src == dst.resolved_path` comparisons agree.
//!
//! Out of scope (see spec "Open questions"): tab/alias/oldname handling in `optlink`,
//! `remove_old_aliases`, GNU `install-info` registration, keg_only refusals, and `link_overwrite`
//! allowlists. The `info` strategy here symlinks the file but does not shell out to `install-info`.

use std::io;
use std::os::unix::fs as unix_fs;
use std::path::{Component, Path, PathBuf};

use crate::config::Config;
use crate::error::{FerroError, Result};

use super::layout::{self, LinkStrategy};
use super::Keg;

/// Counts of symlink/unlink ops (`n`) and `rmdir` ops (`d`), replacing Ruby's global
/// `ObserverPathnameExtension` counters.
#[derive(Debug, Default, Clone, Copy)]
struct LinkCounts {
    n: usize,
    d: usize,
}

/// A linking failure, mapped from the underlying `io::Error` where relevant. Surfaced to callers as
/// [`FerroError`]; kept as a private enum so the rollback logic can branch on the cause.
#[derive(Debug)]
enum LinkFailure {
    /// `<name>` is already linked (a different keg owns `var/homebrew/linked/<name>`).
    AlreadyLinked { name: String, existing: PathBuf },
    /// A real file/dir or a foreign symlink already occupies `dst`.
    Conflict { src: PathBuf, dst: PathBuf },
    /// The destination's parent directory is not writable.
    DirNotWritable { dst: PathBuf },
    /// Any other I/O error.
    Io {
        path: Option<PathBuf>,
        source: io::Error,
    },
}

impl From<LinkFailure> for FerroError {
    fn from(failure: LinkFailure) -> Self {
        match failure {
            LinkFailure::AlreadyLinked { name, existing } => FerroError::Other(format!(
                "Cannot link {name}\nAnother version is already linked: {}",
                existing.display()
            )),
            LinkFailure::Conflict { src, dst } => FerroError::Other(format!(
                "Could not symlink {}\nTarget {} already exists. \
                 To force the link and overwrite all conflicting files: relink with overwrite=true",
                src.display(),
                dst.display()
            )),
            LinkFailure::DirNotWritable { dst } => FerroError::Other(format!(
                "{} is not writable.",
                dst.parent().unwrap_or(&dst).display()
            )),
            LinkFailure::Io { path, source } => match path {
                Some(p) => FerroError::io(p, source),
                None => FerroError::from(source),
            },
        }
    }
}

type LinkResult<T> = std::result::Result<T, LinkFailure>;

/// Symlink the keg's contents into the prefix following Homebrew's dir-vs-file rules.
///
/// On conflict this errors unless `overwrite` is set, in which case the conflicting destination is
/// deleted first. Returns the number of symlink operations performed (`ObserverPathnameExtension.n`
/// in Ruby). On any link error the partial link is rolled back via [`unlink`] before returning.
pub fn link(keg: &Keg, config: &Config, overwrite: bool) -> Result<usize> {
    let macos = layout::is_macos();
    let linked_record = layout::linked_keg_record(config, &keg.name);

    // Step 1: already-linked guard. Ruby checks `linked_keg_record.directory?` (follows the link).
    if linked_record.is_dir() {
        let existing = resolved_path(&linked_record).unwrap_or_else(|| linked_record.clone());
        return Err(LinkFailure::AlreadyLinked {
            name: keg.name.clone(),
            existing,
        }
        .into());
    }

    match link_inner(keg, config, overwrite, macos) {
        Ok(n) => Ok(n),
        Err(failure) => {
            // Best-effort rollback, then surface the original failure (matches Ruby's rescue).
            let _ = unlink(keg, config);
            Err(failure.into())
        }
    }
}

fn link_inner(keg: &Keg, config: &Config, overwrite: bool, macos: bool) -> LinkResult<usize> {
    let mut counts = LinkCounts::default();

    // Step 3: opt symlink BEFORE prefix linking.
    optlink_counts(keg, config, &mut counts)?;

    // Step 4: link_dir for each directory, in this exact order.
    for dir in layout::link_dirs(macos) {
        let dir_owned = dir.to_string();
        link_dir(
            keg,
            config,
            Path::new(&dir_owned),
            overwrite,
            macos,
            &mut counts,
            &|relative| strategy_for(&dir_owned, relative),
        )?;
    }

    // Step 5: mark the keg linked.
    make_relative_symlink(
        &linked_record_for(keg, config),
        &keg.path,
        false,
        &mut counts,
    )?;

    Ok(counts.n)
}

/// `HOMEBREW_LINKED_KEGS/<name>` for this keg.
fn linked_record_for(keg: &Keg, config: &Config) -> PathBuf {
    layout::linked_keg_record(config, &keg.name)
}

/// Dispatch a keg-relative path to the strategy function for its top-level directory.
fn strategy_for(top_dir: &str, relative: &Path) -> LinkStrategy {
    match top_dir {
        "etc" => layout::strategy_etc(relative),
        "bin" | "sbin" => layout::strategy_bin(relative),
        "include" => layout::strategy_include(relative),
        "share" => layout::strategy_share(relative),
        "lib" => layout::strategy_lib(relative),
        "Frameworks" => layout::strategy_frameworks(relative),
        _ => LinkStrategy::Link,
    }
}

/// Create/refresh the stable `opt/<name>` symlink. (Alias/oldname handling is out of scope.)
pub fn optlink(keg: &Keg, config: &Config) -> Result<()> {
    let mut counts = LinkCounts::default();
    optlink_counts(keg, config, &mut counts).map_err(Into::into)
}

fn optlink_counts(keg: &Keg, config: &Config, counts: &mut LinkCounts) -> LinkResult<()> {
    let opt = layout::opt_record(config, &keg.name);
    // Delete an existing opt symlink/entry, then recreate it pointing at this keg.
    if opt.is_symlink() || path_exists(&opt) {
        delete_path(&opt)?;
    }
    make_relative_symlink(&opt, &keg.path, false, counts)
}

/// Remove all symlinks the prefix holds into this keg, plus the linked-keg record, pruning the
/// now-empty mirrored directories. Returns the number of unlink operations performed.
pub fn unlink(keg: &Keg, config: &Config) -> Result<usize> {
    let macos = layout::is_macos();
    let mut counts = LinkCounts::default();
    let mut real_dirs: Vec<PathBuf> = Vec::new();

    for dir in layout::keg_link_directories(macos) {
        let root = keg.path.join(dir);
        if !path_exists(&root) {
            continue;
        }
        unlink_walk(keg, config, &root, &mut counts, &mut real_dirs).map_err(FerroError::from)?;
    }

    // Remove the linked-keg record if it points at this keg, then rmdir-if-possible its parent.
    let linked_record = linked_record_for(keg, config);
    if is_record_for(&linked_record, &keg.path) {
        delete_path(&linked_record).map_err(FerroError::from)?;
        counts.n += 1;
        if let Some(parent) = linked_record.parent() {
            rmdir_if_possible(parent);
        }
    }

    // rmdir the now-empty mirrored dirs deepest-first, never the must-exist set.
    let must_exist = layout::must_exist_subdirectories(config, macos);
    real_dirs.sort();
    real_dirs.dedup();
    for dir in real_dirs.iter().rev() {
        if must_exist.iter().any(|m| m == dir) {
            continue;
        }
        if rmdir_if_possible(dir) {
            counts.d += 1;
        }
    }

    Ok(counts.n)
}

// ---------------------------------------------------------------------------
// link_dir — the core pre-order walker.
// ---------------------------------------------------------------------------

/// Walk `keg.path/relative_dir` pre-order, applying `strategy` to each entry. Mirrors `Keg#link_dir`.
fn link_dir(
    keg: &Keg,
    config: &Config,
    relative_dir: &Path,
    overwrite: bool,
    macos: bool,
    counts: &mut LinkCounts,
    strategy: &dyn Fn(&Path) -> LinkStrategy,
) -> LinkResult<()> {
    let root = keg.path.join(relative_dir);
    if !path_exists(&root) {
        return Ok(());
    }
    let entries = sorted_dir_entries(&root)?;
    for entry in entries {
        visit_link(
            keg, config, &root, &entry, overwrite, macos, counts, strategy,
        )?;
    }
    Ok(())
}

/// Visit one entry during `link_dir`, recursing as the strategy dictates.
#[allow(clippy::too_many_arguments)]
fn visit_link(
    keg: &Keg,
    config: &Config,
    root: &Path,
    src: &Path,
    overwrite: bool,
    macos: bool,
    counts: &mut LinkCounts,
    strategy: &dyn Fn(&Path) -> LinkStrategy,
) -> LinkResult<()> {
    let dst = dst_for(config, &keg.path, src);
    let rel_under_root = src.strip_prefix(root).unwrap_or(src).to_path_buf();

    let meta = symlink_metadata(src)?;
    let is_symlink = meta.file_type().is_symlink();
    let is_dir = !is_symlink && meta.file_type().is_dir();

    if is_symlink || !is_dir {
        // src is a symlink or a regular file.
        if basename(src) == Some(".DS_Store") {
            return Ok(());
        }
        if resolved_path(src).as_deref() == Some(dst.as_path()) {
            return Ok(());
        }
        if is_pyc_in_site_packages(src) {
            return Ok(());
        }
        match strategy(&rel_under_root) {
            LinkStrategy::SkipFile => Ok(()),
            LinkStrategy::Info => {
                if basename(src) == Some("dir") {
                    return Ok(());
                }
                // `install_info` registration is out of scope; the file is still symlinked.
                make_relative_symlink(&dst, src, overwrite, counts)
            }
            _ => make_relative_symlink(&dst, src, overwrite, counts),
        }
    } else {
        // src is a directory.
        if dst.is_dir() && !dst.is_symlink() {
            // dst already a real dir: merge by recursing without pruning.
            return recurse_link(keg, config, root, src, overwrite, macos, counts, strategy);
        }
        if src.extension().and_then(|e| e.to_str()) == Some("app") {
            return Ok(());
        }
        match strategy(&rel_under_root) {
            LinkStrategy::SkipDir => Ok(()),
            LinkStrategy::Mkpath => {
                if !resolve_any_conflicts(config, &dst, overwrite, macos, counts)? {
                    mkpath(&dst)?;
                }
                recurse_link(keg, config, root, src, overwrite, macos, counts, strategy)
            }
            _ => {
                // :link — symlink the whole subtree as one link, then prune. But if a conflict was
                // expanded (dst became a real dir of another keg's per-file links), we neither
                // symlink nor prune: descend so THIS keg's files merge into that real dir too.
                if resolve_any_conflicts(config, &dst, overwrite, macos, counts)? {
                    recurse_link(keg, config, root, src, overwrite, macos, counts, strategy)
                } else {
                    make_relative_symlink(&dst, src, overwrite, counts)?;
                    Ok(())
                }
            }
        }
    }
}

/// Recurse into a directory's children (the non-prune branches of `link_dir`).
#[allow(clippy::too_many_arguments)]
fn recurse_link(
    keg: &Keg,
    config: &Config,
    root: &Path,
    src_dir: &Path,
    overwrite: bool,
    macos: bool,
    counts: &mut LinkCounts,
    strategy: &dyn Fn(&Path) -> LinkStrategy,
) -> LinkResult<()> {
    for child in sorted_dir_entries(src_dir)? {
        visit_link(
            keg, config, root, &child, overwrite, macos, counts, strategy,
        )?;
    }
    Ok(())
}

/// `resolve_any_conflicts`: only acts when `dst` is a symlink. If it points at a directory owned by
/// another keg in this Cellar, that keg's single dir-symlink is exploded into per-file symlinks so
/// the two formulae can coexist; returns true. Broken links are removed (returns false). Foreign
/// (non-keg) symlinks are left alone (returns false).
fn resolve_any_conflicts(
    config: &Config,
    dst: &Path,
    overwrite: bool,
    macos: bool,
    counts: &mut LinkCounts,
) -> LinkResult<bool> {
    if !dst.is_symlink() {
        return Ok(false);
    }
    let target = match resolved_path(dst) {
        Some(t) => t,
        None => return Ok(false),
    };
    // lstat the target: broken link -> remove dst, return false.
    if symlink_metadata(&target).is_err() {
        delete_path(dst)?;
        counts.n += 1;
        return Ok(false);
    }
    // Only directories can be exploded.
    if !target.is_dir() {
        return Ok(false);
    }
    // Is the target inside this Cellar (i.e. another keg)?
    let other = match keg_for(config, &target) {
        Some(keg) => keg,
        None => return Ok(false), // foreign symlink, leave it
    };
    // Unlink dst, then expand the other keg's dir into per-file symlinks (mkpath strategy).
    delete_path(dst)?;
    counts.n += 1;
    let rel = target
        .strip_prefix(&other.path)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    link_dir(
        &other,
        config,
        &rel,
        overwrite,
        macos,
        counts,
        &|_relative| LinkStrategy::Mkpath,
    )?;
    Ok(true)
}

/// Given a path that is inside `<cellar>/<name>/<version>/...`, reconstruct the owning [`Keg`].
/// Returns `None` if the path is not under the Cellar (the "NotAKegError" case).
fn keg_for(config: &Config, path: &Path) -> Option<Keg> {
    let rel = path.strip_prefix(&config.cellar).ok()?;
    let mut comps = rel.components();
    let name = match comps.next()? {
        Component::Normal(n) => n.to_str()?.to_string(),
        _ => return None,
    };
    let version = match comps.next()? {
        Component::Normal(v) => v.to_str()?.to_string(),
        _ => return None,
    };
    Some(Keg::new(&config.cellar, &name, &version))
}

// ---------------------------------------------------------------------------
// unlink walker.
// ---------------------------------------------------------------------------

/// Walk one of the keg's link directories, removing prefix symlinks that point back into this keg
/// and collecting real mirrored dirs for later rmdir. Mirrors the `unlink` loop in `Keg#unlink`.
fn unlink_walk(
    keg: &Keg,
    config: &Config,
    src: &Path,
    counts: &mut LinkCounts,
    real_dirs: &mut Vec<PathBuf>,
) -> LinkResult<()> {
    let dst = dst_for(config, &keg.path, src);

    if dst.is_dir() && !dst.is_symlink() {
        real_dirs.push(dst.clone());
        // Continue walking into src to reach nested links.
        let is_real_src_dir = match symlink_metadata(src) {
            Ok(m) => m.file_type().is_dir(),
            Err(_) => false,
        };
        if is_real_src_dir {
            for child in sorted_dir_entries(src)? {
                unlink_walk(keg, config, &child, counts, real_dirs)?;
            }
        }
        return Ok(());
    }

    if !dst.is_symlink() {
        // Not a symlink and not a real dir we own: still recurse into real src dirs to find links.
        if let Ok(m) = symlink_metadata(src) {
            if m.file_type().is_dir() {
                for child in sorted_dir_entries(src)? {
                    unlink_walk(keg, config, &child, counts, real_dirs)?;
                }
            }
        }
        return Ok(());
    }

    // dst is a symlink; only remove it if it points back into THIS keg.
    if resolved_path(&dst).as_deref() != Some(src) {
        return Ok(());
    }
    delete_path(&dst)?;
    counts.n += 1;
    // Prune: a directory src whose dst we just removed is not descended into.
    Ok(())
}

/// True iff `record` is a symlink resolving (one level) to `keg_path` — the `linked?`/record check.
fn is_record_for(record: &Path, keg_path: &Path) -> bool {
    record.is_symlink() && resolved_path(record).as_deref() == Some(keg_path)
}

// ---------------------------------------------------------------------------
// make_relative_symlink + low-level fs helpers.
// ---------------------------------------------------------------------------

/// Create a RELATIVE symlink `dst -> src`, creating `dst`'s parent dirs first. Implements the
/// EEXIST/EACCES handling and the broken-symlink retry loop from `make_relative_symlink`.
fn make_relative_symlink(
    dst: &Path,
    src: &Path,
    overwrite: bool,
    counts: &mut LinkCounts,
) -> LinkResult<()> {
    // Already pointing at src? Skip.
    if dst.is_symlink() && resolved_path(dst).as_deref() == Some(src) {
        return Ok(());
    }
    if overwrite && (path_exists(dst) || dst.is_symlink()) {
        delete_path(dst)?;
    }

    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let target = relative_path_from(src, parent);

    loop {
        if let Some(p) = dst.parent() {
            mkpath(p)?;
        }
        match unix_fs::symlink(&target, dst) {
            Ok(()) => {
                counts.n += 1;
                return Ok(());
            }
            Err(e) => match e.raw_os_error() {
                Some(libc_eexist) if libc_eexist == EEXIST => {
                    if path_exists(dst) {
                        return Err(LinkFailure::Conflict {
                            src: src.to_path_buf(),
                            dst: dst.to_path_buf(),
                        });
                    }
                    if dst.is_symlink() {
                        // Broken symlink: remove and retry.
                        delete_path(dst)?;
                        continue;
                    }
                    return Err(LinkFailure::Conflict {
                        src: src.to_path_buf(),
                        dst: dst.to_path_buf(),
                    });
                }
                Some(eacces) if eacces == EACCES => {
                    return Err(LinkFailure::DirNotWritable {
                        dst: dst.to_path_buf(),
                    });
                }
                _ => {
                    return Err(LinkFailure::Io {
                        path: Some(dst.to_path_buf()),
                        source: e,
                    });
                }
            },
        }
    }
}

const EEXIST: i32 = 17;
const EACCES: i32 = 13;

/// `mkdir -p` for `path`, mapping errors to [`LinkFailure`].
fn mkpath(path: &Path) -> LinkResult<()> {
    std::fs::create_dir_all(path).map_err(|e| LinkFailure::Io {
        path: Some(path.to_path_buf()),
        source: e,
    })
}

/// Remove a file or symlink (not a directory tree). For directories use `remove_dir`.
fn delete_path(path: &Path) -> LinkResult<()> {
    let result = if is_dir_no_follow(path) {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|e| LinkFailure::Io {
        path: Some(path.to_path_buf()),
        source: e,
    })
}

/// `rmdir_if_possible`: rmdir; if non-empty with only a lone `.DS_Store`, remove it and retry;
/// swallow common transient/permission errors. Returns true if the directory was removed.
fn rmdir_if_possible(dir: &Path) -> bool {
    match std::fs::remove_dir(dir) {
        Ok(()) => true,
        Err(e) => {
            let not_empty = e.raw_os_error() == Some(ENOTEMPTY) || e.raw_os_error() == Some(EEXIST);
            if not_empty && lone_ds_store(dir) {
                let _ = std::fs::remove_file(dir.join(".DS_Store"));
                return std::fs::remove_dir(dir).is_ok();
            }
            false
        }
    }
}

const ENOTEMPTY: i32 = if cfg!(target_os = "linux") { 39 } else { 66 };

/// True iff `dir`'s only child is `.DS_Store`.
fn lone_ds_store(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(rd) => {
            let names: Vec<_> = rd.filter_map(|e| e.ok().map(|e| e.file_name())).collect();
            names.len() == 1 && names[0] == ".DS_Store"
        }
        Err(_) => false,
    }
}

/// `dst = HOMEBREW_PREFIX + src.relative_path_from(keg_path)`.
fn dst_for(config: &Config, keg_path: &Path, src: &Path) -> PathBuf {
    let rel = src.strip_prefix(keg_path).unwrap_or(src);
    config.prefix.join(rel)
}

/// Ruby `resolved_path`: `dirname.join(readlink)` for a symlink. Ruby's `Pathname#join` cleans up
/// `.`/`..` segments lexically (no filesystem access), and `Pathname#==` is a string comparison, so
/// `path == dst.resolved_path` matches only when the joined target lexically equals the keg path.
/// We replicate that by lexically cleaning the joined result. Returns `None` for a non-symlink.
fn resolved_path(link: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(link).ok()?;
    let joined = if target.is_absolute() {
        target
    } else {
        link.parent().unwrap_or_else(|| Path::new(".")).join(target)
    };
    Some(lexically_clean(&joined))
}

/// Resolve `.`/`..` and redundant separators purely lexically, like Ruby's `Pathname#cleanpath`.
fn lexically_clean(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(comp),
            },
            other => out.push(other),
        }
    }
    let mut result = PathBuf::new();
    for comp in out {
        result.push(comp.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

/// `src.relative_path_from(base)` — a relative path from `base` to `src`, the symlink target Ruby
/// stores. Both inputs are treated lexically (no canonicalization), matching Ruby's `Pathname`.
fn relative_path_from(src: &Path, base: &Path) -> PathBuf {
    let src_comps: Vec<Component> = normalize(src);
    let base_comps: Vec<Component> = normalize(base);

    let mut i = 0;
    while i < src_comps.len() && i < base_comps.len() && src_comps[i] == base_comps[i] {
        i += 1;
    }
    let mut result = PathBuf::new();
    for _ in i..base_comps.len() {
        result.push("..");
    }
    for comp in &src_comps[i..] {
        result.push(comp.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

/// Lexically normalize away `.` components and redundant separators (keeps `..` as components).
fn normalize(path: &Path) -> Vec<Component<'_>> {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

/// `Path::exists` follows symlinks; this is the `dst.exist?` semantics (a broken symlink is "not
/// exist" but `is_symlink()` is true).
fn path_exists(path: &Path) -> bool {
    path.exists()
}

/// lstat-style: is this path itself a directory (not following a final symlink)?
fn is_dir_no_follow(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(m) => m.file_type().is_dir(),
        Err(_) => false,
    }
}

/// lstat wrapper that annotates the path on error.
fn symlink_metadata(path: &Path) -> LinkResult<std::fs::Metadata> {
    std::fs::symlink_metadata(path).map_err(|e| LinkFailure::Io {
        path: Some(path.to_path_buf()),
        source: e,
    })
}

/// Directory children, sorted for deterministic pre-order traversal.
fn sorted_dir_entries(dir: &Path) -> LinkResult<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| LinkFailure::Io {
            path: Some(dir.to_path_buf()),
            source: e,
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    Ok(entries)
}

/// The final path component as a `&str`.
fn basename(path: &Path) -> Option<&str> {
    path.file_name().and_then(|n| n.to_str())
}

/// `.pyc`/`.pyo` under a `/site-packages/` path — Python cached objects that are never linked.
fn is_pyc_in_site_packages(src: &Path) -> bool {
    let ext = src.extension().and_then(|e| e.to_str());
    if ext != Some("pyc") && ext != Some("pyo") {
        return false;
    }
    src.components().any(|c| match c {
        Component::Normal(n) => n == "site-packages",
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A temporary sandbox (prefix + cellar) under the system temp dir, removed on drop.
    struct Sandbox {
        root: PathBuf,
        config: Config,
    }

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    impl Sandbox {
        fn new() -> Sandbox {
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "ferrobrew-keg-test-{}-{}-{}",
                std::process::id(),
                id,
                now_nanos()
            ));
            let prefix = root.join("prefix");
            let cellar = root.join("prefix/Cellar");
            fs::create_dir_all(&prefix).unwrap();
            fs::create_dir_all(&cellar).unwrap();
            let config = Config {
                prefix: prefix.clone(),
                repository: prefix.clone(),
                cellar,
                caskroom: prefix.join("Caskroom"),
                cache: root.join("cache"),
                library: prefix.join("Library"),
            };
            Sandbox { root, config }
        }

        /// Create a keg directory tree from `(relative_path, contents)` pairs.
        fn make_keg(&self, name: &str, version: &str, files: &[(&str, &str)]) -> Keg {
            let keg = Keg::new(&self.config.cellar, name, version);
            for (rel, contents) in files {
                let path = keg.path.join(rel);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, contents).unwrap();
            }
            keg
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn now_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn read_link_target(p: &Path) -> PathBuf {
        fs::read_link(p).unwrap()
    }

    #[test]
    fn relative_path_from_basic() {
        assert_eq!(
            relative_path_from(Path::new("/a/b/c/file"), Path::new("/a/b/x")),
            PathBuf::from("../c/file")
        );
        assert_eq!(
            relative_path_from(Path::new("/a/b/c"), Path::new("/a/b")),
            PathBuf::from("c")
        );
        assert_eq!(
            relative_path_from(Path::new("/a/b"), Path::new("/a/b")),
            PathBuf::from(".")
        );
    }

    #[test]
    fn resolved_path_is_one_level_relative() {
        let sb = Sandbox::new();
        let dir = sb.root.join("d");
        fs::create_dir_all(&dir).unwrap();
        let link = dir.join("l");
        unix_fs::symlink("../target", &link).unwrap();
        // `dirname.join(readlink)` then lexically cleaned (Ruby Pathname#join semantics).
        assert_eq!(resolved_path(&link), Some(sb.root.join("target")));
    }

    #[test]
    fn optlink_creates_relative_opt_symlink() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("bin/foo", "#!/bin/sh\n")]);
        optlink(&keg, &sb.config).unwrap();
        let opt = layout::opt_record(&sb.config, "foo");
        assert!(opt.is_symlink());
        // Relative target, resolving to the keg.
        assert!(read_link_target(&opt).is_relative());
        assert_eq!(
            opt.canonicalize().unwrap(),
            keg.path.canonicalize().unwrap()
        );
    }

    #[test]
    fn optlink_refreshes_existing() {
        let sb = Sandbox::new();
        let keg1 = sb.make_keg("foo", "1.0", &[("bin/foo", "a")]);
        let keg2 = sb.make_keg("foo", "2.0", &[("bin/foo", "b")]);
        optlink(&keg1, &sb.config).unwrap();
        optlink(&keg2, &sb.config).unwrap();
        let opt = layout::opt_record(&sb.config, "foo");
        assert_eq!(
            opt.canonicalize().unwrap(),
            keg2.path.canonicalize().unwrap()
        );
    }

    #[test]
    fn link_creates_bin_symlink_and_marks_linked() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("bin/foo", "x"), ("bin/bar", "y")]);
        let n = link(&keg, &sb.config, false).unwrap();
        // opt + 2 bin files + linked record = 4 symlink ops.
        assert_eq!(n, 4);
        let bin_foo = sb.config.prefix.join("bin/foo");
        assert!(bin_foo.is_symlink());
        assert!(read_link_target(&bin_foo).is_relative());
        // linked record points at the keg.
        let record = layout::linked_keg_record(&sb.config, "foo");
        assert!(is_record_for(&record, &keg.path));
    }

    #[test]
    fn bin_subdirs_are_not_recursed() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("bin/foo", "x"), ("bin/sub/nested", "y")]);
        link(&keg, &sb.config, false).unwrap();
        assert!(sb.config.prefix.join("bin/foo").is_symlink());
        // The `bin/sub` subdirectory must NOT be linked (skip_dir prunes it).
        assert!(!sb.config.prefix.join("bin/sub").exists());
        assert!(!sb.config.prefix.join("bin/sub/nested").exists());
    }

    #[test]
    fn lib_default_dir_is_symlinked_as_whole_dir() {
        let sb = Sandbox::new();
        // `lib/foo/` has no special strategy -> :link -> single dir symlink.
        let keg = sb.make_keg("foo", "1.0", &[("lib/foo/a.dylib", "x")]);
        link(&keg, &sb.config, false).unwrap();
        let libfoo = sb.config.prefix.join("lib/foo");
        assert!(libfoo.is_symlink());
    }

    #[test]
    fn lib_pkgconfig_is_mkpath_real_dir() {
        let sb = Sandbox::new();
        // pkgconfig -> :mkpath -> real dir with file-level symlinks.
        let keg = sb.make_keg("foo", "1.0", &[("lib/pkgconfig/foo.pc", "x")]);
        link(&keg, &sb.config, false).unwrap();
        let pc_dir = sb.config.prefix.join("lib/pkgconfig");
        assert!(pc_dir.is_dir() && !pc_dir.is_symlink());
        let pc_file = pc_dir.join("foo.pc");
        assert!(pc_file.is_symlink());
    }

    #[test]
    fn etc_is_real_dir_with_file_links() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("etc/foo.conf", "x")]);
        link(&keg, &sb.config, false).unwrap();
        let etc = sb.config.prefix.join("etc");
        assert!(etc.is_dir() && !etc.is_symlink());
        assert!(sb.config.prefix.join("etc/foo.conf").is_symlink());
    }

    #[test]
    fn conflict_errors_without_overwrite() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("bin/foo", "x")]);
        // Pre-create a real file at the destination.
        let bin = sb.config.prefix.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("foo"), "existing").unwrap();
        let err = link(&keg, &sb.config, false).unwrap_err();
        match err {
            FerroError::Other(msg) => assert!(msg.contains("Could not symlink")),
            other => panic!("expected conflict error, got {other:?}"),
        }
        // Rollback must have removed any partial links (opt symlink removed by unlink? opt survives
        // unlink, but the failed bin link should not exist as our link).
        // The pre-existing real file is untouched.
        assert!(!sb.config.prefix.join("bin/foo").is_symlink());
        assert_eq!(fs::read_to_string(bin.join("foo")).unwrap(), "existing");
    }

    #[test]
    fn conflict_overwrite_replaces_file() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("bin/foo", "x")]);
        let bin = sb.config.prefix.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("foo"), "existing").unwrap();
        link(&keg, &sb.config, true).unwrap();
        assert!(sb.config.prefix.join("bin/foo").is_symlink());
    }

    #[test]
    fn already_linked_guard() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("bin/foo", "x")]);
        link(&keg, &sb.config, false).unwrap();
        // Re-link should hit the already-linked guard (record is a dir via the symlink).
        let err = link(&keg, &sb.config, false).unwrap_err();
        match err {
            FerroError::Other(msg) => assert!(msg.contains("already linked")),
            other => panic!("expected AlreadyLinked, got {other:?}"),
        }
    }

    #[test]
    fn unlink_removes_links_and_record_keeps_opt() {
        let sb = Sandbox::new();
        let keg = sb.make_keg(
            "foo",
            "1.0",
            &[("bin/foo", "x"), ("lib/pkgconfig/foo.pc", "y")],
        );
        link(&keg, &sb.config, false).unwrap();
        let removed = unlink(&keg, &sb.config).unwrap();
        // 2 file links + 1 record removed = 3.
        assert_eq!(removed, 3);
        assert!(!sb.config.prefix.join("bin/foo").exists());
        assert!(!sb.config.prefix.join("lib/pkgconfig/foo.pc").exists());
        // The mirrored real dir is rmdir'd...
        assert!(!sb.config.prefix.join("lib/pkgconfig").exists());
        // ...but the opt symlink survives unlink.
        assert!(layout::opt_record(&sb.config, "foo").is_symlink());
        // ...and the linked record is gone.
        assert!(!layout::linked_keg_record(&sb.config, "foo").is_symlink());
    }

    #[test]
    fn unlink_preserves_must_exist_bin_dir() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("bin/foo", "x")]);
        // Pre-create the must-exist bin dir as a real dir (link will merge into it).
        fs::create_dir_all(sb.config.prefix.join("bin")).unwrap();
        link(&keg, &sb.config, false).unwrap();
        unlink(&keg, &sb.config).unwrap();
        // bin is in must_exist_subdirectories so it must NOT be rmdir'd.
        assert!(sb.config.prefix.join("bin").is_dir());
    }

    #[test]
    fn unlink_only_removes_links_into_this_keg() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("bin/foo", "x")]);
        link(&keg, &sb.config, false).unwrap();
        // A foreign symlink in bin pointing elsewhere must be left alone.
        let foreign = sb.config.prefix.join("bin/elsewhere");
        unix_fs::symlink("/some/other/target", &foreign).unwrap();
        unlink(&keg, &sb.config).unwrap();
        assert!(foreign.is_symlink());
        // Cleanup so the must-exist bin dir check elsewhere is unaffected.
        let _ = fs::remove_file(&foreign);
    }

    #[test]
    fn shared_dir_explodes_on_second_keg() {
        let sb = Sandbox::new();
        // First keg owns lib/shared as a single :link dir-symlink.
        let keg1 = sb.make_keg("one", "1.0", &[("lib/shared/a.dylib", "a")]);
        link(&keg1, &sb.config, false).unwrap();
        assert!(sb.config.prefix.join("lib/shared").is_symlink());

        // Second keg also has lib/shared; linking it must explode keg1's dir-symlink into a real
        // dir holding per-file symlinks of BOTH kegs.
        let keg2 = sb.make_keg("two", "1.0", &[("lib/shared/b.dylib", "b")]);
        link(&keg2, &sb.config, false).unwrap();

        let shared = sb.config.prefix.join("lib/shared");
        assert!(shared.is_dir() && !shared.is_symlink());
        assert!(shared.join("a.dylib").is_symlink());
        assert!(shared.join("b.dylib").is_symlink());
    }

    #[test]
    fn ds_store_is_skipped() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("bin/foo", "x"), ("bin/.DS_Store", "junk")]);
        link(&keg, &sb.config, false).unwrap();
        assert!(sb.config.prefix.join("bin/foo").is_symlink());
        assert!(!sb.config.prefix.join("bin/.DS_Store").exists());
    }

    #[test]
    fn keg_for_reconstructs_owner() {
        let sb = Sandbox::new();
        let keg = sb.make_keg("foo", "1.0", &[("bin/foo", "x")]);
        let owner = keg_for(&sb.config, &keg.path.join("bin/foo")).unwrap();
        assert_eq!(owner.name, "foo");
        assert_eq!(owner.version, "1.0");
        assert!(keg_for(&sb.config, Path::new("/totally/unrelated")).is_none());
    }
}

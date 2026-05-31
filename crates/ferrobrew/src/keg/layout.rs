//! HOMEBREW_PREFIX layout constants and the per-directory link strategy rules.
//!
//! Mirrors `Keg.keg_link_directories`, `must_exist_subdirectories`, and the relative-path
//! strategy decisions baked into `Keg#link` in `Library/Homebrew/keg.rb` (and the macOS override
//! in `extend/os/mac/keg.rb`). Strategy decisions are pure functions of a keg-relative path so they
//! can be unit-tested without touching the filesystem.

use std::path::{Path, PathBuf};

use crate::config::Config;

/// What `link_dir` should do with a given entry. Mirrors the Ruby strategy symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStrategy {
    /// Symlink as-is. For a directory this means a single whole-subtree symlink ("symlink-as-dir").
    Link,
    /// Create the destination as a real directory and recurse into it (file-by-file).
    Mkpath,
    /// Do not recurse into this directory at all (bin/sbin subdirs).
    SkipDir,
    /// Skip this file entirely (e.g. `locale/locale.alias`, `charset.alias`).
    SkipFile,
    /// Symlink the file then register it with GNU `install-info`.
    Info,
}

/// `var/homebrew/linked`, relative to the prefix — the directory of symlinks tracking which keg of
/// each rack is the active (linked) one.
pub const LINKED_KEGS_REL: &str = "var/homebrew/linked";

/// The top-level directories `Keg.keg_link_directories` considers (base list).
///
/// Order is part of the contract: `link` walks them in this order, and `unlink` iterates all of
/// them (including `var`, which `link` never links — see the spec §3/§4 note).
pub const KEG_LINK_DIRECTORIES_BASE: &[&str] =
    &["bin", "etc", "include", "lib", "sbin", "share", "var"];

/// macOS appends `Frameworks` to the base link directories.
pub const FRAMEWORKS_DIR: &str = "Frameworks";

/// True when the host (or, in tests, the simulated platform) is macOS. Frameworks linking and the
/// extra `Frameworks` link directory are macOS-only.
#[inline]
pub fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

/// `Keg.keg_link_directories` for the given platform flag.
pub fn keg_link_directories(macos: bool) -> Vec<&'static str> {
    let mut dirs: Vec<&'static str> = KEG_LINK_DIRECTORIES_BASE.to_vec();
    if macos {
        dirs.push(FRAMEWORKS_DIR);
    }
    dirs
}

/// The directories `link` actually calls `link_dir` on, in the exact spec order. Note this excludes
/// `var` (scanned by `unlink` but never linked) and is `Frameworks`-suffixed only on macOS.
pub fn link_dirs(macos: bool) -> Vec<&'static str> {
    let mut dirs = vec!["etc", "bin", "sbin", "include", "share", "lib"];
    if macos {
        dirs.push(FRAMEWORKS_DIR);
    }
    dirs
}

/// `must_exist_subdirectories`: absolute prefix dirs that must always exist and that `unlink` must
/// never attempt to `rmdir`. `(keg_link_directories - [var]) + [opt, var/homebrew/linked]`,
/// sorted+uniq, plus `Frameworks` on macOS.
pub fn must_exist_subdirectories(config: &Config, macos: bool) -> Vec<PathBuf> {
    let mut rel: Vec<String> = keg_link_directories(macos)
        .into_iter()
        .filter(|d| *d != "var")
        .map(str::to_string)
        .collect();
    rel.push("opt".to_string());
    rel.push(LINKED_KEGS_REL.to_string());
    rel.sort();
    rel.dedup();
    rel.into_iter().map(|r| config.prefix.join(r)).collect()
}

/// `HOMEBREW_LINKED_KEGS` = `prefix/var/homebrew/linked`.
pub fn linked_kegs_dir(config: &Config) -> PathBuf {
    config.prefix.join(LINKED_KEGS_REL)
}

/// `HOMEBREW_LINKED_KEGS/<name>` — the symlink that marks `<name>` as linked.
pub fn linked_keg_record(config: &Config, name: &str) -> PathBuf {
    linked_kegs_dir(config).join(name)
}

/// `HOMEBREW_PREFIX/opt/<name>` — the stable opt symlink.
pub fn opt_record(config: &Config, name: &str) -> PathBuf {
    config.prefix.join("opt").join(name)
}

/// The `INFOFILE_RX` test: `info/([^.].*?\.info(\.gz)?|dir)$`, applied to a `share/`-relative path.
pub fn is_info_file(relative_under_share: &Path) -> bool {
    let s = match relative_under_share.to_str() {
        Some(s) => s,
        None => return false,
    };
    // The rest of the path after the final `info/` segment.
    let Some(idx) = find_segment(s, "info") else {
        return false;
    };
    let rest = &s[idx..];
    if rest == "dir" {
        return true;
    }
    // `[^.].*?\.info(\.gz)?$`: at least one leading non-dot char, ends in `.info` or `.info.gz`,
    // and contains no `/` (it must be the final path component matched by the `$`).
    if rest.contains('/') {
        return false;
    }
    if rest.starts_with('.') {
        return false;
    }
    rest.ends_with(".info") || rest.ends_with(".info.gz")
}

/// Find a `name/` segment and return the byte offset of the text following it, matching Ruby's
/// `%r{info/(...)$}` anchoring where `info/` may appear anywhere in the path.
fn find_segment(s: &str, name: &str) -> Option<usize> {
    let needle = format!("{name}/");
    if let Some(stripped) = s.strip_prefix(&needle) {
        return Some(s.len() - stripped.len());
    }
    let pat = format!("/{needle}");
    s.find(&pat).map(|i| i + pat.len())
}

/// `LOCALEDIR_RX`: a `locale/` or `man/` segment followed by a locale-style component
/// `lang[_TERR][.codeset][@mod]`. We approximate Ruby's unanchored regex match.
fn matches_localedir(s: &str) -> bool {
    for prefix in ["locale", "man"] {
        if let Some(off) = find_segment(s, prefix) {
            let rest = &s[off..];
            let comp = rest.split('/').next().unwrap_or("");
            if is_locale_component(comp) {
                return true;
            }
        }
    }
    false
}

/// `([a-z]{2}|C|POSIX)(_[A-Z]{2})?(\.[a-zA-Z\-0-9]+(@.+)?)?` — the language part of `LOCALEDIR_RX`.
fn is_locale_component(comp: &str) -> bool {
    // Strip optional `@modifier`.
    let (head, modifier) = match comp.split_once('@') {
        Some((h, m)) => (h, Some(m)),
        None => (comp, None),
    };
    if let Some(m) = modifier {
        if m.is_empty() {
            return false;
        }
    }
    // Strip optional `.codeset`.
    let (lang_terr, codeset) = match head.split_once('.') {
        Some((lt, cs)) => (lt, Some(cs)),
        None => (head, None),
    };
    if let Some(cs) = codeset {
        if cs.is_empty() || !cs.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return false;
        }
    }
    // `lang[_TERR]`.
    let (lang, terr) = match lang_terr.split_once('_') {
        Some((l, t)) => (l, Some(t)),
        None => (lang_terr, None),
    };
    let lang_ok = lang == "C"
        || lang == "POSIX"
        || (lang.len() == 2 && lang.chars().all(|c| c.is_ascii_lowercase()));
    if !lang_ok {
        return false;
    }
    if let Some(t) = terr {
        if !(t.len() == 2 && t.chars().all(|c| c.is_ascii_uppercase())) {
            return false;
        }
    }
    true
}

/// `^postgresql@\d+` test against a relative path's first component.
fn starts_with_postgresql_versioned(s: &str) -> bool {
    let first = s.split('/').next().unwrap_or("");
    if let Some(num) = first.strip_prefix("postgresql@") {
        !num.is_empty() && num.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

/// `^python[23]\.\d+` test against a relative path's first component.
fn starts_with_python_versioned(s: &str) -> bool {
    let first = s.split('/').next().unwrap_or("");
    let rest = match first.strip_prefix("python") {
        Some(r) => r,
        None => return false,
    };
    let mut chars = rest.chars();
    match chars.next() {
        Some('2') | Some('3') => {}
        _ => return false,
    }
    let tail: String = chars.collect();
    let dotted = match tail.strip_prefix('.') {
        Some(d) => d,
        None => return false,
    };
    !dotted.is_empty() && dotted.chars().all(|c| c.is_ascii_digit())
}

/// True iff the path's first component starts with `prefix` (Ruby `/^prefix/`).
fn first_component_starts_with(s: &str, prefix: &str) -> bool {
    s.split('/').next().unwrap_or("").starts_with(prefix)
}

/// `SHARE_PATHS`: directories under `share/` that are always real dirs, never symlinks.
const SHARE_PATHS: &[&str] = &[
    "aclocal",
    "cps",
    "doc",
    "info",
    "java",
    "locale",
    "man",
    "man/man1",
    "man/man2",
    "man/man3",
    "man/man4",
    "man/man5",
    "man/man6",
    "man/man7",
    "man/man8",
    "man/cat1",
    "man/cat2",
    "man/cat3",
    "man/cat4",
    "man/cat5",
    "man/cat6",
    "man/cat7",
    "man/cat8",
    "applications",
    "gnome",
    "gnome/help",
    "icons",
    "mime-info",
    "pixmaps",
    "sounds",
    "postgresql",
];

/// Strategy for a path relative to `etc/`. Always `:mkpath` (real dirs; files symlinked within).
pub fn strategy_etc(_relative: &Path) -> LinkStrategy {
    LinkStrategy::Mkpath
}

/// Strategy for a path relative to `bin/` (and `sbin/`). Always `:skip_dir`.
pub fn strategy_bin(_relative: &Path) -> LinkStrategy {
    LinkStrategy::SkipDir
}

/// Strategy for a path relative to `include/`: `:mkpath` for `postgresql@N`, else `:link`.
pub fn strategy_include(relative: &Path) -> LinkStrategy {
    let s = relative.to_str().unwrap_or("");
    if starts_with_postgresql_versioned(s) {
        LinkStrategy::Mkpath
    } else {
        LinkStrategy::Link
    }
}

/// Strategy for a path relative to `share/`.
pub fn strategy_share(relative: &Path) -> LinkStrategy {
    if is_info_file(relative) {
        return LinkStrategy::Info;
    }
    let s = relative.to_str().unwrap_or("");
    if s == "locale/locale.alias" || matches_icon_theme_cache(s) {
        return LinkStrategy::SkipFile;
    }
    if matches_localedir(s)
        || first_component_starts_with(s, "icons")
        || first_component_starts_with(s, "zsh")
        || first_component_starts_with(s, "fish")
        || first_component_starts_with(s, "lua")
        || first_component_starts_with(s, "guile")
        || starts_with_postgresql_versioned(s)
        || first_component_starts_with(s, "pypy")
        || SHARE_PATHS.contains(&s)
    {
        return LinkStrategy::Mkpath;
    }
    LinkStrategy::Link
}

/// `^icons/.*/icon-theme\.cache$`.
fn matches_icon_theme_cache(s: &str) -> bool {
    s.starts_with("icons/") && s.ends_with("/icon-theme.cache")
}

/// Strategy for a path relative to `lib/`.
pub fn strategy_lib(relative: &Path) -> LinkStrategy {
    let s = relative.to_str().unwrap_or("");
    if s == "charset.alias" {
        return LinkStrategy::SkipFile;
    }
    const EXACT_MKPATH: &[&str] = &["cps", "pkgconfig", "cmake", "dtrace", "ghc", "php"];
    if EXACT_MKPATH.contains(&s)
        || first_component_starts_with(s, "gdk-pixbuf")
        || first_component_starts_with(s, "gio")
        || first_component_starts_with(s, "lua")
        || first_component_starts_with(s, "mecab")
        || first_component_starts_with(s, "node")
        || first_component_starts_with(s, "ocaml")
        || first_component_starts_with(s, "perl5")
        || starts_with_postgresql_versioned(s)
        || first_component_starts_with(s, "pypy")
        || starts_with_python_versioned(s)
        || first_component_starts_with(s, "R")
        || first_component_starts_with(s, "ruby")
    {
        return LinkStrategy::Mkpath;
    }
    LinkStrategy::Link
}

/// Strategy for a path relative to `Frameworks/` (macOS): `:mkpath` for `Foo.framework` and
/// `Foo.framework/Versions`, else `:link`.
pub fn strategy_frameworks(relative: &Path) -> LinkStrategy {
    let s = relative.to_str().unwrap_or("");
    // `[^/]*\.framework(/Versions)?$`
    let is_framework = |seg: &str| seg.ends_with(".framework");
    let matches = if let Some(stripped) = s.strip_suffix("/Versions") {
        !stripped.contains('/') && is_framework(stripped)
    } else {
        !s.contains('/') && is_framework(s)
    };
    if matches {
        LinkStrategy::Mkpath
    } else {
        LinkStrategy::Link
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn cfg(prefix: &str) -> Config {
        Config {
            prefix: PathBuf::from(prefix),
            repository: PathBuf::from(prefix),
            cellar: PathBuf::from(prefix).join("Cellar"),
            caskroom: PathBuf::from(prefix).join("Caskroom"),
            cache: PathBuf::from("/cache"),
            library: PathBuf::from(prefix).join("Library"),
        }
    }

    #[test]
    fn must_exist_subdirectories_macos_set() {
        let c = cfg("/opt/homebrew");
        let dirs = must_exist_subdirectories(&c, true);
        let names: Vec<String> = dirs
            .iter()
            .map(|p| {
                p.strip_prefix(&c.prefix)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "Frameworks",
                "bin",
                "etc",
                "include",
                "lib",
                "opt",
                "sbin",
                "share",
                "var/homebrew/linked",
            ]
        );
    }

    #[test]
    fn must_exist_subdirectories_linux_has_no_frameworks() {
        let c = cfg("/home/linuxbrew/.linuxbrew");
        let dirs = must_exist_subdirectories(&c, false);
        assert!(!dirs.iter().any(|p| p.ends_with("Frameworks")));
        assert!(dirs.iter().any(|p| p.ends_with("var/homebrew/linked")));
    }

    #[test]
    fn link_dirs_order_and_var_excluded() {
        assert_eq!(
            link_dirs(false),
            vec!["etc", "bin", "sbin", "include", "share", "lib"]
        );
        assert_eq!(
            link_dirs(true),
            vec![
                "etc",
                "bin",
                "sbin",
                "include",
                "share",
                "lib",
                "Frameworks"
            ]
        );
        assert!(!link_dirs(true).contains(&"var"));
    }

    #[test]
    fn unlink_iterates_var() {
        assert!(keg_link_directories(false).contains(&"var"));
    }

    #[test]
    fn include_postgresql_is_mkpath() {
        assert_eq!(
            strategy_include(Path::new("postgresql@14/foo.h")),
            LinkStrategy::Mkpath
        );
        assert_eq!(
            strategy_include(Path::new("openssl/ssl.h")),
            LinkStrategy::Link
        );
    }

    #[test]
    fn lib_exact_and_prefixed_mkpath() {
        // EXACT_MKPATH dirs mkpath only the dir itself; a child path links (so e.g. `lib/cmake/Foo`
        // becomes a single dir symlink), matching Homebrew's exact-string `case`/`when`.
        assert_eq!(strategy_lib(Path::new("pkgconfig")), LinkStrategy::Mkpath);
        assert_eq!(
            strategy_lib(Path::new("pkgconfig/foo.pc")),
            LinkStrategy::Link
        );
        assert_eq!(
            strategy_lib(Path::new("python3.12/site.py")),
            LinkStrategy::Mkpath
        );
        assert_eq!(strategy_lib(Path::new("ruby/gems")), LinkStrategy::Mkpath);
        assert_eq!(
            strategy_lib(Path::new("charset.alias")),
            LinkStrategy::SkipFile
        );
        assert_eq!(strategy_lib(Path::new("libfoo.dylib")), LinkStrategy::Link);
        // `cmakefoo` must NOT match the exact `cmake` rule.
        assert_eq!(strategy_lib(Path::new("cmakefoo/x")), LinkStrategy::Link);
    }

    #[test]
    fn share_info_locale_and_default() {
        assert_eq!(
            strategy_share(Path::new("info/foo.info")),
            LinkStrategy::Info
        );
        assert_eq!(
            strategy_share(Path::new("info/foo.info.gz")),
            LinkStrategy::Info
        );
        assert_eq!(strategy_share(Path::new("info/dir")), LinkStrategy::Info);
        assert_eq!(
            strategy_share(Path::new("locale/locale.alias")),
            LinkStrategy::SkipFile
        );
        assert_eq!(
            strategy_share(Path::new("icons/hicolor/icon-theme.cache")),
            LinkStrategy::SkipFile
        );
        // `man/man1` is a SHARE_PATH directory -> :mkpath (real dir). The leaf file beneath it
        // matches no rule and is :link, so it lands as a per-file symlink inside the real dir.
        assert_eq!(strategy_share(Path::new("man/man1")), LinkStrategy::Mkpath);
        assert_eq!(
            strategy_share(Path::new("man/man1/foo.1")),
            LinkStrategy::Link
        );
        assert_eq!(
            strategy_share(Path::new("locale/fr/LC_MESSAGES/x.mo")),
            LinkStrategy::Mkpath
        );
        assert_eq!(
            strategy_share(Path::new("zsh/site-functions/_foo")),
            LinkStrategy::Mkpath
        );
        assert_eq!(strategy_share(Path::new("doc")), LinkStrategy::Mkpath);
        assert_eq!(strategy_share(Path::new("myapp/data")), LinkStrategy::Link);
    }

    #[test]
    fn dotfile_info_is_not_infofile() {
        // `[^.]` requires a leading non-dot char.
        assert_ne!(
            strategy_share(Path::new("info/.hidden.info")),
            LinkStrategy::Info
        );
    }

    #[test]
    fn localedir_component_matching() {
        assert!(is_locale_component("fr"));
        assert!(is_locale_component("en_US"));
        assert!(is_locale_component("en_US.UTF-8"));
        assert!(is_locale_component("C"));
        assert!(is_locale_component("POSIX"));
        assert!(is_locale_component("sr@latin"));
        assert!(!is_locale_component("english"));
        assert!(!is_locale_component("e"));
    }

    #[test]
    fn frameworks_strategy() {
        assert_eq!(
            strategy_frameworks(Path::new("Foo.framework")),
            LinkStrategy::Mkpath
        );
        assert_eq!(
            strategy_frameworks(Path::new("Foo.framework/Versions")),
            LinkStrategy::Mkpath
        );
        assert_eq!(
            strategy_frameworks(Path::new("Foo.framework/Versions/A/Foo")),
            LinkStrategy::Link
        );
    }
}

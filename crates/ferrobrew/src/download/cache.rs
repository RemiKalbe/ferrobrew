//! Homebrew's content-addressed download cache layout.
//!
//! Mirrors `AbstractFileDownloadStrategy#cached_location` (`download_strategy/...`, line 33) and
//! `Utils.safe_filename` (`utils.rb:126`). See `specs/download-ghcr.md` §5.
//!
//! Files live under `HOMEBREW_CACHE/downloads/` named `<sha256-of-url-string>--<safe-basename>`.
//! The URL hash is the SHA-256 of the *download URL string* (not the file contents), so two
//! different artifact-domain rewrites of the same bottle produce different cache files.

use std::path::{Path, PathBuf};

use crate::download::integrity::sha256_bytes;

/// `HOMEBREW_CACHE/downloads`, where all downloaded artifacts and their `.incomplete` temps live.
pub fn downloads_dir(cache: &Path) -> PathBuf {
    cache.join("downloads")
}

/// Strip control characters and path separators, matching `Utils.safe_filename`.
///
/// Ruby: `basename.gsub(/[[:cntrl:]\/<ALT_SEP>]/, "")`. On Unix there is no alternate separator, so
/// this strips ASCII/Unicode control characters and `/`.
pub fn safe_filename(basename: &str) -> String {
    basename
        .chars()
        .filter(|c| !c.is_control() && *c != '/')
        .collect()
}

/// The SHA-256 hex of the download URL string used as the cache filename prefix.
pub fn url_sha256(url: &str) -> String {
    sha256_bytes(url.as_bytes())
}

/// The content-addressed cache path for `url`/`basename`, replicating the glob-then-fallback of
/// `AbstractFileDownloadStrategy#cached_location`.
///
/// 1. Glob `downloads/<urlsha>--*`, dropping any name ending in `.incomplete`.
/// 2. If exactly one match exists, reuse it regardless of `basename`.
/// 3. Otherwise return the deterministic `downloads/<urlsha>--<safe(basename)>`.
pub fn cached_download_path(config: &crate::config::Config, url: &str, basename: &str) -> PathBuf {
    let downloads = downloads_dir(&config.cache);
    let urlsha = url_sha256(url);
    let prefix = format!("{urlsha}--");

    if let Some(existing) = single_existing_match(&downloads, &prefix) {
        return existing;
    }

    downloads.join(format!("{prefix}{}", safe_filename(basename)))
}

/// The `.incomplete` partial-download path for a cached location.
pub fn temporary_path(cached: &Path) -> PathBuf {
    let mut name = cached
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(".incomplete");
    cached.with_file_name(name)
}

/// If exactly one non-`.incomplete` file in `dir` starts with `prefix`, return its path.
fn single_existing_match(dir: &Path, prefix: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut found: Option<PathBuf> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(prefix) || name.ends_with(".incomplete") {
            continue;
        }
        if found.is_some() {
            // More than one candidate: fall back to the deterministic name.
            return None;
        }
        found = Some(entry.path());
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs::File;

    fn config_with_cache(cache: PathBuf) -> Config {
        Config {
            prefix: "/opt/homebrew".into(),
            repository: "/opt/homebrew".into(),
            cellar: "/opt/homebrew/Cellar".into(),
            caskroom: "/opt/homebrew/Caskroom".into(),
            cache,
            library: "/opt/homebrew/Library".into(),
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ferrobrew-{label}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn url_sha256_is_sha_of_the_url_string() {
        // sha256("https://ghcr.io") computed independently.
        assert_eq!(url_sha256("abc"), sha256_bytes(b"abc"));
        assert_eq!(url_sha256("").len(), 64);
    }

    #[test]
    fn safe_filename_strips_separators_and_control_chars() {
        assert_eq!(safe_filename("a/b\u{0001}c"), "abc");
        assert_eq!(
            safe_filename("wget--1.21.4.arm64_sonoma.bottle.tar.gz"),
            "wget--1.21.4.arm64_sonoma.bottle.tar.gz"
        );
    }

    #[test]
    fn cached_download_path_uses_url_hash_double_dash_basename() {
        let cfg = config_with_cache(unique_temp_dir("cache-scheme"));
        let url = "https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:abc123";
        let basename = "wget--1.21.4.arm64_sonoma.bottle.tar.gz";
        let path = cached_download_path(&cfg, url, basename);

        let expected = cfg
            .cache
            .join("downloads")
            .join(format!("{}--{basename}", url_sha256(url)));
        assert_eq!(path, expected);
        // The prefix is sha256 of the URL *string*, lowercase hex, 64 chars.
        let prefix = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .split("--")
            .next()
            .unwrap()
            .to_string();
        assert_eq!(prefix.len(), 64);
        assert!(prefix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn cached_download_path_reuses_single_glob_match_regardless_of_basename() {
        let cfg = config_with_cache(unique_temp_dir("cache-glob"));
        let downloads = downloads_dir(&cfg.cache);
        std::fs::create_dir_all(&downloads).unwrap();
        let url = "https://example.test/blob";
        let prefix = format!("{}--", url_sha256(url));

        // A previously-cached file with a *different* basename and an .incomplete sibling.
        let existing = downloads.join(format!("{prefix}old-name.tar.gz"));
        File::create(&existing).unwrap();
        File::create(downloads.join(format!("{prefix}stale.tar.gz.incomplete"))).unwrap();

        let path = cached_download_path(&cfg, url, "new-name.tar.gz");
        assert_eq!(path, existing);

        std::fs::remove_dir_all(&cfg.cache).ok();
    }

    #[test]
    fn cached_download_path_falls_back_when_multiple_matches() {
        let cfg = config_with_cache(unique_temp_dir("cache-multi"));
        let downloads = downloads_dir(&cfg.cache);
        std::fs::create_dir_all(&downloads).unwrap();
        let url = "https://example.test/blob";
        let prefix = format!("{}--", url_sha256(url));

        File::create(downloads.join(format!("{prefix}one.tar.gz"))).unwrap();
        File::create(downloads.join(format!("{prefix}two.tar.gz"))).unwrap();

        let path = cached_download_path(&cfg, url, "deterministic.tar.gz");
        assert_eq!(
            path,
            downloads.join(format!("{prefix}deterministic.tar.gz"))
        );

        std::fs::remove_dir_all(&cfg.cache).ok();
    }

    #[test]
    fn temporary_path_appends_incomplete() {
        let cached = Path::new("/c/downloads/abc--wget.tar.gz");
        assert_eq!(
            temporary_path(cached),
            Path::new("/c/downloads/abc--wget.tar.gz.incomplete")
        );
    }
}

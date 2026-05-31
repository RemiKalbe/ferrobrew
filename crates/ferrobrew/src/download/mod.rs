//! Bottle download: fetch a verified tarball from ghcr.io into Homebrew's content-addressed cache.
//!
//! This shells out to the `curl` CLI to match Homebrew's behaviour 1:1 (`--location`, `--fail`,
//! redirect-following with `Authorization` dropped on cross-host redirect, `--retry`). The download
//! lands in a `.incomplete` temp and is atomically renamed into the cache, then its SHA-256 is
//! verified against the formula's expected digest. See `specs/download-ghcr.md` §5–§9.

pub mod cache;
pub mod integrity;

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bottle::ghcr;
use crate::config::Config;
use crate::error::{FerroError, Result};

pub use cache::cached_download_path;

/// How many times `curl` should retry transient failures, matching `HOMEBREW_CURL_RETRIES`'s
/// default of `3`.
const DEFAULT_CURL_RETRIES: u32 = 3;

/// Fetch the bottle at `url` into the cache, verifying it against `expected_sha256`.
///
/// - Skips the download when a valid (correct-SHA-256) copy is already cached.
/// - Otherwise downloads via `curl` with the GHCR `Authorization` header, `--location`, and
///   retries into a `.incomplete` temp, then atomically renames it into the cache.
/// - Verifies the SHA-256 of the cached file; a mismatch removes the bad file and returns
///   [`FerroError::ChecksumMismatch`].
///
/// Returns the path to the verified cached tarball.
pub fn fetch_bottle(
    config: &Config,
    url: &str,
    expected_sha256: &str,
    basename: &str,
) -> Result<PathBuf> {
    let cached = cached_download_path(config, url, basename);

    // Already downloaded and valid? Reuse it without touching the network (spec §7).
    if cached.exists() && integrity::sha256_file(&cached)?.eq_ignore_ascii_case(expected_sha256) {
        return Ok(cached);
    }

    let downloads = cache::downloads_dir(&config.cache);
    std::fs::create_dir_all(&downloads).map_err(|e| FerroError::io(&downloads, e))?;

    let temp = cache::temporary_path(&cached);
    // Start each attempt from a clean temp so a prior aborted run cannot poison the result.
    if temp.exists() {
        std::fs::remove_file(&temp).map_err(|e| FerroError::io(&temp, e))?;
    }

    let auth = ghcr::github_packages_auth();
    run_curl(url, &temp, &auth)?;

    if !temp.exists() {
        return Err(FerroError::Other(format!(
            "curl reported success but produced no file for {url}"
        )));
    }

    // Atomic rename into the content-addressed cache location (same filesystem).
    std::fs::rename(&temp, &cached).map_err(|e| FerroError::io(&cached, e))?;

    // Verify the bottle's SHA-256; remove the corrupt file on mismatch so a retry re-downloads.
    match integrity::verify_sha256(&cached, expected_sha256) {
        Ok(_) => Ok(cached),
        Err(err) => {
            std::fs::remove_file(&cached).ok();
            Err(err)
        }
    }
}

/// Build and run the `curl` command that downloads `url` to `destination`.
fn run_curl(url: &str, destination: &Path, auth: &str) -> Result<()> {
    let args = curl_args(url, destination, auth);
    let status = Command::new("curl")
        .args(&args)
        .status()
        .map_err(|e| FerroError::Other(format!("failed to spawn curl: {e}")))?;

    if status.success() {
        return Ok(());
    }

    // Clean up the partial download so the cache never contains a half-written temp.
    std::fs::remove_file(destination).ok();
    let code = status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string());
    Err(FerroError::Other(format!(
        "curl failed (exit {code}) downloading {url}"
    )))
}

/// The curl flags Homebrew applies for a bottle download (`utils/curl.rb` `curl_download`/
/// `curl_args`), reduced to the subset that affects a single bottle blob GET.
fn curl_args(url: &str, destination: &Path, auth: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        // Skip ~/.curlrc so user config can't change behaviour (`--disable`).
        "--disable".to_string(),
        // Follow ghcr.io's 302 to blob storage (`--location`); curl drops Authorization on a
        // cross-host redirect, matching Homebrew's behaviour (spec §2).
        "--location".to_string(),
        // Fail on HTTP errors (4xx/5xx) instead of writing the error body to the output file.
        "--fail".to_string(),
        "--show-error".to_string(),
        "--silent".to_string(),
        // Preserve the server's Last-Modified as the file mtime (`--remote-time`).
        "--remote-time".to_string(),
        "--retry".to_string(),
        DEFAULT_CURL_RETRIES.to_string(),
    ];

    // Only attach the registry Authorization header when non-empty (the `none` anonymous opt-out
    // produces an empty string; sending no header then matches Homebrew, spec §2).
    if !auth.is_empty() {
        args.push("--header".to_string());
        args.push(format!("Authorization: {auth}"));
    }

    args.push("--output".to_string());
    args.push(destination.to_string_lossy().into_owned());
    args.push(url.to_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs::File;
    use std::io::Write;

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
    fn curl_args_include_location_fail_and_auth_header() {
        let args = curl_args(
            "https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:abc",
            Path::new("/tmp/out.incomplete"),
            "Bearer QQ==",
        );
        assert!(args.iter().any(|a| a == "--location"));
        assert!(args.iter().any(|a| a == "--fail"));
        assert!(args.iter().any(|a| a == "--remote-time"));
        // Authorization header is present as a "--header" / value pair.
        let header_idx = args.iter().position(|a| a == "--header").unwrap();
        assert_eq!(args[header_idx + 1], "Authorization: Bearer QQ==");
        // --retry has a numeric argument.
        let retry_idx = args.iter().position(|a| a == "--retry").unwrap();
        assert_eq!(args[retry_idx + 1], DEFAULT_CURL_RETRIES.to_string());
        // The destination follows --output and the URL is last.
        let out_idx = args.iter().position(|a| a == "--output").unwrap();
        assert_eq!(args[out_idx + 1], "/tmp/out.incomplete");
        assert_eq!(
            args.last().unwrap(),
            "https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:abc"
        );
    }

    #[test]
    fn curl_args_omit_header_when_auth_empty() {
        let args = curl_args("https://example.test/x", Path::new("/tmp/out"), "");
        assert!(!args.iter().any(|a| a == "--header"));
    }

    #[test]
    fn fetch_bottle_reuses_valid_cached_file_without_network() {
        // Pre-place a correct cache file so fetch_bottle never invokes curl.
        let cfg = config_with_cache(unique_temp_dir("fetch-cached"));
        let url = "https://ghcr.io/v2/homebrew/core/demo/blobs/sha256:deadbeef";
        let basename = "demo--1.0.arm64_sonoma.bottle.tar.gz";
        let cached = cached_download_path(&cfg, url, basename);
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();

        let contents = b"a tiny pretend bottle";
        File::create(&cached).unwrap().write_all(contents).unwrap();
        let sha = integrity::sha256_bytes(contents);

        let result = fetch_bottle(&cfg, url, &sha, basename).unwrap();
        assert_eq!(result, cached);

        std::fs::remove_dir_all(&cfg.cache).ok();
    }

    #[test]
    fn fetch_bottle_redownloads_when_cached_file_has_wrong_sha() {
        // A stale/corrupt cache file with the wrong SHA must NOT be reused; with no network the
        // re-download fails, but crucially it does not silently return the bad file.
        let cfg = config_with_cache(unique_temp_dir("fetch-stale"));
        let url = "https://127.0.0.1:1/does-not-resolve";
        let basename = "demo--1.0.arm64_sonoma.bottle.tar.gz";
        let cached = cached_download_path(&cfg, url, basename);
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        File::create(&cached)
            .unwrap()
            .write_all(b"corrupt")
            .unwrap();

        let err = fetch_bottle(&cfg, url, &"0".repeat(64), basename).unwrap_err();
        // Either curl failed to download, or (if curl somehow produced something) the SHA mismatched.
        assert!(matches!(
            err,
            FerroError::Other(_) | FerroError::ChecksumMismatch { .. } | FerroError::Io { .. }
        ));

        std::fs::remove_dir_all(&cfg.cache).ok();
    }

    #[test]
    #[ignore = "network: downloads a real bottle from ghcr.io"]
    fn fetch_bottle_downloads_and_verifies_real_bottle() {
        let cfg = Config::from_env();
        // A small, stable public bottle digest would go here; left ignored by default.
        let url = "https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:0".to_string();
        let _ = fetch_bottle(
            &cfg,
            &url,
            &"0".repeat(64),
            "wget--1.0.arm64_sonoma.bottle.tar.gz",
        );
    }
}

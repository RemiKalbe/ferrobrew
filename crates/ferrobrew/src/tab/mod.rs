//! `INSTALL_RECEIPT.json` — Homebrew's per-keg install receipt (the "Tab").
//!
//! One receipt is written per installed keg version at `<keg>/INSTALL_RECEIPT.json`, where
//! `<keg>` = `HOMEBREW_CELLAR/<name>/<pkg_version>`. This module reproduces, byte-for-byte, the
//! on-disk JSON that Homebrew's `Tab#to_json` emits for the case ferrobrew implements: a bottle
//! poured from a formula loaded via the JSON API.
//!
//! The full schema, field order, and serialization rules are specified in `specs/receipt-tab.md`.
//! Key facts this module relies on:
//!
//! - Serializer is Ruby's `JSON.pretty_generate`: 2-space indent, `null` for nil, empty array as
//!   `[]`, empty object as `{}`, forward slashes unescaped, and **no trailing newline**.
//!   `serde_json::to_string_pretty` is byte-identical (verified), so we use it directly.
//! - Top-level key order follows `Tab#to_json` (`Library/Homebrew/tab/tab.rb:357`) exactly. Struct
//!   field declaration order below mirrors it.
//! - `stdlib` is the only conditionally-dropped top-level key (omitted when blank/absent). All
//!   other nullable keys (`changed_files`, `time`, `aliases`, `runtime_dependencies`, `arch`,
//!   `built_on`) emit literal `null` when absent — they are NOT skipped.
//! - For a poured-from-API bottle the `source` object's key order is `spec, versions, path,
//!   tap_git_head, tap` (confirmed against real `homebrew/core` receipts), which the typed
//!   [`Source`] struct reproduces.
//! - `homebrew_version` is carried through from the bottle's embedded tab and is NOT overwritten at
//!   pour time; `time` IS reset to the install time (passed in, never read from the clock here).
//! - Current Homebrew writes no `installed_as_dependency` key; "as dependency" is derived as
//!   `!installed_on_request`.

mod built_on;
mod runtime_deps;

pub use built_on::BuiltOn;
pub use runtime_deps::RuntimeDep;

use std::path::Path;

use serde::Serialize;

use crate::error::FerroError;
use crate::Result;

/// Filename of the receipt within a keg. Mirrors `AbstractTab::FILENAME`.
pub const FILENAME: &str = "INSTALL_RECEIPT.json";

/// The `source.versions` object. Field order = on-disk key order:
/// `stable, head, version_scheme, compatibility_version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceVersions {
    /// `formula.stable.version.to_s`.
    pub stable: Option<String>,
    /// `formula.head.version.to_s` — `null` for stable-only formulae.
    pub head: Option<String>,
    /// `formula.version_scheme`, defaults to `0`.
    pub version_scheme: i64,
    /// `formula.compatibility_version` — usually `null`.
    pub compatibility_version: Option<i64>,
}

impl Default for SourceVersions {
    /// `empty_source_versions`: `{stable: nil, head: nil, version_scheme: 0,
    /// compatibility_version: nil}`.
    fn default() -> Self {
        SourceVersions {
            stable: None,
            head: None,
            version_scheme: 0,
            compatibility_version: None,
        }
    }
}

/// The `source` object. Field order = the on-disk key order produced for a fresh poured-from-API
/// bottle install: `spec, versions, path, tap_git_head, tap`.
///
/// `scm_revision` is only present for git/hg HEAD source builds with a cached download; it is
/// absent for bottle pours and so is omitted when `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Source {
    /// `formula.active_spec_sym.to_s`: `"stable"` or `"head"`.
    pub spec: String,
    /// Nested version info.
    pub versions: SourceVersions,
    /// `formula.specified_path.to_s`. For an API formula this is
    /// `HOMEBREW_CACHE/api/formula.jws.json`.
    pub path: String,
    /// Only emitted for git/hg HEAD specs with a cached download; omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scm_revision: Option<String>,
    /// Tap HEAD sha, or `null` when the tap is not installed locally (the common API case).
    pub tap_git_head: Option<String>,
    /// Tap name, e.g. `"homebrew/core"`, or `null`.
    pub tap: Option<String>,
}

/// A single formula install receipt, serialized to `<keg>/INSTALL_RECEIPT.json`.
///
/// Field declaration order is the exact on-disk top-level key order from `Tab#to_json`
/// (`specs/receipt-tab.md` §2). Construct one with [`InstallReceipt::for_poured_bottle`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallReceipt {
    /// The brew version that wrote the bottle (carried through from the embedded tab; NOT the
    /// installing brew's version).
    pub homebrew_version: String,
    /// Build option flags — always `[]` for a bottle pour.
    pub used_options: Vec<String>,
    /// Unused build option flags — always `[]` for a bottle pour.
    pub unused_options: Vec<String>,
    /// `true` when poured from a bottle.
    pub built_as_bottle: bool,
    /// `true` for a bottle pour.
    pub poured_from_bottle: bool,
    /// `true` when the formula definition came from the JSON API.
    pub loaded_from_api: bool,
    /// `true` when loaded from the internal API; typically `false`.
    pub loaded_from_internal_api: bool,
    /// Whether the user explicitly requested this formula. "As dependency" is `!this`.
    pub installed_on_request: bool,
    /// Keg-relative paths that contained relocation placeholders (from the embedded tab). Emits
    /// `null` when absent, `[]` when empty.
    pub changed_files: Option<Vec<String>>,
    /// Unix epoch seconds at install time. Emits `null` when absent.
    pub time: Option<i64>,
    /// Unix epoch seconds the source was last modified (from the embedded tab); `0` when unknown.
    pub source_modified_time: i64,
    /// C++ stdlib (`libcxx`/`libstdcxx`). Omitted entirely when absent/blank (the modern case).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdlib: Option<String>,
    /// Compiler, e.g. `"clang"` on macOS or `"gcc"` on Linux.
    pub compiler: String,
    /// Alias names this formula is known by. Emits `null` when absent, `[]` when empty.
    pub aliases: Option<Vec<String>>,
    /// Resolved runtime dependencies. Emits `null` when absent, `[]` when empty.
    pub runtime_dependencies: Option<Vec<RuntimeDep>>,
    /// The `source` object.
    pub source: Source,
    /// The installing machine's CPU arch, e.g. `"arm64"`. Emits `null` when absent.
    pub arch: Option<String>,
    /// Build-system info from the bottle's embedded tab. Emits `null` when absent.
    pub built_on: Option<BuiltOn>,
}

/// Everything a poured-from-API bottle install knows when writing the receipt.
///
/// Split out from the install-time clock value (passed separately to
/// [`InstallReceipt::for_poured_bottle`]) so callers and tests stay deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PourParams {
    /// The brew version carried through from the bottle's embedded tab.
    pub homebrew_version: String,
    /// Whether the user requested this formula directly (vs pulled in as a dependency).
    pub installed_on_request: bool,
    /// Keg-relative relocated-file paths from the embedded tab (`None`/`Some(vec![])` preserved).
    pub changed_files: Option<Vec<String>>,
    /// Source-modification time from the embedded tab (`0` when unknown).
    pub source_modified_time: i64,
    /// C++ stdlib from the embedded tab, if any (blank/`None` omits the key).
    pub stdlib: Option<String>,
    /// Compiler string from the embedded tab.
    pub compiler: String,
    /// Alias names for this formula (`Some(vec![])` is common).
    pub aliases: Option<Vec<String>>,
    /// Locally-resolved runtime dependencies.
    pub runtime_dependencies: Option<Vec<RuntimeDep>>,
    /// `source.spec`: `"stable"` or `"head"`.
    pub spec: String,
    /// `source.versions`.
    pub versions: SourceVersions,
    /// `source.path`: for an API formula, `HOMEBREW_CACHE/api/formula.jws.json`.
    pub source_path: String,
    /// `source.tap_git_head`: tap HEAD sha, or `None` when the tap is not installed locally.
    pub tap_git_head: Option<String>,
    /// `source.tap`: tap name, e.g. `"homebrew/core"`.
    pub tap: Option<String>,
    /// The installing machine's CPU arch, e.g. `"arm64"`.
    pub arch: Option<String>,
    /// Build-system info carried through from the embedded bottle tab.
    pub built_on: Option<BuiltOn>,
}

impl InstallReceipt {
    /// Build the receipt for a bottle poured from a formula loaded via the JSON API.
    ///
    /// `time` is the Unix-epoch install timestamp; it is taken as a parameter (never read from the
    /// process clock) so callers control it and tests are deterministic. It matches Homebrew's
    /// `tab.time = Time.now.to_i` set freshly at pour time.
    ///
    /// All bottle-only fields are fixed to their pour-path values: `used_options`/`unused_options`
    /// are empty, `built_as_bottle`/`poured_from_bottle`/`loaded_from_api` are `true`, and
    /// `loaded_from_internal_api` is `false`. Everything else comes from `params`.
    pub fn for_poured_bottle(params: PourParams, time: i64) -> Self {
        InstallReceipt {
            homebrew_version: params.homebrew_version,
            used_options: Vec::new(),
            unused_options: Vec::new(),
            built_as_bottle: true,
            poured_from_bottle: true,
            loaded_from_api: true,
            loaded_from_internal_api: false,
            installed_on_request: params.installed_on_request,
            changed_files: params.changed_files,
            time: Some(time),
            source_modified_time: params.source_modified_time,
            stdlib: params.stdlib.filter(|s| !s.is_empty()),
            compiler: params.compiler,
            aliases: params.aliases,
            runtime_dependencies: params.runtime_dependencies,
            source: Source {
                spec: params.spec,
                versions: params.versions,
                path: params.source_path,
                scm_revision: None,
                tap_git_head: params.tap_git_head,
                tap: params.tap,
            },
            arch: params.arch,
            built_on: params.built_on,
        }
    }

    /// Serialize to the exact on-disk JSON form Homebrew produces (no trailing newline).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self)
            .expect("InstallReceipt serializes; it contains no non-string map keys or NaN")
    }

    /// Write the receipt to `<keg_dir>/INSTALL_RECEIPT.json`.
    ///
    /// The byte content is exactly [`InstallReceipt::to_json`] — 2-space-indented JSON in the
    /// schema's key order, with no trailing newline, matching Homebrew's `Tab#write`.
    pub fn write(&self, keg_dir: &Path) -> Result<()> {
        let path = keg_dir.join(FILENAME);
        std::fs::write(&path, self.to_json()).map_err(|source| FerroError::io(path, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_params() -> PourParams {
        PourParams {
            homebrew_version: "5.1.1-76-gd3a5ae4".into(),
            installed_on_request: false,
            changed_files: Some(Vec::new()),
            source_modified_time: 1774552019,
            stdlib: None,
            compiler: "clang".into(),
            aliases: Some(Vec::new()),
            runtime_dependencies: Some(vec![RuntimeDep {
                bottle_rebuild: Some(0),
                ..RuntimeDep::new("libpng", "1.6.58", 0, "1.6.58", true)
            }]),
            spec: "stable".into(),
            versions: SourceVersions {
                stable: Some("2.4.6".into()),
                ..SourceVersions::default()
            },
            source_path: "/Users/remikalbe/Library/Caches/Homebrew/api/formula.jws.json".into(),
            tap_git_head: None,
            tap: Some("homebrew/core".into()),
            arch: Some("arm64".into()),
            built_on: Some(BuiltOn::Macos {
                os: Some("Macintosh".into()),
                os_version: Some("macOS 26.3".into()),
                cpu_family: Some("dunno".into()),
                xcode: Some("26.3".into()),
                clt: Some("26.3.0.0.1.1771626560".into()),
                preferred_perl: Some("5.34".into()),
            }),
        }
    }

    // Mirrors a real `homebrew/core` poured-from-API receipt (libharu 2.4.6) byte-for-byte,
    // minus the legacy `installed_as_dependency` key that current Homebrew no longer writes.
    const EXPECTED: &str = r#"{
  "homebrew_version": "5.1.1-76-gd3a5ae4",
  "used_options": [],
  "unused_options": [],
  "built_as_bottle": true,
  "poured_from_bottle": true,
  "loaded_from_api": true,
  "loaded_from_internal_api": false,
  "installed_on_request": false,
  "changed_files": [],
  "time": 1776820611,
  "source_modified_time": 1774552019,
  "compiler": "clang",
  "aliases": [],
  "runtime_dependencies": [
    {
      "full_name": "libpng",
      "version": "1.6.58",
      "revision": 0,
      "bottle_rebuild": 0,
      "pkg_version": "1.6.58",
      "declared_directly": true
    }
  ],
  "source": {
    "spec": "stable",
    "versions": {
      "stable": "2.4.6",
      "head": null,
      "version_scheme": 0,
      "compatibility_version": null
    },
    "path": "/Users/remikalbe/Library/Caches/Homebrew/api/formula.jws.json",
    "tap_git_head": null,
    "tap": "homebrew/core"
  },
  "arch": "arm64",
  "built_on": {
    "os": "Macintosh",
    "os_version": "macOS 26.3",
    "cpu_family": "dunno",
    "xcode": "26.3",
    "clt": "26.3.0.0.1.1771626560",
    "preferred_perl": "5.34"
  }
}"#;

    #[test]
    fn poured_bottle_receipt_matches_real_homebrew_bytes() {
        let receipt = InstallReceipt::for_poured_bottle(sample_params(), 1776820611);
        assert_eq!(receipt.to_json(), EXPECTED);
    }

    #[test]
    fn serialized_receipt_has_no_trailing_newline() {
        let receipt = InstallReceipt::for_poured_bottle(sample_params(), 1776820611);
        let json = receipt.to_json();
        assert!(!json.ends_with('\n'));
        assert!(json.ends_with('}'));
    }

    #[test]
    fn pour_path_fields_are_fixed_regardless_of_params() {
        let receipt = InstallReceipt::for_poured_bottle(sample_params(), 42);
        assert!(receipt.used_options.is_empty());
        assert!(receipt.unused_options.is_empty());
        assert!(receipt.built_as_bottle);
        assert!(receipt.poured_from_bottle);
        assert!(receipt.loaded_from_api);
        assert!(!receipt.loaded_from_internal_api);
        assert_eq!(receipt.time, Some(42));
    }

    #[test]
    fn blank_stdlib_is_dropped_but_present_stdlib_is_kept() {
        let mut params = sample_params();
        params.stdlib = Some(String::new());
        let dropped = InstallReceipt::for_poured_bottle(params, 1);
        assert!(!dropped.to_json().contains("stdlib"));

        let mut params = sample_params();
        params.stdlib = Some("libcxx".into());
        let kept = InstallReceipt::for_poured_bottle(params, 1);
        assert!(kept.to_json().contains("\"stdlib\": \"libcxx\""));
    }

    #[test]
    fn null_changed_files_serializes_as_json_null() {
        let mut params = sample_params();
        params.changed_files = None;
        let receipt = InstallReceipt::for_poured_bottle(params, 1);
        assert!(receipt.to_json().contains("\"changed_files\": null"));
    }

    #[test]
    fn empty_runtime_dependencies_serializes_as_empty_array() {
        let mut params = sample_params();
        params.runtime_dependencies = Some(Vec::new());
        let receipt = InstallReceipt::for_poured_bottle(params, 1);
        assert!(receipt.to_json().contains("\"runtime_dependencies\": []"));
    }

    #[test]
    fn write_creates_receipt_file_with_exact_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "ferrobrew_tab_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let receipt = InstallReceipt::for_poured_bottle(sample_params(), 1776820611);
        receipt.write(&dir).unwrap();

        let written = std::fs::read_to_string(dir.join(FILENAME)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(written, EXPECTED);
    }

    #[test]
    fn write_errors_carry_the_target_path() {
        let missing = Path::new("/ferrobrew/definitely/missing/keg/dir");
        let receipt = InstallReceipt::for_poured_bottle(sample_params(), 1);
        let err = receipt.write(missing).unwrap_err();
        match err {
            FerroError::Io { path: Some(p), .. } => {
                assert_eq!(p, missing.join(FILENAME));
            }
            other => panic!("expected Io error with path, got {other:?}"),
        }
    }
}

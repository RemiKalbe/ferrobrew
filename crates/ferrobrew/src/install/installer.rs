//! The bottle-pour install pipeline: resolve order → download → verify → extract → relocate →
//! receipt → link. Standalone (no Ruby), driven by the JSON API.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::{FerroError, Result};
use crate::formula::RawFormula;
use crate::install::extract;
use crate::keg::{self, Keg};
use crate::system::Tag;
use crate::tab::{InstallReceipt, PourParams, RuntimeDep, SourceVersions};
use crate::{api, deps, download, relocate};

/// Install each requested formula (and its runtime dependencies) from bottles.
pub fn install(config: &Config, names: &[String]) -> Result<()> {
    let tag = Tag::current();
    let tag_str = tag.to_bottle_tag();
    let arch = tag.arch.as_str();

    for requested in names {
        let order = deps::resolve_install_order(requested, api::fetch_formula)?;

        // name -> pkg_version, used to record dependency versions in receipts.
        let mut versions: HashMap<String, String> = HashMap::new();
        for formula in &order {
            if let Some(version) = formula.pkg_version() {
                versions.insert(formula.name.clone(), version);
            }
        }

        for formula in &order {
            let on_request = &formula.name == requested;
            install_one(config, formula, &tag_str, arch, on_request, &versions)?;
        }
    }
    Ok(())
}

fn install_one(
    config: &Config,
    formula: &RawFormula,
    tag: &str,
    arch: &str,
    installed_on_request: bool,
    versions: &HashMap<String, String>,
) -> Result<()> {
    let name = &formula.name;
    let pkg_version = formula
        .pkg_version()
        .ok_or_else(|| FerroError::Unsupported(format!("{name} has no stable version")))?;
    let keg_dir = config.cellar.join(name).join(&pkg_version);

    if keg_dir.join(crate::tab::FILENAME).exists() {
        println!("{name} {pkg_version} is already installed");
        return Ok(());
    }

    let bottle = formula.bottle.file_for_tag(tag).ok_or_else(|| {
        FerroError::Unsupported(format!(
            "{name}: no bottle for {tag} (source builds are not supported yet)"
        ))
    })?;
    let rebuild = formula
        .bottle
        .stable
        .as_ref()
        .map_or(0, |spec| spec.rebuild);
    let basename = bottle_filename(name, &pkg_version, tag, rebuild);

    println!("==> Fetching {name} {pkg_version}");
    let tarball = download::fetch_bottle(config, &bottle.url, &bottle.sha256, &basename)?;

    println!("==> Pouring {basename}");
    let poured = extract::extract_bottle(&tarball, &config.cellar)?;

    let changed_files = relocate_poured_keg(config, &bottle.cellar, &poured)?;

    write_receipt(
        config,
        formula,
        &pkg_version,
        arch,
        installed_on_request,
        changed_files,
        versions,
        &poured,
    )?;

    let keg = Keg::new(&config.cellar, name, &pkg_version);
    keg::link::optlink(&keg, config)?;
    if formula.keg_only {
        println!("{name} is keg-only and was not linked into the prefix");
    } else {
        let links = keg::link::link(&keg, config, false)?;
        println!("==> Linked {name} ({links} symlinks)");
    }
    Ok(())
}

/// Relocate the poured keg according to its bottle's `cellar` field.
fn relocate_poured_keg(
    config: &Config,
    bottle_cellar: &str,
    keg_dir: &Path,
) -> Result<Vec<String>> {
    // The API encodes symbolic cellars as Ruby symbols (":any", ":any_skip_relocation"); concrete
    // cellars are plain absolute paths with no leading colon.
    let cellar = bottle_cellar.strip_prefix(':').unwrap_or(bottle_cellar);
    match cellar {
        // References nothing prefix-specific: installs anywhere, no relocation.
        "any_skip_relocation" => Ok(Vec::new()),
        // Relocatable: replace the @@HOMEBREW_*@@ placeholders with our paths.
        "any" => relocate::relocate_keg(keg_dir, &relocate::standard_relocation(config)),
        // Built for a concrete cellar. If it matches ours the embedded paths are already correct;
        // otherwise cross-prefix relocation of a concrete (non-placeholder) bottle isn't supported
        // yet — refuse rather than ship a keg with dangling paths.
        concrete if Path::new(concrete) == config.cellar.as_path() => Ok(Vec::new()),
        concrete => Err(FerroError::Unsupported(format!(
            "bottle built for cellar {concrete}; installing into {} needs cross-prefix relocation of a concrete-path bottle",
            config.cellar.display()
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_receipt(
    config: &Config,
    formula: &RawFormula,
    pkg_version: &str,
    arch: &str,
    installed_on_request: bool,
    changed_files: Vec<String>,
    versions: &HashMap<String, String>,
    keg_dir: &Path,
) -> Result<()> {
    let runtime_dependencies = formula
        .dependencies
        .iter()
        .map(|dep| {
            let version = versions.get(dep).cloned().unwrap_or_default();
            RuntimeDep::new(dep.clone(), version.clone(), 0, version, true)
        })
        .collect();

    let params = PourParams {
        homebrew_version: format!("ferrobrew {}", env!("CARGO_PKG_VERSION")),
        installed_on_request,
        changed_files: Some(changed_files),
        source_modified_time: 0,
        stdlib: None,
        compiler: "clang".to_string(),
        aliases: Some(Vec::new()),
        runtime_dependencies: Some(runtime_dependencies),
        spec: "stable".to_string(),
        versions: SourceVersions {
            stable: Some(pkg_version.to_string()),
            ..SourceVersions::default()
        },
        source_path: config
            .cache_api()
            .join("formula.jws.json")
            .to_string_lossy()
            .into_owned(),
        tap_git_head: None,
        tap: Some("homebrew/core".to_string()),
        arch: Some(arch.to_string()),
        built_on: None,
    };

    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    InstallReceipt::for_poured_bottle(params, time).write(keg_dir)
}

/// Homebrew's bottle filename: `<name>--<version>.<tag>.bottle[.<rebuild>].tar.gz`.
fn bottle_filename(name: &str, version: &str, tag: &str, rebuild: u32) -> String {
    if rebuild == 0 {
        format!("{name}--{version}.{tag}.bottle.tar.gz")
    } else {
        format!("{name}--{version}.{tag}.bottle.{rebuild}.tar.gz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottle_filename_includes_rebuild_only_when_nonzero() {
        assert_eq!(
            bottle_filename("wget", "1.25.0", "arm64_tahoe", 0),
            "wget--1.25.0.arm64_tahoe.bottle.tar.gz"
        );
        assert_eq!(
            bottle_filename("wget", "1.25.0", "arm64_tahoe", 1),
            "wget--1.25.0.arm64_tahoe.bottle.1.tar.gz"
        );
    }

    #[test]
    fn relocate_skips_for_skip_relocation_and_matching_concrete_cellar() {
        let config = Config {
            prefix: "/opt/homebrew".into(),
            repository: "/opt/homebrew".into(),
            cellar: "/opt/homebrew/Cellar".into(),
            caskroom: "/opt/homebrew/Caskroom".into(),
            cache: "/tmp/ferrobrew-cache".into(),
            library: "/opt/homebrew/Library".into(),
        };
        let keg = Path::new("/does/not/matter");
        // The Ruby-symbol form and a matching concrete cellar both need no relocation.
        assert!(relocate_poured_keg(&config, ":any_skip_relocation", keg)
            .unwrap()
            .is_empty());
        assert!(relocate_poured_keg(&config, "/opt/homebrew/Cellar", keg)
            .unwrap()
            .is_empty());
        // A bottle built for a different concrete cellar isn't cross-prefix relocatable yet.
        assert!(relocate_poured_keg(&config, "/usr/local/Cellar", keg).is_err());
    }
}

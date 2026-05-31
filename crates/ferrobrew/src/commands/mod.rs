//! Higher-level commands built on the install pipeline and keg subsystems.

use std::fs;

use crate::config::Config;
use crate::error::{FerroError, Result};
use crate::keg::{self, Keg};

/// `ferrobrew list` — the names of installed formulae, sorted.
pub fn list(config: &Config) -> Result<Vec<String>> {
    let cellar = &config.cellar;
    let mut names = Vec::new();
    if !cellar.exists() {
        return Ok(names);
    }
    for entry in fs::read_dir(cellar).map_err(|e| FerroError::io(cellar, e))? {
        let entry = entry.map_err(|e| FerroError::io(cellar, e))?;
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if let Some(name) = entry.file_name().to_str() {
            if is_dir && !name.starts_with('.') {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

/// `ferrobrew uninstall <name>...` — unlink and remove every installed version of each formula.
pub fn uninstall(config: &Config, names: &[String]) -> Result<()> {
    for name in names {
        let rack = config.cellar.join(name);
        if !rack.is_dir() {
            return Err(FerroError::NotFound(format!("{name} is not installed")));
        }

        for entry in fs::read_dir(&rack).map_err(|e| FerroError::io(&rack, e))? {
            let entry = entry.map_err(|e| FerroError::io(&rack, e))?;
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let version = entry.file_name().to_string_lossy().into_owned();
            let keg = Keg::new(&config.cellar, name, &version);
            keg::link::unlink(&keg, config)?;
            fs::remove_dir_all(&keg.path).map_err(|e| FerroError::io(&keg.path, e))?;
        }

        // Remove the opt/<name> symlink and the now-empty rack.
        let opt = config.prefix.join("opt").join(name);
        if opt.symlink_metadata().is_ok() {
            let _ = fs::remove_file(&opt);
        }
        let _ = fs::remove_dir_all(&rack);
        println!("Uninstalled {name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_sandbox(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ferrobrew_{label}_{}_{nanos}", std::process::id()))
    }

    fn sandbox_config(root: &std::path::Path) -> Config {
        let prefix = root.join("prefix");
        Config {
            cellar: prefix.join("Cellar"),
            caskroom: prefix.join("Caskroom"),
            repository: root.join("repo"),
            library: root.join("repo/Library"),
            cache: root.join("cache"),
            prefix,
        }
    }

    #[test]
    fn list_then_uninstall_round_trip() {
        let root = unique_sandbox("cmd");
        let config = sandbox_config(&root);

        // Fabricate an installed keg: Cellar/foo/1.0/bin/foo
        let keg_bin = config.cellar.join("foo/1.0/bin");
        fs::create_dir_all(&keg_bin).unwrap();
        fs::write(keg_bin.join("foo"), b"#!/bin/sh\necho hi\n").unwrap();

        let keg = Keg::new(&config.cellar, "foo", "1.0");
        keg::link::link(&keg, &config, false).unwrap();
        keg::link::optlink(&keg, &config).unwrap();

        assert_eq!(list(&config).unwrap(), vec!["foo".to_string()]);
        assert!(config.prefix.join("bin/foo").exists(), "linked into prefix");

        uninstall(&config, &["foo".to_string()]).unwrap();

        assert!(
            list(&config).unwrap().is_empty(),
            "no formulae after uninstall"
        );
        assert!(
            config.prefix.join("bin/foo").symlink_metadata().is_err(),
            "prefix symlink removed"
        );
        assert!(
            config.prefix.join("opt/foo").symlink_metadata().is_err(),
            "opt symlink removed"
        );
        assert!(!config.cellar.join("foo").exists(), "rack removed");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn uninstall_unknown_formula_errors() {
        let root = unique_sandbox("cmd_missing");
        let config = sandbox_config(&root);
        fs::create_dir_all(&config.cellar).unwrap();
        assert!(uninstall(&config, &["nope".to_string()]).is_err());
        fs::remove_dir_all(&root).ok();
    }
}

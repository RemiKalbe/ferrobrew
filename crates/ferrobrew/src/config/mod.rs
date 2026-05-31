//! Resolution of Homebrew's well-known directories.
//!
//! Standalone ferrobrew has no `brew.sh` to export `HOMEBREW_*` paths, so — unlike the abandoned
//! frontend design — it derives them itself, honouring any `HOMEBREW_*` overrides in the
//! environment and otherwise falling back to the same platform defaults `brew.sh` computes.

use std::path::{Path, PathBuf};

/// Platform-specific path defaults, matching `brew.sh`'s `HOMEBREW_DEFAULT_*` derivation.
#[derive(Debug, Clone)]
pub struct Platform {
    pub is_macos: bool,
    pub default_prefix: PathBuf,
    pub default_repository: PathBuf,
}

impl Platform {
    /// Defaults for the host this binary was compiled for.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
            // Apple Silicon: prefix and repository are the same directory.
            let prefix = PathBuf::from("/opt/homebrew");
            Platform {
                is_macos: true,
                default_repository: prefix.clone(),
                default_prefix: prefix,
            }
        } else if cfg!(target_os = "macos") {
            Platform {
                is_macos: true,
                default_prefix: PathBuf::from("/usr/local"),
                default_repository: PathBuf::from("/usr/local/Homebrew"),
            }
        } else {
            Platform {
                is_macos: false,
                default_prefix: PathBuf::from("/home/linuxbrew/.linuxbrew"),
                default_repository: PathBuf::from("/home/linuxbrew/.linuxbrew/Homebrew"),
            }
        }
    }
}

/// The base directories ferrobrew operates on, mirroring Homebrew's `HOMEBREW_*` layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub prefix: PathBuf,
    pub repository: PathBuf,
    pub cellar: PathBuf,
    pub caskroom: PathBuf,
    pub cache: PathBuf,
    pub library: PathBuf,
}

impl Config {
    /// Resolve from the live process environment for the host platform.
    pub fn from_env() -> Self {
        Self::resolve(|k| std::env::var(k).ok(), &Platform::current())
    }

    /// Resolve from an arbitrary environment lookup and platform — the testable core.
    pub fn resolve<F>(lookup: F, platform: &Platform) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let get = |key: &str| lookup(key).filter(|v| !v.is_empty()).map(PathBuf::from);

        let prefix = get("HOMEBREW_PREFIX").unwrap_or_else(|| platform.default_prefix.clone());
        let repository =
            get("HOMEBREW_REPOSITORY").unwrap_or_else(|| platform.default_repository.clone());
        let cellar = get("HOMEBREW_CELLAR").unwrap_or_else(|| default_cellar(&prefix, &repository));
        let caskroom = get("HOMEBREW_CASKROOM").unwrap_or_else(|| prefix.join("Caskroom"));
        let library = get("HOMEBREW_LIBRARY").unwrap_or_else(|| repository.join("Library"));
        let cache =
            get("HOMEBREW_CACHE").unwrap_or_else(|| default_cache(platform.is_macos, &lookup));

        Config {
            prefix,
            repository,
            cellar,
            caskroom,
            cache,
            library,
        }
    }

    /// `$HOMEBREW_CACHE/api`, where bulk JSON API files are cached.
    pub fn cache_api(&self) -> PathBuf {
        self.cache.join("api")
    }
}

/// `brew.sh`: use `<repository>/Cellar` if it already exists, otherwise `<prefix>/Cellar`.
fn default_cellar(prefix: &Path, repository: &Path) -> PathBuf {
    let repo_cellar = repository.join("Cellar");
    if repo_cellar.is_dir() {
        repo_cellar
    } else {
        prefix.join("Cellar")
    }
}

/// `brew.sh`: macOS caches under `~/Library/Caches/Homebrew`; elsewhere under
/// `${XDG_CACHE_HOME:-~/.cache}/Homebrew`.
fn default_cache<F>(is_macos: bool, lookup: &F) -> PathBuf
where
    F: Fn(&str) -> Option<String>,
{
    let home = lookup("HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
    if is_macos {
        if let Some(ref home) = home {
            return home.join("Library/Caches/Homebrew");
        }
    }
    lookup("HOMEBREW_XDG_CACHE_HOME")
        .or_else(|| lookup("XDG_CACHE_HOME"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".cache")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Homebrew")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    fn arm_macos() -> Platform {
        Platform {
            is_macos: true,
            default_prefix: "/opt/homebrew".into(),
            default_repository: "/opt/homebrew".into(),
        }
    }

    #[test]
    fn arm_macos_defaults_are_prefix_relative() {
        let cfg = Config::resolve(env(&[("HOME", "/Users/me")]), &arm_macos());
        assert_eq!(cfg.prefix, Path::new("/opt/homebrew"));
        assert_eq!(cfg.cellar, Path::new("/opt/homebrew/Cellar"));
        assert_eq!(cfg.caskroom, Path::new("/opt/homebrew/Caskroom"));
        assert_eq!(cfg.cache, Path::new("/Users/me/Library/Caches/Homebrew"));
        assert_eq!(
            cfg.cache_api(),
            Path::new("/Users/me/Library/Caches/Homebrew/api")
        );
    }

    #[test]
    fn explicit_env_overrides_win() {
        let cfg = Config::resolve(
            env(&[
                ("HOMEBREW_PREFIX", "/x"),
                ("HOMEBREW_CELLAR", "/y/Cellar"),
                ("HOMEBREW_CACHE", "/z/cache"),
                ("HOME", "/Users/me"),
            ]),
            &arm_macos(),
        );
        assert_eq!(cfg.prefix, Path::new("/x"));
        assert_eq!(cfg.cellar, Path::new("/y/Cellar"));
        assert_eq!(cfg.caskroom, Path::new("/x/Caskroom"));
        assert_eq!(cfg.cache, Path::new("/z/cache"));
    }

    #[test]
    fn linux_cache_uses_xdg_and_repo_has_homebrew_suffix() {
        let platform = Platform {
            is_macos: false,
            default_prefix: "/home/linuxbrew/.linuxbrew".into(),
            default_repository: "/home/linuxbrew/.linuxbrew/Homebrew".into(),
        };
        let cfg = Config::resolve(
            env(&[("XDG_CACHE_HOME", "/c"), ("HOME", "/home/me")]),
            &platform,
        );
        assert_eq!(cfg.cache, Path::new("/c/Homebrew"));
        assert_eq!(
            cfg.repository,
            Path::new("/home/linuxbrew/.linuxbrew/Homebrew")
        );
        assert_eq!(
            cfg.library,
            Path::new("/home/linuxbrew/.linuxbrew/Homebrew/Library")
        );
    }
}

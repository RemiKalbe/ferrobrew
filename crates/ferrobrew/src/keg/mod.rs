//! Kegs and keg linking.
//!
//! A "keg" is one installed version of a formula: `HOMEBREW_CELLAR/<name>/<version>/`. The
//! [`link`] submodule symlinks a keg's contents into `HOMEBREW_PREFIX` following Homebrew's
//! dir-vs-file rules (see `specs/keg-link.md`), and maintains the stable `opt/<name>` symlink.
//!
//! Ported from `Library/Homebrew/keg.rb` (+ the macOS override in `extend/os/mac/keg.rb`). The
//! scope here is the offline, filesystem-only core: `link`, `optlink`, `unlink`, and the prefix
//! layout. Tab/alias/oldname handling and `link_overwrite` allowlists depend on subsystems that are
//! out of scope (see the spec's "Open questions") and are intentionally not implemented.

pub mod layout;
pub mod link;

use std::path::{Path, PathBuf};

/// One installed version of a formula.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keg {
    /// The keg directory: `<cellar>/<name>/<version>`.
    pub path: PathBuf,
    /// The rack (formula) name.
    pub name: String,
    /// The `PkgVersion` string (e.g. `1.2.3`, `1.2.3_1`, `HEAD-abc123`).
    pub version: String,
}

impl Keg {
    /// Construct a keg from its Cellar, formula name, and version. The keg directory is
    /// `cellar/name/version`; the rack is `cellar/name`. This does not touch the filesystem, so it
    /// can be used to describe a keg that has not been created yet.
    pub fn new(cellar: &Path, name: &str, version: &str) -> Keg {
        Keg {
            path: cellar.join(name).join(version),
            name: name.to_string(),
            version: version.to_string(),
        }
    }

    /// The rack: the parent directory holding all versions of this formula.
    pub fn rack(&self) -> PathBuf {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.path.clone())
    }
}

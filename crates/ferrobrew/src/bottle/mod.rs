//! Bottle metadata from the JSON API and selection of the bottle for a platform tag.

pub mod ghcr;

use std::collections::HashMap;

use serde::Deserialize;

/// The `bottle` stanza of a formula's JSON.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BottleStanza {
    #[serde(default)]
    pub stable: Option<BottleSpec>,
}

/// The `bottle.stable` object: per-tag bottle files sharing a root URL and rebuild number.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BottleSpec {
    #[serde(default)]
    pub rebuild: u32,
    #[serde(default)]
    pub root_url: String,
    #[serde(default)]
    pub files: HashMap<String, BottleFile>,
}

/// One downloadable bottle for a specific platform tag.
#[derive(Debug, Clone, Deserialize)]
pub struct BottleFile {
    /// The Cellar the bottle was built for: `"any"`, `"any_skip_relocation"`, or an absolute path.
    #[serde(default)]
    pub cellar: String,
    /// The full download URL (a ghcr.io OCI blob for the default `homebrew/core` tap).
    pub url: String,
    /// The expected SHA-256 of the downloaded bottle tarball.
    pub sha256: String,
}

impl BottleStanza {
    /// The bottle file for an exact platform tag (e.g. `arm64_tahoe`), if present.
    ///
    /// Note: Homebrew also falls back to older-but-compatible macOS tags and the `all` tag; that
    /// compatibility logic is tracked in `specs/download-ghcr.md` and lands with the installer.
    pub fn file_for_tag(&self, tag: &str) -> Option<&BottleFile> {
        self.stable.as_ref()?.files.get(tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_selects_by_tag() {
        let json = r#"{"stable":{"rebuild":1,"root_url":"https://ghcr.io/v2/homebrew/core",
            "files":{"arm64_tahoe":{"cellar":"/opt/homebrew/Cellar",
            "url":"https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:abc","sha256":"abc"}}}}"#;
        let stanza: BottleStanza = serde_json::from_str(json).unwrap();
        let file = stanza.file_for_tag("arm64_tahoe").expect("tag present");
        assert_eq!(file.sha256, "abc");
        assert_eq!(stanza.stable.as_ref().unwrap().rebuild, 1);
        assert!(stanza.file_for_tag("nonexistent").is_none());
    }
}

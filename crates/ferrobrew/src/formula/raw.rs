//! The `serde` model of a single formula JSON object from the API, plus version helpers.

use serde::Deserialize;

use crate::bottle::BottleStanza;

/// A formula as described by the JSON API. Every field defaults so partial/older payloads parse.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawFormula {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub versions: Versions,
    #[serde(default)]
    pub revision: u32,
    #[serde(default)]
    pub bottle: BottleStanza,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub keg_only: bool,
}

/// The `versions` object.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Versions {
    #[serde(default)]
    pub stable: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub bottle: bool,
}

impl RawFormula {
    /// The versioned directory name under the Cellar, e.g. `1.25.0` or `1.25.0_2` (with revision).
    pub fn pkg_version(&self) -> Option<String> {
        let stable = self.versions.stable.as_deref()?;
        Some(if self.revision == 0 {
            stable.to_string()
        } else {
            format!("{stable}_{}", self.revision)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_version_appends_nonzero_revision() {
        let mut formula = RawFormula {
            revision: 0,
            ..Default::default()
        };
        formula.versions.stable = Some("1.25.0".into());
        assert_eq!(formula.pkg_version().as_deref(), Some("1.25.0"));
        formula.revision = 2;
        assert_eq!(formula.pkg_version().as_deref(), Some("1.25.0_2"));
    }

    #[test]
    fn pkg_version_is_none_without_stable() {
        assert_eq!(RawFormula::default().pkg_version(), None);
    }

    #[test]
    fn parses_real_wget_fixture() {
        let json = include_str!("../../tests/fixtures/wget.json");
        let formula: RawFormula = serde_json::from_str(json).expect("wget.json parses");
        assert_eq!(formula.name, "wget");
        assert_eq!(formula.pkg_version().as_deref(), Some("1.25.0"));
        assert!(!formula.dependencies.is_empty());

        let bottle = formula
            .bottle
            .file_for_tag("arm64_tahoe")
            .expect("arm64_tahoe bottle present");
        assert_eq!(bottle.sha256.len(), 64);
        assert!(bottle.url.ends_with(&bottle.sha256));
        assert!(bottle.url.contains("ghcr.io"));
    }
}

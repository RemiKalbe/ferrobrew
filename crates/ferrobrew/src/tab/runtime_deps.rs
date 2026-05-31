//! Elements of the `runtime_dependencies` array in an INSTALL_RECEIPT.json.
//!
//! Built by `Tab.formula_to_dep_hash` (`Library/Homebrew/tab.rb:160`), one object per resolved
//! runtime dependency, then `.compact`ed (nil values dropped). See `specs/receipt-tab.md` §5.

use serde::Serialize;

/// A single resolved runtime dependency as recorded in the receipt.
///
/// Field declaration order is the on-disk key order:
/// `full_name, version, revision, bottle_rebuild, pkg_version, declared_directly,
/// compatibility_version`.
///
/// Ruby's `.compact` drops `nil` values, so `bottle_rebuild` and `compatibility_version` are
/// omitted entirely when `None` (the common modern case). Every other field is always emitted,
/// including `revision: 0` — only `nil` is dropped, and `revision` defaults to the integer `0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeDep {
    /// Dependency formula full name, e.g. `"openssl@3"`.
    pub full_name: String,
    /// `dep.version.to_s` — the version without any revision suffix.
    pub version: String,
    /// Dependency formula revision (`0` if unset). Always emitted.
    pub revision: i64,
    /// `formula.bottle&.rebuild` — omitted when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bottle_rebuild: Option<i64>,
    /// `dep.pkg_version.to_s` — version plus `_revision` suffix when revision is non-zero.
    pub pkg_version: String,
    /// Whether this dependency is declared directly in the parent formula's `deps`.
    pub declared_directly: bool,
    /// `formula.compatibility_version` — omitted when `None` (usually absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility_version: Option<i64>,
}

impl RuntimeDep {
    /// Construct a runtime dependency entry with the modern (compacted) shape: no
    /// `bottle_rebuild` and no `compatibility_version`.
    pub fn new(
        full_name: impl Into<String>,
        version: impl Into<String>,
        revision: i64,
        pkg_version: impl Into<String>,
        declared_directly: bool,
    ) -> Self {
        RuntimeDep {
            full_name: full_name.into(),
            version: version.into(),
            revision,
            bottle_rebuild: None,
            pkg_version: pkg_version.into(),
            declared_directly,
            compatibility_version: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_dep_omits_optional_keys() {
        let dep = RuntimeDep::new("libpng", "1.6.58", 0, "1.6.58", true);
        let json = serde_json::to_string_pretty(&dep).unwrap();
        assert_eq!(
            json,
            "{\n  \"full_name\": \"libpng\",\n  \"version\": \"1.6.58\",\n  \"revision\": 0,\n  \"pkg_version\": \"1.6.58\",\n  \"declared_directly\": true\n}"
        );
    }

    #[test]
    fn dep_with_optional_keys_keeps_field_order() {
        let dep = RuntimeDep {
            bottle_rebuild: Some(0),
            compatibility_version: Some(2),
            ..RuntimeDep::new("openssl@3", "3.6.0", 1, "3.6.0_1", false)
        };
        let json = serde_json::to_string_pretty(&dep).unwrap();
        assert_eq!(
            json,
            "{\n  \"full_name\": \"openssl@3\",\n  \"version\": \"3.6.0\",\n  \"revision\": 1,\n  \"bottle_rebuild\": 0,\n  \"pkg_version\": \"3.6.0_1\",\n  \"declared_directly\": false,\n  \"compatibility_version\": 2\n}"
        );
    }
}

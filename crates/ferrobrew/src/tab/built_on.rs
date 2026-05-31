//! The `built_on` object of an INSTALL_RECEIPT.json — `DevelopmentTools.build_system_info`.
//!
//! On a bottle pour this block is read verbatim from the bottle's embedded tab (it describes the
//! BUILD machine, not the installing one), so ferrobrew models it as plain data carried through
//! from the API/bottle rather than recomputing it locally. See `specs/receipt-tab.md` §6.
//!
//! Two platform shapes exist, distinguished by key set and order:
//!
//! - macOS (`extend/os/mac/development_tools.rb`): `os, os_version, cpu_family, xcode, clt,
//!   preferred_perl`.
//! - Linux (`extend/os/linux/development_tools.rb`): `os, os_version, cpu_family, glibc_version,
//!   oldest_cpu_family`.
//!
//! Every value may be `null` (Ruby `.presence` turns a blank string into `nil`), and `null` is
//! emitted, never skipped.

use serde::Serialize;

/// The `built_on` object describing the machine a bottle was built on.
///
/// Modelled as an untagged enum so each platform serialises with exactly the key set and order
/// Homebrew's `DevelopmentTools.build_system_info` produces. Field declaration order is the
/// on-disk key order; all values serialize as JSON `null` when `None` (matching Ruby's `.presence`
/// → `nil` behaviour, which is NOT compacted out of `built_on`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum BuiltOn {
    /// macOS build-system info: `os, os_version, cpu_family, xcode, clt, preferred_perl`.
    Macos {
        os: Option<String>,
        os_version: Option<String>,
        cpu_family: Option<String>,
        xcode: Option<String>,
        clt: Option<String>,
        preferred_perl: Option<String>,
    },
    /// Linux build-system info: `os, os_version, cpu_family, glibc_version, oldest_cpu_family`.
    Linux {
        os: Option<String>,
        os_version: Option<String>,
        cpu_family: Option<String>,
        glibc_version: Option<String>,
        oldest_cpu_family: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_built_on_key_order_and_nulls() {
        let built_on = BuiltOn::Macos {
            os: Some("Macintosh".into()),
            os_version: Some("macOS 26.3".into()),
            cpu_family: Some("dunno".into()),
            xcode: Some("26.3".into()),
            clt: None,
            preferred_perl: Some("5.34".into()),
        };
        let json = serde_json::to_string_pretty(&built_on).unwrap();
        assert_eq!(
            json,
            "{\n  \"os\": \"Macintosh\",\n  \"os_version\": \"macOS 26.3\",\n  \"cpu_family\": \"dunno\",\n  \"xcode\": \"26.3\",\n  \"clt\": null,\n  \"preferred_perl\": \"5.34\"\n}"
        );
    }

    #[test]
    fn linux_built_on_key_order() {
        let built_on = BuiltOn::Linux {
            os: Some("Linux".into()),
            os_version: Some("Ubuntu 22.04".into()),
            cpu_family: Some("westmere".into()),
            glibc_version: Some("2.35".into()),
            oldest_cpu_family: Some("core2".into()),
        };
        let json = serde_json::to_string_pretty(&built_on).unwrap();
        assert_eq!(
            json,
            "{\n  \"os\": \"Linux\",\n  \"os_version\": \"Ubuntu 22.04\",\n  \"cpu_family\": \"westmere\",\n  \"glibc_version\": \"2.35\",\n  \"oldest_cpu_family\": \"core2\"\n}"
        );
    }
}

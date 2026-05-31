//! Bottle tags: the `{arch}_{os}` identifiers Homebrew uses to select a bottle for the current
//! platform (e.g. `arm64_sequoia`, `sonoma`, `x86_64_linux`). These are the keys in a formula's
//! `bottle.stable.files` map.

use super::macos_version;

/// CPU architecture as it appears in a bottle tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Arm64,
    X86_64,
}

impl Arch {
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::Arm64 => "arm64",
            Arch::X86_64 => "x86_64",
        }
    }
}

/// Operating system as it appears in a bottle tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Os {
    /// macOS, identified by its release codename (e.g. `"sequoia"`).
    Macos(String),
    Linux,
}

/// A platform tag identifying which bottle to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub arch: Arch,
    pub os: Os,
}

impl Tag {
    /// Render the tag the way it appears as a key in a formula's `bottle.stable.files`.
    ///
    /// macOS x86_64 uses the bare codename (`sonoma`); every other combination is prefixed with
    /// the architecture (`arm64_sonoma`, `arm64_linux`, `x86_64_linux`).
    pub fn to_bottle_tag(&self) -> String {
        match (&self.os, self.arch) {
            (Os::Macos(name), Arch::X86_64) => name.clone(),
            (Os::Macos(name), Arch::Arm64) => format!("arm64_{name}"),
            (Os::Linux, arch) => format!("{}_linux", arch.as_str()),
        }
    }

    /// The tag for the host this binary is running on.
    pub fn current() -> Self {
        let arch = if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else {
            Arch::X86_64
        };
        let os = if cfg!(target_os = "macos") {
            Os::Macos(macos_version::current_codename().unwrap_or_else(|| "sequoia".to_string()))
        } else {
            Os::Linux
        };
        Tag { arch, os }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_arm_tag_is_prefixed() {
        let tag = Tag {
            arch: Arch::Arm64,
            os: Os::Macos("sequoia".into()),
        };
        assert_eq!(tag.to_bottle_tag(), "arm64_sequoia");
    }

    #[test]
    fn macos_intel_tag_has_no_arch_prefix() {
        let tag = Tag {
            arch: Arch::X86_64,
            os: Os::Macos("sonoma".into()),
        };
        assert_eq!(tag.to_bottle_tag(), "sonoma");
    }

    #[test]
    fn linux_tags_are_prefixed() {
        assert_eq!(
            Tag {
                arch: Arch::Arm64,
                os: Os::Linux
            }
            .to_bottle_tag(),
            "arm64_linux"
        );
        assert_eq!(
            Tag {
                arch: Arch::X86_64,
                os: Os::Linux
            }
            .to_bottle_tag(),
            "x86_64_linux"
        );
    }
}

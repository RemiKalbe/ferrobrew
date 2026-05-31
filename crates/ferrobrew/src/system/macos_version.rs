//! macOS release version → Homebrew bottle codename mapping, plus runtime detection.

/// Map a macOS version to its Homebrew bottle codename.
///
/// For macOS 11+ only the major version matters. For 10.x the minor version selects the codename.
/// Returns `None` for versions ferrobrew doesn't have a codename for.
pub fn codename(major: u32, minor: u32) -> Option<&'static str> {
    Some(match major {
        26 => "tahoe",
        15 => "sequoia",
        14 => "sonoma",
        13 => "ventura",
        12 => "monterey",
        11 => "big_sur",
        10 => match minor {
            15 => "catalina",
            14 => "mojave",
            13 => "high_sierra",
            12 => "sierra",
            11 => "el_capitan",
            _ => return None,
        },
        _ => return None,
    })
}

/// Detect the current macOS codename by shelling out to `sw_vers -productVersion`.
#[cfg(target_os = "macos")]
pub fn current_codename() -> Option<String> {
    let output = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    let version = String::from_utf8(output.stdout).ok()?;
    let mut parts = version.trim().split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    codename(major, minor).map(str::to_string)
}

/// Non-macOS hosts have no macOS codename.
#[cfg(not(target_os = "macos"))]
pub fn current_codename() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modern_versions_use_major() {
        assert_eq!(codename(26, 0), Some("tahoe"));
        assert_eq!(codename(15, 1), Some("sequoia"));
        assert_eq!(codename(14, 0), Some("sonoma"));
        assert_eq!(codename(13, 0), Some("ventura"));
        assert_eq!(codename(11, 7), Some("big_sur"));
    }

    #[test]
    fn legacy_versions_use_minor() {
        assert_eq!(codename(10, 15), Some("catalina"));
        assert_eq!(codename(10, 14), Some("mojave"));
        assert_eq!(codename(10, 99), None);
        assert_eq!(codename(99, 0), None);
    }
}

//! macOS Mach-O dynamic-linkage relocation (spec §7, §10).
//!
//! For the **pour** direction we rewrite install names / ids / rpaths whose value starts with a
//! placeholder token (`@@HOMEBREW_CELLAR@@` then `@@HOMEBREW_PREFIX@@`, cellar first per spec §7
//! `relocated_name_for`) to the concrete target path, then **mandatorily** ad-hoc codesign any
//! edited binary (required on arm64; conditional on Intel — spec §10).
//!
//! Reading the load commands and editing them is done by shelling out to the system
//! `/usr/bin/otool`, `/usr/bin/install_name_tool` and `/usr/bin/codesign`, which the spec
//! identifies as the lowest-risk path that mirrors ruby-macho's effective behaviour (it also
//! handles fat binaries transparently). Magic-number sniffing pre-filters candidates so we only
//! invoke the tools on real Mach-O files.
//!
//! Everything here is compiled only on macOS; [`super::relocate_keg`] skips it on other hosts.

use std::path::Path;
use std::process::Command;

use crate::error::{FerroError, Result};

use super::text::{Key, Relocation, CELLAR_PLACEHOLDER, PREFIX_PLACEHOLDER};

const OTOOL: &str = "/usr/bin/otool";
const INSTALL_NAME_TOOL: &str = "/usr/bin/install_name_tool";
const CODESIGN: &str = "/usr/bin/codesign";

/// Mach-O magic numbers (little/big-endian, 32/64-bit, and the two fat magics). Matching any means
/// the file is Mach-O and worth handing to `otool` (spec §7 type detection / Rust notes).
const MACHO_MAGICS: [[u8; 4]; 6] = [
    [0xfe, 0xed, 0xfa, 0xce], // MH_MAGIC (32, BE)
    [0xce, 0xfa, 0xed, 0xfe], // MH_CIGAM (32, LE)
    [0xfe, 0xed, 0xfa, 0xcf], // MH_MAGIC_64 (BE)
    [0xcf, 0xfa, 0xed, 0xfe], // MH_CIGAM_64 (LE)
    [0xca, 0xfe, 0xba, 0xbe], // FAT_MAGIC
    [0xbe, 0xba, 0xfe, 0xca], // FAT_CIGAM
];

/// Whether the first bytes are a Mach-O (thin or fat) magic.
pub fn is_macho(head: &[u8]) -> bool {
    head.len() >= 4 && MACHO_MAGICS.iter().any(|m| head[..4] == *m)
}

/// The `change_rpath`/`change_install_name`/`change_dylib_id` decision (spec §7
/// `relocated_name_for`): if `old_name` starts with the cellar token, swap that; else if it starts
/// with the prefix token, swap that; else `None` (leave it). Cellar is checked first because it is
/// a subdirectory of prefix.
fn relocated_name_for(old_name: &str, reloc: &Relocation) -> Option<String> {
    if let Some(cellar) = reloc.new_for(Key::Cellar) {
        if let Some(rest) = old_name.strip_prefix(CELLAR_PLACEHOLDER) {
            return Some(format!("{cellar}{rest}"));
        }
    }
    if let Some(prefix) = reloc.new_for(Key::Prefix) {
        if let Some(rest) = old_name.strip_prefix(PREFIX_PLACEHOLDER) {
            return Some(format!("{prefix}{rest}"));
        }
    }
    None
}

/// `VARIABLE_REFERENCE_RX` filter (spec §7 `each_linkage_for`): drop names starting with
/// `@loader_path` / `@executable_path` / `@rpath`. These never carry a placeholder, and rewriting
/// them is unsafe.
fn is_variable_reference(name: &str) -> bool {
    name.starts_with("@loader_path")
        || name.starts_with("@executable_path")
        || name.starts_with("@rpath")
}

/// The load-command data `otool -l` exposes that we need.
struct Linkage {
    dylib_id: Option<String>,
    /// LC_LOAD_DYLIB / LC_LOAD_WEAK_DYLIB etc. names.
    load_names: Vec<String>,
    /// LC_RPATH paths.
    rpaths: Vec<String>,
    is_dylib: bool,
}

/// Parse `otool -l <file>` output for ids, load-dylib names and rpaths.
///
/// `otool -l` prints one load command per stanza; we walk it line-by-line. For `LC_ID_DYLIB`,
/// `LC_LOAD*_DYLIB` and `LC_RPATH` the following lines carry a `name <value> (...)` or
/// `path <value> (...)` field. This is the same information ruby-macho reads from the binary.
fn read_linkage(file: &Path) -> Result<Linkage> {
    let output = Command::new(OTOOL)
        .arg("-l")
        .arg(file)
        .output()
        .map_err(|e| FerroError::io(file, e))?;
    if !output.status.success() {
        return Err(FerroError::Other(format!(
            "otool -l failed for {}: {}",
            file.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);

    let mut dylib_id = None;
    let mut load_names = Vec::new();
    let mut rpaths = Vec::new();
    let mut is_dylib = false;

    #[derive(PartialEq)]
    enum Cmd {
        None,
        IdDylib,
        LoadDylib,
        Rpath,
    }
    let mut current = Cmd::None;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(cmd) = trimmed.strip_prefix("cmd ") {
            current = match cmd {
                "LC_ID_DYLIB" => {
                    is_dylib = true;
                    Cmd::IdDylib
                }
                "LC_LOAD_DYLIB"
                | "LC_LOAD_WEAK_DYLIB"
                | "LC_REEXPORT_DYLIB"
                | "LC_LAZY_LOAD_DYLIB"
                | "LC_LOAD_UPWARD_DYLIB" => Cmd::LoadDylib,
                "LC_RPATH" => Cmd::Rpath,
                _ => Cmd::None,
            };
            continue;
        }
        match current {
            Cmd::IdDylib | Cmd::LoadDylib => {
                if let Some(value) = field_after(trimmed, "name ") {
                    match current {
                        Cmd::IdDylib => dylib_id = Some(value),
                        Cmd::LoadDylib => load_names.push(value),
                        _ => unreachable!(),
                    }
                    current = Cmd::None;
                }
            }
            Cmd::Rpath => {
                if let Some(value) = field_after(trimmed, "path ") {
                    rpaths.push(value);
                    current = Cmd::None;
                }
            }
            Cmd::None => {}
        }
    }

    Ok(Linkage {
        dylib_id,
        load_names,
        rpaths,
        is_dylib,
    })
}

/// Extract the value of an `otool` field line like `name /path (offset 24)` — returns `/path`,
/// stripping the trailing ` (offset N)` annotation.
fn field_after(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?;
    let value = match rest.rfind(" (") {
        Some(idx) => &rest[..idx],
        None => rest,
    };
    Some(value.trim().to_string())
}

/// Relocate one Mach-O file's dynamic linkage in place (spec §7), returning whether it changed.
///
/// On any change the binary's signature is invalidated, so we ad-hoc codesign afterwards (spec
/// §10) — mandatory on arm64, conditional on Intel.
pub fn relocate_macho(file: &Path, reloc: &Relocation) -> Result<bool> {
    let linkage = read_linkage(file)?;
    let mut modified = false;

    // 1. dylib id (LC_ID_DYLIB) — only for dylibs.
    if linkage.is_dylib {
        if let Some(id) = &linkage.dylib_id {
            if let Some(new_id) = relocated_name_for(id, reloc) {
                if &new_id != id {
                    run_install_name_tool(&["-id", &new_id], file)?;
                    modified = true;
                }
            }
        }
    }

    // 2. install names (LC_LOAD_DYLIB ...), skipping @loader_path/@executable_path/@rpath.
    for old in &linkage.load_names {
        if is_variable_reference(old) {
            continue;
        }
        if let Some(new) = relocated_name_for(old, reloc) {
            if &new != old {
                run_install_name_tool(&["-change", old, &new], file)?;
                modified = true;
            }
        }
    }

    // 3. rpaths (LC_RPATH), skipping variable references.
    for old in &linkage.rpaths {
        if is_variable_reference(old) {
            continue;
        }
        if let Some(new) = relocated_name_for(old, reloc) {
            if &new != old {
                run_install_name_tool(&["-rpath", old, &new], file)?;
                modified = true;
            }
        }
    }

    if modified {
        codesign_patched_binary(file)?;
    }
    Ok(modified)
}

/// Run `install_name_tool <args> <file>` (spec §7 edit primitives).
fn run_install_name_tool(args: &[&str], file: &Path) -> Result<()> {
    let status = Command::new(INSTALL_NAME_TOOL)
        .args(args)
        .arg(file)
        .output()
        .map_err(|e| FerroError::io(file, e))?;
    if !status.status.success() {
        return Err(FerroError::Other(format!(
            "install_name_tool {} failed for {}: {}",
            args.join(" "),
            file.display(),
            String::from_utf8_lossy(&status.stderr).trim()
        )));
    }
    Ok(())
}

/// Mandatory ad-hoc codesigning after a binary edit (spec §10).
///
/// Gated on macOS >= 11 (Big Sur). On non-arm64 hosts we first `codesign --verify` and only
/// re-sign when the signature is actually broken ("invalid signature"). The signing args and their
/// order are exact (spec §10): `codesign --sign - --force
/// --preserve-metadata=entitlements,requirements,flags,runtime <file>`. On failure we apply the
/// documented inode-swap workaround (copy to a tmp path, move it back to change the inode, retry).
pub fn codesign_patched_binary(file: &Path) -> Result<()> {
    if !codesigning_supported() {
        return Ok(());
    }

    // Intel: only re-sign if the existing signature is broken.
    if !cfg!(target_arch = "aarch64") {
        let verify = Command::new(CODESIGN)
            .arg("--verify")
            .arg(file)
            .output()
            .map_err(|e| FerroError::io(file, e))?;
        let stderr = String::from_utf8_lossy(&verify.stderr).to_lowercase();
        if !stderr.contains("invalid signature") {
            return Ok(());
        }
    }

    if try_codesign(file)? {
        return Ok(());
    }

    // Apple-codesign-bug workaround: change the inode via copy-to-tmp + move-back, then retry once.
    swap_inode(file)?;
    if try_codesign(file)? {
        return Ok(());
    }
    Err(FerroError::Other(format!(
        "failed to ad-hoc codesign {} after relocation",
        file.display()
    )))
}

/// One `codesign --sign - --force --preserve-metadata=... <file>` attempt. Returns success.
fn try_codesign(file: &Path) -> Result<bool> {
    let status = Command::new(CODESIGN)
        .args([
            "--sign",
            "-",
            "--force",
            "--preserve-metadata=entitlements,requirements,flags,runtime",
        ])
        .arg(file)
        .output()
        .map_err(|e| FerroError::io(file, e))?;
    Ok(status.status.success())
}

/// Copy `file` to a sibling temp path then rename it back, giving the file a fresh inode. This is
/// the documented workaround for the Apple codesign caching bug (spec §10 step 5).
fn swap_inode(file: &Path) -> Result<()> {
    let parent = file.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = parent.to_path_buf();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workaround");
    tmp.push(format!(".{base}.ferrobrew-codesign.{pid}.{nanos}"));

    std::fs::copy(file, &tmp).map_err(|e| FerroError::io(&tmp, e))?;
    // Preserve permissions of the original on the temp copy.
    if let Ok(meta) = std::fs::metadata(file) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, file).map_err(|e| FerroError::io(file, e))?;
    Ok(())
}

/// Whether codesigning applies on this host: macOS >= 11 (Big Sur). Reads the major version from
/// `/usr/bin/sw_vers -productVersion`; if it can't be determined we conservatively codesign (the
/// safe default on modern macOS).
fn codesigning_supported() -> bool {
    let Ok(output) = Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
    else {
        return true;
    };
    if !output.status.success() {
        return true;
    }
    let version = String::from_utf8_lossy(&output.stdout);
    match version
        .trim()
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
    {
        Some(major) => major >= 11,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pour_table() -> Relocation {
        let mut r = Relocation::new();
        r.add_literal(Key::Prefix, PREFIX_PLACEHOLDER, "/opt/homebrew");
        r.add_literal(Key::Cellar, CELLAR_PLACEHOLDER, "/opt/homebrew/Cellar");
        r
    }

    #[test]
    fn macho_magic_recognised() {
        assert!(is_macho(&[0xcf, 0xfa, 0xed, 0xfe, 0x07]));
        assert!(is_macho(&[0xca, 0xfe, 0xba, 0xbe]));
        assert!(!is_macho(&[0x7f, b'E', b'L', b'F']));
        assert!(!is_macho(&[0x00, 0x01]));
    }

    #[test]
    fn relocated_name_prefers_cellar_then_prefix() {
        let r = pour_table();
        assert_eq!(
            relocated_name_for("@@HOMEBREW_CELLAR@@/wget/1.0/lib/libwget.dylib", &r),
            Some("/opt/homebrew/Cellar/wget/1.0/lib/libwget.dylib".to_string())
        );
        assert_eq!(
            relocated_name_for("@@HOMEBREW_PREFIX@@/lib/libfoo.dylib", &r),
            Some("/opt/homebrew/lib/libfoo.dylib".to_string())
        );
    }

    #[test]
    fn relocated_name_none_for_unrelated() {
        let r = pour_table();
        assert!(relocated_name_for("/usr/lib/libSystem.dylib", &r).is_none());
        assert!(relocated_name_for("@rpath/libfoo.dylib", &r).is_none());
    }

    #[test]
    fn variable_references_detected() {
        assert!(is_variable_reference("@loader_path/../lib/x.dylib"));
        assert!(is_variable_reference("@executable_path/x"));
        assert!(is_variable_reference("@rpath/x.dylib"));
        assert!(!is_variable_reference("@@HOMEBREW_PREFIX@@/lib/x.dylib"));
        assert!(!is_variable_reference("/usr/lib/x.dylib"));
    }

    #[test]
    fn field_after_strips_offset_annotation() {
        assert_eq!(
            field_after("name @@HOMEBREW_PREFIX@@/lib/x.dylib (offset 24)", "name "),
            Some("@@HOMEBREW_PREFIX@@/lib/x.dylib".to_string())
        );
        assert_eq!(
            field_after("path @@HOMEBREW_CELLAR@@/x (offset 12)", "path "),
            Some("@@HOMEBREW_CELLAR@@/x".to_string())
        );
        assert_eq!(field_after("cmdsize 56", "name "), None);
    }
}

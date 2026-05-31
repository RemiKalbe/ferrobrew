//! Linux ELF dynamic-linkage relocation (spec §8).
//!
//! For the **pour** direction we rewrite an ELF's RPATH/RUNPATH and PT_INTERP so they point at the
//! target machine's prefix, then write the result with `patchelf` (which forces DT_RUNPATH in
//! patchelf-compatible mode — spec §8). Reading (ELF type, interpreter, rpath, section names,
//! PT_DYNAMIC presence) is done by hand-parsing the ELF, matching the manual sniffing the spec
//! describes; writing shells out to the `patchelf` binary, the spec's recommended low-risk path.
//!
//! Compiled only on Linux; [`super::relocate_keg`] skips it on other hosts.

use std::path::Path;
use std::process::Command;

use crate::error::{FerroError, Result};

use super::text::{Key, Relocation};

const PATCHELF: &str = "patchelf";

/// `$ORIGIN` — kept as-is in RPATH entries (spec §8 step 3).
const ORIGIN: &str = "$ORIGIN";

/// Parsed ELF facts we need for relocation.
pub struct ElfInfo {
    pub is_elf: bool,
    /// `:executable` (ET_EXEC=2 / ET_DYN=3 with interp) or `:dylib` (ET_DYN=3). We keep the raw
    /// `e_type` and a `dynamic` flag rather than Homebrew's exact enum.
    pub is_executable: bool,
    pub is_dylib: bool,
    /// Has a PT_DYNAMIC segment (`dynamic_elf?`).
    pub dynamic: bool,
    /// PT_INTERP value, if present.
    pub interpreter: Option<String>,
    /// Raw RUNPATH (preferred) or RPATH colon string, if present.
    pub rpath: Option<String>,
    /// Non-blank section names.
    pub section_names: Vec<String>,
    is_64: bool,
    little_endian: bool,
}

/// Whether `head` begins with the ELF magic and a System-V/Linux OS-ABI byte (spec §8): magic
/// `\x7fELF` at 0 and byte 0x07 in {0,3}.
pub fn is_elf(head: &[u8]) -> bool {
    head.len() > 0x07 && &head[0..4] == b"\x7fELF" && (head[0x07] == 0 || head[0x07] == 3)
}

/// Read the ELF facts needed for relocation, parsing `bytes` (the whole file) by hand.
///
/// Returns `is_elf: false` when the magic/ABI check fails so the caller can skip the file. Any
/// structural surprise (truncated headers, out-of-range offsets) yields a non-dynamic / field-less
/// result rather than a panic — we never edit a file we could not fully understand.
pub fn read_elf(bytes: &[u8]) -> ElfInfo {
    let mut info = ElfInfo {
        is_elf: false,
        is_executable: false,
        is_dylib: false,
        dynamic: false,
        interpreter: None,
        rpath: None,
        section_names: Vec::new(),
        is_64: false,
        little_endian: true,
    };
    if !is_elf(bytes) {
        return info;
    }
    info.is_elf = true;
    info.is_64 = bytes[0x04] == 2; // EI_CLASS: 1=32, 2=64
    info.little_endian = bytes[0x05] != 2; // EI_DATA: 1=LE, 2=BE

    let rd = Reader {
        bytes,
        is_64: info.is_64,
        le: info.little_endian,
    };

    // e_type at 0x10 (u16).
    if let Some(e_type) = rd.u16(0x10) {
        info.is_executable = e_type == 2;
        info.is_dylib = e_type == 3;
    }

    parse_program_headers(&rd, &mut info);
    parse_section_names(&rd, &mut info);
    info
}

/// Walk the program headers for PT_INTERP, PT_DYNAMIC, and (via PT_DYNAMIC) the dynamic entries
/// that carry RPATH/RUNPATH.
fn parse_program_headers(rd: &Reader, info: &mut ElfInfo) {
    // e_phoff: u64@0x20 (64-bit) / u32@0x1C (32-bit). e_phentsize@0x36/0x2A, e_phnum@0x38/0x2C.
    let (e_phoff, e_phentsize, e_phnum) = if rd.is_64 {
        (rd.u64(0x20), rd.u16(0x36), rd.u16(0x38))
    } else {
        (rd.u32(0x1C).map(|v| v as u64), rd.u16(0x2A), rd.u16(0x2C))
    };
    let (Some(phoff), Some(phentsize), Some(phnum)) = (e_phoff, e_phentsize, e_phnum) else {
        return;
    };

    let mut dynamic_off_size: Option<(u64, u64)> = None;
    for i in 0..phnum as u64 {
        let base = phoff + i * phentsize as u64;
        // p_type: u32 at offset 0 of the program header.
        let Some(p_type) = rd.u32(base as usize) else {
            continue;
        };
        match p_type {
            3 => {
                // PT_INTERP: file offset p_offset, size p_filesz.
                if let Some((off, sz)) = ph_offset_filesz(rd, base) {
                    info.interpreter = read_cstr(rd.bytes, off as usize, sz as usize);
                }
            }
            2 => {
                // PT_DYNAMIC.
                info.dynamic = true;
                if let Some((off, sz)) = ph_offset_filesz(rd, base) {
                    dynamic_off_size = Some((off, sz));
                }
            }
            _ => {}
        }
    }

    if let Some((dyn_off, dyn_sz)) = dynamic_off_size {
        parse_dynamic(rd, info, dyn_off, dyn_sz);
    }
}

/// `(p_offset, p_filesz)` for a program header at `base`.
fn ph_offset_filesz(rd: &Reader, base: u64) -> Option<(u64, u64)> {
    let base = base as usize;
    if rd.is_64 {
        // p_offset @ +0x08 (u64), p_filesz @ +0x20 (u64).
        Some((rd.u64(base + 0x08)?, rd.u64(base + 0x20)?))
    } else {
        // p_offset @ +0x04 (u32), p_filesz @ +0x10 (u32).
        Some((rd.u32(base + 0x04)? as u64, rd.u32(base + 0x10)? as u64))
    }
}

/// Parse the dynamic section: find DT_STRTAB (the string table) and DT_RUNPATH (preferred) /
/// DT_RPATH (fallback) offsets, then resolve the rpath string. DT_STRTAB holds a *virtual address*;
/// we map it back to a file offset via the program headers' PT_LOAD entries.
fn parse_dynamic(rd: &Reader, info: &mut ElfInfo, dyn_off: u64, dyn_sz: u64) {
    let entry_size: u64 = if rd.is_64 { 16 } else { 8 };
    let mut strtab_vaddr: Option<u64> = None;
    let mut runpath_off: Option<u64> = None;
    let mut rpath_off: Option<u64> = None;

    let mut pos = dyn_off;
    let end = dyn_off + dyn_sz;
    while pos + entry_size <= end {
        let (tag, val) = if rd.is_64 {
            (rd.i64(pos as usize), rd.u64(pos as usize + 8))
        } else {
            (
                rd.i32(pos as usize).map(|v| v as i64),
                rd.u32(pos as usize + 4).map(|v| v as u64),
            )
        };
        let (Some(tag), Some(val)) = (tag, val) else {
            break;
        };
        match tag {
            0 => break,                    // DT_NULL terminates.
            5 => strtab_vaddr = Some(val), // DT_STRTAB
            15 => rpath_off = Some(val),   // DT_RPATH
            29 => runpath_off = Some(val), // DT_RUNPATH
            _ => {}
        }
        pos += entry_size;
    }

    let chosen = runpath_off.or(rpath_off);
    if let (Some(strtab_vaddr), Some(str_off)) = (strtab_vaddr, chosen) {
        if let Some(strtab_file_off) = vaddr_to_offset(rd, strtab_vaddr) {
            info.rpath = read_cstr(rd.bytes, (strtab_file_off + str_off) as usize, usize::MAX);
        }
    }
}

/// Translate a virtual address to a file offset using PT_LOAD segments (p_vaddr/p_offset/p_filesz).
fn vaddr_to_offset(rd: &Reader, vaddr: u64) -> Option<u64> {
    let (e_phoff, e_phentsize, e_phnum) = if rd.is_64 {
        (rd.u64(0x20)?, rd.u16(0x36)?, rd.u16(0x38)?)
    } else {
        (rd.u32(0x1C)? as u64, rd.u16(0x2A)?, rd.u16(0x2C)?)
    };
    for i in 0..e_phnum as u64 {
        let base = (e_phoff + i * e_phentsize as u64) as usize;
        let p_type = rd.u32(base)?;
        if p_type != 1 {
            continue; // PT_LOAD only
        }
        let (p_offset, p_vaddr, p_filesz) = if rd.is_64 {
            (
                rd.u64(base + 0x08)?,
                rd.u64(base + 0x10)?,
                rd.u64(base + 0x20)?,
            )
        } else {
            (
                rd.u32(base + 0x04)? as u64,
                rd.u32(base + 0x08)? as u64,
                rd.u32(base + 0x10)? as u64,
            )
        };
        if vaddr >= p_vaddr && vaddr < p_vaddr + p_filesz {
            return Some(p_offset + (vaddr - p_vaddr));
        }
    }
    None
}

/// Collect non-blank section names from the section-header string table.
fn parse_section_names(rd: &Reader, info: &mut ElfInfo) {
    let (e_shoff, e_shentsize, e_shnum, e_shstrndx) = if rd.is_64 {
        (rd.u64(0x28), rd.u16(0x3A), rd.u16(0x3C), rd.u16(0x3E))
    } else {
        (
            rd.u32(0x20).map(|v| v as u64),
            rd.u16(0x2E),
            rd.u16(0x30),
            rd.u16(0x32),
        )
    };
    let (Some(shoff), Some(shentsize), Some(shnum), Some(shstrndx)) =
        (e_shoff, e_shentsize, e_shnum, e_shstrndx)
    else {
        return;
    };
    if shoff == 0 || shnum == 0 || shstrndx >= shnum {
        return;
    }

    // sh_offset of the section-name string table section.
    let shstr_base = (shoff + shstrndx as u64 * shentsize as u64) as usize;
    let Some(shstr_off) = (if rd.is_64 {
        rd.u64(shstr_base + 0x18)
    } else {
        rd.u32(shstr_base + 0x10).map(|v| v as u64)
    }) else {
        return;
    };

    for i in 0..shnum as u64 {
        let base = (shoff + i * shentsize as u64) as usize;
        // sh_name: u32 at offset 0 — index into the section-name string table.
        let Some(name_idx) = rd.u32(base) else {
            continue;
        };
        if let Some(name) = read_cstr(rd.bytes, (shstr_off + name_idx as u64) as usize, usize::MAX)
        {
            if !name.is_empty() {
                info.section_names.push(name);
            }
        }
    }
}

/// Read a NUL-terminated string starting at `off`, up to `max_len` bytes. Returns `None` if the
/// offset is out of range. Only valid UTF-8 is returned (ELF strings are ASCII paths/names).
fn read_cstr(bytes: &[u8], off: usize, max_len: usize) -> Option<String> {
    if off >= bytes.len() {
        return None;
    }
    let slice = &bytes[off..];
    let upper = slice.len().min(max_len);
    let end = slice[..upper].iter().position(|&b| b == 0).unwrap_or(upper);
    std::str::from_utf8(&slice[..end])
        .ok()
        .map(|s| s.to_string())
}

/// Little/big-endian, 32/64-bit integer reads over the ELF byte buffer.
struct Reader<'a> {
    bytes: &'a [u8],
    is_64: bool,
    le: bool,
}

impl Reader<'_> {
    fn u16(&self, off: usize) -> Option<u16> {
        let b = self.bytes.get(off..off + 2)?;
        let arr = [b[0], b[1]];
        Some(if self.le {
            u16::from_le_bytes(arr)
        } else {
            u16::from_be_bytes(arr)
        })
    }
    fn u32(&self, off: usize) -> Option<u32> {
        let b = self.bytes.get(off..off + 4)?;
        let arr = [b[0], b[1], b[2], b[3]];
        Some(if self.le {
            u32::from_le_bytes(arr)
        } else {
            u32::from_be_bytes(arr)
        })
    }
    fn i32(&self, off: usize) -> Option<i32> {
        self.u32(off).map(|v| v as i32)
    }
    fn u64(&self, off: usize) -> Option<u64> {
        let b = self.bytes.get(off..off + 8)?;
        let arr = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        Some(if self.le {
            u64::from_le_bytes(arr)
        } else {
            u64::from_be_bytes(arr)
        })
    }
    fn i64(&self, off: usize) -> Option<i64> {
        self.u64(off).map(|v| v as i64)
    }
}

/// Whether the formula is glibc (skip ELF relocation entirely — spec §8) or gcc (skip the
/// `lib/gcc/<N>` -> `lib/gcc/current` rewrite — spec §8 step 3). Matches
/// `Version.formula_optionally_versioned_regex`: bare name or `name@<version>`.
fn formula_matches(name: &str, base: &str) -> bool {
    if name == base {
        return true;
    }
    if let Some(version) = name.strip_prefix(base).and_then(|s| s.strip_prefix('@')) {
        return !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    }
    false
}

/// Replace only the first occurrence of `from` with `to`, matching Ruby's `String#sub` (the spec
/// uses `sub`, not `gsub`, on rpath/interpreter entries — first-occurrence semantics).
fn replace_first(haystack: &str, from: &str, to: &str) -> String {
    match haystack.find(from) {
        Some(idx) => format!("{}{to}{}", &haystack[..idx], &haystack[idx + from.len()..]),
        None => haystack.to_string(),
    }
}

/// Compute the new RPATH/RUNPATH from the raw old value (spec §8 step 3).
///
/// Algorithm: split on `:`; substitute `old_prefix` -> `new_prefix` in each entry; keep only
/// entries that start with `new_prefix` or `$ORIGIN`; append `<new_prefix>/lib` if absent; unless
/// the formula is gcc, rewrite a trailing `lib/gcc/<digits>` to `lib/gcc/current`; join with `:`.
/// Returns `Some(new)` only when it differs from `old`.
fn compute_rpath(
    old: &str,
    old_prefix: &str,
    new_prefix: &str,
    formula_name: &str,
) -> Option<String> {
    let new_lib = format!("{new_prefix}/lib");
    let mut entries: Vec<String> = old
        .split(':')
        .filter(|e| !e.is_empty())
        .map(|e| replace_first(e, old_prefix, new_prefix))
        .filter(|e| e.starts_with(new_prefix) || e.starts_with(ORIGIN))
        .collect();

    if !entries.iter().any(|e| e == &new_lib) {
        entries.push(new_lib);
    }

    if !formula_matches(formula_name, "gcc") {
        for entry in &mut entries {
            *entry = rewrite_gcc_current(entry);
        }
    }

    let joined = entries.join(":");
    if joined == old {
        None
    } else {
        Some(joined)
    }
}

/// Rewrite a trailing `lib/gcc/<digits>` to `lib/gcc/current` (spec §8 step 3, the
/// `%r{lib/gcc/\d+$}` substitution).
fn rewrite_gcc_current(entry: &str) -> String {
    const MARKER: &str = "lib/gcc/";
    if let Some(idx) = entry.rfind(MARKER) {
        let tail = &entry[idx + MARKER.len()..];
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
            return format!("{}{MARKER}current", &entry[..idx]);
        }
    }
    entry.to_string()
}

/// Compute the new interpreter (spec §8 step 4): if `<new_prefix>/lib/ld.so` is readable use it,
/// else prefix-substitute the old interpreter. Returns `Some(new)` only when it differs.
fn compute_interpreter(old: &str, old_prefix: &str, new_prefix: &str) -> Option<String> {
    let ld_so = format!("{new_prefix}/lib/ld.so");
    let new = if Path::new(&ld_so).exists() {
        ld_so
    } else {
        replace_first(old, old_prefix, new_prefix)
    };
    if new == old {
        None
    } else {
        Some(new)
    }
}

/// Relocate one ELF file's dynamic linkage in place (spec §8), returning whether it changed.
///
/// `formula_name` gates the glibc skip (whole file) and gcc skip (rpath rewrite). `skip_protodesc`
/// is true during bottling — at pour time it is false.
pub fn relocate_elf(
    file: &Path,
    reloc: &Relocation,
    formula_name: &str,
    skip_protodesc: bool,
) -> Result<bool> {
    // Skip glibc entirely — patching its linker breaks it (spec §8).
    if formula_matches(formula_name, "glibc") {
        return Ok(false);
    }

    let (Some(old_prefix), Some(new_prefix)) = (
        Some(super::text::PREFIX_PLACEHOLDER),
        reloc.new_for(Key::Prefix),
    ) else {
        return Ok(false);
    };

    let bytes = std::fs::read(file).map_err(|e| FerroError::io(file, e))?;
    let info = read_elf(&bytes);
    if !info.is_elf || !info.dynamic {
        return Ok(false);
    }
    // patchelf corrupts binaries carrying a `protodesc_cold` section; skip at bottling time only.
    if skip_protodesc && info.section_names.iter().any(|s| s == "protodesc_cold") {
        return Ok(false);
    }

    let new_interp = info
        .interpreter
        .as_deref()
        .and_then(|old| compute_interpreter(old, old_prefix, new_prefix));
    let new_rpath = info
        .rpath
        .as_deref()
        .and_then(|old| compute_rpath(old, old_prefix, new_prefix, formula_name));

    if new_interp.is_none() && new_rpath.is_none() {
        return Ok(false);
    }

    let mut cmd = Command::new(PATCHELF);
    if let Some(interp) = &new_interp {
        cmd.arg("--set-interpreter").arg(interp);
    }
    if let Some(rpath) = &new_rpath {
        // `--set-rpath` in patchelf writes DT_RUNPATH (patchelf-compatible mode), matching the
        // patchelf.rb gem's `patchelf_compatible: true` (spec §8).
        cmd.arg("--set-rpath").arg(rpath);
    }
    cmd.arg(file);

    let status = cmd.output().map_err(|e| FerroError::io(file, e))?;
    if !status.status.success() {
        return Err(FerroError::Other(format!(
            "patchelf failed for {}: {}",
            file.display(),
            String::from_utf8_lossy(&status.stderr).trim()
        )));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_first_only_first_occurrence() {
        assert_eq!(
            replace_first("@@P@@/lib:@@P@@/x", "@@P@@", "/opt"),
            "/opt/lib:@@P@@/x"
        );
        assert_eq!(replace_first("nochange", "@@P@@", "/opt"), "nochange");
    }

    #[test]
    fn elf_magic_and_abi() {
        let mut head = vec![0u8; 16];
        head[0..4].copy_from_slice(b"\x7fELF");
        head[0x07] = 0; // System V
        assert!(is_elf(&head));
        head[0x07] = 3; // Linux
        assert!(is_elf(&head));
        head[0x07] = 9; // unknown ABI
        assert!(!is_elf(&head));
        assert!(!is_elf(b"\x7fELF")); // too short
        assert!(!is_elf(b"MZ\x00\x00\x00\x00\x00\x00\x00")); // not ELF
    }

    #[test]
    fn formula_versioned_regex() {
        assert!(formula_matches("glibc", "glibc"));
        assert!(formula_matches("glibc@2.13", "glibc"));
        assert!(!formula_matches("glibcxx", "glibc"));
        assert!(!formula_matches("glibc@", "glibc"));
        assert!(formula_matches("gcc", "gcc"));
        assert!(formula_matches("gcc@11", "gcc"));
        assert!(!formula_matches("gccgo", "gcc"));
    }

    #[test]
    fn rpath_keeps_origin_and_prefixed_entries_only() {
        let new = compute_rpath(
            "@@HOMEBREW_PREFIX@@/lib:$ORIGIN/../lib:/build/tmp/lib:/usr/lib",
            "@@HOMEBREW_PREFIX@@",
            "/opt/homebrew",
            "wget",
        )
        .expect("changed");
        // Foreign /build and /usr entries dropped; prefix-substituted; $ORIGIN kept; new lib present.
        assert_eq!(new, "/opt/homebrew/lib:$ORIGIN/../lib");
    }

    #[test]
    fn rpath_appends_new_lib_when_absent() {
        let new = compute_rpath(
            "$ORIGIN/../lib",
            "@@HOMEBREW_PREFIX@@",
            "/opt/homebrew",
            "wget",
        )
        .expect("changed");
        assert_eq!(new, "$ORIGIN/../lib:/opt/homebrew/lib");
    }

    #[test]
    fn rpath_unchanged_returns_none() {
        // Already-relocated rpath that contains the new lib and only valid entries.
        let result = compute_rpath(
            "/opt/homebrew/lib",
            "@@HOMEBREW_PREFIX@@",
            "/opt/homebrew",
            "wget",
        );
        assert!(result.is_none());
    }

    #[test]
    fn gcc_current_rewrite_applies_for_non_gcc() {
        let new = compute_rpath(
            "@@HOMEBREW_PREFIX@@/lib/gcc/13",
            "@@HOMEBREW_PREFIX@@",
            "/opt/homebrew",
            "wget",
        )
        .expect("changed");
        assert!(new.contains("/opt/homebrew/lib/gcc/current"));
    }

    #[test]
    fn gcc_current_rewrite_skipped_for_gcc_formula() {
        let new = compute_rpath(
            "@@HOMEBREW_PREFIX@@/lib/gcc/13",
            "@@HOMEBREW_PREFIX@@",
            "/opt/homebrew",
            "gcc",
        )
        .expect("changed");
        // gcc formula keeps the versioned dir; only the prefix is substituted (plus new lib added).
        assert!(new.contains("/opt/homebrew/lib/gcc/13"));
        assert!(!new.contains("lib/gcc/current"));
    }

    #[test]
    fn gcc_current_only_for_trailing_digits() {
        assert_eq!(rewrite_gcc_current("/x/lib/gcc/13"), "/x/lib/gcc/current");
        assert_eq!(
            rewrite_gcc_current("/x/lib/gcc/current"),
            "/x/lib/gcc/current"
        );
        assert_eq!(
            rewrite_gcc_current("/x/lib/gcc/13/extra"),
            "/x/lib/gcc/13/extra"
        );
        assert_eq!(rewrite_gcc_current("/x/lib"), "/x/lib");
    }

    #[test]
    fn interpreter_prefix_substituted() {
        let new = compute_interpreter(
            "@@HOMEBREW_PREFIX@@/lib/ld-linux.so.2",
            "@@HOMEBREW_PREFIX@@",
            "/opt/homebrew",
        )
        .expect("changed");
        assert_eq!(new, "/opt/homebrew/lib/ld-linux.so.2");
    }

    #[test]
    fn interpreter_unchanged_returns_none() {
        let result =
            compute_interpreter("/lib64/ld-linux-x86-64.so.2", "@@HOMEBREW_PREFIX@@", "/x");
        assert!(result.is_none());
    }

    #[test]
    fn read_cstr_bounds() {
        let data = b"abc\x00def";
        assert_eq!(read_cstr(data, 0, usize::MAX), Some("abc".to_string()));
        assert_eq!(read_cstr(data, 4, usize::MAX), Some("def".to_string()));
        assert_eq!(read_cstr(data, 100, usize::MAX), None);
        // max_len bound.
        assert_eq!(read_cstr(b"abcdef", 0, 3), Some("abc".to_string()));
    }

    #[test]
    fn non_elf_read_yields_not_elf() {
        let info = read_elf(b"not an elf file at all");
        assert!(!info.is_elf);
        assert!(!info.dynamic);
        assert!(info.interpreter.is_none());
        assert!(info.rpath.is_none());
    }
}

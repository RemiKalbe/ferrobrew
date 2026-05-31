//! File classification: which keg files get text substitution, and text-vs-binary detection.
//!
//! Homebrew shells out to `file --no-dereference` and `grep` for this (spec §9). Per the spec's
//! "Text substitution & file classification" note, a direct Rust NUL-scan plus shebang/libtool
//! handling is an accepted substitute, and it avoids depending on the host `file`/`grep` being
//! present (which Homebrew's `text_files` bails out on anyway). We implement the documented
//! heuristics directly.

use std::path::Path;

/// Libtool archive extensions — always relocation candidates regardless of content
/// (`LIBTOOL_EXTENSIONS`, spec §9).
pub const LIBTOOL_EXTENSIONS: [&str; 2] = ["la", "lai"];

/// `Metafiles::EXTENSIONS` — the documentation-markup extensions Homebrew's `text_files` rejects
/// from substitution (`Library/Homebrew/metafiles.rb`, resolved exactly rather than guessed).
/// `.rb`/`.json` are deliberately NOT here — the `.brew/<name>.rb` formula copy is rejected by name
/// instead (see `is_skipped_by_name`), matching upstream.
pub const METAFILE_EXTENSIONS: [&str; 18] = [
    "adoc",
    "asc",
    "asciidoc",
    "creole",
    "html",
    "markdown",
    "md",
    "mdown",
    "mediawiki",
    "mkdn",
    "org",
    "pod",
    "rdoc",
    "rst",
    "rtf",
    "textile",
    "txt",
    "wiki",
];

/// A file we keep no matter what (python virtualenv marker), spec §9.
const ALWAYS_KEEP_BASENAME: &str = "orig-prefix.txt";

/// How much of a file to read when sniffing a shebang (Homebrew reads the first 1024 bytes).
const SHEBANG_SNIFF_BYTES: usize = 1024;

/// Whether `bytes` contain a NUL — Homebrew's `binary_file?` test (spec §9): presence of a NUL
/// byte means binary.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&super::text::NULL_BYTE)
}

/// Whether the leading bytes look like a shebang script (`/\A#!\s*\S+/` over the first 1024 bytes,
/// spec §9). Such files are always treated as text.
pub fn has_shebang(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(SHEBANG_SNIFF_BYTES)];
    let mut it = head.iter();
    if it.next() != Some(&b'#') || it.next() != Some(&b'!') {
        return false;
    }
    // Skip optional whitespace, then require at least one non-whitespace byte (\s*\S+).
    let rest: Vec<u8> = it.copied().collect();
    let after_ws = rest.iter().skip_while(|b| b.is_ascii_whitespace());
    after_ws.clone().count() != 0 && after_ws.clone().any(|b| !b.is_ascii_whitespace())
}

/// Whether a file's extension (case-sensitive, matching Ruby's `extname`) is one of `exts`.
fn has_extension(path: &Path, exts: &[&str]) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => exts.contains(&ext),
        None => false,
    }
}

/// Whether this is a libtool archive (`.la`/`.lai`).
pub fn is_libtool(path: &Path) -> bool {
    has_extension(path, &LIBTOOL_EXTENSIONS)
}

/// Whether the keg-relative path should be skipped from text substitution by name/extension alone
/// (spec §9 `text_files` reject list): the `.brew/<name>.rb` formula copy, and `Metafiles`
/// extensions — except `orig-prefix.txt`, which is always kept.
///
/// `keg_relative` is the path relative to the keg root; `formula_name` is the installed formula's
/// name (so `.brew/<name>.rb` can be matched).
pub fn is_skipped_by_name(keg_relative: &Path, formula_name: &str) -> bool {
    if let Some(name) = keg_relative.file_name().and_then(|n| n.to_str()) {
        if name == ALWAYS_KEEP_BASENAME {
            return false;
        }
    }
    // .brew/<name>.rb — the per-keg formula copy.
    if keg_relative == Path::new(".brew").join(format!("{formula_name}.rb")) {
        return true;
    }
    has_extension(keg_relative, &METAFILE_EXTENSIONS)
}

/// Whether a Homebrew-generated service file (`homebrew.*.plist|.service|.timer`, spec §9
/// `homebrew_created_file?`). These get the full-prefix table even in `/usr/local` mode; we expose
/// the predicate for callers that implement that branch.
pub fn is_homebrew_created_file(path: &Path) -> bool {
    let basename_ok = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("homebrew."))
        .unwrap_or(false);
    basename_ok && has_extension(path, &["plist", "service", "timer"])
}

/// Decide whether a regular (non-symlink, non-dir) file is a text-substitution candidate.
///
/// Order mirrors `text_files | libtool_files` (spec §9):
///   1. name/extension reject list (`is_skipped_by_name`), unless `orig-prefix.txt`;
///   2. libtool archives are always candidates;
///   3. shebang scripts are always text;
///   4. otherwise text iff it contains no NUL byte.
///
/// `keg_relative` is used for the name-based reject; `bytes` is the file content (or a prefix of
/// it large enough to contain any NUL and the shebang).
pub fn is_text_candidate(keg_relative: &Path, formula_name: &str, bytes: &[u8]) -> bool {
    if is_libtool(keg_relative) {
        return true;
    }
    if is_skipped_by_name(keg_relative, formula_name) {
        return false;
    }
    if has_shebang(bytes) {
        return true;
    }
    !is_binary(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn nul_byte_is_binary() {
        assert!(is_binary(b"abc\x00def"));
        assert!(!is_binary(b"plain text"));
    }

    #[test]
    fn shebang_detected() {
        assert!(has_shebang(b"#!/bin/sh\necho hi"));
        assert!(has_shebang(b"#! /usr/bin/env perl\n"));
        assert!(!has_shebang(b"#not a shebang"));
        assert!(!has_shebang(b"plain"));
        assert!(!has_shebang(b"#!   "));
    }

    #[test]
    fn shebang_only_in_first_1024_bytes() {
        let mut bytes = vec![b'x'; 2000];
        bytes[0] = b'#';
        bytes[1] = b'!';
        // The shebang is at the very start, so still detected.
        assert!(has_shebang(&bytes));
        // A shebang pushed past byte 1024 is not detected.
        let mut late = vec![b' '; 1100];
        late[1050] = b'#';
        late[1051] = b'!';
        assert!(!has_shebang(&late));
    }

    #[test]
    fn libtool_extensions() {
        assert!(is_libtool(Path::new("lib/foo.la")));
        assert!(is_libtool(Path::new("lib/foo.lai")));
        assert!(!is_libtool(Path::new("lib/foo.so")));
    }

    #[test]
    fn libtool_is_candidate_even_with_nul() {
        // .la files are always candidates even if they happen to contain a NUL.
        assert!(is_text_candidate(
            Path::new("lib/foo.la"),
            "foo",
            b"deps\x00here"
        ));
    }

    #[test]
    fn metafile_extension_skipped() {
        assert!(is_skipped_by_name(Path::new("share/doc/README.md"), "foo"));
        assert!(is_skipped_by_name(Path::new("share/doc/NOTES.rst"), "foo"));
        assert!(!is_skipped_by_name(Path::new("bin/foo"), "foo"));
        // .json / .rb are NOT in Metafiles::EXTENSIONS, so they are not skipped by extension.
        assert!(!is_skipped_by_name(Path::new("share/config.json"), "foo"));
    }

    #[test]
    fn brew_formula_copy_skipped() {
        // .rb is not a metafile extension, so the .brew/<name>.rb special case is what skips it.
        let p = PathBuf::from(".brew").join("wget.rb");
        assert!(is_skipped_by_name(&p, "wget"));
        // A different formula's .brew copy name is not matched, and .rb alone is not skipped.
        assert!(!is_skipped_by_name(&p, "curl"));
        assert!(!is_skipped_by_name(Path::new("lib/ruby/foo.rb"), "foo"));
    }

    #[test]
    fn orig_prefix_txt_always_kept() {
        // Even though .txt is a metafile extension, orig-prefix.txt is kept.
        assert!(!is_skipped_by_name(
            Path::new("lib/python/orig-prefix.txt"),
            "foo"
        ));
        assert!(is_text_candidate(
            Path::new("lib/python/orig-prefix.txt"),
            "foo",
            b"/opt/homebrew\n"
        ));
    }

    #[test]
    fn plain_text_is_candidate() {
        assert!(is_text_candidate(
            Path::new("bin/script"),
            "foo",
            b"export PATH=@@HOMEBREW_PREFIX@@/bin\n"
        ));
    }

    #[test]
    fn binary_without_shebang_is_not_candidate() {
        assert!(!is_text_candidate(
            Path::new("bin/foo"),
            "foo",
            b"\x7fELF\x00\x00stuff"
        ));
    }

    #[test]
    fn shebang_binary_still_text() {
        // A shebang script that contains a NUL later is still treated as text.
        assert!(is_text_candidate(
            Path::new("bin/script"),
            "foo",
            b"#!/bin/sh\n\x00weird"
        ));
    }

    #[test]
    fn homebrew_service_file_detected() {
        assert!(is_homebrew_created_file(Path::new(
            "homebrew.mxcl.foo.plist"
        )));
        assert!(is_homebrew_created_file(Path::new("homebrew.foo.service")));
        assert!(is_homebrew_created_file(Path::new("homebrew.foo.timer")));
        assert!(!is_homebrew_created_file(Path::new(
            "com.example.foo.plist"
        )));
        assert!(!is_homebrew_created_file(Path::new("homebrew.foo.conf")));
    }
}

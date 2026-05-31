//! The substitution table ([`Relocation`]) and the text-replacement engine.
//!
//! This models Homebrew's `Relocation` class (`keg_relocate.rb` lines 18-69) for the **pour**
//! direction used by [`super::relocate_keg`]: placeholder tokens (`@@HOMEBREW_*@@`) are mapped
//! back to the target machine's concrete paths. In that direction every matcher is an unambiguous
//! literal placeholder string (the spec, §5: "placeholders are unambiguous tokens"), so no regex
//! or path-boundary wrapping is required for the standard map.
//!
//! The type nonetheless preserves the full ordered-map semantics and the
//! `RELOCATABLE_PATH_REGEX_PREFIX` boundary check (spec §2 / "Lookbehind workaround") so that a
//! path-anchored literal matcher behaves correctly if one is ever added — the boundary scan is
//! implemented by hand because the `regex` crate has no lookbehind.

use crate::config::Config;

/// Placeholder tokens embedded in a portable bottle (spec §1). Each maps to an env-derived target
/// resolved at pour time from [`Config`].
pub const PREFIX_PLACEHOLDER: &str = "@@HOMEBREW_PREFIX@@";
pub const CELLAR_PLACEHOLDER: &str = "@@HOMEBREW_CELLAR@@";
pub const REPOSITORY_PLACEHOLDER: &str = "@@HOMEBREW_REPOSITORY@@";
pub const LIBRARY_PLACEHOLDER: &str = "@@HOMEBREW_LIBRARY@@";
pub const PERL_PLACEHOLDER: &str = "@@HOMEBREW_PERL@@";
pub const JAVA_PLACEHOLDER: &str = "@@HOMEBREW_JAVA@@";

/// One literal NUL byte; marks a file as binary (spec §1, `NULL_BYTE`).
pub const NULL_BYTE: u8 = 0x00;

/// Which logical slot a replacement pair fills. Preserved for ordering parity and debugging; the
/// engine sorts purely on matcher kind/length so the key is informational.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Prefix,
    Cellar,
    Repository,
    Library,
    Perl,
    Java,
}

/// How a replacement pair matches the old text.
#[derive(Debug, Clone)]
pub enum Matcher {
    /// A plain literal needle. When `path_boundary` is set, a match is only accepted when its left
    /// edge satisfies `RELOCATABLE_PATH_REGEX_PREFIX` (spec §2): preceded by a compiler flag
    /// (`-F`/`-I`/`-L`/`-isystem`) or by a non-alphanumeric byte (or at offset 0). The standard
    /// pour-direction map never sets this — placeholders are unambiguous — but a path-anchored
    /// literal is supported for completeness.
    Literal { needle: String, path_boundary: bool },
}

/// One ordered `old -> new` substitution.
#[derive(Debug, Clone)]
pub struct ReplacementPair {
    pub key: Key,
    pub matcher: Matcher,
    pub new: String,
}

/// An ordered set of placeholder -> replacement pairs (Homebrew's `Relocation`).
#[derive(Debug, Clone, Default)]
pub struct Relocation {
    entries: Vec<ReplacementPair>,
}

impl Relocation {
    /// An empty table.
    pub fn new() -> Self {
        Relocation {
            entries: Vec::new(),
        }
    }

    /// Append a literal `old -> new` pair (mirrors `add_replacement_pair(..., path: false)`).
    pub fn add_literal(
        &mut self,
        key: Key,
        old: impl Into<String>,
        new: impl Into<String>,
    ) -> &mut Self {
        self.entries.push(ReplacementPair {
            key,
            matcher: Matcher::Literal {
                needle: old.into(),
                path_boundary: false,
            },
            new: new.into(),
        });
        self
    }

    /// Append a path-anchored literal pair (mirrors `add_replacement_pair(..., path: true)` for the
    /// pour direction where the wrapped value is still a literal). The left-boundary check of
    /// `RELOCATABLE_PATH_REGEX_PREFIX` is applied when this matcher is run.
    pub fn add_path_literal(
        &mut self,
        key: Key,
        old: impl Into<String>,
        new: impl Into<String>,
    ) -> &mut Self {
        self.entries.push(ReplacementPair {
            key,
            matcher: Matcher::Literal {
                needle: old.into(),
                path_boundary: true,
            },
            new: new.into(),
        });
        self
    }

    /// The ordered pairs, as added.
    pub fn entries(&self) -> &[ReplacementPair] {
        &self.entries
    }

    /// Whether the table has no pairs.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The `new` string for the first pair with the given key, if any.
    pub fn new_for(&self, key: Key) -> Option<&str> {
        self.entries
            .iter()
            .find(|p| p.key == key)
            .map(|p| p.new.as_str())
    }

    /// The replacement order Homebrew applies (`replace_text!`, spec §2 step 2):
    ///
    /// 1. collapse to a map keyed by the `old` matcher (last write wins on duplicate matchers);
    /// 2. sort the matchers so regexes (weight 999) come first, then string literals longest-first.
    ///
    /// With only literal matchers here, this is "by descending needle length", ties broken by
    /// original insertion order (stable sort), matching Ruby's stable `sort_by { ... }.reverse`.
    fn ordered(&self) -> Vec<&ReplacementPair> {
        // Collapse on the matcher's needle: last pair with a given needle wins.
        let mut seen: Vec<(&str, usize)> = Vec::new();
        for (idx, pair) in self.entries.iter().enumerate() {
            let Matcher::Literal { needle, .. } = &pair.matcher;
            if let Some(slot) = seen.iter_mut().find(|(n, _)| *n == needle.as_str()) {
                slot.1 = idx;
            } else {
                seen.push((needle.as_str(), idx));
            }
        }
        let mut chosen: Vec<&ReplacementPair> =
            seen.iter().map(|(_, i)| &self.entries[*i]).collect();
        // Ruby applies regexes (weight 999) first, then string literals longest-first. Here every
        // matcher is a literal, so this is a stable descending sort by needle length. Equal-length
        // distinct needles keep their collapsed insertion order; for the disjoint placeholder
        // tokens this crate uses, such ties never affect the result.
        chosen.sort_by_key(|p| std::cmp::Reverse(weight(p)));
        chosen
    }

    /// Apply every pair, in `ordered()` order, as a global find/replace over `bytes`.
    ///
    /// Returns `true` iff at least one pair changed the buffer (Homebrew's `replace_text!` return).
    /// `bytes` is replaced with the substituted content.
    pub fn replace_text(&self, bytes: &mut Vec<u8>) -> bool {
        let mut changed = false;
        for pair in self.ordered() {
            let Matcher::Literal {
                needle,
                path_boundary,
            } = &pair.matcher;
            if needle.is_empty() {
                continue;
            }
            if replace_literal(
                bytes,
                needle.as_bytes(),
                pair.new.as_bytes(),
                *path_boundary,
            ) {
                changed = true;
            }
        }
        changed
    }
}

/// Sort weight for the matcher (spec §2 step 2): regex => 999, literal => its byte length.
fn weight(pair: &ReplacementPair) -> usize {
    match &pair.matcher {
        Matcher::Literal { needle, .. } => needle.len(),
    }
}

/// Global, non-overlapping literal substitution over a byte buffer. When `path_boundary` is set,
/// only matches whose left edge satisfies `RELOCATABLE_PATH_REGEX_PREFIX` are replaced; the scan
/// continues past a rejected match without consuming it as a replacement. Returns whether anything
/// changed.
fn replace_literal(
    bytes: &mut Vec<u8>,
    needle: &[u8],
    replacement: &[u8],
    path_boundary: bool,
) -> bool {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    let mut changed = false;
    while i < bytes.len() {
        if bytes[i..].starts_with(needle) && (!path_boundary || left_boundary_ok(&out)) {
            out.extend_from_slice(replacement);
            i += needle.len();
            changed = true;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    if changed {
        *bytes = out;
    }
    changed
}

/// The `RELOCATABLE_PATH_REGEX_PREFIX` left-boundary test (spec §2), evaluated against the bytes
/// already emitted before the candidate match. Accept iff:
///   - nothing precedes (offset 0), or
///   - the immediately-preceding bytes are one of the compiler flags `-F`/`-I`/`-L`/`-isystem`, or
///   - the immediately-preceding byte is not `[A-Za-z0-9]`.
fn left_boundary_ok(prefix: &[u8]) -> bool {
    if prefix.is_empty() {
        return true;
    }
    const FLAGS: [&[u8]; 4] = [b"-F", b"-I", b"-L", b"-isystem"];
    if FLAGS.iter().any(|flag| prefix.ends_with(flag)) {
        return true;
    }
    let last = prefix[prefix.len() - 1];
    !last.is_ascii_alphanumeric()
}

/// Render a path as a UTF-8 string for use as a replacement value.
///
/// Relocation values are always paths Homebrew itself owns and are valid UTF-8 in every supported
/// install. A non-UTF-8 path (vanishingly unlikely) yields `None`, and `standard_relocation` simply
/// omits that pair — leaving the corresponding placeholder untouched rather than substituting a
/// lossily-decoded path, which is the conservative choice for the riskiest module.
fn path_string(path: &std::path::Path) -> Option<String> {
    path.to_str().map(|s| s.to_string())
}

/// Build the standard pour-direction relocation table (placeholders -> concrete paths).
///
/// This is the shared (non-OS) `prepare_relocation_to_locations` table (spec §5). All pairs are
/// **literal** placeholder -> path because placeholder tokens are unambiguous.
///
/// `:repository` is mapped **unconditionally** here (the pour direction). Only the bottling
/// direction (`prepare_relocation_to_placeholders`) skips it when `prefix == repository`; skipping
/// it here would leave `@@HOMEBREW_REPOSITORY@@` unreplaced on layouts where prefix == repository
/// (e.g. Apple Silicon `/opt/homebrew`), corrupting the installed files (spec §5).
///
/// The `perl` and `java` slots are resolved to their shared-version targets
/// (`<prefix>/opt/perl/bin/perl` and `<prefix>/opt/<openjdk>/libexec`); the OS-specific deeper
/// `.jdk/Contents/Home` / system-perl variants (spec §5 macOS override) are not modelled here
/// because they require receipt `runtime_dependencies`, which the caller does not yet plumb
/// through. Including the shared defaults is safe: a keg that never embedded `@@HOMEBREW_PERL@@` /
/// `@@HOMEBREW_JAVA@@` is simply unaffected.
pub fn standard_relocation(config: &Config) -> Relocation {
    let mut reloc = Relocation::new();
    let prefix = path_string(&config.prefix);
    let cellar = path_string(&config.cellar);
    let repository = path_string(&config.repository);
    let library = path_string(&config.library);

    if let Some(prefix) = &prefix {
        reloc.add_literal(Key::Prefix, PREFIX_PLACEHOLDER, prefix.clone());
    }
    if let Some(cellar) = &cellar {
        reloc.add_literal(Key::Cellar, CELLAR_PLACEHOLDER, cellar.clone());
    }
    // Always map :repository in the pour direction (placeholder tokens are distinct, so there is
    // no collision with :prefix even when the two paths coincide).
    if let Some(repository) = &repository {
        reloc.add_literal(Key::Repository, REPOSITORY_PLACEHOLDER, repository.clone());
    }
    if let Some(library) = &library {
        reloc.add_literal(Key::Library, LIBRARY_PLACEHOLDER, library.clone());
    }
    if let Some(prefix) = &prefix {
        reloc.add_literal(
            Key::Perl,
            PERL_PLACEHOLDER,
            format!("{prefix}/opt/perl/bin/perl"),
        );
    }
    reloc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Platform};

    fn config_with(prefix: &str, repository: &str, cellar: &str, library: &str) -> Config {
        let platform = Platform {
            is_macos: true,
            default_prefix: prefix.into(),
            default_repository: repository.into(),
        };
        Config::resolve(
            |k| match k {
                "HOMEBREW_PREFIX" => Some(prefix.to_string()),
                "HOMEBREW_REPOSITORY" => Some(repository.to_string()),
                "HOMEBREW_CELLAR" => Some(cellar.to_string()),
                "HOMEBREW_LIBRARY" => Some(library.to_string()),
                "HOME" => Some("/Users/me".to_string()),
                _ => None,
            },
            &platform,
        )
    }

    #[test]
    fn substitutes_each_placeholder() {
        let cfg = config_with(
            "/opt/homebrew",
            "/opt/homebrew/Homebrew",
            "/opt/homebrew/Cellar",
            "/opt/homebrew/Homebrew/Library",
        );
        let reloc = standard_relocation(&cfg);
        let mut buf = b"prefix=@@HOMEBREW_PREFIX@@ cellar=@@HOMEBREW_CELLAR@@".to_vec();
        assert!(reloc.replace_text(&mut buf));
        assert_eq!(
            buf,
            b"prefix=/opt/homebrew cellar=/opt/homebrew/Cellar".to_vec()
        );
    }

    #[test]
    fn no_change_returns_false() {
        let cfg = config_with(
            "/opt/homebrew",
            "/opt/homebrew/Homebrew",
            "/opt/homebrew/Cellar",
            "/opt/homebrew/Homebrew/Library",
        );
        let reloc = standard_relocation(&cfg);
        let mut buf = b"nothing to relocate here".to_vec();
        assert!(!reloc.replace_text(&mut buf));
        assert_eq!(buf, b"nothing to relocate here".to_vec());
    }

    #[test]
    fn repository_mapped_even_when_equal_to_prefix() {
        // arm64-mac style: prefix == repository. The pour direction must STILL map :repository,
        // otherwise @@HOMEBREW_REPOSITORY@@ would survive into the installed files.
        let cfg = config_with(
            "/opt/homebrew",
            "/opt/homebrew",
            "/opt/homebrew/Cellar",
            "/opt/homebrew/Library",
        );
        let reloc = standard_relocation(&cfg);
        assert_eq!(reloc.new_for(Key::Repository), Some("/opt/homebrew"));
        let mut buf = b"@@HOMEBREW_REPOSITORY@@/x".to_vec();
        assert!(reloc.replace_text(&mut buf));
        assert_eq!(buf, b"/opt/homebrew/x".to_vec());
    }

    #[test]
    fn repository_present_when_distinct() {
        let cfg = config_with(
            "/usr/local",
            "/usr/local/Homebrew",
            "/usr/local/Cellar",
            "/usr/local/Homebrew/Library",
        );
        let reloc = standard_relocation(&cfg);
        assert_eq!(reloc.new_for(Key::Repository), Some("/usr/local/Homebrew"));
        let mut buf = b"r=@@HOMEBREW_REPOSITORY@@".to_vec();
        assert!(reloc.replace_text(&mut buf));
        assert_eq!(buf, b"r=/usr/local/Homebrew".to_vec());
    }

    #[test]
    fn cellar_replaced_independently_of_prefix() {
        // Placeholder tokens are distinct strings, so cellar is not eaten by the prefix pair even
        // though the concrete cellar path lives under the concrete prefix.
        let cfg = config_with(
            "/opt/homebrew",
            "/opt/homebrew/Homebrew",
            "/opt/homebrew/Cellar",
            "/opt/homebrew/Homebrew/Library",
        );
        let reloc = standard_relocation(&cfg);
        let mut buf = b"@@HOMEBREW_CELLAR@@/wget/1.0".to_vec();
        assert!(reloc.replace_text(&mut buf));
        assert_eq!(buf, b"/opt/homebrew/Cellar/wget/1.0".to_vec());
    }

    #[test]
    fn longest_literal_applied_first() {
        // Two literals where one is a prefix of the other: the longer must win where it matches.
        let mut reloc = Relocation::new();
        reloc.add_literal(Key::Prefix, "@@A@@", "short");
        reloc.add_literal(Key::Cellar, "@@A@@B@@", "long");
        let mut buf = b"x@@A@@B@@y @@A@@z".to_vec();
        assert!(reloc.replace_text(&mut buf));
        assert_eq!(buf, b"xlongy shortz".to_vec());
    }

    #[test]
    fn perl_placeholder_resolves_to_prefix_path() {
        let cfg = config_with(
            "/opt/homebrew",
            "/opt/homebrew",
            "/opt/homebrew/Cellar",
            "/opt/homebrew/Library",
        );
        let reloc = standard_relocation(&cfg);
        let mut buf = b"#!@@HOMEBREW_PERL@@\n".to_vec();
        assert!(reloc.replace_text(&mut buf));
        assert_eq!(buf, b"#!/opt/homebrew/opt/perl/bin/perl\n".to_vec());
    }

    #[test]
    fn path_boundary_rejects_alphanumeric_left_edge() {
        let mut reloc = Relocation::new();
        reloc.add_path_literal(Key::Prefix, "/usr/local", "/opt/homebrew");
        // Preceded by 'x' (alphanumeric) => not a path boundary, must NOT replace.
        let mut buf = b"x/usr/local".to_vec();
        assert!(!reloc.replace_text(&mut buf));
        assert_eq!(buf, b"x/usr/local".to_vec());
    }

    #[test]
    fn path_boundary_accepts_non_alphanumeric_left_edge() {
        let mut reloc = Relocation::new();
        reloc.add_path_literal(Key::Prefix, "/usr/local", "/opt/homebrew");
        let mut buf = b"PATH=/usr/local/bin".to_vec();
        assert!(reloc.replace_text(&mut buf));
        assert_eq!(buf, b"PATH=/opt/homebrew/bin".to_vec());
    }

    #[test]
    fn path_boundary_accepts_compiler_flag_left_edge() {
        let mut reloc = Relocation::new();
        reloc.add_path_literal(Key::Prefix, "/usr/local", "/opt/homebrew");
        for flag in ["-F", "-I", "-L", "-isystem"] {
            let mut buf = format!("{flag}/usr/local/include").into_bytes();
            assert!(reloc.replace_text(&mut buf), "flag {flag} should match");
            assert_eq!(buf, format!("{flag}/opt/homebrew/include").into_bytes());
        }
    }

    #[test]
    fn path_boundary_accepts_offset_zero() {
        let mut reloc = Relocation::new();
        reloc.add_path_literal(Key::Prefix, "/usr/local", "/opt/homebrew");
        let mut buf = b"/usr/local/bin".to_vec();
        assert!(reloc.replace_text(&mut buf));
        assert_eq!(buf, b"/opt/homebrew/bin".to_vec());
    }

    #[test]
    fn replaces_inside_binary_buffer_with_nul_bytes() {
        let cfg = config_with(
            "/opt/homebrew",
            "/opt/homebrew",
            "/opt/homebrew/Cellar",
            "/opt/homebrew/Library",
        );
        let reloc = standard_relocation(&cfg);
        let mut buf = Vec::new();
        buf.extend_from_slice(b"\x00\x01");
        buf.extend_from_slice(b"@@HOMEBREW_PREFIX@@/lib");
        buf.push(NULL_BYTE);
        assert!(reloc.replace_text(&mut buf));
        let mut expected = Vec::new();
        expected.extend_from_slice(b"\x00\x01");
        expected.extend_from_slice(b"/opt/homebrew/lib");
        expected.push(NULL_BYTE);
        assert_eq!(buf, expected);
    }

    #[test]
    fn duplicate_matcher_last_wins() {
        let mut reloc = Relocation::new();
        reloc.add_literal(Key::Prefix, "@@X@@", "first");
        reloc.add_literal(Key::Cellar, "@@X@@", "second");
        let mut buf = b"@@X@@".to_vec();
        assert!(reloc.replace_text(&mut buf));
        assert_eq!(buf, b"second".to_vec());
    }
}

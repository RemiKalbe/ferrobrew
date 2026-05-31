# Bottle relocation (placeholder substitution + Mach-O/ELF dynamic linkage rewriting + receipt changed_files)

## Key reference files
- `Library/Homebrew/keg_relocate.rb`
- `Library/Homebrew/extend/os/mac/keg_relocate.rb`
- `Library/Homebrew/extend/os/linux/keg_relocate.rb`
- `Library/Homebrew/extend/os/mac/keg.rb`
- `Library/Homebrew/keg.rb`
- `Library/Homebrew/os/mac/mach.rb`
- `Library/Homebrew/os/linux/elf.rb`
- `Library/Homebrew/extend/pathname.rb`
- `Library/Homebrew/dev-cmd/bottle.rb`
- `Library/Homebrew/formula_installer.rb`
- `Library/Homebrew/tab/tab.rb`
- `Library/Homebrew/extend/os/mac/extend/pathname.rb`
- `Library/Homebrew/extend/os/mac/extend/pathname/os.rb`

## Specification
# Bottle Relocation Subsystem

Bottle relocation has two symmetric directions, both implemented as methods on the `Keg` class (a keg = the installed-formula directory `path` = `$CELLAR/<name>/<version>`):

- **Bottling (`brew bottle`)**: replace concrete absolute paths in the keg with **placeholder tokens** so the tarball is portable. Direction = "locations → placeholders". Returns `changed_files` (array of keg-relative `Pathname`s) which is stored in the receipt.
- **Pouring (`brew install` from a bottle)**: replace placeholder tokens back with the **target machine's** concrete paths. Direction = "placeholders → locations". Driven by the receipt's `changed_files` list.

Each direction does TWO things: (1) text-file substitution (and binary text-string substitution), and (2) dynamic-linkage rewriting (Mach-O on macOS, ELF on Linux).

## 1. Placeholder Tokens (constants on `Keg`, file `keg_relocate.rb` lines 9-16)

| Constant | String value | Maps to (env var / global) |
|---|---|---|
| `PREFIX_PLACEHOLDER` | `@@HOMEBREW_PREFIX@@` | `HOMEBREW_PREFIX` (e.g. `/opt/homebrew` on arm64, `/usr/local` on Intel, `/home/linuxbrew/.linuxbrew` on Linux) |
| `CELLAR_PLACEHOLDER` | `@@HOMEBREW_CELLAR@@` | `HOMEBREW_CELLAR` (= `HOMEBREW_PREFIX/Cellar` typically) |
| `REPOSITORY_PLACEHOLDER` | `@@HOMEBREW_REPOSITORY@@` | `HOMEBREW_REPOSITORY` (the brew git checkout root) |
| `LIBRARY_PLACEHOLDER` | `@@HOMEBREW_LIBRARY@@` | `HOMEBREW_LIBRARY` (= `HOMEBREW_REPOSITORY/Library`) |
| `PERL_PLACEHOLDER` | `@@HOMEBREW_PERL@@` | a perl interpreter path (computed at pour time, see §6) |
| `JAVA_PLACEHOLDER` | `@@HOMEBREW_JAVA@@` | an openjdk libexec path (only if an openjdk runtime dep exists) |
| `NULL_BYTE` | `"\x00"` (one literal NUL byte) | used by build-prefix binary patching |
| `NULL_BYTE_STRING` | `"\\x00"` (the 4-char literal string backslash-x-0-0) | passed to grep, which interprets it as a NUL byte |

## 2. `Relocation` class (the substitution table) — `keg_relocate.rb` lines 18-69

Holds an ordered map `@replacement_map : Hash[Symbol, [old(String|Regexp), new(String)]]`. Keys used: `:prefix, :cellar, :repository, :library, :perl, :java`, plus (in `new_usr_local` mode) the new_usr_local keys listed in §3. Rust: model as a `Vec<(Key, OldMatcher, String)>` preserving insertion order; `OldMatcher` is either a literal string or a compiled regex.

`add_replacement_pair(key, old, new, path: false)`: if `path:true`, `old` is first transformed by `path_to_regex` (see below) and stored as a Regex; otherwise stored literally.

`path_to_regex(path)` (line 59): builds a Regex. If `path` is a String it is `Regexp.escape`d; if already a Regex its `.source` is used. The result is `RELOCATABLE_PATH_REGEX_PREFIX + escaped_path` where:
```
RELOCATABLE_PATH_REGEX_PREFIX = /(?:(?<=-F|-I|-L|-isystem)|(?<![a-zA-Z0-9]))/
```
Meaning: a path match is only valid if immediately preceded by one of the compiler-flag tokens `-F`/`-I`/`-L`/`-isystem`, OR not preceded by an alphanumeric char. This prevents matching e.g. `/usr/local` inside a longer identifier. Rust regex crate lacks lookbehind — must reimplement with manual boundary checks: at each candidate match offset, accept iff the preceding bytes are one of `-F`,`-I`,`-L`,`-isystem` immediately before, or the preceding byte is not `[A-Za-z0-9]` (or offset==0).

`replace_text!(text)` (line 43) — IMPORTANT ORDERING:
1. Collapse `@replacement_map.values.to_h` into a hash keyed by the `old` matcher → `new` string. (If two keys had the same `old`, last wins.)
2. Sort the keys (the `old` matchers): `sort_by { key.is_a?(String) ? key.length : 999 }.reverse`. So **String matchers are applied longest-first; all Regex matchers get sort weight 999 and thus come first (before any String)**. Net order applied: regexes first (relative order among equal-999 is Ruby's stable sort over the hash's insertion order), then string literals descending by length.
3. For each matcher in that order, do an in-place global substitution (`gsub!`) over `text`. Track whether ANY substitution changed the text. Return `true` iff at least one matcher modified the text.

Rust: `replace_text!` returns `(modified: bool, new_bytes)`. Apply each matcher as a global find/replace over the byte buffer in the sorted order. Note `\\1` backrefs are used in the perl regex replacement (see §5) — your replacement engine must support capture-group backreferences in the replacement string.

## 3. Building the "→ placeholders" table — `prepare_relocation_to_placeholders` (lines 143-170)

`new_usr_local_relocation?` (private, line 464): `true` iff `HOMEBREW_PREFIX == "/usr/local"` AND (the formula is unavailable/has no tap, OR the formula's tap does NOT list this formula in `tap.disabled_new_usr_local_relocation_formulae`). Otherwise `false`. On non-`/usr/local` prefixes always `false`.

If `new_usr_local_relocation` is true, add these pairs (all `path: true`), from `new_usr_local_replacement_pairs` (lines 93-141). `name` = formula name. These exist to avoid clobbering system `/usr/local/bin` etc:

| key | old | new |
|---|---|---|
| `:prefix` | `/usr/local/opt` | `@@HOMEBREW_PREFIX@@/opt` |
| `:caskroom` | `/usr/local/Caskroom` | `@@HOMEBREW_PREFIX@@/Caskroom` |
| `:etc_name` | `/usr/local/etc/<name>` | `@@HOMEBREW_PREFIX@@/etc/<name>` |
| `:var_homebrew` | `/usr/local/var/homebrew` | `@@HOMEBREW_PREFIX@@/var/homebrew` |
| `:var_www` | `/usr/local/var/www` | `@@HOMEBREW_PREFIX@@/var/www` |
| `:var_name` | `/usr/local/var/<name>` | `@@HOMEBREW_PREFIX@@/var/<name>` |
| `:var_log_name` | `/usr/local/var/log/<name>` | `@@HOMEBREW_PREFIX@@/var/log/<name>` |
| `:var_lib_name` | `/usr/local/var/lib/<name>` | `@@HOMEBREW_PREFIX@@/var/lib/<name>` |
| `:var_run_name` | `/usr/local/var/run/<name>` | `@@HOMEBREW_PREFIX@@/var/run/<name>` |
| `:var_db_name` | `/usr/local/var/db/<name>` | `@@HOMEBREW_PREFIX@@/var/db/<name>` |
| `:share_name` | `/usr/local/share/<name>` | `@@HOMEBREW_PREFIX@@/share/<name>` |

Else (normal): single pair `:prefix` old=`HOMEBREW_PREFIX` new=`@@HOMEBREW_PREFIX@@` (path:true).

Then ALWAYS (both modes):
- `:cellar` old=`HOMEBREW_CELLAR` new=`@@HOMEBREW_CELLAR@@` (path:true)
- `:repository` old=`HOMEBREW_REPOSITORY` new=`@@HOMEBREW_REPOSITORY@@` (path:true) — **SKIPPED if `HOMEBREW_PREFIX == HOMEBREW_REPOSITORY`** (to avoid ambiguity)
- `:library` old=`HOMEBREW_LIBRARY` new=`@@HOMEBREW_LIBRARY@@` (path:true)
- `:perl` old = regex `\A#![ \t]*(?:/usr/bin/perl\d\.\d+|<HOMEBREW_PREFIX>/opt/perl/bin/perl)( |$)` (note `/o` once-flag and interpolated prefix), new = `"#!@@HOMEBREW_PERL@@\\1"` (backref `\1` re-inserts the trailing ` ` or end). **NOT path-wrapped** — added as a raw regex.
- `:java` old = `JAVA_REGEX`, new = `@@HOMEBREW_JAVA@@`. `JAVA_REGEX` (line 91) = `%r{<HOMEBREW_PREFIX>/opt/openjdk(@\d+(\.\d+)*)?/libexec(/openjdk\.jdk/Contents/Home)?}`. **NOT path-wrapped.**

## 4. Bottling flow (`replace_locations_with_placeholders`, lines 172-177)

```
relocation = prepare_relocation_to_placeholders.freeze
relocate_dynamic_linkage(relocation, skip_protodesc_cold: true)   # Mach-O/ELF rewrite, see §7/§8
changed_files = replace_text_in_files(relocation)                 # text substitution, see §9
return changed_files
```

Driven from `dev-cmd/bottle.rb` (~line 491-604): inside `keg.lock`, it first `keg.delete_pyc_files!`, then `changed_files = keg.replace_locations_with_placeholders` (unless `--skip-relocation`). After clearing caches, it reads `tab = keg.tab`, sets `tab.changed_files = changed_files.dup`, writes the receipt (or, with `--only-json-tab`, deletes the receipt filename `AbstractTab::FILENAME` (= `"INSTALL_RECEIPT.json"`) from the list and unlinks the tabfile). Creates the SBOM, makes symlink perms reproducible (`consistent_reproducible_symlink_permissions!` → `lchmod 0777` on every symlink), tars+gzips. In the `ensure` block it ALWAYS reverses the relocation: `keg.replace_placeholders_with_locations(changed_files)` so the on-disk keg is restored to concrete paths after the tarball is built. `skip_relocation` for the bottle metadata = `relocatable && !keg.require_relocation?` (i.e. if no dynamic-linkage edits were needed, the bottle can be poured without re-running linkage fixes).

## 5. Building the "→ locations" table — `prepare_relocation_to_locations` (lines 179-192), used at pour time

Shared (non-OS) version adds **literal string** pairs (NOT path-wrapped — placeholders are unambiguous tokens):
- `:prefix` `@@HOMEBREW_PREFIX@@` → `HOMEBREW_PREFIX`
- `:cellar` `@@HOMEBREW_CELLAR@@` → `HOMEBREW_CELLAR`
- `:repository` `@@HOMEBREW_REPOSITORY@@` → `HOMEBREW_REPOSITORY`
- `:library` `@@HOMEBREW_LIBRARY@@` → `HOMEBREW_LIBRARY`
- `:perl` `@@HOMEBREW_PERL@@` → `"<HOMEBREW_PREFIX>/opt/perl/bin/perl"`
- `:java` `@@HOMEBREW_JAVA@@` → `"<HOMEBREW_PREFIX>/opt/<openjdk_dep>/libexec"` — only if `openjdk_dep_name_if_applicable` returns non-nil.

`openjdk_dep_name_if_applicable` (line 201): from `runtime_dependencies` (the receipt's array of hashes), collect each dep's `"full_name"`, return the first matching `Version.formula_optionally_versioned_regex(:openjdk)` (i.e. `openjdk` or `openjdk@<version>`).

### macOS override of `prepare_relocation_to_locations` (mac keg_relocate.rb lines 226-255)
Calls `super` then OVERWRITES `:perl` and `:java`:
- Perl path resolution order: (a) if a runtime dep with `"full_name"=="perl"` AND `"declared_directly"` truthy, OR formula `name=="perl"` → `<HOMEBREW_PREFIX>/opt/perl/bin/perl`; else (b) if `tab.built_on["preferred_perl"]` matches `/^\d+\.\d+$/` and `/usr/bin/perl<that>` exists → that system perl; else (c) `/usr/bin/perl<MacOS.preferred_perl_version>`.
- Java: if openjdk dep applicable, `:java` → `<HOMEBREW_PREFIX>/opt/<openjdk>/libexec/openjdk.jdk/Contents/Home` (note the deeper `.jdk/Contents/Home` suffix vs the shared version).

## 6. Pour flow (`replace_placeholders_with_locations`, lines 194-199)
```
relocation = prepare_relocation_to_locations.freeze
relocate_dynamic_linkage(relocation) unless skip_linkage   # skip_linkage = bottle_spec.skip_relocation?
replace_text_in_files(relocation, files:)                  # files = tab.changed_files (the receipt list)
```
Called from `formula_installer.rb` ~line 1566-1568 after the receipt is written. If `ENV["HOMEBREW_RELOCATE_BUILD_PREFIX"]` is set and cellar/prefix differ, also calls `relocate_build_prefix` (§11).

## 7. macOS dynamic-linkage rewrite — `relocate_dynamic_linkage` (mac keg_relocate.rb lines 26-55)

Iterate `mach_o_files` (lines 206-224): walk `path.find`, skip symlinks/dirs, wrap as `MachOPathname` (a Pathname mixed with `MachOShim`); keep only files where `dylib? || mach_o_bundle? || mach_o_executable?`; dedupe by `[stat.dev, stat.ino]` (so hardlinks processed once). Mach-O type detection (`os/mac/mach.rb`): parse with ruby-macho; for FatFile iterate sub-machos; `filetype` `:dylib`→dylib, `:bundle`→bundle, `:execute`→executable. A file is `dylib?`/`mach_o_bundle?`/`mach_o_executable?` if ANY arch slice matches that type.

For each file, inside `ensure_writable` (chmod u+rw if needed, restore after):
1. `modified=false; needs_codesigning=false`.
2. If `dylib?`: compute new id via `relocated_name_for(file.dylib_id, relocation)`; if non-nil call `change_dylib_id(id, file)`; OR `needs_codesigning`.
3. For each library in `each_linkage_for(file, :dynamically_linked_libraries)`: `new = relocated_name_for(old, relocation)`; if non-nil `change_install_name(old, new, file)`; OR `needs_codesigning`.
4. For each rpath in `each_linkage_for(file, :rpaths)`: same with `change_rpath`.
5. If `needs_codesigning`: `codesign_patched_binary(file.to_s)` (§10).

`each_linkage_for(file, type, resolve_variable_references: false)` (lines 144-149): call `file.<type>(resolve_variable_references:)`, then `grep_v(VARIABLE_REFERENCE_RX)` to drop any name starting with `@loader_path`/`@executable_path`/`@rpath` (`VARIABLE_REFERENCE_RX = /^@(loader_|executable_|r)path/`), then yield each.

`relocated_name_for(old_name, relocation)` (lines 173-183): fetch `:prefix` and `:cellar` pairs. If `old_name` starts with `old_cellar` → substitute cellar; elsif starts with `old_prefix` → substitute prefix; else return nil (no change). NOTE at pour time `old_*` are the placeholder tokens; at bottling `old_*` are the path-regexes (but for Mach-O, `relocated_name_for` uses the pair's stored matcher which when path-wrapped is a Regex — `start_with?` on a Regex returns nil/false, so on macOS the install-name rewriting during BOTTLING effectively never matches via this path; the real bottling-direction placeholdering of install names relies on the path being already a placeholder. In practice install names already contain `@@HOMEBREW_*@@` because they are set at build time. For Rust: at pour time the matcher is a literal placeholder string — straightforward `starts_with` + replace. Cellar checked before prefix because cellar is a subdir of prefix.)

### Mach-O edit primitives (mac keg.rb lines 42-103) — each returns bool "modified", each calls `require_relocation!` and uses ruby-macho via `MachOShim`:
- `change_dylib_id(id, file)`: no-op (return false) if `file.dylib_id == id`; else `file.change_dylib_id(id, strict:false)` → ruby-macho `change_dylib_id` then `write!`. Equivalent to `install_name_tool -id <id> <file>`.
- `change_install_name(old, new, file)`: no-op if `old==new`; else `install_name_tool -change <old> <new> <file>` equivalent.
- `change_rpath(old, new, file)`: no-op if `old==new`; else `install_name_tool -rpath <old> <new> <file>` equivalent.
- `delete_rpath(rpath, file)` (used only by `fix_dynamic_linkage`, not bottling): `install_name_tool -delete_rpath` equivalent; `MachOShim#delete_rpath` deletes the LAST matching instance (resolving variable names) to preserve search order.
- All re-raise `MachO::MachOError` after `onoe`.

### MachOShim reads (os/mac/mach.rb):
- `dynamically_linked_libraries(resolve_variable_references:true)`: ruby-macho `dylib_load_commands`, map `.name`, `uniq`, then resolve `@loader_path`/`@executable_path`/`@rpath` if requested.
- `rpaths(resolve_variable_references:true)`: ruby-macho `rpaths`, resolve variable names (without recursing rpaths).
- `resolve_variable_name`: `@loader_path`→`dirname`; `@executable_path`→`dirname` (only if executable); `@rpath`→search rpaths for an existing file. Uses `cleanpath`.

## 8. Linux dynamic-linkage rewrite — `relocate_dynamic_linkage` (linux keg_relocate.rb lines 13-25)

**Skip entirely if formula name matches `Version.formula_optionally_versioned_regex(:glibc)`** (patching glibc's linker breaks it).

Fetch `:prefix` pair → `(old_prefix, new_prefix)`. Iterate `elf_files` (lines 90-108): `path.find`, skip symlink/dir, wrap `ELFPathname`, keep only `dylib? || binary_executable?`, dedupe by `[dev, ino]`. ELF detection (`os/linux/elf.rb`): magic `\x7fELF` at offset 0; OS/ABI byte at 0x07 must be 0 (System V) or 3 (Linux); type at 0x10 (2=executable→`:executable`, 3=shared→`:dylib`); arch at 0x12.

For each elf file, inside `ensure_writable`: `change_rpath!(file, old_prefix, new_prefix, skip_protodesc_cold:)`.

`change_rpath!` (lines 27-73):
1. Return false if `!file.elf? || !file.dynamic_elf?`.
2. If `skip_protodesc_cold` (true during bottling) AND `file.section_names` includes `"protodesc_cold"` → return false (patchelf corrupts these; only skipped at bottling time, not pouring, to not break existing bottles).
3. RPATH rewrite: take `file.rpath` (the `DT_RUNPATH` if present else `DT_RPATH`, raw colon string). Split on `:`, `sub(old_prefix, new_prefix)` each entry, then `select` only entries starting with `new_prefix` or `$ORIGIN` (drops foreign/build paths). Append `"<new_prefix>/lib"` if not already present. Then, UNLESS formula name matches `gcc` versioned regex, rewrite each entry's trailing `lib/gcc/<digits>` → `lib/gcc/current` (regex `%r{lib/gcc/\d+$}`). Join with `:`. Record `updated[:rpath]` only if changed from original raw `old_rpath`.
4. Interpreter rewrite: `old_interpreter = file.interpreter` (PT_INTERP). If nil → nil. Elsif `<new_prefix>/lib/ld.so` is readable → use it. Else `old_interpreter.sub(old_prefix, new_prefix)`. Record `updated[:interpreter]` only if changed.
5. If `updated` empty → return false. Else `file.patch!(interpreter: updated[:interpreter], rpath: updated[:rpath])` then `require_relocation!`; return true.

`file.patch!` (os/linux/elf.rb 146-151): no-op if both nil/blank; else `save_using_patchelf_rb`: sets `patcher.interpreter`/`patcher.rpath` (only when present) and `patcher.save(patchelf_compatible: true)`. Ruby uses the **patchelf.rb gem** (pure-ruby, `PatchELF::Patcher.new(path, on_error: :silent)`), NOT the `patchelf` binary. Behavior equals: `patchelf --set-interpreter <i> --set-rpath <r> <file>` writing DT_RUNPATH (patchelf-compatible mode forces RUNPATH). `section_names` = ELF section names (non-blank). `dynamic_elf?` = has a PT_DYNAMIC segment.

There is NO codesigning on Linux and NO dylib-id concept (`change_rpath!` is the whole operation; interpreter+rpath only).

## 9. Text-file substitution — `replace_text_in_files` (lines 217-247)

`files ||= text_files | libtool_files` (union, dedup). At pour time `files` = the receipt's `changed_files` (keg-relative). Each entry is joined to `path`. Group by `stat.ino` (so hardlinks share one read/write). For each inode group `(first, *rest)`:
1. Read `first` fully in binary mode (`open("rb", &:read)`).
2. Pick the relocation table: if `new_usr_local_relocation? && homebrew_created_file?(first)` use `prepare_relocation_to_placeholders(new_usr_local_relocation: false)` (full prefix replacement for Homebrew-generated service files), else use the passed `relocation`.
   - `homebrew_created_file?(file)`: basename starts with `"homebrew."` AND extname in `[".plist", ".service", ".timer"]`.
3. `next unless file_relocation.replace_text!(s)` — skip if no change.
4. `changed_files += [first, *rest].map { relative_path_from(path) }` — **records ALL hardlinks in the group as keg-relative paths**.
5. Write: try `first.atomic_write(s)` (write to temp then rename, preserving uid/gid/mode — see pathname.rb 105-145). On `SystemCallError` fallback: `first.ensure_writable { open("wb"){ write } }`. On success path (no exception), re-link the rest: `FileUtils.ln(first, file, force: true)` for each hardlink (recreates hardlinks broken by atomic_write's rename).

Returns `changed_files`.

### Detecting text vs binary files

`text_files` (lines 349-389):
- Returns empty if `which("file")` or `which("xargs")` missing.
- Walk `path.find`, build a Set, REJECTING: symlinks; directories; the file `.brew/<name>.rb`; files whose extname ∈ `Metafiles::EXTENSIONS`. KEEP `orig-prefix.txt` (python virtualenv marker) even though next checks might reject. If `pn.text_executable?` (starts with `#!` shebang — `/\A#!\s*\S+/` on first 1024 bytes) → add to `text_files` AND reject from the file-command set (shebang scripts are always text).
- Run `xargs -0 file --no-dereference --print0` with stdin = NUL-joined file list. Parse output: split each line on first `\0` into `path, info`; skip lines where `info` is nil (file prints multi-line output; continuation lines lack the NUL); keep file iff `info.include?("text")` and the path is in the original set. Force output encoding ASCII-8BIT (binary) before line iteration.

`libtool_files` (lines 391-401): walk `path.find`, keep non-symlink non-dir files whose extname ∈ `LIBTOOL_EXTENSIONS = [".la", ".lai"]`.

`binary_file?(file)` (lines 329-337): grep the file for `NULL_BYTE_STRING` ("\x00") with the OS grep args; presence of a NUL byte ⇒ binary. macOS `egrep_args` (mac keg_relocate.rb 264-269) = `["egrep", "--files-with-matches"]`; shared/Linux `egrep_args` (line 300) = `["grep", ["--files-with-matches","--perl-regexp","--binary-files=text"]]`.

`each_unique_file_matching(string)` (lines 312-327): `fgrep <recursive_fgrep_args> <string> <keg_path>`; recursive_fgrep_args = `"-lr"` (GNU/Linux) / `"-lrO"` (macOS, BSD, no-symlink-recurse). Yields each matching file once per unique inode, skipping symlinks.

## 10. Ad-hoc codesigning (macOS only) — `codesign_patched_binary` (mac keg.rb 108-150)

Required on arm64 because any byte change invalidates the existing signature and the kernel refuses to exec/load unsigned-but-was-signed arm64 binaries.

1. Return if `MacOS.version < :big_sur` (11). 
2. On non-arm (Intel): run `codesign --verify <file>`; return early UNLESS stderr matches `/invalid signature/i` (i.e. Intel only re-signs if the signature is actually broken). On arm: always proceed.
3. `prepare_codesign_writable_files(file)` (153-172): run `codesign --display --file-list - <file>` to list every file the signature covers (e.g. all slices/resources); for each not-writable file save its mode and `chmod u+rw`; restore modes in `ensure`.
4. First attempt: `quiet_system("codesign", "--sign", "-", "--force", "--preserve-metadata=entitlements,requirements,flags,runtime", file)`. `--sign -` = ad-hoc identity. Return on success.
5. If it failed: Apple-codesign-bug workaround — copy file to a tmp path (`Dir::Tmpname.create("workaround")`) then `mv` it back (`force:true`), changing the inode. Then retry the same `codesign` invocation (2nd try) via `system_command(..., print_stderr:false)`. Return on success; else `onoe` with stderr.

Rust: shell out to `/usr/bin/codesign`. Exact args (order matters): `codesign --sign - --force --preserve-metadata=entitlements,requirements,flags,runtime <file>`. Implement the verify-first short-circuit on Intel and the inode-swap retry.

## 11. Build-prefix relocation — `relocate_build_prefix(keg, old_prefix, new_prefix)` (lines 249-287)

Only when `ENV["HOMEBREW_RELOCATE_BUILD_PREFIX"]` is set and the bottle's cellar/prefix differ from the host's. Walks `each_unique_file_matching(old_prefix)`. Per file: skip unless `keg.binary_file?(file)` (only binaries need NUL padding); skip if `file.text_executable?` (shar archives break). Then: read binary, `split(NULL_BYTE, -1)` into NUL-delimited segments, find segments containing `old_prefix`, for each do `gsub(old_prefix, new_prefix).ljust(original_segment_size, NULL_BYTE)` (RIGHT-PAD with NULs so the segment keeps its byte length — required so total file size is unchanged and offsets stay valid). Rejoin with NUL. Raise if total size changed. `atomic_write`, then `codesign_patched_binary`. (This is the "old build path → new prefix" patch, distinct from placeholder relocation, for binaries that embed the build-time prefix as a string.)

## 12. `changed_files` in the receipt (Tab) — `tab/tab.rb`

`Tab#changed_files : Array[Pathname]?` (attr_accessor). Serialized in receipt JSON under key **`"changed_files"`** as `changed_files&.map(&:to_s)` (keg-relative path strings). Present in BOTH `to_json` (full `INSTALL_RECEIPT.json`, line 367) and `to_bottle_hash`/bottle-tab (line 388). Deserialized (tab.rb 84-85): `@changed_files = value&.map { Pathname(f) }`. The receipt filename constant is `AbstractTab::FILENAME = "INSTALL_RECEIPT.json"`. At bottling, if `--only-json-tab`, the receipt's own filename is removed from `changed_files` and the tabfile unlinked.

So the contract: bottling computes `changed_files` (union of every text/libtool/binary file actually modified by text substitution, expanded to include hardlinks), stores it in the receipt; pouring reads it back and re-runs text substitution on exactly those files (plus, separately, the Mach-O/ELF linkage pass over all binaries unless `skip_relocation`).

## 13. `require_relocation!` flag (keg.rb 215-264)
Boolean on Keg, default false. Set true by `change_dylib_id`/`change_install_name`/`change_rpath` (macOS) and `change_rpath!` (Linux, via `require_relocation!`). Read by bottling to decide `skip_relocation = relocatable && !keg.require_relocation?` — if no binary needed linkage edits, the poured bottle can skip the linkage pass.

## Rust implementation notes
## Data model
- `enum Placeholder` with the six tokens; a `const` table mapping each to its env-derived target. Resolve `HOMEBREW_PREFIX/CELLAR/REPOSITORY/LIBRARY` from env at startup (these come from the brew bash entrypoint; replicate the same env vars). Hold them as `PathBuf`/`String`.
- `struct Relocation { entries: Vec<ReplacementPair> }`, `struct ReplacementPair { key: Key, matcher: Matcher, new: String }`, `enum Matcher { Literal(String), Regex(regex::Regex) }`. Preserve insertion order; `replace_text!` sorts a working copy: regexes first, then literals by descending length. Build the sort key as `match matcher { Regex(_) => 999, Literal(s) => s.len() }`, sort descending (Ruby `.reverse` on ascending sort). Apply each in sequence over a `Vec<u8>` buffer; track a `changed` bool.
- Replacement strings contain `\1` backreferences (perl pair). Use the `regex` crate's `Regex::replace_all` with `$1` (convert `\\1`→`$1`), but note Ruby's perl regex has the once-flag `/o`; it still matches per call.

## Lookbehind workaround
The `regex` crate has NO lookbehind. `RELOCATABLE_PATH_REGEX_PREFIX` needs custom handling: do a literal/substring scan for the path, and at each candidate offset validate the left boundary manually (offset==0, or preceding bytes equal `-F`/`-I`/`-L`/`-isystem`, or preceding byte ∉ `[A-Za-z0-9]`). Implement as a hand-rolled scanner rather than one regex. Build it once per pair.

## Mach-O (macOS, arm64 + x86_64)
- Use the `object` crate to PARSE Mach-O (read dylib id = LC_ID_DYLIB, LC_LOAD_DYLIB names, LC_RPATH paths, filetype, fat slices, sections). For WRITING install names/ids/rpaths, the `object` crate does not edit load commands well; either (a) shell out to `/usr/bin/install_name_tool` with `-id`, `-change <old> <new>`, `-rpath <old> <new>`, `-delete_rpath`, which exactly mirrors what ruby-macho does, or (b) use a Mach-O editing crate. Shelling out to `install_name_tool` is the lowest-risk path and matches Homebrew's effective behavior; it also handles fat binaries transparently.
- Type detection: a file is dylib/bundle/executable if ANY arch slice has that `filetype` (`MH_DYLIB`, `MH_BUNDLE`, `MH_EXECUTE`).
- Variable-reference filter: skip any name matching `^@(loader_|executable_|r)path`. Resolve `@loader_path`→file dir, `@executable_path`→file dir (executables only), `@rpath`→search resolved rpaths for an existing file (use cleanpath/canonicalize WITHOUT following nonexistent).
- Dedup binaries by `(st_dev, st_ino)`.
- **Codesigning is mandatory after any edit on arm64.** Shell out: `codesign --sign - --force --preserve-metadata=entitlements,requirements,flags,runtime <file>`. On Intel, first `codesign --verify <file>` and only re-sign if stderr matches case-insensitive "invalid signature". Implement `codesign --display --file-list -` to chmod-writable the covered files first, and the copy-to-tmp+mv-back inode-swap retry on failure. Gate on macOS ≥ 11 (Big Sur). Use libc `stat` for dev/ino and mode save/restore.

## ELF (Linux)
- Recommended crate: `patchelf` is not in Rust; either shell out to the `patchelf` binary (`patchelf --set-interpreter <i> --set-rpath <r> <file>` — note `--set-rpath` writes DT_RUNPATH like patchelf-compatible mode) OR use the `goblin`/`object` crate to read + a writer. Reading (interpreter from PT_INTERP, rpath from DT_RUNPATH else DT_RPATH, section names, PT_DYNAMIC presence, DT_FLAGS_1) is easy with `goblin`. For writing, shelling out to `patchelf` is simplest and matches Homebrew semantics (it forces RUNPATH).
- Manual ELF header sniffing matches Ruby exactly: magic `\x7fELF` at 0; byte 0x07 ∈ {0,3}; type u16 at 0x10 (2=exec,3=dylib); arch u16 at 0x12. Use these to pre-filter without a full parse.
- Skip glibc formula entirely (name regex). Skip files with a `protodesc_cold` section when bottling (`skip_protodesc_cold=true`); do NOT skip when pouring.
- RPATH algorithm: split `:`, substitute prefix, keep only entries starting with new_prefix or `$ORIGIN`, ensure `<new_prefix>/lib` present, rewrite `lib/gcc/<N>$`→`lib/gcc/current` unless formula is gcc. Interpreter: prefer `<new_prefix>/lib/ld.so` if readable else prefix-substitute. No codesigning, no dylib-id.

## Text substitution & file classification
- Group files by inode; read once; substitute; on change, atomic-write (temp file + rename, preserve uid/gid/mode via `nix`/`libc`) then re-create hardlinks with `link(2)` (`FileUtils.ln force:true`). Record every hardlink in the group (keg-relative) into `changed_files`.
- Text detection: replicate `file --no-dereference` "text" check OR implement directly (no NUL byte in first N KB + valid-ish text heuristic). Homebrew literally shells out to `file`; matching it exactly means shelling out, but a Rust NUL-scan + UTF-8/ASCII heuristic is acceptable if you accept minor divergence. Always treat shebang files (`#!` in first 1024 bytes) as text. Always treat `.la`/`.lai` (libtool) as candidates. Skip `.brew/<name>.rb`, `Metafiles::EXTENSIONS`, symlinks, dirs. `binary_file?` = contains a NUL byte.
- `homebrew_created_file?`: basename starts with `"homebrew."` and ext ∈ {`.plist`,`.service`,`.timer`} → use full-prefix (non-new_usr_local) table even in /usr/local mode.

## Receipt
- JSON key is exactly `"changed_files"`, array of keg-relative path strings. Read it for pour, write it after bottling. Receipt file = `INSTALL_RECEIPT.json`. The bottle-tab subset (`to_bottle_hash`) also carries `changed_files`, `runtime_dependencies`, `source_modified_time`, `stdlib`, `compiler`, `arch`, `built_on`, `homebrew_version`.

## Gotchas
- Cellar matcher must be checked BEFORE prefix in install-name relocation (cellar is a prefix subdir).
- `:repository` pair omitted when prefix==repository.
- `path_to_regex` boundary semantics are load-bearing — getting them wrong over- or under-matches paths in scripts.
- atomic_write breaks hardlinks; you MUST re-link siblings or you silently un-share inodes.
- The bottling `ensure` block reverses relocation on disk — bottling is non-destructive to the working keg.

## Open questions
- ruby-macho's change_install_name/change_rpath/change_dylib_id with strict:false: confirm exact byte-level behavior vs install_name_tool (e.g. handling when new name is longer than old, LC_RPATH growth, header padding). If shelling out to install_name_tool in Rust, behavior should match but the in-place vs codesign-invalidation timing must be validated on arm64.
- Whether HOMEBREW_RELOCATABLE_INSTALL_NAMES (mac keg_relocate.rb loader_name_for) path is exercised during normal bottle pour — it only affects fix_dynamic_linkage (build-time), not the bottling/pour relocate_dynamic_linkage path; confirm it can be ignored for pure pour relocation in Rust.
- patchelf.rb gem's `patchelf_compatible: true` exact semantics (always DT_RUNPATH? handling of existing DT_RPATH removal) vs the patchelf C binary --set-rpath; verify the Rust shell-out or crate produces byte-identical dynamic section for reproducible bottles.
- MacOS.preferred_perl_version and tab.built_on['preferred_perl'] sourcing was not fully traced (defined in os/mac.rb / build environment); needs reading if perl-formula relocation fidelity matters.
- Metafiles::EXTENSIONS exact contents (require 'metafiles') not read — needed for an exact text_files reject list.

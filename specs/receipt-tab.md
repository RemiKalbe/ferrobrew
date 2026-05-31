# INSTALL_RECEIPT.json + Tab

## Key reference files
- `Library/Homebrew/tab.rb`
- `Library/Homebrew/tab/tab.rb`
- `Library/Homebrew/development_tools.rb`
- `Library/Homebrew/extend/os/mac/development_tools.rb`
- `Library/Homebrew/extend/os/linux/development_tools.rb`
- `Library/Homebrew/formula_installer.rb`
- `Library/Homebrew/utils/bottles.rb`
- `Library/Homebrew/resource.rb`
- `Library/Homebrew/api/formula.rb`
- `Library/Homebrew/api/internal.rb`
- `Library/Homebrew/formula.rb`
- `Library/Homebrew/bottle.rb`
- `Library/Homebrew/keg.rb`
- `Library/Homebrew/keg_relocate.rb`
- `Library/Homebrew/extend/pathname.rb`
- `Library/Homebrew/extend/file/atomic.rb`

## Specification
# INSTALL_RECEIPT.json (Tab) — Implementation Spec

## 0. File identity

- Filename constant: `AbstractTab::FILENAME = "INSTALL_RECEIPT.json"`.
- Lives at `<keg>/INSTALL_RECEIPT.json` where `<keg>` = `formula.prefix` = `HOMEBREW_CELLAR/<name>/<pkg_version>`. The `tabfile` for a fresh install is set to `formula.prefix/FILENAME`.
- One receipt per installed keg (per version). `keg.tab` == `Tab.for_keg(keg)`.

## 1. The two Tab classes

- `AbstractTab` (`tab.rb`) is the base; `Tab < AbstractTab` (`tab/tab.rb`) is the formula receipt (Casks have their own `Cask::Tab`, out of scope). All serialization for formulae is `Tab#to_json` in `tab/tab.rb`.
- All field reads tolerate missing keys (old receipts). Writes always emit the full current schema (minus conditional drops, see §4).

## 2. Field-by-field schema produced by `Tab#to_json` (THE on-disk format)

`to_json` builds a Ruby Hash in EXACTLY this insertion order, then `JSON.pretty_generate`s it. JSON object key order follows insertion order. The keys, in order:

1. `homebrew_version` — String. From in-memory `@homebrew_version`, originally `HOMEBREW_VERSION` = `ENV.fetch("HOMEBREW_VERSION")` (e.g. `"4.x.y"` or `">=4.x.y (shallow or no git repository)"`-style; just the raw env string). May be a git-describe string.
2. `used_options` — Array<String>. Build option flags. For a pour-from-bottle install this is forced to `[]` (see §3). Serialized via `used_options.as_flags` (Options→flag strings like `"--with-foo"`). Empty array for bottles.
3. `unused_options` — Array<String>. Same, forced `[]` for bottles.
4. `built_as_bottle` — Boolean. `true` when poured from a bottle (set in pour path). For built-from-source it's `build.bottle?`.
5. `poured_from_bottle` — Boolean. `true` for bottle pour, `false` for source build.
6. `loaded_from_api` — Boolean (nilable in memory but always a bool here). `formula.loaded_from_api?` — true when the formula definition came from the JSON API (`formula.jws.json`).
7. `loaded_from_internal_api` — Boolean. `formula.loaded_from_internal_api?`. NOTE: this key is in the real schema even though the prompt's example omitted it. Typically `false`.
8. `installed_on_request` — Boolean. Whether the user explicitly requested this formula (vs pulled in as a dependency). Computed at write time = `installed_on_request?` (the FormulaInstaller flag). There is NO `installed_as_dependency` key written by current Homebrew — "installed as dependency" is derived as `!installed_on_request`. (Older receipts / external tools may carry `installed_as_dependency`; current read code reads only `installed_on_request`. See `cmd/info.rb:189` TODO comment.)
9. `changed_files` — Array<String> or null. `changed_files&.map(&:to_s)`. These are keg-relative paths (e.g. `"lib/pkgconfig/x.pc"`, `".brew/foo.rb"`) that contained relocation placeholder tokens and were rewritten. For a bottle, this value is READ from the bottle's embedded tab (see §3/§6), not recomputed at pour time. When source-built without relocation, may be `null`.
10. `time` — Integer (Unix epoch seconds) or null. `Time.now.to_i` at install. For bottle pour, set fresh to `Time.now.to_i` at pour.
11. `source_modified_time` — Integer (Unix epoch seconds). Always `source_modified_time.to_i`; the getter is `Time.at(@source_modified_time || 0)` so a nil becomes `0`. For source build = `formula.source_modified_time.to_i`. For a bottle this comes from the bottle's embedded tab.
12. `stdlib` — String, CONDITIONALLY PRESENT. `stdlib&.to_s`. **Deleted from the hash if blank** (`attributes.delete("stdlib") if attributes["stdlib"].blank?`). On modern macOS/Linux bottles this is absent (nil), so the key does NOT appear. Only present for older C++-stdlib-tracked builds (`:libcxx`, `:libstdcxx`).
13. `compiler` — String. `compiler.to_s`. Getter falls back to `DevelopmentTools.default_compiler` when nil → `"clang"` on macOS, `"gcc"` on Linux. Stored in memory as a Symbol; serialized as its string.
14. `aliases` — Array<String> or null. `formula.aliases` (alias names this formula is known by). May be `[]`.
15. `runtime_dependencies` — Array<Object> or null. See §5 for element shape. For Homebrew < 1.1.6 the getter returns nil (guards against historically-wrong lists), but freshly written receipts always have current version so always an array (possibly empty `[]`).
16. `source` — Object. See §4 for nested shape and key order.
17. `arch` — String or null. Stored as Symbol `Hardware::CPU.arch` (e.g. `:arm64`, `:x86_64`); JSON renders the symbol as a string `"arm64"`.
18. `built_on` — Object or null. See §6.

`changed_files`, `time`, `aliases`, `runtime_dependencies`, `arch`, `built_on` may serialize as JSON `null` if nil — they are NOT compacted out (only `stdlib` is conditionally deleted). So `null` literally appears in the file for those when nil.

## 3. Exact computation when POURING a bottle loaded from the API

Path: `FormulaInstaller#pour` (formula_installer.rb ~1543–1564). Sequence:

1. `Tab.clear_cache`.
2. `tab = Utils::Bottles.load_tab(formula)` (utils/bottles.rb:120). For an API-loaded bottle (no local bottle file), `bottle_json_path` is nil and `formula.bottle_tab_attributes.presence` is used:
   - `formula.bottle_tab_attributes` → `T.must(bottle).tab_attributes` (formula.rb:3686; returns `{}` unless `bottled?`).
   - `Bottle#tab_attributes` (bottle.rb:170) → `github_packages_manifest_resource.tab` when the manifest resource is downloaded, else `{}`.
   - `BottleManifest#tab` (resource.rb:409) reads `manifest_annotations["sh.brew.tab"]` (a JSON STRING) from the OCI image manifest annotations and `JSON.parse`s it. This is the tab that was embedded at bottling time and is the source of `changed_files`, `source_modified_time`, `stdlib`, `compiler`, `built_on`, the original `runtime_dependencies`, and original `source` block.
   - `tab = Tab.from_file_content(tab_attributes.to_json, tabfile)`. `load_tab` returns this tab early ONLY if `tab.built_on&.["os"] == HOMEBREW_SYSTEM` (matching OS). Otherwise it falls through and recomputes runtime_dependencies via `Tab.runtime_deps_hash(formula, formula.runtime_dependencies(read_from_tab: false))`.
3. Back in `pour`, the loaded tab is overwritten ("fill in missing/outdated parts", kept in sync with `Tab#to_bottle_hash`):
   - `tab.used_options = []`
   - `tab.unused_options = []`
   - `tab.built_as_bottle = true`
   - `tab.poured_from_bottle = true`
   - `tab.loaded_from_api = formula.loaded_from_api?`  → `true` for API installs
   - `tab.loaded_from_internal_api = formula.loaded_from_internal_api?`
   - `tab.installed_on_request = installed_on_request?`  (the installer's user-intent flag)
   - `tab.time = Time.now.to_i`  (fresh timestamp, NOT the bottle build time)
   - `tab.aliases = formula.aliases`
   - `tab.arch = Hardware::CPU.arch`  (the INSTALLING machine's arch, as Symbol)
   - `tab.source["versions"]["stable"] = formula.stable.version.to_s`
   - `tab.source["versions"]["version_scheme"] = formula.version_scheme`
   - `tab.source["path"] = formula.specified_path.to_s`  → for API formula this is `Homebrew::API::Formula.cached_json_file_path` = `HOMEBREW_CACHE_API/"formula.jws.json"` (constant `DEFAULT_API_FILENAME = "formula.jws.json"`). For internal API: `Homebrew::API::Internal.cached_packages_json_file_path` = `HOMEBREW_CACHE_API/<packages_endpoint>`.
   - `tab.source["tap_git_head"] = formula.tap&.installed? ? formula.tap&.git_head : nil` → `null` when the tap (e.g. homebrew/core) is NOT installed locally (the common API case), else the tap's git HEAD sha.
   - `tab.tap = formula.tap`  → sets `source["tap"]` to tap name string (e.g. `"homebrew/core"`).
4. `tab.write` (atomic write — §7).
5. `keg.replace_placeholders_with_locations(tab.changed_files, skip_relocation:)` — uses the `changed_files` list to rewrite placeholder tokens (§8) inside the staged keg. This consumes but does not modify the serialized `changed_files`.

So fields whose values come straight from the bottle's embedded tab (READ, then re-written verbatim): `homebrew_version` (whatever bottling brew wrote — NOT the current brew's version, because `homebrew_version` is never reassigned in the pour path), `changed_files`, `source_modified_time`, `stdlib`, `compiler`, `built_on`, the `source` sub-keys `spec`/`versions.head`/`scm_revision` (unless overwritten), and `runtime_dependencies` (unless OS mismatch forces recompute). Fields freshly computed on the installing machine: `used_options`, `unused_options`, `built_as_bottle`, `poured_from_bottle`, `loaded_from_api`, `loaded_from_internal_api`, `installed_on_request`, `time`, `aliases`, `arch`, `source.versions.stable`, `source.versions.version_scheme`, `source.path`, `source.tap_git_head`, `source.tap`.

After pour, in `caveats`/`finish` (formula_installer.rb ~1016–1021) the tab is updated AGAIN with actual runtime deps and re-written: `tab.runtime_dependencies = Tab.runtime_deps_hash(formula, formula.runtime_dependencies(read_from_tab: false)); tab.write`. So the final receipt's `runtime_dependencies` reflects locally-resolved deps, not the bottle's.

## 4. `source` object — nested shape and key order

For a freshly poured/built formula, `source` accumulates keys in this order (as set across `AbstractTab.create` + `Tab.create`/pour):

```
"source": {
  "tap":          <String|null>,   # tap name e.g. "homebrew/core"; set last via tab.tap=
  "tap_git_head": <String|null>,   # tap HEAD sha, or null if tap not installed
  "spec":         <"stable"|"head">,  # formula.active_spec_sym.to_s
  "path":         <String>,           # formula.specified_path.to_s
  "scm_revision": <String>,           # OPTIONAL, only for git/hg HEAD specs with cached download
  "versions": {
    "stable":                <String|null>,  # formula.stable.version.to_s
    "head":                  <String|null>,  # formula.head.version.to_s
    "version_scheme":        <Integer>,       # formula.version_scheme, default 0
    "compatibility_version": <Integer|null>   # formula.compatibility_version (usually absent/null)
  }
}
```

Notes:
- Insertion order in `AbstractTab.create` writes `tap`, `tap_git_head` first. `Tab.create` then adds `spec`, `path`, optional `scm_revision`, `versions`. The pour path mutates existing keys in place (does not reorder). Practically the on-disk order is `spec, versions, path, tap_git_head, tap` is NOT guaranteed — order is whatever the *last writer* produced. For a bottle the `source` block initially comes from the embedded tab JSON (whatever key order it had), then individual keys are reassigned (reassignment does not move position in a Ruby Hash). New keys (`path`, `tap_git_head`, `tap`) appended in assignment order. **For Rust: do not assume a fixed order for `source`'s keys; match Homebrew's by preserving the embedded tab's key order and appending newly-set keys (`path`, `tap_git_head`, `tap`) at the end if absent.** The example in the prompt shows `spec, versions, path, tap_git_head, tap`.
- `versions.version_scheme` defaults to `0`. `empty_source_versions` = `{"stable"=>nil,"head"=>nil,"version_scheme"=>0,"compatibility_version"=>nil}`.
- `scm_revision` only added by `Tab.create` (source build of git/hg) when `downloader.cached_location.exist?` and `downloader.source_revision.present?`. Absent for normal bottle pours unless present in embedded tab.
- On READ (`from_file_content`, tab/tab.rb:105): backfills `source["spec"]` from the directory basename's PkgVersion (`"head"` if head?, else `"stable"`) when missing; backfills `source["versions"]` to `empty_source_versions` when missing; coerces `versions["stable"]` and `versions["head"]` empty-strings to nil (`.presence`); remaps legacy taps: `tab.tapped_from` → `tap` unless it's `"path or URL"`; `"mxcl/master"` and `"Homebrew/homebrew"` → `"homebrew/core"`.

## 5. `runtime_dependencies` element shape

Built by `Tab.runtime_deps_hash(formula, deps)` → for each dep `formula_to_dep_hash(dep.to_formula, formula.deps.map(&:name))` (tab.rb:160). Hash built then `.compact` (drops nil values):

```
{
  "full_name":             <String>,            # dep formula full name e.g. "openssl@3"
  "version":               <String>,            # dep.version.to_s
  "revision":              <Integer>,           # dep formula revision (0 if unset)
  "bottle_rebuild":        <Integer>,           # OPTIONAL: formula.bottle&.rebuild; dropped if nil
  "pkg_version":           <String>,            # dep.pkg_version.to_s (version + _revision)
  "declared_directly":     <Boolean>,           # whether dep name is in the parent formula.deps
  "compatibility_version": <Integer>            # OPTIONAL: dep formula.compatibility_version; dropped if nil
}
```

Key insertion order: `full_name, version, revision, bottle_rebuild, pkg_version, declared_directly, compatibility_version`. After `.compact`, `bottle_rebuild` and `compatibility_version` are typically absent (nil). The prompt's example listing `full_name, version, revision, pkg_version, declared_directly` matches the post-compact common case. `revision` is `0` (NOT dropped — only nil is dropped, and revision defaults to integer 0). `declared_directly` is `true`/`false` (boolean, never dropped).

## 6. `built_on` object — `DevelopmentTools.build_system_info`

Base (`development_tools.rb:180`), key order:
```
"os":         HOMEBREW_SYSTEM,                  # ENV["HOMEBREW_SYSTEM"]: "Macintosh" on macOS, "Linux" on Linux
"os_version": OS_VERSION,                       # ENV["HOMEBREW_OS_VERSION"]: e.g. "macOS 15", "Ubuntu 22.04"
"cpu_family": Hardware::CPU.family.to_s         # e.g. "arm64", "westmere", etc.
```

macOS prepend (`extend/os/mac/development_tools.rb:73`) merges these AFTER base (so appended in this order):
```
"xcode":          MacOS::Xcode.version.to_s.presence,   # String or null
"clt":            MacOS::CLT.version.to_s.presence,     # Command Line Tools version String or null
"preferred_perl": MacOS.preferred_perl_version          # String or null
```
Resulting macOS order: `os, os_version, cpu_family, xcode, clt, preferred_perl`.

Linux prepend (`extend/os/linux/development_tools.rb:67`) merges:
```
"glibc_version":     OS::Linux::Glibc.version.to_s.presence,
"oldest_cpu_family": Hardware.oldest_cpu.to_s
```
Resulting Linux order: `os, os_version, cpu_family, glibc_version, oldest_cpu_family`.

`build_system_info` returns `T::Hash[String, T.nilable(String)]` — values may be `null`. On a bottle pour, `built_on` is taken from the embedded bottle tab (the BUILD machine's info), NOT recomputed locally — but the early-return in `load_tab` requires `built_on["os"] == HOMEBREW_SYSTEM` for the embedded tab to be used as-is.

## 7. JSON serialization details (CRITICAL for byte-exact reproduction)

- Serializer: `JSON.pretty_generate(attributes)` (Ruby stdlib).
- Indentation: 2 spaces per nesting level.
- Object: `"{\n"` then `<indent>"key": value` per entry, separated by `",\n"`, then `"\n}"` at parent indent. Space after the colon (`": "`). No space before colon.
- Arrays: `"[\n"`, each element on its own line at increased indent, separated by `",\n"`, then `"\n]"`. Empty array renders as `[]` (no inner newlines). Empty object renders as `{}`.
- NO trailing newline appended by `pretty_generate` (verified: output ends with `}`). `atomic_write` writes the string verbatim with no added newline. **The receipt file therefore does NOT end with a newline.**
- Ruby Symbols serialize as their string form (`:arm64` → `"arm64"`, `:clang` → `"clang"`).
- `nil` → `null`. `true`/`false` → `true`/`false`. Integers unquoted.
- Unicode: standard JSON escaping (Ruby default does not escape non-ASCII; emits UTF-8 bytes). Forward slashes are NOT escaped.
- Key order = Hash insertion order (Ruby preserves insertion order). Reproduce the exact insertion order in §2 for top level and §4/§5/§6 for nested.

## 8. Placeholder relocation tokens (relevant to `changed_files`)

Defined in `keg_relocate.rb`:
- `PREFIX_PLACEHOLDER = "@@HOMEBREW_PREFIX@@"`
- `CELLAR_PLACEHOLDER = "@@HOMEBREW_CELLAR@@"`
- `REPOSITORY_PLACEHOLDER = "@@HOMEBREW_REPOSITORY@@"`
- `LIBRARY_PLACEHOLDER = "@@HOMEBREW_LIBRARY@@"`
- `PERL_PLACEHOLDER = "@@HOMEBREW_PERL@@"`
- `JAVA_PLACEHOLDER = "@@HOMEBREW_JAVA@@"`

`changed_files` (keg-relative path strings) are the files in which these tokens were found at bottling time. On pour, `keg.replace_placeholders_with_locations(changed_files, skip_relocation:)` rewrites tokens back to real absolute paths (`@@HOMEBREW_PREFIX@@` → `HOMEBREW_PREFIX`, etc.). The Tab itself only stores the file list; the token catalog above is used by the relocation engine, not stored in the receipt.

## 9. Read-back / parse path

- `Tab.from_file(path)`: caches by path; `File.read`; if content blank → `Tab.empty`; else `from_file_content`.
- `from_file_content`: `JSON.parse(content)` (raises wrapped error `"Cannot parse #{path}: ..."` on failure), sets `attributes["tabfile"] = path`, then constructs via `new(attributes)` then runs the legacy-fixups in §4.
- `new(attributes)` iterates attributes: special-cases `:installed_on_request` (nil→false, marks `@installed_on_request_present=true`) and `:changed_files` (maps strings to Pathname). All other keys set via `instance_variable_set(:"@#{key}", value)` — so unknown keys in the JSON are silently set as ivars and ignored on re-serialize (re-serialize only emits the known schema). This means a round-trip DROPS unknown/legacy keys like `tapped_from`, `installed_as_dependency`, `HEAD`, `compiler` symbol coercion, etc.
- Reading is lenient; writing is strict to the current schema.

## 10. `empty` tab (for not-installed formulae)

`Tab.empty` (tab/tab.rb:214) builds: `homebrew_version=HOMEBREW_VERSION, installed_on_request=false, loaded_from_api=false, loaded_from_internal_api=false, time=nil, runtime_dependencies=nil, arch=nil, source={path:nil,tap:nil,tap_git_head:nil}, built_on=DevelopmentTools.build_system_info`, then sets `used_options=[]`, `unused_options=[]`, `built_as_bottle=false`, `poured_from_bottle=false`, `source_modified_time=0`, `stdlib=nil`, `compiler=default_compiler`, `aliases=[]`, `source["spec"]="stable"`, `source["versions"]=empty_source_versions`.

## Rust implementation notes
Data model:

- Define `struct InstallReceipt` with fields in §2 order. Use `serde_json` with `serde_json::ser::PrettyFormatter` configured for 2-space indent (the default `pretty` uses 2 spaces — matches Ruby exactly). Crucially: serde's `to_string_pretty` ALSO emits no trailing newline, matching Ruby. Verify empty arrays render as `[]` and empty objects as `{}` — serde does this. Forward slashes not escaped by serde — matches.
- KEY ORDER: serde preserves struct field declaration order, so declare fields in the exact §2 order. For `source`, this is the tricky one — its key order is not fixed across paths. Use `serde_json::Map` (which preserves insertion order when the `preserve_order` feature is enabled — enable it: `serde_json = { features = ["preserve_order"] }`, backed by indexmap) OR a typed `Source` struct if you accept the canonical order `spec, versions, path, tap_git_head, tap, scm_revision`. The prompt example uses `spec, versions, path, tap_git_head, tap` — a typed struct with that field order is the pragmatic choice and matches a fresh pour; only differs from Ruby when round-tripping a weirdly-ordered embedded tab. For byte-exact round-trips through `load_tab`, use `preserve_order` + a generic `Map<String,Value>` for `source`.
- Optional/conditional fields:
  - `stdlib`: use `#[serde(skip_serializing_if = "Option::is_none")]` AND treat empty-string as none (Ruby drops on `.blank?` which includes ""). Implement a custom skip: store `Option<String>`, set to None when blank.
  - dep hash `bottle_rebuild` and `compatibility_version`: `#[serde(skip_serializing_if = "Option::is_none")]` (Ruby `.compact` drops nils). All other dep fields always emitted, including `revision: 0`.
  - `changed_files`, `time`, `aliases`, `runtime_dependencies`, `arch`, `built_on`: emit `null` when None (do NOT skip). So use `Option<T>` WITHOUT skip_serializing_if — serde emits `null`.
- Types: `homebrew_version: String`, `used_options: Vec<String>`, `unused_options: Vec<String>`, `built_as_bottle: bool`, `poured_from_bottle: bool`, `loaded_from_api: bool`, `loaded_from_internal_api: bool`, `installed_on_request: bool`, `changed_files: Option<Vec<String>>`, `time: Option<i64>`, `source_modified_time: i64` (always present; default 0), `stdlib: Option<String>` (skip if none), `compiler: String` (default "clang"/"gcc"), `aliases: Option<Vec<String>>`, `runtime_dependencies: Option<Vec<RuntimeDep>>`, `source: Source`, `arch: Option<String>`, `built_on: Option<BuildSystemInfo>`.
- `RuntimeDep`: `full_name: String, version: String, revision: i64, bottle_rebuild: Option<i64> (skip), pkg_version: String, declared_directly: bool, compatibility_version: Option<i64> (skip)`.
- `Source`: `spec: String, versions: SourceVersions, path: String, scm_revision: Option<String> (skip), tap_git_head: Option<String>, tap: Option<String>`. `SourceVersions`: `stable: Option<String>, head: Option<String>, version_scheme: i64 (default 0), compatibility_version: Option<i64>`.
- `BuildSystemInfo`: platform-conditional. macOS: `os, os_version, cpu_family, xcode: Option<String>, clt: Option<String>, preferred_perl: Option<String>`. Linux adds `glibc_version, oldest_cpu_family`. Use `#[cfg]` or a generic `Map<String,Value>` to keep platform key ordering. Keys emit `null` when value is None (Ruby `.presence` yields nil → null).

Atomic write (match `File.atomic_write`):
- Create a tempfile in the SAME directory as the target (`Tempfile.open(".<basename>", dir)` — leading dot prefix). Use `tempfile` crate's `NamedTempFile::new_in(dir)` or `tempfile_in`.
- Write bytes (binmode — no newline translation; on Unix this is moot). No trailing newline.
- Copy permissions: if target exists, stat it and apply its uid/gid (chown) and mode (chmod) to the tempfile; ignore EPERM/EACCES. If target doesn't exist, probe the directory's default perms by touching a temp file and stat'ing it. Use `std::os::unix::fs::PermissionsExt` and `nix`/`libc` for `chown`. On macOS, mode includes ACL-affecting bits — `chmod` mirrors Ruby.
- `rename(tempfile, target)` — atomic on same filesystem (`std::fs::rename`).
- After rename, Pathname#atomic_write ALSO re-applies the old file's uid/gid/mode (a second chown/chmod pass), tolerating EPERM/EACCES. Replicate or fold into the single pass.

Gotchas:
- `homebrew_version` is NOT updated when pouring a bottle — it carries whatever the bottling machine wrote (read from embedded tab). Only `create`/`empty` set it to local `HOMEBREW_VERSION`. Don't overwrite on pour.
- `time` IS reset to now on pour; `source_modified_time` is NOT (comes from bottle).
- `arch` and `compiler` are Ruby Symbols in memory but always serialize as strings — store as String in Rust.
- No `installed_as_dependency` key is written by current Homebrew. Derive "as dependency" as `!installed_on_request`. Reading should ignore any `installed_as_dependency` present in legacy files (round-trip drops it).
- Unknown JSON keys on read are accepted and dropped on rewrite — use `#[serde(default)]` on all optional fields and do NOT use `deny_unknown_fields`.
- The second `tab.write` after post_install replaces `runtime_dependencies` with locally-resolved deps; ensure your install flow writes the receipt at least twice (once in pour, once after dependency resolution) or computes final runtime deps before the single write.
- Codesigning: not directly part of receipt writing, but the keg's binaries are relocated using `changed_files` before/around the write; that is a separate subsystem.
- Symlinks: `formula.prefix` may be reached via `opt_prefix`/`linked_keg` symlinks in read paths (`for_formula`), but the receipt is written to the real keg dir (`formula.prefix/FILENAME`). Resolve symlinks when locating an existing receipt (Ruby uses `resolved_path`).

## Open questions
- The on-disk key order of the `source` object is not deterministic across code paths (fresh pour vs round-tripped embedded tab) because Ruby Hash preserves insertion/assignment order and the pour path mutates an existing hash. A typed Source struct gives the common fresh-pour order (spec, versions, path, tap_git_head, tap) but may differ byte-for-byte from a receipt produced by re-serializing an embedded bottle tab. Confirm whether ferrobrew needs byte-exact round-trips or only needs to match fresh installs.
- Whether ferrobrew must reproduce the legacy read-fixups (tapped_from remap, mxcl/master and Homebrew/homebrew -> homebrew/core, empty-string version coercion, spec backfill from dir basename) for compatibility with very old receipts, or can assume modern receipts only.
- The exact format of `Hardware::CPU.family.to_s` and `Hardware.oldest_cpu.to_s` strings (microarchitecture names) was not enumerated here; needs a separate hardware-detection spec to fully reproduce `built_on.cpu_family`/`oldest_cpu_family`.
- `MacOS::Xcode.version`, `MacOS::CLT.version`, `MacOS.preferred_perl_version`, `OS::Linux::Glibc.version` formats (and when they are nil/blank) are external to tab.rb and need their own specs to reproduce `built_on` exactly.

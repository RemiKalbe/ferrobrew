# ferrobrew architecture (synthesized from subsystem specs)

# ferrobrew — Crate Architecture

## 0. Integration boundary (verified against `brew.sh`)

`brew.sh` pre-computes and **exports every `HOMEBREW_*` path/config var**, then for an allowlisted command set (`autoremove cleanup fetch search info list outdated postinstall install reinstall update-report upgrade uninstall rs-*`) on arm64 macOS / arm64+x86_64 Linux, execs `vendor/ferrobrew/ferrobrew <command> <args…>`. Bypasses ferrobrew entirely when `HOMEBREW_NO_INSTALL_FROM_API` is set, in non-developer mode, or when the re-entrancy guard env `HOMEBREW_FERROBREW_DISABLE` is present.

**Consequences for the crate:**
- ferrobrew **does NOT recompute the path derivation logic** from `brew.sh` §1 of the CLI spec. It reads `HOMEBREW_PREFIX`, `HOMEBREW_CELLAR`, `HOMEBREW_CACHE`, etc. via `env::var` (failing loudly if absent, mirroring `startup.rb`'s `raise unless HOMEBREW_BREW_FILE`). The full Bash-derivation port is deferred to a far-future "standalone" milestone and is out of scope for parity.
- `HOMEBREW_NO_INSTALL_FROM_API` never reaches us as an active path — the public JSON-API path is the only backend we must implement first. `HOMEBREW_USE_INTERNAL_API` is a separate later backend.
- The existing `src/fallback.rs` (`exec_ruby` sets `HOMEBREW_FERROBREW_DISABLE=1`) is the universal escape hatch: any command, sub-path, or formula shape we cannot yet handle byte-compatibly **re-execs Ruby instead of doing it wrong**. This is the single most important architectural lever — every native module returns a `Result<_, FerroError>` where one variant is `Unsupported(reason)`, which `main` turns into a Ruby fallback.

## 1. Module tree (`src/`)

```
src/
  main.rs               # arg[0] dispatch → command modules; Unsupported → fallback::exec_ruby
  fallback.rs           # (exists) re-exec Ruby brew with HOMEBREW_FERROBREW_DISABLE=1
  error.rs              # FerroError enum, incl. Unsupported(Cow<str>) → drives fallback
  config/
    mod.rs              # Config: reads exported HOMEBREW_* env into typed PathBufs + flags
    env_config.rs       # EnvConfig: the ENVS registry (boolean/string/int + FALSY_VALUES)
    placeholders.rs     # const JSON placeholders ($HOMEBREW_PREFIX) + keg tokens (@@HOMEBREW_PREFIX@@)
  system/
    mod.rs              # SimulateSystem: current Os/Arch, current_tag() e.g. "arm64_sequoia"
    macos_version.rs    # MacOSVersion + SYMBOLS table (tahoe..catalina), Version::NULL sentinel
    tag.rs              # BottleTag parse/format, valid os/arch combinations
  cli/
    mod.rs
    parser.rs           # hand-rolled Parser builder (switch/flag/comma_array/conflicts/depends_on)
    args.rs             # Args (HashMap<String,ArgValue>), NamedArgs, os_arch_combinations
    error.rs            # UsageError variants w/ exact Homebrew message strings
    spec.rs             # per-command flag inventories (install/list/info/... allowlist)
  api/
    mod.rs              # ApiClient: fetch_json_api_file (cond GET, fallback, mtime cache)
    jws.rs              # verify_and_parse_jws (RSA-PSS SHA512, embedded homebrew-1.pem)
    formula.rs          # bulk formula.jws.json → name→RawFormula map + aliases/renames
    cask.rs             # bulk cask.jws.json (later)
    tap_migrations.rs   # *_tap_migrations.jws.json
    cache.rs            # $HOMEBREW_CACHE/api layout, names/aliases index .txt files
    names.rs            # alias→rename→tap-migration resolution (FromAPILoader equivalent)
  formula/
    mod.rs              # Formula (resolved, installable) + Spec (stable/head)
    raw.rs              # RawFormula: serde model of one API JSON object (every field #[serde(default)])
    variations.rs       # merge_variations(base, current_tag) shallow merge, drop "variations"
    placeholders.rs     # recursive string replace over serde_json::Value (/$HOME, $HOMEBREW_*)
    struct_gen.rs       # generate_formula_struct: RawFormula → Formula (deps, bottle, keg_only…)
    pkg_version.rs      # PkgVersion (Version + _revision), Version ordering/version_scheme
  deps/
    mod.rs
    dependency.rs       # Dependency, UsesFromMacOS, Tag, predicates (build?/required?/…)
    expand.rs           # Dependency::expand — post-order DFS topo sort + merge_repeats
    uses_from_macos.rs  # use_macos_install?(bottle_os_version) OS-bound resolution
    requirement.rs      # Requirement (arch/linux/macos/maximum_macos/xcode), max-keep set
  bottle/
    mod.rs              # Bottle, BottleSpecification, CellarSpec (:any/:any_skip_relocation/path)
    filename.rs         # BottleFilename (double-dash github_packages, single-dash url_encode)
    ghcr.rs             # GhcrUrls: root_url, blob URL, manifest URL, image_formula_name, version_rebuild
    auth.rs             # GhcrAuth: HOMEBREW_GITHUB_PACKAGES_AUTH precedence ("Bearer QQ==")
    manifest.rs         # OCI image index parse: sh.brew.tab / digest / ref.name / sizes
  download/
    mod.rs              # DownloadQueue (bounded concurrency), RetryableDownload, .poured marker
    strategy.rs         # CurlGitHubPackages strategy: resolved_basename, headers, artifact-domain
    curl.rs             # curl wrapper (--location, --continue-at -, retries, header injection)
    cache.rs            # downloads/<sha256(url)>--<file>, .incomplete, relative symlink, flock
    integrity.rs        # sha256 verify (sha2), ChecksumMismatch
  install/
    mod.rs              # InstallSession (the threaded &mut context: attempted/fetched/installed/locked)
    plan.rs             # install_formula? gate, pour_bottle? decision (PourDecision enum)
    installer.rs        # FormulaInstaller: prelude → install → finish orchestration
    pour.rs             # pour(): rack.mkpath, move/extract from TEMP_CELLAR, fill tab, relocate
    finish.rs           # link, service, post_install hint, runtime-deps tab rewrite, SBOM (later)
    rollback.rs         # RAII KegGuard: uninstall partial keg on drop-without-commit
    lock.rs             # FormulaLock (flock on HOMEBREW_LOCKS), Drop-released
  keg/
    mod.rs              # Keg (path/name/rack/version), linked?/optlinked?, resolved_path (1-level)
    link.rs             # link/optlink/unlink, link_dir walker, LinkStrategy table, conflicts
    layout.rs           # keg_link_directories, must_exist_subdirectories, share/lib/include rules
    overwrite.rs        # link_overwrite? glob matching, backup-to-cache flow
  relocate/
    mod.rs              # Relocation table (ordered, regex-first/literal-longest sort), replace_text!
    boundary.rs         # RELOCATABLE_PATH_REGEX_PREFIX hand-rolled left-boundary scanner (no lookbehind)
    text.rs             # replace_text_in_files: inode-group, atomic write, re-link hardlinks
    classify.rs         # text vs binary detection (NUL scan / shebang / .la/.lai / skip lists)
    relocator.rs        # trait Relocator (OS-specific dynamic linkage) — see §3
    macos.rs            # MachoRelocator: install_name_tool + codesign (cfg(target_os="macos"))
    linux.rs            # ElfRelocator: patchelf rpath/interpreter (cfg(target_os="linux"))
  tab/
    mod.rs              # Tab/InstallReceipt serde struct, field order = §16/§2, atomic write
    runtime_deps.rs     # RuntimeDep element (compact), declared_runtime_dependencies
    built_on.rs         # BuildSystemInfo (os/os_version/cpu_family + OS-specific keys)
  commands/
    mod.rs
    install.rs          # InstallCmd::run — first native command
    list.rs info.rs outdated.rs search.rs fetch.rs cleanup.rs
    uninstall.rs reinstall.rs upgrade.rs autoremove.rs postinstall.rs
  util/
    atomic.rs           # atomic_write (temp-in-dir + rename + chown/chmod preserve)
    fs.rs               # relative symlink (pathdiff), rmdir_if_possible, inode grouping
    ohai.rs             # ohai/opoo/onoe/oh1 output helpers matching Homebrew formatting
```

## 2. Key data types

- **`Config`** — typed snapshot of exported env (`prefix`, `cellar`, `caskroom`, `cache`, `logs`, `temp`, derived `linked_kegs`, `locks`, `temp_cellar`, `cache_api`, …). Built once at startup; passed by `&` everywhere. Does NOT assume Cellar is under prefix.
- **`SimulateSystem` / `BottleTag`** — current `{arch}_{os_codename}` (e.g. `arm64_sequoia`, `x86_64_linux`). Drives variation merge + bottle selection.
- **`RawFormula`** — `serde`-backed model of one API JSON object, every optional field `#[serde(default)]`, no `deny_unknown_fields`. Holds `bottle.stable.files: HashMap<String, BottleFile>`, untagged dep enums, parallel `uses_from_macos`/`uses_from_macos_bounds`.
- **`Formula`** — resolved/installable form produced by `struct_gen` after variation-merge + placeholder substitution: name, `PkgVersion`, `BottleSpecification`, `Dependencies`, `Requirements`, `keg_only_reason`, `caveats`, `tap`, `tap_git_head`.
- **`Bottle` / `BottleSpecification` / `CellarSpec`** — `CellarSpec = Any | AnySkipRelocation | Path(String)`; carries `root_url`, per-tag `{cellar, url, sha256, rebuild}`. `skip_relocation()`, `compatible_locations()`.
- **`Dependency` / `UsesFromMacOS` / `Tag`** — `tags: Vec<Tag>` (order-significant for Eq/Hash), predicates, `expand()` topo result.
- **`Keg`** — `{path, name, rack, linked_keg_record, opt_record, version}`; `linked?`, `optlinked?`, one-level `resolved_path`.
- **`Tab` / `InstallReceipt`** — serde struct, fields in exact §16/§2 order; `stdlib` skip-if-blank, `source`/`runtime_dependencies` compact rules; atomic write, no trailing newline.
- **`ApiClient`** — owns `Config`, HTTP client, on-disk cache; `fetch_json_api_file`, JWS verify.
- **`InstallSession`** — the explicit `&mut` context replacing Ruby's class-global `attempted/fetched/installed/locked` sets; threaded through recursive dependency installs (no global statics).
- **`Relocation`** — ordered `Vec<ReplacementPair>` with `Matcher = Literal(String) | Regex` and the regex-first/literal-longest sort.

## 3. OS-specific relocation abstraction

```rust
trait Relocator {
    /// Rewrite dynamic linkage in one binary; returns whether it was modified.
    fn relocate_binary(&self, file: &Path, reloc: &Relocation, skip_protodesc_cold: bool)
        -> Result<bool, FerroError>;
    /// Files to scan: Mach-O (dylib/bundle/executable) on macOS; ELF (exec/shared) on Linux.
    fn is_relocatable_binary(&self, file: &Path) -> bool;
}
```
- `MachoRelocator` (`cfg(target_os="macos")`): parse Mach-O with `object` (read dylib id / LC_LOAD_DYLIB / LC_RPATH / filetype / fat slices), **edit by shelling to `install_name_tool`** (`-id`, `-change`, `-rpath`), then **mandatory ad-hoc `codesign --sign - --force --preserve-metadata=…`** after any edit (Intel: verify-first, re-sign only on "invalid signature"; inode-swap retry on failure). Gate ≥ Big Sur.
- `ElfRelocator` (`cfg(target_os="linux"`): sniff ELF header, rewrite rpath/interpreter by shelling to `patchelf --set-interpreter --set-rpath` (forces DT_RUNPATH). Skip glibc; skip `protodesc_cold` sections at bottle-build time only. No codesigning, no dylib-id.
- The text-substitution pass (`relocate/text.rs`) and the boundary-aware `Relocation` engine are **shared, OS-agnostic, and unit-testable on any platform**; only `relocate_binary` is behind the trait — matching the AGENTS.md guidance to keep `extend/os/*` thin.

## 4. JWS verification

`api/jws.rs`: parse outer JSON General Serialization, pick `kid == "homebrew-1"`, require protected header `alg == "PS512"` and `b64 == false`, build signing input `format!("{protected_b64url}.{payload_string}")` (payload NOT re-encoded), RSA-PSS verify (SHA512, salt_len = 64, MGF1-SHA512) against the **embedded** `Library/Homebrew/api/homebrew-1.pem` (verified present, 800 bytes) via `include_str!`. base64url with padding tolerance.

## 5. Concurrency & atomicity

- DownloadQueue replicates Homebrew's two-phase model only as far as needed: fetch (+pre-extract to `HOMEBREW_TEMP_CELLAR` with the `.poured` **symlink** marker) concurrently, then install serially. v1 may start fully serial (fetch-then-install) and add concurrency later; the `.poured` handshake is the coordination contract with `pour()`.
- Atomicity is per-keg: `pour()` extracts into TEMP_CELLAR then `rename` into Cellar; a `KegGuard` RAII removes the partial keg on any error before commit. Receipt/text writes use temp-in-same-dir + `rename` + chown/chmod preserve. Critical rename windows avoid early returns (SIGINT-tolerant), mirroring Ruby `ignore_interrupts`.

## Modules

- **main.rs / fallback.rs** — arg[0] dispatch into command modules; any Err(Unsupported) or unimplemented path re-execs Ruby via exec_ruby (HOMEBREW_FERROBREW_DISABLE=1). The fallback is the central safety lever for byte-compatibility. _(types: FerroError::Unsupported)_
- **config** — Read the HOMEBREW_* env vars already exported by brew.sh into a typed Config (paths + derived dirs); EnvConfig encodes the ENVS registry with boolean/string/int semantics and FALSY_VALUES; placeholder constants. _(types: Config, EnvConfig, EnvKind)_
- **system** — Current OS/arch detection, BottleTag (arm64_sequoia / x86_64_linux), MacOSVersion with the SYMBOLS table and Version::NULL sentinel for uses_from_macos comparison. _(types: SimulateSystem, BottleTag, MacOSVersion)_
- **cli** — Hand-rolled parser modeled on cli/parser.rs: switch/flag/comma_array/conflicts/depends_on/named_args, -- passthrough, env-backed default-on switches, tolerate-unknown-after-formula-names, exact UsageError strings; per-command flag inventories for the allowlist. _(types: Parser, Args, ArgValue, NamedArgs, UsageError)_
- **api** — JSON API client: fetch_json_api_file with If-Modified-Since conditional GET, primary→default-domain fallback, mtime-as-token cache, JSON-parse-retry; JWS verify on every read of .jws.json; bulk formula map + alias/rename/tap-migration resolution; on-disk cache + names/aliases index files. _(types: ApiClient, JwsEnvelope, RawFormulaMap)_
- **formula** — RawFormula serde model (every field #[serde(default)]); merge_variations shallow-merge before extraction; recursive placeholder substitution over the JSON Value; struct_gen producing the resolved installable Formula; PkgVersion/Version. _(types: RawFormula, Formula, Spec, PkgVersion, BottleFile)_
- **deps** — Dependency/UsesFromMacOS/Tag model with predicates; Dependency::expand post-order DFS yielding install order + merge_repeats tag-merging dedup; uses_from_macos OS-bound resolution; Requirement max-keep set. _(types: Dependency, UsesFromMacOS, Tag, NodeAction, Requirement)_
- **bottle** — BottleSpecification + CellarSpec (Any/AnySkipRelocation/Path); BottleFilename (double-dash github_packages vs single-dash url_encode); GHCR URL construction (root_url, blob, manifest, image_formula_name, version_rebuild); GhcrAuth precedence with literal Bearer QQ==; OCI manifest annotation parse. _(types: BottleSpecification, CellarSpec, BottleFilename, GhcrUrls, GhcrAuth, BottleManifest)_
- **download** — curl wrapper (--location, --continue-at - resume, retries, header injection, artifact-domain interleave); content-addressed cache downloads/<sha256(url)>--<file> with .incomplete + relative symlink + flock; sha256 integrity verify; DownloadQueue with .poured pre-extraction handshake. _(types: CurlDownloader, DownloadQueue, CachedDownload, RetryableDownload)_
- **install** — Orchestration: install_formula? skip gate, pour_bottle? PourDecision short-circuit ladder, FormulaInstaller prelude/install/finish, pour() (move-or-extract + tab fill + relocate), KegGuard rollback, FormulaLock; InstallSession threads the attempted/fetched/installed/locked sets by &mut. _(types: FormulaInstaller, InstallSession, PourDecision, KegGuard, FormulaLock)_
- **keg** — Keg model (linked?/optlinked?/one-level resolved_path); link/optlink/unlink with the link_dir pre-order walker and per-directory LinkStrategy table (share/lib/include rules); link_overwrite glob matching + backup-to-cache; layout constants (keg_link_directories, must_exist_subdirectories). _(types: Keg, LinkStrategy, LinkCounts, LinkError)_
- **relocate** — Shared OS-agnostic Relocation engine (ordered table, regex-first/literal-longest sort, hand-rolled left-boundary scanner replacing Ruby lookbehind); inode-grouped atomic text substitution with hardlink re-linking; text/binary classification; Relocator trait with Mach-O (install_name_tool+codesign) and ELF (patchelf) impls. _(types: Relocation, ReplacementPair, Matcher, Relocator, MachoRelocator, ElfRelocator)_
- **tab** — InstallReceipt/Tab serde struct with exact §16/§2 field order; stdlib skip-if-blank, runtime_dependencies/source compact rules; built_on platform-conditional; atomic write with no trailing newline matching JSON.pretty_generate; declared_runtime_dependencies computation. _(types: Tab, RuntimeDep, Source, BuildSystemInfo)_
- **commands** — One module per allowlisted command; install.rs is the first native command end-to-end. Each returns Result so unsupported shapes fall back to Ruby. _(types: InstallCmd)_
- **util** — Cross-cutting helpers: atomic_write (temp-in-dir + rename + chown/chmod preserve), relative symlink via pathdiff, rmdir_if_possible, inode grouping, ohai/opoo/onoe output formatting. _(types: AtomicWriter)_

## Crates
- reqwest (blocking, rustls-tls, gzip): HTTP for the JSON API client — needs If-Modified-Since, --compressed/gzip, and timeouts; rustls avoids an OpenSSL build dep. Alternatively ureq if a lighter sync client is preferred.
- serde + serde_json (with preserve_order feature): deserialize the API JSON and the OCI manifest; serialize INSTALL_RECEIPT.json. preserve_order (indexmap-backed) is REQUIRED to reproduce the source object's insertion order for byte-compatible receipts.
- rsa + sha2 + base64: JWS RSA-PSS verification (PS512, salt_len=64, MGF1-SHA512) and base64url decode of protected/signature; sha2 also does the bottle sha256 integrity check and the download-URL cache-key hash.
- rsa companion: pkcs1/pkcs8/spki via the rsa crate's pem features to parse the embedded homebrew-1.pem RSA public key once at startup.
- flate2 + tar: extract bottle .tar.gz into HOMEBREW_TEMP_CELLAR (bottles are gzip per HOMEBREW_BOTTLES_EXTNAME_REGEX). Shelling to system tar is the fallback if exact-match behavior with Homebrew's UnpackStrategy is needed.
- object: parse Mach-O (read dylib id, LC_LOAD_DYLIB, LC_RPATH, filetype, fat slices) and sniff ELF headers; editing is done by shelling to install_name_tool/patchelf rather than via object.
- pathdiff: compute relative symlink targets (Ruby uses relative_path_from for all keg symlinks and the download symlink); load-bearing for relocatability.
- fs2 (or rustix flock): advisory file locks for FormulaLock under HOMEBREW_LOCKS and per-download lockfiles to prevent concurrent installs/downloads.
- tempfile: NamedTempFile::new_in for the temp-in-same-dir atomic write pattern (receipt + relocated files) before rename.
- libc (or nix): fstat dev/ino for hardlink/inode grouping and dedup, chown/chmod to preserve ownership in atomic_write, and lchmod/fchmodat for reproducible symlink perms on macOS — no safe stdlib equivalent.
- regex: link_overwrite glob matching (literal * → .*?) and any internal regex needs; NOT used for the relocation left-boundary (hand-rolled, since regex has no lookbehind).
- time (or httpdate): format mtime as an HTTP date for the If-Modified-Since conditional request and parse Last-Modified.
- num_cpus: implement HOMEBREW_DOWNLOAD_CONCURRENCY=auto (cores*2) and any make_jobs default.
- shell-words: split HOMEBREW_CASK_OPTS exactly like Ruby Shellwords.shellsplit (cask path, lower priority).
- thiserror: ergonomic FerroError/UsageError enums with the exact Homebrew message strings.

## Milestones
1. M0 — Crate builds + Ruby fallback intact: workspace compiles with the module skeleton; main.rs dispatches the allowlisted commands but every command returns Unsupported and falls through to fallback::exec_ruby. Establish FerroError + the Unsupported→fallback contract and the Config env reader.
2. M1 — Config + system + CLI parse: Config reads exported HOMEBREW_* env; SimulateSystem.current_tag works; the hand-rolled CLI parser handles `install` flags (and -- passthrough, unknown-after-formula-name tolerance) producing Args/NamedArgs. No install yet — still falls back, but arg parsing is exercised by unit tests against the install spec.
3. M2 — JSON API read path: ApiClient fetches formula.jws.json with conditional GET + domain fallback + mtime cache; jws.rs verifies the embedded homebrew-1.pem signature; RawFormula deserializes; build name→object map + alias/rename maps; `brew info <formula>` (read-only) renders from the API as the first genuinely-native command to prove the pipeline.
4. M3 — Formula resolution: merge_variations for current tag, recursive placeholder substitution, struct_gen producing a resolved Formula with BottleSpecification, CellarSpec, and the dependency arrays parsed. Validate against a handful of real formulae (no deps, :any_skip_relocation bottle).
5. M4 — Bottle download: GHCR URL construction (blob + manifest), GhcrAuth (Bearer QQ==), curl wrapper, content-addressed cache + relative symlink, sha256 verify, OCI manifest tab extraction. `brew fetch <leaf>` becomes native.
6. M5 — Pour a single leaf bottled formula end-to-end: extract tarball, rename into Cellar, write INSTALL_RECEIPT.json (tab field order + compact rules byte-exact), relocate (start with :any_skip_relocation so NO binary relocation), link into prefix, optlink. Target: a leaf formula with no deps installs byte-identically to real brew (Cellar layout + receipt). This is the headline milestone.
7. M6 — Relocation correctness: shared text-substitution + boundary scanner; MachoRelocator (install_name_tool + codesign ad-hoc) on arm64 macOS and ElfRelocator (patchelf) on Linux. Now `:any`/path-cellar bottles relocate correctly. Validate codesigned binaries actually exec on arm64.
8. M7 — Dependency resolution + recursive install: Dependency::expand topo order, uses_from_macos OS resolution, install_dependency with .tmp backup/rollback, FormulaLock, runtime-deps tab rewrite in finish. Installs a formula WITH a dependency tree (e.g. wget) byte-compatibly.
9. M8 — install/reinstall/upgrade gates + keg_only + link_overwrite: install_formula? skip/upgrade gate, keg_only linking (opt-only), conflict handling with backup-to-cache, reinstall/upgrade commands. install + reinstall + upgrade reach parity for the bottled-formula path.
10. M9 — Remaining allowlisted commands: list, outdated, search, info, fetch, cleanup, autoremove, uninstall, postinstall implemented natively reading the API + local Cellar/receipt state. update-report stays a Ruby fallback (it mutates the git checkout).
11. M10 — Full command parity + internal-API backend: HOMEBREW_USE_INTERNAL_API path, cask install path, attestation, source builds — each either implemented or explicitly delegated to Ruby via Unsupported so behavior is never wrong, only deferred.

## Risks
- arm64 macOS codesigning: ANY byte edit to a Mach-O invalidates its signature and the kernel refuses to exec/load it. After every install_name_tool change we MUST ad-hoc re-sign (codesign --sign - --force --preserve-metadata=…), handle the verify-first short-circuit on Intel, and implement the copy-to-tmp+mv-back inode-swap retry. Getting this subtly wrong yields kegs that install but crash on run — the single highest-risk area. install_name_tool/codesign also require Xcode CLT, which a pure-bottle machine may lack; :any_skip_relocation bottles (M5) avoid this entirely and must be the first target.
- Relocation correctness: the RELOCATABLE_PATH_REGEX_PREFIX boundary semantics (only after -F/-I/-L/-isystem or a non-alphanumeric) have no Rust lookbehind equivalent and must be hand-rolled; over- or under-matching silently corrupts scripts/.pc files. The regex-first/literal-longest replacement ordering and the cellar-before-prefix install-name check are load-bearing. atomic_write breaks hardlinks — failing to re-link siblings silently un-shares inodes and bloats kegs.
- Atomic install / rollback: pour must extract to TEMP_CELLAR then rename into Cellar with a guard that removes the partial keg on any error; dependency installs need the .tmp keg backup/restore + relink-previous semantics. SIGINT during the rename/cleanup window can leave a half-installed keg; the critical sections must be interrupt-tolerant (mirror Ruby ignore_interrupts). Cross-filesystem rename fallback (copy+remove) is needed though TEMP_CELLAR and Cellar are normally same-FS.
- Byte-exact INSTALL_RECEIPT.json: serde_json pretty matches Ruby (2-space, no trailing newline, unescaped slashes) but the `source` object key order is path-dependent in Ruby (mutated embedded-tab hash) — using preserve_order + a Map for source is required for true round-trips; a typed struct only matches fresh pours. stdlib skip-if-blank and .compact (omit-nil) on dep entries must be replicated exactly or hashes/audits diverge.
- JWS signature verification: RSA-PSS parameter mismatch (salt length must equal digest length 64, MGF1 must be SHA512, payload must be concatenated verbatim because b64:false, not re-encoded) silently fails closed; a wrong base64url padding mode rejects valid payloads. Embedding and parsing homebrew-1.pem once and matching Ruby's Base64.urlsafe_decode64 padding expectation is essential — a MITM-detection false positive aborts every install.
- Bottle/manifest annotation typing: sh.brew.bottle.size etc. are JSON strings (.to_s at upload) not numbers; deserialize as String then parse. The manifest match requires both sh.brew.bottle.digest == checksum AND ref.name == version_rebuild(tag); a mismatch triggers a clear-cache-and-retry that, if mis-implemented, loops or aborts.
- Scope creep via the allowlist: install pulls in the entire dependency/relocation/link/tab stack at once. The Unsupported→Ruby-fallback discipline is the mitigation — every code path that hits an unhandled formula shape (head-only, source-only, options, internal API, cask, custom requirements, non-:any cellar before M6) MUST fall back rather than produce a wrong install. Failing to be conservative here risks corrupting users' real Cellars.

# End-to-end bottle install flow (`brew install <formula>` from a bottle)

## Key reference files
- `Library/Homebrew/cmd/install.rb`
- `Library/Homebrew/install.rb`
- `Library/Homebrew/extend/os/install.rb`
- `Library/Homebrew/formula_installer.rb`
- `Library/Homebrew/extend/os/mac/formula_installer.rb`
- `Library/Homebrew/formula.rb`
- `Library/Homebrew/keg.rb`
- `Library/Homebrew/keg_relocate.rb`
- `Library/Homebrew/extend/os/mac/keg_relocate.rb`
- `Library/Homebrew/tab.rb`
- `Library/Homebrew/tab/tab.rb`
- `Library/Homebrew/utils/bottles.rb`
- `Library/Homebrew/bottle.rb`
- `Library/Homebrew/bottle_specification.rb`
- `Library/Homebrew/resource.rb`
- `Library/Homebrew/retryable_download.rb`
- `Library/Homebrew/startup/config.rb`

## Specification
# Bottle Install Flow — Implementation Spec

This describes `brew install <formula>` when the result is a **poured bottle** (binary package), not a from-source build. Source build is out of scope except where the two share code.

## 0. Key path constants (from `startup/config.rb` and `keg.rb`)

- `HOMEBREW_PREFIX` — install root (e.g. `/opt/homebrew` on arm64 macOS, `/usr/local` on Intel, `/home/linuxbrew/.linuxbrew` on Linux).
- `HOMEBREW_CELLAR` = `HOMEBREW_PREFIX/Cellar` (default). Versioned kegs live here.
- `HOMEBREW_CACHE` — downloaded bottle tarballs.
- `HOMEBREW_LOGS` — `formula.logs` = `HOMEBREW_LOGS/<name>`.
- `HOMEBREW_LINKED_KEGS` = `HOMEBREW_PREFIX/var/homebrew/linked`. Symlink-per-formula directory recording linked kegs.
- `HOMEBREW_TEMP_CELLAR` = `HOMEBREW_PREFIX/var/homebrew/tmp/.cellar`. Staging dir where the download queue pre-extracts bottles.
- Per-formula paths:
  - `rack` = `HOMEBREW_CELLAR/<name>` (parent dir holding all versions).
  - `versioned_prefix(v)` = `rack/<pkg_version>` (the keg).
  - `prefix` = `versioned_prefix(pkg_version)`, BUT returns `opt_prefix` instead when `version == pkg_version && versioned_prefix.directory? && Keg(versioned_prefix).optlinked?` and not inside install/post_install (`@prefix_returns_versioned_prefix` false). During install the raw versioned path is used.
  - `opt_prefix` = `HOMEBREW_PREFIX/opt/<name>` (stable symlink → active keg).
  - `linked_keg` = first of `possible_names` (name + aliases + oldnames) found under `HOMEBREW_LINKED_KEGS`, else `HOMEBREW_LINKED_KEGS/<name>`.
  - `bottle_prefix` = `prefix/.bottle` (holds captured etc/var files).
  - `<keg>/.brew/<name>.rb` — the formula Ruby source copied into the keg (only written on from-source build; bottles already contain it).
  - Receipt/tab: `<keg>/INSTALL_RECEIPT.json` (constant `AbstractTab::FILENAME = "INSTALL_RECEIPT.json"`).

`pkg_version` = `Version` + optional `_<revision>` suffix (PkgVersion). Keg dirs are named exactly by `pkg_version.to_s`.

## 1. Command entry: `cmd/install.rb` `InstallCmd#run`

Ordered steps (formula-only path; cask branch omitted):

1. For each named arg, if it maps to a tap (`Tap.with_formula_name`), call `tap.ensure_installed!`.
2. If `--ignore-dependencies`, print a warning (it is an unsupported dev option).
3. Resolve named args to Formula/Cask via `to_formulae_and_casks`, partition into `formulae`/`casks`.
4. If attestation enabled, reorder: `formulae = Homebrew::Attestation.sort_formulae_for_install(formulae)` (ensures `gh` and its deps install first since attestation verification shells out to `gh`).
5. If DevelopmentTools NOT installed and any of `--HEAD/--build-bottle/--build-from-source` were passed → raise `BuildFlagsError` (can't build without toolchain). This is the gate that forces bottle-only installs on machines with no compiler.
6. `installed_formulae = formulae.select { |f| Install.install_formula?(f, head:, fetch_head:, only_dependencies:, force:, quiet:, skip_link:, overwrite:) }` — see §2. Filters out already-installed/up-to-date formulae (returns false), keeps those needing install/upgrade (returns true).
7. If `formulae.any?` but `installed_formulae.empty?` and no casks → return (nothing to do).
8. `Install.perform_preinstall_checks_once` (memoized; see §3) and `Install.check_cc_argv(args.cc)`.
9. `formulae_installers = Install.formula_installers(installed_formulae, installed_on_request: !args.as_dependency?, ...)` — one `FormulaInstaller` per formula (see §4). `installed_on_request` is `true` for top-level requested formulae unless `--as-dependency`.
10. Compute `dependants` (outdated dependents to upgrade afterwards) via `Upgrade.dependants`.
11. If `--ask`, print dry-run plan and prompt.
12. If not dry-run and there is work: create `Homebrew::DownloadQueue.new(pour: true)`, then:
    - `Install.show_combined_fetch_downloads_heading(...)`
    - `formulae_installers = Install.enqueue_formulae(formulae_installers, download_queue:)` — runs `prelude_fetch`→`prelude`→`enqueue_fetch` on each, dropping ones that raise `CannotInstallFormulaError`/`UnsatisfiedRequirements`/`DownloadError`/`ChecksumMismatchError` (these are reported via `ofail`, NOT fatal). See §5.
    - Enqueue cask downloads (omitted).
    - `download_queue.fetch` — performs all downloads concurrently. For bottles with `pour: true`, also **pre-extracts** each bottle into `HOMEBREW_TEMP_CELLAR` (see §6).
    - `ensure download_queue.shutdown`.
13. `exit 1 if Homebrew.failed?`
14. `Install.install_formulae(formulae_installers, dry_run:, verbose:)` — per installer, run `install` then `finish` (see §7+).
15. `Upgrade.upgrade_dependents(...)`; cask installs; `Cleanup.periodic_clean!`; `Homebrew.messages.display_messages`.

## 2. Pre-flight "already installed?" gate — `Install.install_formula?` (`install.rb`)

Returns `true` (proceed with install/upgrade) or `false` (skip). Side effect: if skipping but the keg is already installed, it marks `installed_on_request = true` in the existing tab and writes it (so a previously-dependency install becomes "on request").

Ordered checks (each `odie` aborts the whole process):
1. `!head && formula.stable.nil?` → odie "HEAD-only formula" (must pass `--HEAD`).
2. `head && formula.head.nil?` → odie "No head is defined".
3. Compute `new_head_installed` = installed head exists and not outdated. `prefix_installed = formula.prefix.exist? && !empty`.
4. **Different-tap conflict**: if `any_version_installed?` and installed keg's tab `source["tap"]` != current formula tap → odie telling user to `brew uninstall` first.
5. **keg_only + optlinked + !force**: handle outdated→upgrade (`return true` unless `HOMEBREW_NO_INSTALL_UPGRADE`/pinned) or print "already installed and up-to-date / reinstall" and fall through to `return false`.
6. **`(head && new_head_installed) || prefix_installed`**: this exact version is installed.
   - linked but linked_version != installed_version → warn "currently linked version is X".
   - `only_dependencies || (!linked? && overwrite)` → `return true`.
   - `!linked? || keg_only?` → message "it's just not linked, run brew link".
   - else "already installed and up-to-date, run brew reinstall" → fall to `return false`.
7. `!any_version_installed? && old_installed_formulae.first` exists → message about old name installed.
8. `migration_needed? && !force` → message about `brew migrate`.
9. `formula.linked?` (a different version is linked):
   - outdated && !head → upgrade (`return true` unless NO_INSTALL_UPGRADE/pinned) or onoe "run brew upgrade".
   - `only_dependencies || skip_link` → `return true`.
   - else onoe "first run brew unlink".
10. else → `return true` (FormulaInstaller handles it).

After messages, the tail: `return false unless formula.opt_prefix.directory?`; otherwise mark existing tab `installed_on_request = true`, write, `return false`.

`HOMEBREW_NO_INSTALL_UPGRADE` env disables auto-upgrade of an already-installed-but-outdated formula on plain `brew install`.

## 3. Global pre-install checks — `Install.perform_preinstall_checks` (`install.rb`, memoized by `perform_preinstall_checks_once`)

1. `check_prefix` — macOS: odie if Intel/Rosetta in arm-default prefix, or arm in Intel-default prefix.
2. `check_cpu` — odie on PowerPC.
3. `attempt_directory_creation` — `mkdir_p` each of `Keg.must_exist_directories` (ignore errors). That set = `must_exist_subdirectories` + `[HOMEBREW_CELLAR]`, where `must_exist_subdirectories` = `(keg_link_directories - [var] + [opt, var/homebrew/linked])` mapped under `HOMEBREW_PREFIX`, sorted/uniq. `keg_link_directories = [bin etc include lib sbin share var]`.
4. `Diagnostic.checks(:supported_configuration_checks)` and `:fatal_preinstall_checks`.

## 4. FormulaInstaller construction — `FormulaInstaller#initialize`

Stores all option flags. Key derivations:
- `link_keg = link_keg || !formula.keg_only? || auto_link_versioned_keg_only?`. So a non-keg-only formula always links; a keg-only formula only auto-links if it's a versioned keg-only with no other version installed and no conflicting installed relatives (`auto_link_versioned_keg_only?`, only when `installed_on_request?`).
- `formula.force_bottle ||= force_bottle`; `@force_bottle` cached.
- `@download_queue = Homebrew.default_download_queue` (overwritten by caller).
- If a previously-fetched formula with same `full_name` and `active_spec_sym` exists in `self.class.fetched`, reuse that instance (`previously_fetched_formula`), so bottle/source instances of the same formula are coalesced.

Class-level mutable sets (process-global): `attempted`, `installed`, `fetched`, `locked` — all `Set`/`Array`. `attempted` prevents double-install; `fetched` prevents double-download; `locked` holds formula locks.

## 5. `pour_bottle?` decision — `FormulaInstaller#pour_bottle?` (THE bottle-vs-source switch)

Returns true iff a bottle should be poured. Order (short-circuits):
1. `false` if `!formula.bottle_tag? && !formula.local_bottle_path` (no bottle for this OS/arch and no local `.tar.gz`).
2. `true` if `force_bottle?`.
3. `false` if `build_from_source? || build_bottle? || interactive?`.
4. `false` if `@cc` (custom compiler) present.
5. `false` unless `options.empty?` (any non-default build options force source).
6. `false` unless `formula.pour_bottle?` (the formula DSL check; see §5a). If `output_warning`, opoo the unsatisfied reason.
7. `true` if `formula.local_bottle_path.present?`.
8. `bottle = formula.bottle_for_tag(Utils::Bottles.tag)`; `false` if nil.
9. `false` unless `bottle.compatible_locations?` (cellar/prefix match; see §5b). If `output_warning`, opoo telling user the required `HOMEBREW_CELLAR`/`HOMEBREW_PREFIX`.
10. else `true`.

### 5a. Formula DSL `pour_bottle?` (`formula.rb`)
Default `Formula#pour_bottle?` returns `true`. The DSL `pour_bottle?(only_if:)` / block installs a `PourBottleCheck` with a `satisfy {}` predicate and a `reason` string accessible via `pour_bottle_check_unsatisfied_reason`. Built-in `only_if` presets: `:clt_installed` (macOS: requires Xcode CLT at `/Library/Developer/CommandLineTools`), `:default_prefix` (requires `HOMEBREW_PREFIX == Homebrew::DEFAULT_PREFIX`).

### 5b. `compatible_locations?` (`bottle_specification.rb`)
`cellar = tag_to_cellar(tag)` = the spec's per-tag cellar, or `tag.default_cellar`. Return `true` if cellar ∈ `[:any, :any_skip_relocation]` (RELOCATABLE_CELLARS). Otherwise treat cellar as a path string; `prefix = parent(cellar)`; require `cellar == HOMEBREW_CELLAR.to_s && prefix == HOMEBREW_PREFIX.to_s` (unless `HOMEBREW_RELOCATE_BUILD_PREFIX` set, which relaxes to size-based comparison).

`skip_relocation?` is true only when the matched spec's `cellar == :any_skip_relocation`.

## 6. Fetch + pre-extract — `enqueue_fetch`/`prelude_fetch`/`prelude` and DownloadQueue

### prelude_fetch (`FormulaInstaller#prelude_fetch`)
1. Deprecate/disable check (`DeprecateDisable.type`): `:deprecated`→opoo; `:disabled`→opoo if `force?` else raise `CannotInstallFormulaError` (and emit GHA error annotation).
2. `forbidden_tap_check(formula_only: true)` and `forbidden_formula_check(formula_only: true)` (checks `HOMEBREW_FORBIDDEN_TAPS`, `HOMEBREW_ALLOWED_TAPS`, `HOMEBREW_FORBIDDEN_FORMULAE`) — done **before** any download.
3. If `pour_bottle?` → `fetch_bottle_tab(enqueue: true)` (queues the GitHub Packages OCI manifest resource, which carries tab attributes + sizes). Else if loaded from API → enqueue source download.
4. `fetch_fetch_deps` unless `ignore_deps?`.

### prelude (`FormulaInstaller#prelude`)
1. `prelude_fetch` unless already run.
2. `determine_bottle_tab_attributes` — see §6a.
3. `verify_deps_exist` → `compute_dependencies` (raises FormulaUnavailableError with `dependent` set).
4. `forbidden_license_check`, `forbidden_tap_check`, `forbidden_formula_check` (full, including deps).
5. `check_install_sanity` — see §8.
6. If `download_concurrency <= 1`, `install_fetch_deps` (serial install of implicit deps like `ca-certificates`).

### 6a. `determine_bottle_tab_attributes`
`Tab.clear_cache`. Read `formula.bottle_tab_attributes` (from the downloaded GH manifest). Build `@bottle_tab_runtime_dependencies = { dep["full_name"] => dep_hash }` from `runtime_dependencies` array. If the bottle's tag `system != :all`, set `@bottle_built_os_version = bottle_tab_attributes.dig("built_on","os_version")`. These pin dependency resolution to the versions/OS the bottle was actually built against (so e.g. installing a Sonoma bottle on Sequoia resolves deps per Sonoma). Rescue `Resource::BottleManifest::Error` → assume full deps.

### enqueue_fetch (`FormulaInstaller#enqueue_fetch`)
1. Return if `previously_fetched_formula`.
2. `fetch_dependencies` (recurse: create child FormulaInstaller per dep via `fetch_dependency`, run their `prelude` + `enqueue_fetch`).
3. Return if `only_deps?` or local bottle path.
4. `downloadable_object = downloadable` (see below).
5. If `pour_bottle?(output_warning: true)`: `fetch_bottle_tab(enqueue: true)`; `check_attestation = !cached_download.exist?`. Else: source path branch.
6. `check_attestation &&= Attestation.enabled? && formula.tap&.core_tap? && name != "gh"`.
7. `download_queue.enqueue(downloadable_object, check_attestation:)`.
8. `self.class.fetched << formula`.
9. On `CannotInstallFormulaError`: unlink the cached download and re-raise.

`downloadable` returns: `Resource::Local.new(local_bottle_path)` if local; else `formula.bottle` (a `Bottle`, `include Downloadable`) if pouring; else `formula.resource` (source).

### 6b. DownloadQueue pre-extraction (`retryable_download.rb`, when `pour: true`)
After download + integrity verify, for a `Bottle`:
1. `mkpath HOMEBREW_TEMP_CELLAR`.
2. `bottle_filename = bottle.filename`; `bottle_tmp_keg = HOMEBREW_TEMP_CELLAR/<name>/<version>`; `bottle_poured_file = "<bottle_tmp_keg>.poured"`.
3. If `.poured` marker not present: clean stale, then `UnpackStrategy.detect(download, prioritize_extension: true).extract_nestedly(to: HOMEBREW_TEMP_CELLAR)` (untar). Then `ln_s(bottle_tmp_keg, bottle_poured_file)` as completion marker (symlink so `exist?` checks the real dir too).

Bottle tarball internal layout: top-level `<name>/<pkg_version>/...` containing the keg contents (incl. `.brew/<name>.rb` and `INSTALL_RECEIPT.json`). The bottle filename format (`Bottle::Filename`): `"<name>--<version>.<tag>.bottle[.<rebuild>].tar.gz"` where `<tag>` is the unstandardized arch+os symbol; download/staged name `"<name>-<version>...tar.gz"`.

## 7. Per-formula install — `Install.install_formula(fi, upgrade:)` then `fi.install` + `fi.finish`

`install_formula`:
1. `fi.check_installation_already_attempted` (raise `FormulaInstallationAlreadyAttemptedError` if in `attempted` set).
2. If `upgrade`: print upgrade message, collect outdated linked kegs. Else `formula.print_tap_action`.
3. Unlink outdated kegs (`kegs.each(&:unlink)`) before installing the new version (avoids the old build interfering).
4. `fi.install` (§9), `fi.finish` (§10).
5. `ensure`: if build failed (`!latest_version_installed?`), re-link the previously-linked kegs.

## 8. `check_install_sanity` (`formula_installer.rb`)

1. `check_installation_already_attempted`.
2. `force_bottle? && !pour_bottle?` → raise `CannotInstallFormulaError "--force-bottle passed but no bottle"`.
3. Default-prefix core-tap non-bottle gate: if default prefix, not building from source/bottle, not head, core tap, not in integration test, and `!pour_bottle?` → raise `CannotInstallFormulaError` (no bottle available / pour check failed) with the "Tier 3, build from source" guidance.
4. Return if `ignore_deps?`.
5. If developer mode: detect direct/transitive cyclic self-dependency → raise.
6. `recursive_deps = pour_bottle? ? formula.runtime_dependencies : formula.recursive_dependencies`. For each dep: check installed-keg tab `arch` matches `Hardware::CPU.arch` (collect `invalid_arch_dependencies`); collect pinned-but-unsatisfied deps.
7. Raise `CannotInstallFormulaError` if invalid arch deps exist.
8. Raise `CannotInstallFormulaError` ("brew unpin ...") if pinned unsatisfied deps exist.

## 9. `FormulaInstaller#install` (the heart)

1. `lock` — lock formula + recursive deps via `FormulaLock` (process-global `self.class.locked`).
2. `start_time = Time.now`. If not pouring and DevTools installed, run build-from-source checks (N/A for bottle).
3. If newer version exists in tap (and not quiet): opoo.
4. `check_conflicts` (§9a).
5. `raise UnbottledError, [formula]` if `!pour_bottle? && !DevelopmentTools.installed?` (can't build, no bottle).
6. Dependencies (unless `ignore_deps?`):
   - `deps = compute_dependencies(use_cache: false)` (§9b).
   - If pouring without DevTools (or build_bottle) and any dep is unbottled → `raise UnbottledError, unbottled`.
   - `install_dependencies(deps)` (§9c) — recursively installs each dep first.
7. `return if only_deps?`.
8. Print deprecated-flag warnings.
9. `oh1 "Installing <name> <options>"` if `show_header?`.
10. Report analytics (`Utils::Analytics.report_package_event(:formula_install, on_request: installed_on_request?, ...)`).
11. `self.class.attempted << formula`.
12. **If `pour_bottle?`**: `begin pour rescue Exception => uninstall keg if prefix exists, re-raise; else @poured_bottle = true`. (§9d) — On ANY exception during pour, the partial keg is removed (`Keg.new(formula.prefix).ignore_interrupts_and_uninstall!`).
13. `puts_requirement_messages`.
14. `build_bottle_preinstall` if build_bottle (snapshots etc/var).
15. **If NOT poured** (source build): `build`, `clean`, write `<keg>/.brew/<name>.rb` (formula source with `bottle do...end` stripped), create `Keg`, set tab `installed_on_request`, write tab. (Bottle path skips this — the bottle already contains `.brew/` and a receipt, finalized in `pour`.)
16. `build_bottle_postinstall` if build_bottle.
17. opoo "Nothing was installed" if `!latest_version_installed?`.
18. `Homebrew.messages.package_installed(name, elapsed)`.

### 9a. `check_conflicts`
Skip if `force?`. For each `formula.conflicts`: load conflicting formula; if its `linked_keg.exist? && opt_prefix.exist?` (i.e. actively linked) → collect. Raise `FormulaConflictError.new(formula, conflicts)` if any. Missing/tap-unavailable conflicts are tolerated (opoo, not fatal unless developer mode).

### 9b. `compute_dependencies` / dependency ordering
Memoized in `@compute_dependencies`. Steps:
1. `fetch_bottle_tab if pour_bottle?` (need manifest for runtime deps).
2. `check_requirements(expand_requirements)` — collects unsatisfied `Requirement`s; raise `UnsatisfiedRequirements` for any fatal one. MacOSRequirement `<=` is skipped if the dependent is already installed.
3. `expand_dependencies` → `Dependency.expand(formula)` producing an **ordered** `Array[Dependency]` (topologically sorted, dependencies before dependents; this is the install order). Within `expand_dependencies_for_formula` the block decides per-dep:
   - `keep_build_test`: keep test dep only if `include_test?` and dependent is the named formula; keep build dep only if NOT installing a bottle for the dependent AND (formula head or dependent not installed). Since for a bottle install of a bottled formula `install_bottle_for?` is true, **build/test deps are pruned** — bottle installs only pull runtime deps.
   - `dep.satisfied?(minimum_version:, minimum_revision:, bottle_os_version:)` → `Dependable::SKIP` (already satisfied; min version/revision/os come from `@bottle_tab_runtime_dependencies`).
   - `Dependable::PRUNE` to drop a dep + its subtree.

### 9c. `install_dependencies` → `install_dependency(dep)`
For each dep in order: print `oh1 "Installing <formula> dependency: <dep>"`. Special-case: wrap `install_dependency` in `with_env(HOMEBREW_INSTALLING_BUBBLEWRAP: "1")` for bubblewrap and deps up to it (Linux sandbox bootstrap).

`install_dependency(dep)`:
1. If dep currently linked: capture its `Keg`+tab, record `keg_had_linked_keg`/`keg_was_linked`, **unlink** it.
2. If dep version already installed: rename existing keg to `<keg>.tmp` (backup for rollback).
3. Cross-tap guard: if installed-from a different tap → odie.
4. Compute `options` = tab used options ∪ remapped deprecated options ∩ dep.options. `installed_on_request` carried from existing tab (a dep stays "on request" if it was).
5. New child `FormulaInstaller` with `installed_on_request:` derived, `link_keg: keg_had_linked_keg && keg_was_linked`, `force_bottle: false`. Run `prelude`, `install`, `finish`.
6. **rescue Exception** (rollback): rename `.tmp` keg back, re-link the previously-linked keg. Swallow `FormulaInstallationAlreadyAttemptedError` (already handled elsewhere). **else**: remove the `.tmp` backup keg.

### 9d. `pour` (the actual bottle install)
`HOMEBREW_CELLAR.cd do`:
1. `ohai "Pouring <downloader.basename>"`.
2. `formula.rack.mkpath`.
3. Compute `bottle_tmp_keg = HOMEBREW_TEMP_CELLAR/(formula.prefix relative_to HOMEBREW_CELLAR)` (= `HOMEBREW_TEMP_CELLAR/<name>/<pkg_version>`), `bottle_poured_file = "<bottle_tmp_keg>.poured"`.
4. **If `.poured` exists** (pre-extracted by download queue, §6b): `rm` the marker, `mv(bottle_tmp_keg, formula.prefix)` (atomic move into Cellar), `rmdir_if_possible` the temp parent.
   **Else**: `downloadable.downloader.stage` — extract the tarball directly into CWD (=`HOMEBREW_CELLAR`), creating `<name>/<pkg_version>/`.
5. `Tab.clear_cache`.
6. `tab = Utils::Bottles.load_tab(formula)` — loads the receipt from the bottle/GH manifest tab attributes or the staged `INSTALL_RECEIPT.json`.
7. **Fill/refresh tab fields** (keep in sync with `Tab#to_bottle_hash`):
   - `used_options = []`, `unused_options = []`
   - `built_as_bottle = true`, `poured_from_bottle = true`
   - `loaded_from_api`, `loaded_from_internal_api` from formula
   - `installed_on_request = installed_on_request?`
   - `time = Time.now.to_i`
   - `aliases = formula.aliases`
   - `arch = Hardware::CPU.arch`
   - `source["versions"]["stable"] = formula.stable.version.to_s`
   - `source["versions"]["version_scheme"] = formula.version_scheme`
   - `source["path"] = formula.specified_path.to_s`
   - `source["tap_git_head"] = tap installed ? tap.git_head : nil`
   - `tab.tap = formula.tap`
   - `tab.write` → atomic-writes `<keg>/INSTALL_RECEIPT.json`.
8. **Relocate**: `keg = Keg.new(formula.prefix)`; `skip_linkage = bottle_specification.skip_relocation?`; `keg.replace_placeholders_with_locations(tab.changed_files, skip_linkage:)` (§11).
9. **Build-prefix relocation (rare)**: `cellar = bottle_specification.tag_to_cellar(tag)`; return if `cellar ∈ [:any, :any_skip_relocation]`; `prefix = parent(cellar)`; return if cellar==HOMEBREW_CELLAR && prefix==HOMEBREW_PREFIX; return unless `HOMEBREW_RELOCATE_BUILD_PREFIX` set; else `keg.relocate_build_prefix(keg, prefix, HOMEBREW_PREFIX)` (null-padded binary string substitution).

## 10. `FormulaInstaller#finish`

`return if only_deps?`. Ordered:
1. `keg = Keg.new(formula.prefix)`.
2. **Link**: if `skip_link?` → print "Skipping link". Else `link(keg)` (§12) + opoo `link_manual_command_warning` if applicable.
3. `install_service` — write launchd plist (`<keg>/<plist_name>.plist`, mode 0644) and/or systemd unit+timer if the formula defines a service. Errors are non-fatal (`ofail`).
4. `fix_dynamic_linkage(keg)` unless poured && `skip_relocation?` — re-points absolute symlinks/install-names. Errors non-fatal (`ofail` + summary heading).
5. `Homebrew::Install.global_post_install` (no-op on macOS core).
6. **post_install**: if `build_bottle?` or `skip_post_install?` → print "you can run brew postinstall". Else:
   - `formula.install_etc_var` — restore captured `etc`/`var` files from `bottle_prefix/etc` and `/var` into `HOMEBREW_PREFIX` using `InstallRenamed` (existing user-modified files get `.default` suffix handling).
   - If `post_install_steps_defined?` → run them (warn on conflict with `post_install`). elsif `post_install_defined?` → `post_install` (forks `postinstall.rb` in a sandbox; see §13). Errors non-fatal.
7. `keg.prepare_debug_symbols` if `--debug-symbols` (no-op on bottle path normally).
8. **Linkage cache**: `CacheStoreDatabase.use(:linkage)` → rebuild `LinkageChecker` cache for the keg.
9. **Update tab runtime deps**: `Tab.clear_cache`; `f_runtime_deps = formula.runtime_dependencies(read_from_tab: false)`; `tab.runtime_dependencies = Tab.runtime_deps_hash(formula, f_runtime_deps)`; `tab.write`. This replaces bottle-declared deps with the resolved actual runtime deps.
10. **SBOM**: unless build_bottle, `SBOM.create(formula, tab).write`.
11. Special cases: clear git available cache if name=="git"; `Sandbox.reset_state!` if name=="bubblewrap"; set `SSL_CERT_FILE`/`GIT_SSL_*` env if name=="ca-certificates"; set `HOMEBREW_CURL` if name=="curl".
12. `caveats` (§14).
13. `ohai "Summary"` if verbose/summary heading; `puts summary` (prefix + disk usage + build time).
14. `self.class.installed << formula`.
15. `ensure unlock` (release all locks).

## 11. Relocation — `Keg#replace_placeholders_with_locations` (`keg_relocate.rb`)

Bottles are built with placeholder tokens; pouring substitutes real paths. Placeholders (constants):
- `@@HOMEBREW_PREFIX@@`, `@@HOMEBREW_CELLAR@@`, `@@HOMEBREW_REPOSITORY@@`, `@@HOMEBREW_LIBRARY@@`, `@@HOMEBREW_PERL@@`, `@@HOMEBREW_JAVA@@`.

`replace_placeholders_with_locations(files, skip_linkage:)`:
1. `relocation = prepare_relocation_to_locations` — replacement pairs: PREFIX→HOMEBREW_PREFIX, CELLAR→HOMEBREW_CELLAR, REPOSITORY→HOMEBREW_REPOSITORY, LIBRARY→HOMEBREW_LIBRARY, PERL→`<prefix>/opt/perl/bin/perl` (macOS picks `/usr/bin/perlX.Y` or brewed perl based on tab `built_on.preferred_perl`), JAVA→openjdk libexec if an openjdk runtime dep exists.
2. `relocate_dynamic_linkage(relocation)` unless `skip_linkage` — **macOS**: for every Mach-O file (dylib/bundle/executable, dedup by dev+inode to skip hardlinks): rewrite dylib ID, install names (LC_LOAD_DYLIB), and rpaths via `install_name_tool`/equivalent; **codesign** the binary afterward if modified (ad-hoc resign required after editing Mach-O). Linux: patchelf-equivalent (in `extend/os/linux`). The generic base impl is a no-op.
3. `replace_text_in_files(relocation, files:)` — for text/script files (`tab.changed_files` = the list recorded in the receipt, or `text_files | libtool_files`): read, `relocation.replace_text!` (regex substitution; longer keys first), atomic-write, re-link hardlinks. Returns changed files.

Replacement matching uses `RELOCATABLE_PATH_REGEX_PREFIX` (`(?:(?<=-F|-I|-L|-isystem)|(?<![a-zA-Z0-9]))`) so tokens are only replaced at word/flag boundaries.

`tab.changed_files` is the authoritative list of files needing text relocation (recorded when the bottle was built); the receipt JSON carries `"changed_files"` as relative paths.

## 12. Linking — `Keg#link` / `Keg#optlink` (`keg.rb`)

`FormulaInstaller#link(keg)`:
- `Formula.clear_cache`.
- If a cask with the same name is installed → skip linking, set `@link_keg = false`.
- **If `!link_keg`** (keg-only or `--skip-link`): only `keg.optlink` (create `opt/<name>` symlink). On `Keg::LinkError` → `ofail "Failed to create opt_prefix"`, continue. Return.
- Else (full link):
  - If `keg.linked?` → opoo + remove stale linked record.
  - `Homebrew::Unlink.unlink_link_overwrite_formulae`.
  - `keg.link(verbose:, overwrite:)`:
    - raise `AlreadyLinkedError` if `linked_keg_record.directory?`.
    - `optlink` first (creates `opt/<name>`, alias opt records, oldname opt records).
    - `link_dir` for each of: `etc`(:mkpath), `bin`(:skip_dir), `sbin`(:skip_dir), `include`, `share`, `lib`, `Frameworks` — each with per-subpath strategy (`:link` = symlink to Cellar, `:mkpath` = create real dir and recurse so multiple formulae can share, `:info` = also run `install-info`, `:skip_file`/`:skip_dir`). Special handling for info files (`INFOFILE_RX`), locale dirs, zsh/fish completions, versioned dirs (postgresql@N), language dirs (perl5, python3.X, ruby, R, ...).
    - Finally create `HOMEBREW_LINKED_KEGS/<name>` symlink → keg path.
  - **Conflict handling**: on `Keg::ConflictError`, if `formula.link_overwrite?(conflict_file)` → back up conflicting file to `HOMEBREW_CACHE/Backup/<rel path>` and `retry`. Otherwise `ofail "brew link did not complete"`, print conflicting files (dry-run link), set summary heading.
  - `Keg::LinkError` → ofail with "try again brew link". Other Exception → ofail, unlink, restore backups, re-raise.
  - If backups were made, opoo listing overwritten files (backed up to cache).

**keg_only formulae**: `link_keg` is false → only `opt/<name>` symlink is created; no symlinks into `HOMEBREW_PREFIX/{bin,lib,...}`. The keg is reachable only via `opt_prefix`. `link_manual_command_warning` tells the user to `brew link` for versioned keg-only formulae.

## 13. `post_install` fork (`FormulaInstaller#post_install`)
Forks `nice ruby ... postinstall.rb <post_install_formula_path>` inside a `Sandbox` (deny network unless `network_access_allowed?(:postinstall)`, allow write to cellar + keg-link dirs under HOMEBREW_PREFIX, deny read of home, deny write of homebrew repo). `post_install_formula_path` prefers the keg's `.brew/<name>.rb` for API/local-bottle/from-source installs or when tap pkg_version differs, else the tap formula. Failures are non-fatal (`Homebrew.failed = true`, opoo).

## 14. Caveats (`FormulaInstaller#caveats`)
`return if only_deps?`. Developer mode: `audit_installed` (PATH checks for non-keg-only bin/sbin). `return unless installed_on_request?` and `return if quiet?` — **caveats only print for the explicitly-requested formula, not dependencies**. Build `Caveats.new(formula)`; record completions/elisp; if non-empty caveats text: set summary heading, `ohai "Caveats", text`, `Homebrew.messages.record_caveats`.

## 15. installed_on_request vs installed_as_dependency
- A top-level `brew install <f>` sets `installed_on_request: true` (unless `--as-dependency`). Written into the receipt as `"installed_on_request": true`.
- Dependencies installed transitively get `installed_on_request` derived from any existing tab (preserves prior on-request status) but default `false` for fresh deps → recorded as `false` (installed as dependency).
- The receipt JSON key is `"installed_on_request"` (boolean). It governs: whether caveats print, and whether `brew autoremove`/leaves treats the formula as a leaf vs. removable dependency. If a formula previously installed as a dep is later requested explicitly, `install_formula?` flips the existing tab's flag to true and rewrites it even when no install happens.

## 16. Receipt / Tab JSON structure (`Tab#to_json`, `tab/tab.rb`)
File `<keg>/INSTALL_RECEIPT.json`, pretty-printed JSON, keys in this exact order:
`homebrew_version, used_options, unused_options, built_as_bottle, poured_from_bottle, loaded_from_api, loaded_from_internal_api, installed_on_request, changed_files, time, source_modified_time, stdlib, compiler, aliases, runtime_dependencies, source, arch, built_on`. `"stdlib"` is dropped if blank. For a poured bottle: `built_as_bottle=true`, `poured_from_bottle=true`, `used_options/unused_options=[]`.
- `source` sub-hash keys: `spec` ("stable"/"head"), `path`, `tap`, `tap_git_head`, `versions` (`stable`, `head`, `version_scheme`, `compatibility_version`), optionally `scm_revision`.
- `runtime_dependencies`: array of `{full_name, version, revision, bottle_rebuild, pkg_version, declared_directly, compatibility_version}` (nils compacted out). After `finish`, this is rewritten with the *actual* resolved runtime deps.
- `built_on`: build-system info hash (os, os_version, cpu_family, xcode, clt, preferred_perl, etc.).
- `arch`: `Hardware::CPU.arch` (e.g. `arm64`/`x86_64`).

## 17. Failure / rollback summary
- **Pour fails**: any exception → `Keg.new(formula.prefix).ignore_interrupts_and_uninstall!` (removes the partial keg), re-raise. Nothing left installed.
- **Source build fails** (`build`): `rm_r(formula.prefix)`, `rack.rmdir_if_possible`, re-raise.
- **Dependency install fails**: restore the renamed `.tmp` keg, re-link the previously-linked keg (in `install_dependency` rescue).
- **Link fails**: non-fatal — keg stays in Cellar but unlinked; user told to `brew link`. Other link exceptions → unlink + restore link-overwrite backups + re-raise.
- **fix_dynamic_linkage / clean / service / post_install fail**: non-fatal — `Homebrew.failed = true`, opoo/ofail, summary heading set, install otherwise considered successful.
- **Already attempted**: `FormulaInstallationAlreadyAttemptedError` is swallowed (formula was installed as part of another tree).
- Locks (`FormulaLock`) are always released in `finish`'s `ensure unlock`.

## Rust implementation notes
Recommended structure:

- `enum InstallSource { Bottle(BottleRef), LocalBottle(PathBuf), Source(ResourceRef) }` mirrors `FormulaInstaller#downloadable`. The whole subsystem branches on `pour_bottle()` returning a bool computed by the staged short-circuit logic in §5 — implement as a function returning `enum PourDecision { Pour, BuildFromSource(reason), NoBottle }` so you can surface the warning strings.

- `struct FormulaInstaller` holding the option flags (all the booleans from initialize). The process-global sets (`attempted`, `installed`, `fetched`, `locked`) map to a shared `InstallSession`/context struct passed by `&mut` rather than Ruby class variables — do NOT use global statics; thread an explicit context (these sets are mutated across the recursive dependency installs).

- Dependency ordering: `Dependency.expand` is a topological sort yielding deps-before-dependents with PRUNE/SKIP semantics. Use `petgraph` (DiGraph + `toposort`) or hand-rolled DFS. The prune/skip decision is a per-node closure; model it as a callback `Fn(&Dependent, &Dep) -> NodeAction { Prune, Skip, Keep }`. Cache key in Ruby is per-expansion (`Time.now.to_f`) so just don't share the cache across installer instances.

- Tab/receipt: define `struct Tab` with `#[derive(Serialize, Deserialize)]` using `serde_json`. CRITICAL: field **order must be preserved** in output (Ruby uses `JSON.pretty_generate` with an ordered Hash). serde_json with a struct preserves declaration order — declare fields in the §16 order. Use `#[serde(skip_serializing_if = "Option::is_none")]` only where Ruby compacts (the dep hash). `"stdlib"` is conditionally removed when blank — handle with a custom serialize or post-process. Pretty-print is 2-space indent. Receipt filename is exactly `INSTALL_RECEIPT.json`. Use atomic write (write temp + rename) — Ruby `Pathname#atomic_write`.

- Bottle extraction: the tarball is `.tar.gz`; top-level dir is `<name>/<pkg_version>/`. Use `flate2` + `tar` crates, or shell out to `tar` to match Homebrew exactly (Homebrew uses system `tar` via `Utils.popen_read`/UnpackStrategy). Pre-extraction goes to `HOMEBREW_TEMP_CELLAR` then atomic `rename` (std::fs::rename) into `HOMEBREW_CELLAR/<name>/<version>`. The `.poured` completion marker is a **symlink** (`std::os::unix::fs::symlink`) pointing at the extracted keg dir — used as an interrupt-safe completion flag. Cross-filesystem rename can fail (TEMP_CELLAR and CELLAR are both under HOMEBREW_PREFIX so normally same FS — but guard with copy+remove fallback).

- Relocation is the hardest part:
  - Text substitution: read file bytes, replace placeholder tokens (`@@HOMEBREW_PREFIX@@` etc.) at word/flag boundaries (port `RELOCATABLE_PATH_REGEX_PREFIX`). Use `regex` crate or `memchr`-based scanning. Atomic write; re-link hardlinks (group files by inode via `std::os::unix::fs::MetadataExt::ino`).
  - Mach-O relocation (macOS): you must rewrite dylib ID, LC_LOAD_DYLIB install names, and LC_RPATH entries. Crate `object` can parse Mach-O but does NOT edit load commands well; realistically shell out to `install_name_tool` (`-id`, `-change`, `-rpath`/`-delete_rpath`) as Homebrew effectively does, OR use a Mach-O editing crate. **GOTCHA: after editing a Mach-O binary you MUST re-codesign it ad-hoc** (`codesign -s - -f`), otherwise it won't run on arm64 macOS (kernel rejects unsigned/invalid-signature binaries). Homebrew calls `codesign_patched_binary` whenever a Mach-O is modified.
  - ELF relocation (Linux): equivalent uses patchelf semantics (rpath/interpreter).
  - Skip relocation entirely when `cellar == :any_skip_relocation`.

- Symlink linking (`Keg#link`): create relative symlinks from `HOMEBREW_PREFIX/{bin,etc,include,lib,sbin,share}` into the Cellar keg. Use `std::os::unix::fs::symlink` with relative targets (Homebrew uses relative symlinks). Implement the per-directory strategy table (link vs mkpath vs skip) from §12 — this is a large match on relative path patterns. The `opt/<name>` and `var/homebrew/linked/<name>` symlinks are always created (optlink) even for keg-only. `info` files additionally need `install-info` invocation.

- keg_only: skip the prefix-linking step; only create the `opt` symlink. The `link_keg` bool is precomputed (§4); a versioned keg-only auto-links under narrow conditions.

- Conflicts: `FormulaConflictError` if a conflicting formula is actively linked (`linked_keg.exist? && opt_prefix.exist?`). Link-step conflicts can be force-overwritten with backup to `HOMEBREW_CACHE/Backup/`.

- Atomicity/rollback: wrap pour in a guard that removes the keg dir on any error (RAII drop guard or explicit catch). For dependency installs, implement the `.tmp` keg backup/restore. Use a cancellation-safe approach — Ruby uses `ignore_interrupts`; in Rust, perform the critical rename/cleanup steps without early returns and consider blocking SIGINT during the rename window.

- Locks: `FormulaLock` is a file lock (flock on a lock file under HOMEBREW_LOCKS). Use `fs2`/`fd-lock` crate. Lock the formula + all recursive deps before install, release in a Drop guard.

- Env vars consumed: `HOMEBREW_NO_INSTALL_UPGRADE`, `HOMEBREW_FORBIDDEN_LICENSES`, `HOMEBREW_FORBIDDEN_TAPS`, `HOMEBREW_ALLOWED_TAPS`, `HOMEBREW_FORBIDDEN_FORMULAE`, `HOMEBREW_FORBIDDEN_OWNER[_CONTACT]`, `HOMEBREW_RELOCATE_BUILD_PREFIX`, `HOMEBREW_RELOCATABLE_INSTALL_NAMES`, `HOMEBREW_NO_ENV_HINTS`, `HOMEBREW_NO_EMOJI`, `HOMEBREW_INTEGRATION_TEST`, `HOMEBREW_INSTALLING_BUBBLEWRAP`, `HOMEBREW_DOWNLOAD_CONCURRENCY` (via EnvConfig), `SSL_CERT_FILE`/`GIT_SSL_CAINFO`/`GIT_SSL_CAPATH`/`HOMEBREW_CURL` (set as side effects).

## Open questions
- DownloadQueue concurrency model: with HOMEBREW_DOWNLOAD_CONCURRENCY > 1 the fetch/pre-extract happens in parallel worker processes (retryable_download.rb) and dependency installs are deferred until after all downloads; with <=1 it falls back to serial fetch+install of implicit deps in prelude (install_fetch_deps). The Rust port needs to decide whether to replicate the two-phase concurrent model or do simpler serial fetch-then-install. The .poured marker handshake between the queue and FormulaInstaller#pour is the coordination point.
- Attestation verification (Homebrew::Attestation) shells out to `gh attestation verify` for core-tap bottles and forces gh+deps to install first. Did not trace the gh invocation details / cosign bundle format — needs a separate spec if attestation is in scope for the Rust port.
- UnpackStrategy.detect(..., prioritize_extension: true).extract_nestedly handles nested archives and multiple compression formats (gzip/zstd/xz). Confirm which compression bottles actually use today (gzip per HOMEBREW_BOTTLES_EXTNAME_REGEX `.tar.gz`) vs. whether zstd bottles exist, to size the decompression dependency.
- Exact Mach-O editing approach (in-process crate vs shelling to install_name_tool/codesign) is a build-vs-buy decision; install_name_tool is only present with Xcode CLT, which may not be guaranteed on a pure-bottle machine — verify whether Homebrew bundles or requires these for relocation, or whether :any_skip_relocation bottles avoid the need entirely on typical installs.
- InstallRenamed semantics for install_etc_var (how existing user-modified config files in etc/ are preserved with .default suffixes) were not fully read — needs detail if config-file handling matters.

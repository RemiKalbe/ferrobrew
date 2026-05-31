# CLI surface + global config (cli/parser.rb, cli/args.rb, global.rb / startup/config.rb, env_config.rb, bin/brew + brew.sh derivation, allowlisted command option blocks)

## Key reference files
- `Library/Homebrew/cli/parser.rb`
- `Library/Homebrew/cli/args.rb`
- `Library/Homebrew/cli/error.rb`
- `Library/Homebrew/global.rb`
- `Library/Homebrew/startup/config.rb`
- `Library/Homebrew/startup.rb`
- `Library/Homebrew/env_config.rb`
- `Library/Homebrew/abstract_command.rb`
- `bin/brew`
- `Library/Homebrew/brew.sh`
- `Library/Homebrew/cmd/install.rb`
- `Library/Homebrew/cmd/uninstall.rb`
- `Library/Homebrew/cmd/list.rb`
- `Library/Homebrew/cmd/info.rb`
- `Library/Homebrew/cmd/outdated.rb`
- `Library/Homebrew/cmd/search.rb`
- `Library/Homebrew/cmd/upgrade.rb`
- `Library/Homebrew/cmd/fetch.rb`
- `Library/Homebrew/cmd/cleanup.rb`
- `Library/Homebrew/cmd/autoremove.rb`
- `Library/Homebrew/cmd/reinstall.rb`
- `Library/Homebrew/cmd/postinstall.rb`
- `Library/Homebrew/cmd/update-report.rb`

## Specification
# CLI Surface + Global Config Spec (for Rust reimplementation)

## 0. Two-layer architecture (CRITICAL)

Homebrew is bootstrapped by Bash, then hands off to Ruby. **All `HOMEBREW_*` path env vars are computed in Bash (`bin/brew` + `Library/Homebrew/brew.sh`) and *exported*; Ruby then just does `ENV.fetch("HOMEBREW_PREFIX")` etc. — it does NOT recompute them.** Ferrobrew must replicate the Bash derivation logic itself (there will be no Bash wrapper), then expose the values to the rest of the program. The Ruby files `startup/config.rb` and `global.rb` are just `ENV.fetch(...).freeze` consumers and document the required keys.

`startup.rb` line 4 enforces: `raise unless ENV["HOMEBREW_BREW_FILE"]` — i.e. `bin/brew` MUST have exported it. Ruby aborts if not called via `bin/brew`.

---

## 1. HOMEBREW_* path/dir constant table (derivation + defaulting)

Derivation happens in this order. `${VAR:-default}` = use VAR if set+nonempty else default. Realpath canonicalization steps exist to collapse symlinked parents (e.g. Fedora Silverblue `/home -> /var/home`).

### 1a. Platform detection (`brew.sh` lines 6-31)
- `HOMEBREW_PROCESSOR`: from `$MACHTYPE` — `arm64-*`/`aarch64-*` => `"arm64"`; `x86_64-*` => `"x86_64"`; else `uname -m`. In Rust: read target arch / `uname -m`.
- `HOMEBREW_SYSTEM`: from `$OSTYPE` — `darwin*` => `"Darwin"` (also sets internal flag `HOMEBREW_MACOS=1`); `linux*` => `"Linux"` (sets `HOMEBREW_LINUX=1`); else `uname -s`.
- `HOMEBREW_PHYSICAL_PROCESSOR` = `HOMEBREW_PROCESSOR`.

### 1b. Default prefixes/repositories (`brew.sh` lines 33-50)
Hardcoded:
- `HOMEBREW_MACOS_ARM_DEFAULT_PREFIX = "/opt/homebrew"`; `..._REPOSITORY` = same `/opt/homebrew`.
- `HOMEBREW_LINUX_DEFAULT_PREFIX = "/home/linuxbrew/.linuxbrew"`; `..._REPOSITORY = "/home/linuxbrew/.linuxbrew/Homebrew"`.
- `HOMEBREW_GENERIC_DEFAULT_PREFIX = "/usr/local"`; `..._REPOSITORY = "/usr/local/Homebrew"`.
- Select `HOMEBREW_DEFAULT_PREFIX`/`HOMEBREW_DEFAULT_REPOSITORY`: if macOS+arm64 => ARM defaults; elif Linux => Linux defaults; else generic. (Note: macOS x86_64 falls to the GENERIC `/usr/local` branch.)

### 1c. Default cache/logs/temp (`brew.sh` lines 52-67)
- macOS: `HOMEBREW_DEFAULT_CACHE="$HOME/Library/Caches/Homebrew"`, `HOMEBREW_DEFAULT_LOGS="$HOME/Library/Logs/Homebrew"`, `HOMEBREW_DEFAULT_TEMP="/private/tmp"`.
- non-macOS: `CACHE_HOME="${HOMEBREW_XDG_CACHE_HOME:-$HOME/.cache}"`; `HOMEBREW_DEFAULT_CACHE="$CACHE_HOME/Homebrew"`, `HOMEBREW_DEFAULT_LOGS="$CACHE_HOME/Homebrew/Logs"`; `HOMEBREW_DEFAULT_TEMP="/var/tmp"` if `/var/tmp` is readable+writable else `"/tmp"`.

### 1d. PREFIX/REPOSITORY/LIBRARY (`bin/brew` lines 73-108)
- `BREW_FILE_DIRECTORY` = canonical dir of the running `brew` executable (`pwd -P` of `${0%/*}`). `HOMEBREW_BREW_FILE = "$BREW_FILE_DIRECTORY/$(basename)"`.
- `HOMEBREW_PREFIX = "${HOMEBREW_BREW_FILE%/*/*}"` (strip last two path components: `<prefix>/bin/brew` -> `<prefix>`). If empty or equals the brew file itself => `HOMEBREW_PREFIX="/"`.
- `HOMEBREW_REPOSITORY = HOMEBREW_PREFIX` initially. If `bin/brew` is a symlink, resolve symlink target dir, set `HOMEBREW_REPOSITORY = "${target_dir%/*}"` (parent of the real `bin/`).
- macOS x86_64 special case: if `/usr/local/bin/brew` is a symlink AND `${HOMEBREW_PREFIX}/Cellar` is NOT a symlink AND the resolved `/usr/local` repository equals `HOMEBREW_REPOSITORY`, then force `HOMEBREW_PREFIX="/usr/local"`.
- `HOMEBREW_LIBRARY = "${HOMEBREW_REPOSITORY}/Library"`.
- Symlinked-parent canonicalization (`brew.sh` 73-87): if `HOMEBREW_PREFIX != HOMEBREW_DEFAULT_PREFIX` but `realpath(HOMEBREW_DEFAULT_PREFIX) == HOMEBREW_PREFIX`, replace with the default. Same for REPOSITORY.

### 1e. CELLAR / CASKROOM (`brew.sh` lines 89-108)
- `HOMEBREW_CELLAR`: if dir `${HOMEBREW_REPOSITORY}/Cellar` exists => that; else `${HOMEBREW_PREFIX}/Cellar`.
- Symlinked-parent canonicalization vs `${HOMEBREW_DEFAULT_PREFIX}/Cellar`.
- `HOMEBREW_CASKROOM = "${HOMEBREW_PREFIX}/Caskroom"`.

### 1f. CACHE/LOGS/TEMP (`brew.sh` 110-116, 937-951)
- `HOMEBREW_CACHE="${HOMEBREW_CACHE:-$HOMEBREW_DEFAULT_CACHE}"` (user override allowed).
- `HOMEBREW_LOGS="${HOMEBREW_LOGS:-$HOMEBREW_DEFAULT_LOGS}"`.
- `HOMEBREW_TEMP="${HOMEBREW_TEMP:-$HOMEBREW_DEFAULT_TEMP}"`; if not writable, reset to `HOMEBREW_DEFAULT_TEMP`.
- Late writability fallback: if `HOMEBREW_CACHE` dir exists but not writable (or parent not writable), warn and use `${HOMEBREW_REPOSITORY}/tmp/cache`, mkdir `<cache>/api`, copy old `api/.`.

### 1g. Derived-in-Ruby constants (`startup/config.rb`) — relative to the above
These are NOT env-derived; compute directly:
- `HOMEBREW_ORIGINAL_BREW_FILE` = `ENV[HOMEBREW_ORIGINAL_BREW_FILE]` (bin/brew sets `=HOMEBREW_BREW_FILE`; differs only when `HOMEBREW_FORCE_BREW_WRAPPER` set).
- `HOMEBREW_TEMP`: `mkpath` if missing, then `realpath` it.
- `HOMEBREW_SHIMS_PATH = HOMEBREW_LIBRARY/"Homebrew/shims"`.
- `HOMEBREW_DATA_PATH = HOMEBREW_LIBRARY/"Homebrew/data"`.
- `HOMEBREW_LINKED_KEGS = HOMEBREW_PREFIX/"var/homebrew/linked"`.
- `HOMEBREW_PINNED_KEGS = HOMEBREW_PREFIX/"var/homebrew/pinned"`.
- `HOMEBREW_PINNED_CASKS = HOMEBREW_PREFIX/"var/homebrew/pinned_casks"`.
- `HOMEBREW_LOCKS = HOMEBREW_PREFIX/"var/homebrew/locks"`.
- `HOMEBREW_TEMP_CELLAR = HOMEBREW_PREFIX/"var/homebrew/tmp/.cellar"`.
- `HOMEBREW_CACHE_FORMULA = HOMEBREW_CACHE/"Formula"`.
- `HOMEBREW_LOGS = Pathname(ENV[HOMEBREW_LOGS]).expand_path` (tilde-expanded).
- `HOMEBREW_TAP_DIRECTORY = HOMEBREW_LIBRARY/"Taps"`.
- `HOMEBREW_ALIASES`: first existing of `~/.config/brew-aliases` then `~/.brew-aliases` (realpath'd), else the first path unresolved.
- `HOMEBREW_RUBY_EXEC_ARGS = [RUBY_PATH, ENV[HOMEBREW_RUBY_WARNINGS], ENV[HOMEBREW_RUBY_DISABLE_OPTIONS]]` (Ruby-specific; ferrobrew can ignore).

### 1h. Constants from `global.rb` (also env-fetched, hardcoded www URLs)
- `HOMEBREW_WWW = "https://brew.sh"`, `HOMEBREW_API_WWW = "https://formulae.brew.sh"`, `HOMEBREW_DOCS_WWW = "https://docs.brew.sh"`.
- `HOMEBREW_PRODUCT`, `HOMEBREW_VERSION`, `HOMEBREW_SYSTEM`, `HOMEBREW_PROCESSOR`, `HOMEBREW_PHYSICAL_PROCESSOR` from env.
- `Homebrew::DEFAULT_PREFIX`, `Homebrew::DEFAULT_REPOSITORY` from `ENV[HOMEBREW_DEFAULT_PREFIX/REPOSITORY]`. `DEFAULT_CELLAR = "#{DEFAULT_PREFIX}/Cellar"`.
- `Homebrew.default_prefix?(prefix=HOMEBREW_PREFIX)` => `prefix.to_s == DEFAULT_PREFIX`.

### 1i. PLACEHOLDER tokens (used for relocatable/portable paths — keep EXACT strings)
- `HOMEBREW_PREFIX_PLACEHOLDER = "$HOMEBREW_PREFIX"`
- `HOMEBREW_CELLAR_PLACEHOLDER = "$HOMEBREW_CELLAR"`
- `HOMEBREW_HOME_PLACEHOLDER = "/$HOME"` (note leading slash, intentional)
- `HOMEBREW_CASK_APPDIR_PLACEHOLDER = "$APPDIR"`

### 1j. User config dir (`bin/brew` 165-175)
`HOMEBREW_USER_CONFIG_HOME`: `${XDG_CONFIG_HOME}/homebrew` if `$XDG_CONFIG_HOME` set; elif `${HOMEBREW_XDG_CONFIG_HOME}/homebrew`; else `${HOME}/.homebrew`.

### 1k. brew.env files (loaded in `bin/brew` 124-181) — config-from-disk, simple `KEY=VALUE` lines
Load order: `/etc/homebrew/brew.env` (system), `${HOMEBREW_PREFIX}/etc/homebrew/brew.env` (prefix), `${HOMEBREW_USER_CONFIG_HOME}/brew.env` (user); if `HOMEBREW_SYSTEM_ENV_TAKES_PRIORITY` set, re-load system last. Only lines matching `^(HOMEBREW_|SUDO_ASKPASS=|(all|no|ftp|https?)_proxy=)` are accepted. Lines matching `BIN_BREW_EXPORTED_VARS` (`HOMEBREW_BREW_FILE|HOMEBREW_PREFIX|HOMEBREW_REPOSITORY|HOMEBREW_LIBRARY|HOMEBREW_USER_CONFIG_HOME|HOMEBREW_ORIGINAL_BREW_FILE`) are forbidden (skipped). `HOMEBREW_EXPERIMENTAL_RUST_FRONTEND` from an env file is ignored with a warning.

---

## 2. Global config env-var registry (`env_config.rb`, `Homebrew::EnvConfig`)

`ENVS` is an ordered Hash (128 `HOMEBREW_*` keys + `SUDO_ASKPASS`, `all_proxy`, `ftp_proxy`, `http_proxy`, `https_proxy`, `no_proxy`). Each value is a metadata hash with optional keys:
- `description: String` (man-page text)
- `boolean: true` (when present, the var is a flag)
- `default: <value>` (String/Integer/constant/lambda) — only meaningful for non-boolean
- `default_text: String` (display-only override of default in docs)
- `hidden: true` (suppress in man page; used by parser to suppress "Enabled by default..." note)

### 2a. Accessor method-name generation (lines 733-782)
For each `(env, hash)`: `method = env.to_s.sub(/^HOMEBREW_/,"").downcase`; if `hash[:boolean]`, append `"?"`. Then:
- boolean => method returns true iff `ENV[env]` present AND `ENV[env].downcase NOT in FALSY_VALUES`.
- non-boolean with `default` => `ENV[env].presence || default.to_s`.
- non-boolean no default => `ENV[env].presence` (nilable String).

`FALSY_VALUES = ["false","no","off","nil","0"]`. Any other nonempty value (e.g. `"1"`, `"true"`, `"x"`) is truthy.

Examples: `HOMEBREW_DEBUG` (boolean) => `debug?`; `HOMEBREW_NO_AUTO_UPDATE` => `no_auto_update?`; `HOMEBREW_MAKE_JOBS` (no `?`, has default lambda) => `make_jobs`; `HOMEBREW_API_DOMAIN` => `api_domain` returning default `HOMEBREW_API_DEFAULT_DOMAIN`.

### 2b. CUSTOM_IMPLEMENTATIONS (NOT auto-generated; hand-written, lines 744-756 & 784-922)
`HOMEBREW_BUNDLE_DESCRIBE, HOMEBREW_BUNDLE_JOBS, HOMEBREW_BUNDLE_NO_SECRETS, HOMEBREW_CASK_OPTS, HOMEBREW_CASK_OPTS_BINARIES, HOMEBREW_CASK_OPTS_REQUIRE_SHA, HOMEBREW_DOWNLOAD_CONCURRENCY, HOMEBREW_FORBID_PACKAGES_FROM_PATHS, HOMEBREW_MAKE_JOBS, HOMEBREW_SANDBOX_LINUX, HOMEBREW_USE_INTERNAL_API`. Notable: `cask_opts` = `Shellwords.shellsplit(ENV["HOMEBREW_CASK_OPTS"])`; `download_concurrency` = if `"auto"` => `Hardware::CPU.cores*2` else `.to_i`, clamped min 1; `make_jobs` = `ENV[HOMEBREW_MAKE_JOBS].to_i` if positive else default lambda. `devcmdrun?` reads `Homebrew::Settings.read("devcmdrun")=="true"` (NOT env). `cask_opts_binaries?`/`quarantine?`/`require_sha?` scan `cask_opts` array right-to-left for `--binaries`/`--no-binaries` etc. then fall back to `HOMEBREW_CASK_OPTS_*` env.

### 2c. ENVS keys with explicit defaults (the non-boolean ones that matter)
`HOMEBREW_API_AUTO_UPDATE_SECS=450`, `HOMEBREW_API_DOMAIN=HOMEBREW_API_DEFAULT_DOMAIN`, `HOMEBREW_ARCH="native"`, `HOMEBREW_BOTTLE_DOMAIN=HOMEBREW_BOTTLE_DEFAULT_DOMAIN`, `HOMEBREW_BREW_GIT_REMOTE=HOMEBREW_BREW_DEFAULT_GIT_REMOTE`, `HOMEBREW_CACHE=HOMEBREW_DEFAULT_CACHE`, `HOMEBREW_CLEANUP_MAX_AGE_DAYS=120`, `HOMEBREW_CLEANUP_PERIODIC_FULL_DAYS=30`, `HOMEBREW_CORE_GIT_REMOTE=HOMEBREW_CORE_DEFAULT_GIT_REMOTE`, `HOMEBREW_CURL_PATH="curl"`, `HOMEBREW_CURL_RETRIES=3`, `HOMEBREW_DOWNLOAD_CONCURRENCY="auto"`, `HOMEBREW_FAIL_LOG_LINES=15`, `HOMEBREW_FORBIDDEN_OWNER="you"`, `HOMEBREW_GIT_PATH="git"`, `HOMEBREW_INSTALL_BADGE="🍺"`, `HOMEBREW_LIVECHECK_WATCHLIST="${HOMEBREW_USER_CONFIG_HOME}/livecheck_watchlist.txt"`, `HOMEBREW_LOGS=HOMEBREW_DEFAULT_LOGS`, `HOMEBREW_TEMP=HOMEBREW_DEFAULT_TEMP`.

### 2d. Boolean ENVS keys that the CLI/parsing layer cares about most
`HOMEBREW_DEBUG`(=>debug?), `HOMEBREW_VERBOSE`(=>verbose?), `HOMEBREW_COLOR`, `HOMEBREW_NO_COLOR`, `HOMEBREW_NO_EMOJI`, `HOMEBREW_DEVELOPER`, `HOMEBREW_ASK`, `HOMEBREW_NO_ASK`, `HOMEBREW_NO_AUTO_UPDATE`, `HOMEBREW_NO_INSTALL_CLEANUP`, `HOMEBREW_NO_INSTALL_UPGRADE`, `HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK`, `HOMEBREW_NO_INSTALL_FROM_API`, `HOMEBREW_EVAL_ALL`, `HOMEBREW_DISPLAY_INSTALL_TIMES`, `HOMEBREW_UPGRADE_GREEDY`, `HOMEBREW_NO_UPGRADE_QUIT_CASKS`, `HOMEBREW_NO_AUTOREMOVE`, `HOMEBREW_NO_GITHUB_API`, `HOMEBREW_NO_ANALYTICS`. (Full set of 128 is in the file; replicate all keys as a static table even if not all are wired to behavior yet.)

---

## 3. The CLI parser (`cli/parser.rb`) — semantics ferrobrew must reproduce

### 3a. Parser identity & constants
- `HIDDEN_DESC_PLACEHOLDER = "@@HIDDEN@@"` — used as the description for `hidden:` options; later stripped from help output via regex `/\n.*?@@HIDDEN@@.*?(?=\n)/`. Hidden options are still parsed/accepted; just not shown.
- `SYMBOL_TO_USAGE_MAPPING`: `{service: "<service>", text_or_regex: "<text>|`/`<regex>`/`", url: "<URL>"}` — used in usage-banner generation for named-arg types.
- `option_to_name(option)` (the canonical key): `option.sub(/\A--?(\[no-\])?/,"").tr("-","_").delete("=")`. So `--build-from-source` => `build_from_source`; `--[no-]binaries` => `binaries`; `--os=` => `os`. (NOTE: `Args#option_to_name`, the private one, is simpler: `sub(/\A--?/,"").tr("-","_")` — used only for reconstructing cli_args.)

### 3b. Global options (applied to EVERY command, `global_options`, lines 152-160)
Added in the Parser constructor via `switch` with `method: :on_tail` (so they sort to the bottom of help):
- `-d`, `--debug` => "Display any debugging information." => accessor `debug?`
- `-q`, `--quiet` => "Make some output more quiet." => `quiet?`
- `-v`, `--verbose` => "Make some output more verbose." => `verbose?`
- `-h`, `--help` => "Show this message." => `help?`

Each global switch is registered with `env: option_to_name(long)` so its default comes from the env var of the same name UPPERCASED: `--debug` <- `HOMEBREW_DEBUG`, `--verbose` <- `HOMEBREW_VERBOSE`, `--quiet` <- `HOMEBREW_QUIET` (note: there is no `HOMEBREW_QUIET` in EnvConfig, so it resolves via the raw `ENV.fetch("HOMEBREW_QUIET")` fallback path in `value_for_env`). `--help` <- `HOMEBREW_HELP` (also raw fallback).

NOTE: `--force`/`-f` and `-n`/`--dry-run` are NOT global — they are declared per-command (see §4).

### 3c. switch() / flag() / comma_array() declaration semantics
- `switch(*names, description:, env:, depends_on:, method: :on, hidden:, replacement:, odeprecated:, odisabled:, disable:, subcommands:)`: a boolean option. Names like `--[no-]binaries` create a negatable switch (value can be true/false). Default value when `--[no-]` absent is `false`; when present, default is `nil`. Accessor is `"#{name}?"`. When passed, value is `true` unless a `--[no-]` form (then the parsed bool). The boolean accessor for `--foo` is `foo?`.
- `flag(*names, ...)`: a value option. If any name ends with `=` => `REQUIRED_ARGUMENT` (type `:required_flag`); else `OPTIONAL_ARGUMENT` (type `:optional_flag`). Names have trailing `=` chomped. Accessor is `name` (no `?`), value is the String. Default `nil`.
- `comma_array(name, ...)`: `--language` style; `REQUIRED_ARGUMENT` parsed as Array (comma-split). Accessor `name` returns `Array[String]`.
- `env:` on a switch/flag: if env var set+truthy, switch defaults ON (source `:env`). When `env` is a 2-tuple `[env, counterpart]`, help text appends "Enabled by default if `$HOMEBREW_X` is set and `<counterpart>` is passed." Skipped if option or env is `hidden`. `value_for_env`: returns `false` if `env=="ask"` and `EnvConfig.no_ask?`; else if `EnvConfig` responds to `"#{env}?"` uses that, else `ENV.fetch("HOMEBREW_#{env.upcase}")`.
- `depends_on: "--X"`: records a constraint that this option requires `--X` also be passed (else `OptionConstraintError`, "`--this` cannot be passed without `--X`.").
- `conflicts(*opts)`: records mutual-exclusion group (>=2 passed => `OptionConflictError`, "Options `--a` and `--b` are mutually exclusive.") — UNLESS exactly one came from CLI and others from env, in which case env ones are silently disabled.
- `odeprecated`/`odisabled`/`replacement`/`disable`: deprecation machinery. `odisabled`/`odeprecated` force `hidden=true`. Disabled options still parse but error/warn. For ferrobrew: treat `odisabled: true` options as accepted-but-hidden no-ops; `hidden:` => accept but omit from help.
- `subcommands:` on an option: restricts the option to named subcommands of the command (parser tracks `@option_subcommands`); passing an option not allowed for the active subcommand => `UsageError "The `<sub>` subcommand does not accept the `--x` <switch|flag>."`.

### 3d. named_args(type, number:, min:, max:, without_api:)
Declares positional args. `type` is a Symbol, Array of Symbols/Strings, or `:none`. Common types: `:formula`, `:cask`, `:installed_formula`, `:installed_cask`, `:text_or_regex`, `:none`. `[:formula, :cask]` means accepts either. `:none` => max 0. `number: N` => min=max=N. `min:`/`max:` set bounds. Count validation raises:
- exact mismatch (min==max) => `NumberOfNamedArgumentsError("This command requires exactly N <types> argument(s).")`
- below min => `MinNamedArgumentsError("This command requires at least N <types> argument(s).")`
- above max => `MaxNamedArgumentsError`. If max==0: "This command does not take named arguments." else "This command does not take more than N <types> argument(s)."
- types in the message are `.tr("_"," ")`-joined with " or ".

### 3e. parse() flow (lines 457-537) — the dispatch ferrobrew must mirror
1. If command declared `formula_options` and argv is not cask-only (`only_casks?` = argv contains `--casks` or `--cask`): first pass parses with `ignore_invalid_options:true` to extract formula names, loads each formula, and dynamically appends each formula's custom options (as switch/flag, conflicting with `--cask`). (Ferrobrew can defer the dynamic formula-option injection — it needs the formula DSL — but must still tolerate unknown `--foo` after formula names rather than erroring.)
2. `parse_remaining(argv)`: splits on `--` separator (`split_non_options`: everything before `--` is options-eligible, everything after is literal non-options). Parses token-by-token. Unknown option => if `ignore_invalid_options` OR (named_args allows `:command` AND token resolves to a command path) => keep as remaining; else print help to stderr and raise `InvalidOption`. `MissingArgument` => try consuming the next token as the value.
3. `named_args = remaining + non_options`.
4. Subcommand alias handling: if subcommands declared and first named arg is an alias mapped in `alias_options`, set that switch true.
5. For non-dev commands: `set_default_options` + `validate_options` (both no-ops in base; overridable). Then `check_constraint_violations` (invalid-constraint check, conflicts, depends-on), `check_named_args` (count), `check_subcommand_violations`.
6. If subcommands: resolve `subcommand` (first named arg or the `default:` subcommand); strip it from named args; set `args.subcommand`.
7. Freeze named args (build a `NamedArgs` with `force_bottle: table[:force_bottle?]`, `override_spec: :head if --HEAD`, `cask_options`, `without_api`), freeze remaining, freeze processed_options.
8. If `help?` set (and not ignore mode): print generated help text and `exit` (0).

### 3f. Help text generation (`generate_help_text`, 545-590)
Builds banner + description + (Subcommands list) + option summaries; then applies: strip `@@HIDDEN@@` lines, prefix with `"Usage: brew "` (bold), bold backticked spans, format URLs in `<...>`, underline `*...*` and `<...>` spans. Usage banner auto-generated (`generate_usage_banner`, 742-796): `\`<cmd>\`, \`<alias>\`...` + options summary (` [options]` if >2 non-global options, else explicit `[\`--x\`]`/`[\`--x=\`]`) + named-args portion using `SYMBOL_TO_USAGE_MAPPING` and min/max to choose `[<x>]` / `[<x> ...]` / `<x>` / `<x> ...`.

---

## 4. Args object accessors (`cli/args.rb`)

`Args` is the parsed-result object (frozen after parse). Conceptually it is a `HashMap<String, Value>` (`@table`) where:
- boolean switches => key `"#{name}?"` => bool (default false, or nil for `--[no-]` un-passed).
- flags => key `name` => `Option<String>`.
- comma_array => key `name` => `Vec<String>`.
- `subcommand` => `Option<String>` (when subcommands used).

Important methods/fields ferrobrew should expose:
- `named` => the positional args (`NamedArgs`, lazily resolves to formulae/casks). `no_named?` => empty.
- `remaining`, `options_only` (cli tokens starting with `-`), `flags_only` (tokens starting with `--`).
- `value(name)` => for `--name=val` returns `val`.
- `context` => `{debug, quiet, verbose}` global verbosity struct.
- `only_formula_or_cask` => `:formula` if `formula? && !cask?`; `:cask` if reverse; else nil.
- `os_arch_combinations`: resolves `--os`/`--arch`/`--all-platforms` into `[(os,arch)]`. `--all-platforms` == `--os=all --arch=all`. nil os => current os; `:all` => `OnSystem::ALL_OS_OPTIONS`; else `[sym]`. Same for arch (`OnSystem::ARCH_OPTIONS`). When `:all` used, invalid (os,arch) bottle-tag combos are filtered out.
- `build_from_source_formulae` / `include_test_formulae`: full names when `--build-from-source`/`--HEAD`/`--build-bottle` / `--include-test`.
- `OptionsType` element shape: `[short:Option<String>, long:Option<String>, desc:String, hidden:bool]`.

---

## 5. Per-command flag inventory (allowlisted commands)

Notation: switch `=>` accessor `foo?`; flag `--x=` `=>` `x` (String); env-backed shown as `<-ENV`. Conflicts and named_args noted.

### install (`cmd/install.rb`) — `named_args [:formula, :cask], min: 1`
Global-ish per-cmd: `-d/--debug`, `-f/--force`, `-v/--verbose`, `-n/--dry-run`, `--display-times`(<-display_install_times), `--ask`(<-ask).
Formula-side (each conflicts `--cask`): `--formula/--formulae`, `--env=`(hidden), `--ignore-dependencies`, `--only-dependencies`, `--cc=`, `-s/--build-from-source`, `--force-bottle`, `--include-test`, `--HEAD`, `--fetch-HEAD`, `--keep-tmp`, `--debug-symbols`(depends_on `--build-from-source`), `--build-bottle`, `--skip-post-install`, `--skip-link`, `--as-dependency`, `--bottle-arch=`(depends_on `--build-bottle`), `-i/--interactive`, `-g/--git`, `--overwrite`. Then `formula_options`.
Cask-side (each conflicts `--formula`): `--cask/--casks`, `--[no-]binaries`(<-cask_opts_binaries), `--require-sha`(<-cask_opts_require_sha), `--[no-]quarantine`(<-cask_opts_quarantine, odisabled), `--adopt`, `--skip-cask-deps`, `--zap`. Then `cask_options` (adds the 17 `--*dir=` cask dirs + `--language`).
Conflicts: `--ignore-dependencies`/`--only-dependencies`; `--build-from-source`/`--build-bottle`/`--force-bottle`; `--adopt`/`--force`.

### uninstall (`cmd/uninstall.rb`) — `named_args [:installed_formula, :installed_cask], min: 1`
`-f/--force`, `--zap`, `--ignore-dependencies`, `--formula/--formulae`, `--cask/--casks`. Conflicts: `--formula`/`--cask`; `--formula`/`--zap`.

### list (`cmd/list.rb`) — `named_args [:installed_formula, :installed_cask]`
`--formula/--formulae`, `--cask/--casks`, `--full-name`, `--versions`, `--json`, `--multiple`, `--pinned`, `--installed-on-request`, `--installed-as-dependency`, `--poured-from-bottle`, `--built-from-source`, plus ls passthrough `-1`, `-l`, `-r`, `-t`. Extensive conflicts (see file): `--formula`/`--cask`; `--multiple`/`--cask`; `--pinned`/`--multiple`; each of the four install-state switches conflicts `--cask`,`--versions`,`--multiple`,`--pinned`,`-l`; each ls flag conflicts `--versions`,`--multiple`,`--pinned`; `--full-name` conflicts each of `--versions`/`--multiple`/`--pinned`/`-l`/`-r`/`-t`.

### info (`cmd/info.rb`) — `named_args [:formula, :cask]`
`--analytics`, `--days=`(depends `--analytics`), `--category=`(depends `--analytics`), `--github-packages-downloads`(hidden), `--github`, `--fetch-manifest`, `--json` (flag, value e.g. `v1`/`v2`), `--installed`, `--eval-all`(depends `--json`), `--variations`(depends `--json`), `-v/--verbose`, `--formula/--formulae`, `--cask/--casks`, `--sizes`. Conflicts: `--installed`/`--eval-all`; `--formula`/`--cask`; `--fetch-manifest`/`--cask`; `--fetch-manifest`/`--json`.

### outdated (`cmd/outdated.rb`) — `named_args [:formula, :cask]`
`-q/--quiet`, `-v/--verbose`, `--formula/--formulae`, `--cask/--casks`, `--json`(flag v1/v2), `--minimum-version=`/`--min-version=`, `--fetch-HEAD`, `-g/--greedy`(<-upgrade_greedy), `--greedy-latest`, `--greedy-auto-updates`. Conflicts: `--quiet`/`--verbose`/`--json`; `--formula`/`--cask`.

### search (`cmd/search.rb`) — `named_args :text_or_regex, min: 1`
`--formula/--formulae`, `--cask/--casks`, `--desc`, `--eval-all`(<-eval_all), `--pull-request`, `--open`(depends `--pull-request`), `--closed`(depends `--pull-request`), plus one switch per package manager in `PACKAGE_MANAGERS` (e.g. `--repology`, `--macports`, etc.). Conflicts: `--desc`/`--pull-request`; `--open`/`--closed`; all package-manager switches mutually exclusive.

### upgrade (`cmd/upgrade.rb`) — `named_args [:installed_formula, :installed_cask]` (no min)
Like install: `-d/--debug`, `--display-times`(<-display_install_times), `-f/--force`, `-v/--verbose`, `-n/--dry-run`, `--minimum-version=`/`--min-version=`, `--ask`(<-ask). Formula-side (conflict `--cask`): `--formula/--formulae`, `-s/--build-from-source`, `-i/--interactive`, `--force-bottle`, `--fetch-HEAD`, `--keep-tmp`, `--debug-symbols`(depends `--build-from-source`), `--overwrite`. `formula_options`. Cask-side (conflict `--formula`): `--cask/--casks`, `--skip-cask-deps`, `--no-quit`(<-no_upgrade_quit_casks), `-g/--greedy`(<-upgrade_greedy), `--greedy-latest`, `--greedy-auto-updates`, `--[no-]binaries`(<-cask_opts_binaries), `--require-sha`(<-cask_opts_require_sha), `--[no-]quarantine`(<-cask_opts_quarantine, odisabled). `cask_options`. Conflict: `--build-from-source`/`--force-bottle`.

### fetch (`cmd/fetch.rb`) — `named_args [:formula, :cask], min: 1`
`--os=`, `--arch=`, `--all-platforms`, `--bottle-tag=`, `--HEAD`, `-f/--force`, `-v/--verbose`, `--retry`, `--deps`, `-s/--build-from-source`, `--build-bottle`, `--force-bottle`, `--[no-]quarantine`(<-cask_opts_quarantine, odisabled), `--formula/--formulae`, `--cask/--casks`. Many conflicts: `--build-from-source`/`--build-bottle`/`--force-bottle`/`--bottle-tag`; `--cask` vs each of `--HEAD`,`--deps`,`-s`,`--build-bottle`,`--force-bottle`,`--bottle-tag`; `--formula`/`--cask`; `--os`/`--bottle-tag`; `--arch`/`--bottle-tag`; `--all-platforms` vs `--os`/`--arch`/`--bottle-tag`.

### cleanup (`cmd/cleanup.rb`) — `named_args [:formula, :cask]`
`--prune=` (days or `all`), `-n/--dry-run`, `-s/--scrub`, `--prune-prefix`. Default-age in description uses `HOMEBREW_CLEANUP_MAX_AGE_DAYS` (120).

### autoremove (`cmd/autoremove.rb`) — `named_args :none`
`-n/--dry-run` only.

### reinstall (`cmd/reinstall.rb`) — `named_args [:formula, :cask], min: 1`
Like install (subset): `-d/--debug`, `--display-times`(<-display_install_times), `-f/--force`, `-v/--verbose`, `--ask`(<-ask). Formula-side (conflict `--cask`): `--formula/--formulae`, `-s/--build-from-source`, `-i/--interactive`, `--force-bottle`, `--keep-tmp`, `--debug-symbols`(depends `--build-from-source`), `-g/--git`. `formula_options`. Cask-side (conflict `--formula`): `--cask/--casks`, `--[no-]binaries`(<-cask_opts_binaries), `--require-sha`(<-cask_opts_require_sha), `--[no-]quarantine`(<-cask_opts_quarantine, odisabled), `--adopt`, `--skip-cask-deps`, `--zap`. `cask_options`. Conflict: `--build-from-source`/`--force-bottle`.

### postinstall (`cmd/postinstall.rb`) — `named_args :installed_formula, min: 1`
No options (only the 4 global ones).

### update-report (`cmd/update-report.rb`) — Ruby half of `brew update`; `hide_from_man_page!`, never called manually
`--auto-update`/`--preinstall`, `-f/--force`. Reads env vars at runtime: `HOMEBREW_UPDATE_BEFORE`, `HOMEBREW_UPDATE_AFTER` (required, else `odie`), `HOMEBREW_UPDATE_FAILED`, `HOMEBREW_UPDATE_REPORT_ONLY_INSTALLED`, `HOMEBREW_NO_UPDATE_REPORT_NEW`, `HOMEBREW_AUTO_UPDATE_COMMAND`. Writes `Settings` keys `latesttag`. (Out of scope for arg-parsing layer but listed for completeness.)

### cask_options (shared, parser.rb `global_cask_options`) — added by commands calling `cask_options`
17 target-dir flags, each `--<x>=`: `--appdir=`, `--appimagedir=`, `--keyboard-layoutdir=`, `--colorpickerdir=`, `--prefpanedir=`, `--qlplugindir=`, `--mdimporterdir=`, `--dictionarydir=`, `--fontdir=`, `--servicedir=`, `--input-methoddir=`, `--internet-plugindir=`, `--audio-unit-plugindir=`, `--vst-plugindir=`, `--vst3-plugindir=`, `--screen-saverdir=`; plus `--language` (comma_array). Each conflicts `--formula`. Defaults come from `Cask::Config::DEFAULT_DIRS`.

---

## 6. Command-class wiring (`abstract_command.rb`)

- `command_name`: `Utils.underscore(ClassName.split("::").last).tr("_","-").delete_suffix("-cmd")`. So `InstallCmd` => `install`, `UpdateReportCmd`/`UpdateReport` => `update-report`.
- `command(name)` finds subclass by command_name. `dev_cmd?` = class name starts with `Homebrew::DevCmd`. `ruby_cmd?` = not a ShellCommand.
- `cmd_args(&block)` stores the parser DSL block; `parser` => `CLI::Parser.new(self, &block)`. `initialize(argv=ARGV)` => `@args = parser.parse(argv)`.
- For non-dev commands, `set_default_options`/`validate_options` run during parse (base no-op).

## Rust implementation notes
Recommended Rust structure:

- **Path config**: a `Config` struct holding `prefix, repository, library, cellar, caskroom, cache, logs, temp` plus all derived dirs (`linked_kegs`, `pinned_kegs`, `pinned_casks`, `locks`, `temp_cellar`, `cache_formula`, `tap_directory`, `shims_path`, `data_path`, `aliases`) as `PathBuf`. Build it once at startup by replicating §1's Bash logic in Rust (do NOT shell out to bash). Detect arch via `std::env::consts::ARCH` (map `aarch64`->`arm64`, `x86_64` stays) and OS via `cfg!(target_os)` or `std::env::consts::OS`. Compute `prefix` by walking up from `std::env::current_exe()` two components, then apply the symlink/realpath canonicalization with `std::fs::canonicalize`. GOTCHA: canonicalize fails on nonexistent paths — guard with `.exists()` first, mirroring the Bash `realpath()` (which is `cd && pwd -P`, i.e. only works on existing dirs). The `/usr/local` symlink special-case (`bin/brew` 92-102) needs `std::fs::read_link` + checking `Cellar` is not itself a symlink. CELLAR selection branches on directory existence — check `repository.join("Cellar").is_dir()`.

- **Env registry**: encode `ENVS` as a `static` table — e.g. `phf` map or a `Vec<EnvVar>` of `{ name: &str, kind: EnvKind, default: Default, hidden: bool }` where `EnvKind` is `Boolean | StringVal | IntVal`. Implement the accessor logic once: boolean => `env_truthy(name)` = value present && `!FALSY_VALUES.contains(&value.to_ascii_lowercase().as_str())` with `FALSY_VALUES = ["false","no","off","nil","0"]`. Non-boolean => `env::var(name).ok().filter(|s| !s.is_empty()).unwrap_or(default)`. Keep CUSTOM_IMPLEMENTATIONS as hand-written fns (cask_opts shell-split via the `shell-words` crate; download_concurrency via `num_cpus::get()*2`). Note `devcmdrun` reads a settings file, not env.

- **CLI parsing**: clap derive does NOT cleanly model Homebrew's semantics (env-backed default-on switches, `--[no-]x` negation pairs, conflicts/depends-on declared at runtime, dynamic per-formula options, the `--` passthrough plus tolerate-unknown-after-formula-names behavior, subcommand-scoped options). Recommend a hand-rolled parser modeled on `parser.rb`: a `Parser` builder with `switch()/flag()/comma_array()/conflicts()/depends_on()/named_args()`, producing an `Args` map (`HashMap<String, ArgValue>` where `ArgValue = Bool(bool) | Opt(Option<String>) | List(Vec<String>)`). Canonicalize option->key with the exact regex `^--?(\[no-\])?` strip, `-`->`_`, drop `=`. Reproduce: (1) global `-d/-q/-v/-h` on every command with env defaults from `HOMEBREW_DEBUG/QUIET/VERBOSE/HELP`; (2) `--` separator splitting (everything after is literal); (3) two-pass parse when `formula_options` present (you can stub the dynamic-formula-option injection initially but MUST still treat unknown `--flags` after formula names as passthrough, not errors); (4) the validation order: invalid-constraint -> conflicts (with env-source demotion) -> depends-on -> named-arg count -> subcommand-scope; (5) `--help` prints help and exits 0. Error types map directly to the `UsageError` subclasses in `cli/error.rb` with the exact message strings.

- **Negatable switches** (`--[no-]binaries`): represent as a tri-state `Option<bool>` (None=unset, Some(true)=--binaries, Some(false)=--no-binaries) since default behavior differs from explicit-true.

- **Placeholder tokens** are literal magic strings used elsewhere (relocation); store as `const` exactly: `"$HOMEBREW_PREFIX"`, `"$HOMEBREW_CELLAR"`, `"/$HOME"` (leading slash!), `"$APPDIR"`, and `HIDDEN_DESC_PLACEHOLDER="@@HIDDEN@@"`.

- **brew.env loading**: parse `KEY=VALUE` lines from the 3 locations (system/prefix/user) in order, filter by the allowed-prefix regex, forbid the 6 BIN_BREW_EXPORTED_VARS, ignore `HOMEBREW_EXPERIMENTAL_RUST_FRONTEND` with a warning. Use the `dotenv`-style manual parse (don't pull a heavy crate; lines are simple `export`-able assignments without quotes-stripping in the Bash version — it does raw `export "${line}"`).

- **Atomicity/gotchas**: `HOMEBREW_TEMP` is mkdir'd + realpath'd at startup (config.rb 29-32) — replicate (create dir, canonicalize). `HOMEBREW_LOGS` is tilde-expanded. The cache-writability fallback (brew.sh 937-951) copies `api/` contents — replicate if you implement cache. None of this layer touches codesigning.

## Open questions
- The dynamic per-formula option injection (parser.rb 464-484, `formula_options`) requires loading the Formula DSL to read each formula's `option`/`depends_on` declarations; ferrobrew must decide whether to support custom formula options at all in v1 or just tolerate-and-passthrough unknown flags after formula names.
- `PACKAGE_MANAGERS` (search.rb) is an external constant defining which `--<pm>` search switches exist (repology, macports, etc.); its full key list was not read here and must be sourced from its defining file.
- `Cask::Config::DEFAULT_DIRS` provides the default values shown in the 17 cask `--*dir=` flag descriptions; the actual default paths live in cask/config.rb and were not enumerated in this pass.
- `OnSystem::ALL_OS_OPTIONS` / `ARCH_OPTIONS` and `Utils::Bottles::Tag#valid_combination?` (used by `os_arch_combinations`) are defined elsewhere; the concrete OS/arch lists and validity matrix need to be pulled from those files.
- Whether ferrobrew is invoked through the existing Bash `bin/brew`+`brew.sh` (env pre-exported) or standalone determines how much of the §1 derivation must be reimplemented vs. trusting `ENV`; the `HOMEBREW_EXPERIMENTAL_RUST_FRONTEND` references in brew.sh suggest a hybrid hand-off is intended, so confirm the integration boundary.
- `Homebrew::Settings` (read/write of keys like `devcmdrun`, `latesttag`) is a file-backed key/value store not covered here; needed for `devcmdrun?` and update-report.

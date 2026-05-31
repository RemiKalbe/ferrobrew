# JSON API client (Homebrew::API and api/*) — formulae.brew.sh bulk + per-formula JSON, JWS verification, on-disk cache, staleness/conditional requests, oldnames/aliases resolution, and the formula JSON fields needed to install.

## Key reference files
- `Library/Homebrew/api.rb`
- `Library/Homebrew/api/formula.rb`
- `Library/Homebrew/api/cask.rb`
- `Library/Homebrew/api/internal.rb`
- `Library/Homebrew/api/json_download.rb`
- `Library/Homebrew/api/source_download.rb`
- `Library/Homebrew/api/formula_struct.rb`
- `Library/Homebrew/api/formula/formula_struct_generator.rb`
- `Library/Homebrew/api_hashable.rb`
- `Library/Homebrew/formulary.rb`
- `Library/Homebrew/tap/core_tap.rb`
- `Library/Homebrew/tap.rb`
- `Library/Homebrew/global.rb`
- `Library/Homebrew/brew.sh`
- `Library/Homebrew/env_config.rb`
- `Library/Homebrew/utils/curl.rb`
- `Library/Homebrew/formula.rb`

## Specification
# Homebrew JSON API Client — Implementation Spec

## 1. Domains, env vars, constants

Hardcoded defaults (set in `Library/Homebrew/brew.sh` lines 637-638, 816-817; exported to the Ruby process):
- `HOMEBREW_API_DEFAULT_DOMAIN = "https://formulae.brew.sh/api"`
- `HOMEBREW_BOTTLE_DEFAULT_DOMAIN = "https://ghcr.io/v2/homebrew/core"`
- `HOMEBREW_CURL_SPEED_LIMIT = 100` (bytes/sec)
- `HOMEBREW_CURL_SPEED_TIME = 5` (seconds)

These are read in Ruby via `ENV.fetch(...)` in `global.rb` (lines 8-9) and become module constants `HOMEBREW_API_DEFAULT_DOMAIN`, `HOMEBREW_BOTTLE_DEFAULT_DOMAIN`. Ferrobrew should hardcode the two URL defaults and the two curl speed numbers, but still allow env overrides of `HOMEBREW_API_DEFAULT_DOMAIN` / `HOMEBREW_BOTTLE_DEFAULT_DOMAIN` (Homebrew lets the env shadow them).

Env config accessors (defaults from `env_config.rb`):
- `HOMEBREW_API_DOMAIN` — `EnvConfig.api_domain`. Default = `HOMEBREW_API_DEFAULT_DOMAIN`. This is the *primary* mirror; if a request fails, code falls back to `HOMEBREW_API_DEFAULT_DOMAIN`.
- `HOMEBREW_API_AUTO_UPDATE_SECS` — `EnvConfig.api_auto_update_secs`. Default = `450` (integer seconds).
- `HOMEBREW_CURL_RETRIES` — `EnvConfig.curl_retries`. Default = `3`.
- `HOMEBREW_ARTIFACT_DOMAIN` / `HOMEBREW_ARTIFACT_DOMAIN_NO_FALLBACK` — used by the *download/bottle* layer (not the JSON API client itself), to prefix or replace download URLs. Out of scope for the JSON fetch but relevant when ferrobrew later builds bottle download URLs.
- `HOMEBREW_NO_INSTALL_FROM_API` — `EnvConfig.no_install_from_api?` (boolean). When set, Homebrew bypasses the API entirely and reads tap files on disk. The CoreTap API-backed overrides all `return super` when this is set.
- `HOMEBREW_USE_INTERNAL_API` — `EnvConfig.use_internal_api?`: returns false if `no_install_from_api?`; else true iff the env var is present AND its lowercased value is not in `FALSY_VALUES`. Switches the whole client to the "internal API" path (single per-tag packages file, struct-based; see §9). Treat as an alternative backend; the default/public path is the focus.
- `HOMEBREW_FORCE_API_AUTO_UPDATE` — `EnvConfig.force_api_auto_update?` (boolean).
- `HOMEBREW_NO_AUTO_UPDATE` — `EnvConfig.no_auto_update?` (boolean).
- `HOMEBREW_API_UPDATED` — internal flag env var set to `"1"` once `fetch_api_files!` runs in a process.
- `HOMEBREW_AUTO_UPDATE_COMMAND` — presence => `Homebrew.auto_update_command?` true.

Cache path constants (`api.rb` lines 27-29):
- `HOMEBREW_CACHE_API = HOMEBREW_CACHE/"api"` (directory `$HOMEBREW_CACHE/api`)
- `HOMEBREW_CACHE_API_SOURCE = HOMEBREW_CACHE/"api-source"` (`$HOMEBREW_CACHE/api-source`)
- `DEFAULT_API_STALE_SECONDS = 7 * 24 * 60 * 60 = 604800` (7 days)

Placeholder tokens (`global.rb` 46-50) used to genericize machine-specific paths in JSON, and re-substituted on read:
- `HOMEBREW_PREFIX_PLACEHOLDER = "$HOMEBREW_PREFIX"`
- `HOMEBREW_CELLAR_PLACEHOLDER = "$HOMEBREW_CELLAR"`
- `HOMEBREW_HOME_PLACEHOLDER = "/$HOME"` (note leading slash)
- `HOMEBREW_CASK_APPDIR_PLACEHOLDER = "$APPDIR"` (cask only)

## 2. Endpoints and filenames

Two distinct fetch modes, both relative to `{api_domain}/<endpoint>`:

### Bulk (default install-from-API path)
- Formulae: endpoint `"formula.jws.json"` → `EnvConfig.api_domain + "/formula.jws.json"`. Cached at `$HOMEBREW_CACHE/api/formula.jws.json` (`API::Formula::DEFAULT_API_FILENAME`, `cached_json_file_path`).
- Casks: endpoint `"cask.jws.json"` → cached at `$HOMEBREW_CACHE/api/cask.jws.json` (`API::Cask::DEFAULT_API_FILENAME`).
- Formula tap migrations: `"formula_tap_migrations.jws.json"` → `$HOMEBREW_CACHE/api/formula_tap_migrations.jws.json`.
- Cask tap migrations: `"cask_tap_migrations.jws.json"` → `$HOMEBREW_CACHE/api/cask_tap_migrations.jws.json`.

The bulk formula/cask JSON payload (inside the JWS) is a **JSON array** of formula/cask objects.

### Per-package (single-formula refresh)
- `fetch_formula_json!(name)` uses endpoint `"formula/#{name}.json"` → cached at `$HOMEBREW_CACHE/api/formula/<name>.json`. This is a **plain JSON object** (NOT JWS — the endpoint does not end with `.jws.json`, so no signature verification).
- `fetch_cask_json!(name)` uses endpoint `"cask/#{name}.json"` → `$HOMEBREW_CACHE/api/cask/<name>.json`.
- These are used by `API::Formula.formula_json(name)` / `API::Cask.cask_json(name)` (info/display paths). The default *install* path uses the bulk file (`all_formulae`), not these.

### Internal API path (HOMEBREW_USE_INTERNAL_API)
- Endpoint built per OS/arch tag: `"internal/packages.#{SimulateSystem.current_tag}.jws.json"` (e.g. `internal/packages.arm64_sonoma.jws.json`), cached at `$HOMEBREW_CACHE/api/internal/packages.<tag>.jws.json`. JWS-wrapped; payload is a single object with keys: `formulae`, `casks`, `formula_aliases`, `formula_renames`, `cask_renames`, `formula_tap_git_head`, `cask_tap_git_head`, `formula_tap_migrations`, `cask_tap_migrations`.

### command-not-found executables (separate, GHCR OCI)
- `download_executables_file_from_github_packages!`: fetches OCI manifest from `https://ghcr.io/v2/homebrew/command-not-found/executables/manifests/latest` with header `Accept: application/vnd.oci.image.manifest.v1+json` (+ optional `Authorization: <HOMEBREW_GITHUB_PACKAGES_AUTH>`), finds the layer whose `annotations["org.opencontainers.image.title"]` == target basename, then downloads `.../blobs/<digest>`. Niche; implement later.

## 3. Core fetch with conditional request + fallback (`API.fetch_json_api_file`, api.rb 49-165)

Signature: `fetch_json_api_file(endpoint, target: HOMEBREW_CACHE_API/endpoint, stale_seconds: nil, download_queue:, enqueue: false) -> [data, updated_bool]`.

Procedure:
1. `url = api_domain + "/" + endpoint`; `default_url = HOMEBREW_API_DEFAULT_DOMAIN + "/" + endpoint`.
2. Root guard: if `running_as_root_but_not_owned_by_root?` AND (`!target.exist?` || target empty) → fatal: "Need to download {url} but cannot as root! Run `brew update` without `sudo` first then try again." (`running_as_root_but_not_owned_by_root?` = process euid 0 AND the `brew` executable's owner uid != 0; `global.rb` 121-124.)
3. Build curl args: `Utils::Curl.curl_args(retries: 0)` + `["--compressed", "--speed-limit", HOMEBREW_CURL_SPEED_LIMIT, "--speed-time", HOMEBREW_CURL_SPEED_TIME]`.
4. `insecure_download` = true if dev-tools need a CA-file or curl substitution (macOS old-curl workaround); if true append `--insecure` and warn.
5. `skip_download = skip_download?(target:, stale_seconds:)` (see §4).
6. **enqueue mode** (`enqueue: true`, used by bulk `fetch_api_files!`): if not skipping, build `API::JSONDownload.new(endpoint, target:, stale_seconds:)` and `download_queue.enqueue(...)`; return `[{}, false]` immediately (the queue performs the actual fetch later, which re-enters this same function with `enqueue: false`).
7. **Conditional request**: if `target.exist? && !target.empty?`, prepend curl args `--time-cond <target_path>`. This is the *only* conditional mechanism — **curl `--time-cond` sends `If-Modified-Since` derived from the local file mtime**; if the server returns 304 the file is left untouched. There is **no ETag handling in the JSON API client** (ETag exists only in `curl_check_http_content`, an unrelated audit path). Ferrobrew: send `If-Modified-Since: <httpdate(target.mtime)>` and treat 304 as "not updated, use cached file".
8. If not `skip_download`: print `Downloading {url}` (only if stdout tty and not quiet), then `curl_download(*args, url, to: target, retries: 0, show_error: false)`; set `download_succeeded = true`.
9. **Error fallback** (`rescue ErrorDuringExecution`):
   - If `url == default_url`: re-raise unless `target.exist?` and non-empty (i.e. only tolerate failure if a cached copy exists).
   - Else if first retry (`retry_count.zero?`) or no/empty cache: set `url = default_url`, unlink target if it exists-but-empty, `skip_download = false`, and `retry` the begin block once (falls back to the default domain).
   - Otherwise: warn "{basename}: update failed, falling back to cached version." and continue using the cache.
10. **mtime touch**: only if `download_succeeded`, `FileUtils.touch(target, mtime: ...)` where mtime = `Time.new(1970,1,1)` when `insecure_download` else `Time.now`. (Touching after a *failed* download is deliberately avoided so a stale cache is not marked fresh.) Ferrobrew: after a successful 200, set the cache file mtime to now (or epoch 0 if insecure). After a 304, do NOT touch (the file keeps its old mtime — this is why `--time-cond` keeps revalidating until a real 200).
11. Parse: `JSON.parse(File.read(target, UTF-8), freeze: true)`. On `JSON::ParserError`: unlink target, `retry_count += 1`, `skip_download = false`, retry; if `retry_count > curl_retries` → fatal "Cannot download non-corrupt {url}!".
12. If `endpoint.end_with?(".jws.json")`: run `verify_and_parse_jws(json_data)` (see §5); on failure unlink target and die with a "Potential MITM attempt detected. Please run `brew update`..." message. On success return `[payload, !skip_download]`.
13. Else return `[json_data, !skip_download]`.

The returned boolean `updated` = `!skip_download` (true when a network attempt was made this call, i.e. cache was considered stale). Callers use it to decide whether to regenerate the on-disk names/aliases files.

`API.fetch(endpoint)` (api.rb 31-47) is a *simpler, in-memory-only* variant used for ad-hoc endpoints: `curl_output("--fail", url)`, fall back to default domain if `api_domain != default`, `JSON.parse(stdout, freeze: true)`, memoized in an in-process `cache[endpoint]` hash. No on-disk cache, no JWS. Lower priority.

## 4. Staleness check (`skip_download?`, api.rb 49-56)
Returns true (skip network) when:
- `running_as_root_but_not_owned_by_root?` → true (never download as wrong root).
- `!target.exist? || target.empty?` → false (must download).
- `stale_seconds` is nil → true (treat as fresh forever / never auto-refresh).
- Otherwise: `(Time.now - stale_seconds) < target.mtime` — i.e. skip if the file's mtime is newer than `now - stale_seconds` (file age < stale_seconds). Download if older.

`stale_seconds` selection in `fetch_api_files!` (api.rb 184-215):
- If `ENV["HOMEBREW_API_UPDATED"]` already set, OR (`no_auto_update?` AND not `force_api_auto_update?`) → `nil` (don't refresh).
- Elsif `auto_update_command?` → `api_auto_update_secs.to_i` (default 450).
- Else → `DEFAULT_API_STALE_SECONDS` (604800 / 7 days).
- Tap-migrations files always use `DEFAULT_API_STALE_SECONDS`.
After enqueuing, set `ENV["HOMEBREW_API_UPDATED"] = "1"`, then `download_queue.fetch` (blocking) and `download_queue.shutdown` in ensure.

## 5. JWS signature wrapper (`verify_and_parse_jws`, api.rb 333-360)

Outer JSON object (the `.jws.json` file content) shape — JWS JSON General Serialization:
```
{
  "payload": "<string: the actual JSON document, as a STRING not nested object>",
  "signatures": [
    { "header": { "kid": "homebrew-1" },
      "protected": "<base64url-encoded JOSE protected header>",
      "signature": "<base64url-encoded signature bytes>" },
    ... possibly more ...
  ]
}
```
Verification steps:
1. From `signatures`, pick the entry where `sig.dig("header","kid") == "homebrew-1"`. If none → `[false, "key not found"]`.
2. `header = JSON.parse(base64url_decode(sig["protected"]))`. Require `header["alg"] == "PS512"` AND `header["b64"] == false` (exactly the JSON boolean false; the comment notes `nil`/absent would mean true). Else → `[false, "invalid algorithm"]`. `b64:false` means the payload is NOT base64url-encoded — it is the raw string.
3. Public key: read PEM from `HOMEBREW_LIBRARY_PATH/"api/homebrew-1.pem"` (file lives at `Library/Homebrew/api/homebrew-1.pem` in the repo) as RSA.
4. `signing_input = sig["protected"] + "." + json_data["payload"]` (the base64url protected header, a literal `.`, then the raw payload string — because b64:false, payload is concatenated verbatim, not re-encoded).
5. Verify with RSA-PSS: digest **SHA512**, salt length = `:digest` (i.e. equal to hash length, 64 bytes), MGF1 hash **SHA512**, over the UTF-8 bytes of `signing_input`, against `base64url_decode(sig["signature"])`. On mismatch → `[false, "signature mismatch"]`.
6. On success → `[true, JSON.parse(json_data["payload"], freeze: true)]`.

Rust: parse outer JSON; base64url (URL-safe, with padding tolerance — Ruby `Base64.urlsafe_decode64` requires correct padding) decode `protected` and `signature`; RSA-PSS verify via e.g. `rsa` crate (`Pss` with `Sha512`, salt_len = hash len) or `ring`/`openssl`. Embed `homebrew-1.pem` at build time. The signed string is `"<protected_b64url>.<payload_string>"`.

## 6. On-disk cache layout (`$HOMEBREW_CACHE/api/`)
- `formula.jws.json`, `cask.jws.json`, `formula_tap_migrations.jws.json`, `cask_tap_migrations.jws.json` — bulk JWS files (raw bytes as downloaded; the JWS wrapper is kept on disk, verification happens on read).
- `formula/<name>.json`, `cask/<token>.json` — per-package plain JSON.
- `internal/packages.<tag>.jws.json` — internal API.
- `internal/executables.txt` — command-not-found data.
- Generated index text files (written by `write_*` helpers, see §7): `formula_names.txt`, `cask_names.txt`, `formula_aliases.txt`, `cask_aliases.txt` (cask aliases not actually written by default), plus `internal/executables.txt`.
- Source downloads cache: `$HOMEBREW_CACHE/api-source/<org>/<repo>/<git_head>/<path>` (formula `.rb` source and local patches) and `.../Cask/` for casks (see §8).

## 7. Names / aliases / renames index files

`write_names_file!(names, type, regenerate:)` (api.rb 228-238): path `$HOMEBREW_CACHE/api/<type>_names.txt`; if file missing or `regenerate` true, write `names.sort.join("\n")` (newline-separated, sorted, NO trailing newline). type ∈ {"formula","cask"}.

`write_aliases_file!(aliases, type, regenerate:)` (240-253): path `<type>_aliases.txt`; lines `"#{alias_name}|#{real_name}"`, sorted, `\n`-joined, no trailing newline. So alias→target separated by a literal `|`.

`write_executables_file!(formulae, regenerate:)` (255-291): path `internal/executables.txt`; for each `name => hash` with non-blank `hash["executables"]` array, line `"#{name}:#{executables.join(" ")}"`; sorted, `\n`-joined, WITH trailing `\n`. If empty list, unlink the file. Only rewrites when `regenerate` or content changed.

`regenerate` is passed = the `updated` boolean returned from the fetch (true when the bulk file was re-downloaded this run). These files are caches for fast name lookups; ferrobrew can regenerate them whenever the bulk JSON changes.

## 8. Bulk data caching & maps (`API::Formula`, api/formula.rb)

`download_and_cache_data!` (formula.rb 143-162):
1. `json_formulae, updated = fetch_api!` → `json_formulae` is the **array** of formula objects (JWS payload).
2. Build in-memory caches:
   - `cache["aliases"]`: for each formula, for each `alias_name` in `json_formula["aliases"]`, `aliases[alias_name] = json_formula["name"]`. (alias → canonical name)
   - `cache["renames"]`: for each `oldname` in `(json_formula["oldnames"] || [json_formula["oldname"]].compact)`, `renames[oldname] = json_formula["name"]`. (old name → current name) — note it tolerates BOTH a plural `oldnames` array and a legacy singular `oldname`.
   - `cache["formulae"]`: hash `name => json_formula.except("name")` (the per-formula object keyed by its `name`, with the `name` key removed from the value).
3. Returns `updated`.

`all_formulae` / `all_aliases` / `all_renames` lazily call `download_and_cache_data!` then `write_names_and_aliases(regenerate: updated)`.

`tap_migrations`: lazily `fetch_tap_migrations!` → caches the parsed JWS payload (an object: old fully-qualified name → new tap/name string).

Cask equivalent (`api/cask.rb`): `download_and_cache_data!` builds `cache["renames"]` from each cask's `old_tokens` array (`renames[old_token] = token`) and `cache["casks"]` = `token => json_cask.except("token")`. There is no cask aliases map.

## 9. Internal API maps (`api/internal.rb`)
Single object payload provides directly: `formula_aliases` (alias→name map), `formula_renames` (old→new), `cask_renames`, `formula_tap_migrations`, `cask_tap_migrations`, `formula_tap_git_head`, `cask_tap_git_head`, `formulae` (name→hash), `casks` (token→hash). Each formula/cask hash is deserialized into a `FormulaStruct`/`CaskStruct` (compact pre-processed form) rather than the verbose public JSON. Different on-disk shape; treat as a separate code path keyed on `use_internal_api?`.

## 10. Name resolution: alias / rename / tap-migration → canonical formula

When installing `brew install <ref>`, the API maps feed CoreTap, which feeds `Formulary.tap_formula_name_type` (formulary.rb 1177-1229). For the default (API) path, `CoreTap` overrides (`tap/core_tap.rb`) return the API maps unless `no_install_from_api?`:
- `alias_table` → `API.formula_aliases` (alias → name) [line 165-174]
- `formula_renames` → `API.formula_renames` (oldname → name) [101-112]
- `tap_migrations` → `API.formula_tap_migrations` [114-125]
- `formula_names` → `API.formula_names` (= `all_formulae.keys`) [183-188]
- `formula_files_by_name` → synthesized paths `Formula/<subdir>/<name.downcase>.rb` for each API name [190-207]

`API.formula_aliases`/`formula_renames`/`formula_tap_migrations`/`formula_names` (api.rb 374-408) dispatch to internal vs `API::Formula.all_aliases`/`all_renames`/`tap_migrations`/`all_formulae.keys`.

`tap_formula_name_type(tapped_name, warn:)` resolution order for a given tapped name (after `Tap.with_formula_name` splits tap/name):
1. **alias**: if `tap.alias_table[key]` present → `name = name_from_full_name(alias_target)`, type `:alias`. (key = bare `name` for core tap, else `"<tap>/<name>"`.)
2. **rename**: elsif `tap.formula_renames[name]` present → `name = new_name`, type `:rename`.
3. **tap migration**: elsif `tap.tap_migrations[name]` present → resolve into the new tap (recursively re-run `tap_formula_name_type` on the migrated `<new_tap>/<new_name>`), type `:migration`.
Returns `[name, tap, type]`. With `warn`, if renamed and the destination exists (file present, or core tap + not-no-api + `API.formula_names.include?(name)`), prints `"Formula <old> was renamed to <new>."`.

So a user-typed name is canonicalized by: alias map first, then rename(oldname) map, then tap-migration map. The bulk JSON's per-formula `aliases` (array) and `oldnames` (array; legacy singular `oldname`) are precisely what populate these maps (§8).

`FromAPILoader.try_new` (formulary.rb ~903-930) only matches a ref if it is in `API.formula_names`, OR a key of `API.formula_aliases`, OR a key of `API.formula_renames`.

## 11. From bulk JSON to an installable Formula

`FromAPILoader#load_from_api` (formulary.rb 947-957, public/default path):
1. `api_source = API::Formula.all_formulae[name]` (the per-formula object, `name` key stripped); raise `FormulaUnavailableError` if nil.
2. `tap_git_head = api_source.fetch("tap_git_head", "")`.
3. `formula_struct = FormulaStructGenerator.generate_formula_struct_hash(api_source)`.
4. `Formulary.load_formula_from_struct!(name, formula_struct, api_source:, tap_git_head:, flags:)` builds the Formula class.

### 11a. `generate_formula_struct_hash` (api/formula/formula_struct_generator.rb) — transforms the raw JSON object into struct fields. This is the authoritative mapping of JSON keys → install data:
- `merge_variations(hash, bottle_tag:)` first (see §12), then `deep_stringify_keys`.
- `caveats` → `Formulary.replace_placeholders` (substitutes `$HOMEBREW_PREFIX`, `$HOMEBREW_CELLAR`, `/$HOME`).
- `bottle_checksums`: from `hash.dig("bottle","stable","files")` (a map `tag => {cellar, url, sha256}`); produces an array of `{ cellar: <string-or-symbol>, <tag.to_sym> => sha256 }`. `cellar` via `convert_to_string_or_symbol` (symbol like `:any`/`:any_skip_relocation` if it starts with `:`, else a path string).
- `bottle_rebuild`: `hash.dig("bottle","stable","rebuild")` (integer, default 0).
- `conflicts`: zip `conflicts_with` (array of names) with `conflicts_with_reasons` → `[name, {because: reason}]` or `[name, {}]`.
- `deprecate_args`/`disable_args`: from `deprecate_args`/`disable_args` objects; `because` mapped via `DeprecateDisable.to_reason_string_or_symbol`.
- `head_url_args`: `[hash.dig("urls","head","url") || "", {branch:, using:(symbol)}]` (compact_blank).
- `keg_only_args`: if `keg_only_reason` present → `[convert_to_string_or_symbol(reason), explanation?]`. JSON `keg_only_reason` = `{reason:, explanation:}` (reason may be a symbol-string like `:provided_by_macos`).
- `license`: `SPDX.string_to_license_expression(hash["license"])` (SPDX string → expression tree).
- `link_overwrite_paths`: from `link_overwrite` array.
- `no_autobump_args`: from `no_autobump_message`.
- `pour_bottle_args`: `{only_if: hash["pour_bottle_only_if"].to_sym}`.
- `ruby_source_checksum`: `hash.dig("ruby_source_checksum","sha256")`.
- `service_*`: from `service` object via `Homebrew::Service.from_hash`.
- `stable_checksum`: `hash.dig("urls","stable","checksum")`.
- `stable_url_args`: `[hash.dig("urls","stable","url"), {tag:, revision:, using:(symbol)}]`.
- `stable_version`: `hash.dig("versions","stable")`.
- Dependencies (the install-critical part): from these JSON keys, each an array:
  - `dependencies` (runtime), `build_dependencies`, `test_dependencies`, `recommended_dependencies`, `optional_dependencies`, `uses_from_macos`, `uses_from_macos_bounds`.
  - `head_dependencies` is a separate object (same sub-keys) present only when head deps differ from stable; else reuse stable.
  - `requirements` (array of requirement objects) → filtered to those whose `specs` include the active spec, restricted to supported names `[:arch,:linux,:macos,:maximum_macos,:xcode]` (`:codesign` and custom unsupported).
  - Each dep entry is either a string `"foo"` or an object `{ "foo": "build" }` / `{ "foo": ["build","test"] }`; build/test/recommended/optional arrays get merged into typed entries.
  - `uses_from_macos` entry pairs with same-index `uses_from_macos_bounds` entry (an object like `{since: "catalina"}`); names may be strings or objects.
- Predicate booleans recomputed: `bottle_present = bottle.present?`, `head_present = urls.head present`, `keg_only_present = keg_only_reason present`, `stable_present = urls.stable present`, plus deprecate/disable/no_autobump/pour_bottle/service/service_run/service_name.
- `FormulaStruct.from_hash(hash)` then runs `Formula.deep_remove_placeholders` over the whole hash (re-substitutes the three path placeholders in every string), symbolizes keys, slices to known props, `compact_blank`.

### 11b. The full set of JSON keys ferrobrew must read from a per-formula object to install (canonical list, from `Formula#to_hash`, formula.rb 2926-3019, which is what generates the JSON):
Top-level string keys:
- `name`, `full_name`, `tap`, `oldnames` (array), `aliases` (array, sorted), `versioned_formulae` (array of names), `desc`, `license` (SPDX string), `homepage`.
- `versions`: `{ "stable": <ver string|null>, "head": <ver string|null>, "bottle": <bool> }`.
- `urls`: `{ "stable": {url, tag, revision, using, checksum}, "head": {url, branch, using} }` (keys present only when that spec exists; `using` only included when it is a symbol).
- `revision` (int), `version_scheme` (int), `compatibility_version`.
- `bottle`: `{ "stable": { "rebuild": <int>, "root_url": <string, default ghcr>, "files": { "<tag>": { "cellar": <string or ":symbol">, "url": "<root_url>/<path>", "sha256": "<hex>" }, ... } } }` — present only when a stable bottle is defined. The bottle `url` is the full ghcr.io OCI blob/manifest URL; ferrobrew uses `files.<tag>.sha256` + `cellar` for install and the bottle download layer for the URL.
- `pour_bottle_only_if` (string|null), `keg_only` (bool), `keg_only_reason` (`{reason, explanation}`|null).
- Dependency arrays: `build_dependencies`, `dependencies`, `test_dependencies`, `recommended_dependencies`, `optional_dependencies`, `uses_from_macos` (entries: string or `{name: type}` or `{name: [types]}`), `uses_from_macos_bounds` (array of bound objects, index-aligned with uses_from_macos). Plus optional `head_dependencies` object with the same sub-keys.
- `requirements`: array of `{name, cask, download, version, contexts (array), specs (array of spec names)}`.
- `conflicts_with` (array of names), `conflicts_with_reasons` (array, index-aligned, may contain null).
- `link_overwrite` (array of path strings), `caveats` (string with `$HOMEBREW_*` placeholders | null).
- `patches`: array of `{strip, ...}` — external: `{url, sha256, apply?, directory?}`; local: `{file}`; data: `{data:true}`.
- `service`: service object | null (run/run_type/keep_alive/etc., see Service.from_hash).
- `deprecated`/`disabled` booleans and `deprecation_*`/`disable_*` and `deprecate_args`/`disable_args`.
- `post_install_defined` (bool), `post_install_steps`.
- `tap_git_head` (string), `ruby_source_path` (string, e.g. `Formula/<sub>/<name>.rb`), `ruby_source_checksum` (`{sha256}`).
- `no_autobump_message`, `autobump`, `skip_livecheck` — metadata, not install-critical.
- `installed`, `linked_keg`, `pinned`, `outdated` — local-state keys; ABSENT in the API JSON (the API generator omits them / they are merged in locally). Ferrobrew computes these itself.

## 12. Variations (`merge_variations`, api.rb 167-182)
Per-formula JSON may contain `"variations"`: an object keyed by bottle tag string (e.g. `"arm64_sonoma"`, `"x86_64_linux"`). Resolution: `bottle_tag ||= SimulateSystem.current_tag`; look up `variations[tag.to_s]` (then `variations[tag.to_sym]`); if a non-blank variation object found, `json = json.merge(variation)` (variation keys override base keys); finally drop the `"variations"` key. This must happen BEFORE all field extraction, so the per-OS/arch overrides (different deps, bottle, caveats, etc.) take effect. Ferrobrew: determine current tag (arch + os codename, e.g. `arm64_sequoia`, `x86_64_linux`), shallow-merge the matching variation object over the base object, then drop `variations`.

## 13. Source downloads (`api/formula.rb`, `api/cask.rb`, `api/source_download.rb`)
For source installs / patches, the `.rb` source is fetched from GitHub raw:
- Formula: `https://raw.githubusercontent.com/<tap_full_name|Homebrew/homebrew-core>/<tap_git_head|HEAD>/<ruby_source_path|Formula/<name>.rb>`, cached under `$HOMEBREW_CACHE/api-source/<tap>/<git_head>/<path.dirname>`; checksum = `ruby_source_checksum`. `symlink_location = cache/name`.
- Cask: same pattern with `Homebrew/homebrew-cask` default and a mirror `<HOMEBREW_API_DEFAULT_DOMAIN>/cask-source/<basename>`, cache dir `.../Cask`.
- `LocalPatch.valid_path?` guard: source path must be a relative path inside the repo (reject absolute/`..`).
- `tap_from_source_download(path)` reverses a cached source path back to its `Tap.fetch(org, repo)` by taking the first two path components under `api-source/`.

## 14. DownloadQueue integration
`JSONDownload` (api/json_download.rb) wraps a `URL` using `API::JSONDownloadStrategy`; its `cached_location` = the target path; `fetch` re-enters `API.fetch_json_api_file(url, target:, stale_seconds:)`. `download_queue_type` strings: `"JSON API"` and `"API Source"`. The queue is just a concurrency mechanism; the actual fetch logic is §3. Ferrobrew can fetch the 4 bulk files (formula/cask/their tap-migrations) concurrently then block.

## Rust implementation notes
Crates: `reqwest`/`ureq` for HTTP (must support `If-Modified-Since`, gzip via `--compressed` → enable `gzip` feature, and a speed-limit/timeout analog — implement via a read-timeout + min-throughput watchdog mirroring `--speed-limit 100 --speed-time 5`). For JWS: `serde_json` for the outer/inner JSON, `base64` (URL_SAFE, but Ruby's urlsafe_decode64 expects padding — use `base64::engine::general_purpose::URL_SAFE` and tolerate/normalize padding), and RSA-PSS verification via the `rsa` crate (`rsa::pss::VerifyingKey<Sha512>` with `salt_len = 64` = digest length, MGF1-SHA512) or `openssl` (`Verifier` with `set_rsa_padding(PKCS1_PSS)`, `set_rsa_pss_saltlen(DIGEST)`, `set_rsa_mgf1_md(sha512)`). Embed `Library/Homebrew/api/homebrew-1.pem` with `include_str!` and parse once. The signing input is the literal bytes `format!("{protected_b64url}.{payload_string}")` — do NOT re-encode payload (b64:false).

Data model: define `RawFormula` as `serde_json::Value`-backed or a struct with `#[serde(default)]` on every optional field. Mirror the field names exactly (snake_case JSON keys). Represent `bottle.stable.files` as `HashMap<String /*tag*/, BottleFile{cellar: String, url: String, sha256: String}>`; `cellar` can be a path string OR a symbol-string like `":any"`/`":any_skip_relocation"` — parse leading `:` as the symbolic cellar enum. Dependency entries are an untagged enum: `Name(String)` | `Typed(HashMap<String, OneOrMany<String>>)`. `uses_from_macos` pairs index-wise with `uses_from_macos_bounds`. `conflicts_with` pairs index-wise with `conflicts_with_reasons` (latter may contain null). Requirements: filter by `specs.contains(active_spec)` and by an allowlist `{arch, linux, macos, maximum_macos, xcode}`.

Variations: implement as a shallow merge — load base object, look up `variations[current_tag]`, overlay its keys, drop `variations`. Determine current tag as `{arch}_{os}` (arch ∈ arm64/x86_64; os = macOS codename or `linux`). This must run before extracting any field.

Placeholders: after merge, run a recursive string replace over the whole JSON value: `"/$HOME"→home_dir`, `"$HOMEBREW_PREFIX"→prefix`, `"$HOMEBREW_CELLAR"→cellar` (order: HOME first as in Ruby). Also apply to `caveats` specifically. For casks also `$APPDIR`.

Caching & conditional GET: cache file path = `$HOMEBREW_CACHE/api/<endpoint>`. Use the file mtime as the conditional token (send `If-Modified-Since: httpdate(mtime)`; there is NO ETag in this subsystem). On 200, write bytes then set mtime = now (or UNIX epoch 0 if an insecure/old-curl substitution path is active). On 304 or any network failure with an existing non-empty cache, reuse the cached bytes and do NOT update mtime (so the next run re-revalidates). On total failure with no cache, error. Implement the primary→default-domain fallback: try `HOMEBREW_API_DOMAIN`, on failure retry once against `https://formulae.brew.sh/api`. Gotchas: (1) the cached `.jws.json` on disk still contains the JWS wrapper — verify on every read, not just on download. (2) `skip_download` semantics: stale_seconds=None means "never refresh"; compute age = now - mtime and refresh only when age >= stale_seconds. (3) Root-ownership guard: refuse to download when running as root but the brew binary is owned by a non-root user and no cache exists. (4) JSON parse failure → delete cache and retry up to `curl_retries` (default 3). (5) Bulk payload is a JSON array; per-formula endpoint is a JSON object (and is NOT signed). (6) The `name` key is stripped from the value when building the name→object map; keep `name` separately. (7) oldnames may appear as plural array `oldnames` or legacy singular `oldname`; handle both when building the rename map. (8) Symlinks: source downloads use a `symlink_location = cache/name`; replicate atomically (write temp + rename, create symlink) — relevant only for the source-install path.

Name resolution: build three maps from the bulk array — aliases (alias→name from each `aliases[]`), renames (oldname→name from `oldnames[]`/`oldname`), and tap_migrations (from the separate `*_tap_migrations.jws.json`). Resolve a user ref in order alias → rename → tap-migration (tap-migration is recursive into the target tap). The index text files (`formula_names.txt` etc.) are derived caches — regenerate from the bulk JSON; format is sorted, `\n`-joined, aliases as `alias|target`, names file with no trailing newline, executables file with trailing newline.

## Open questions
- The exact base64url padding behavior: Ruby's Base64.urlsafe_decode64 raises on incorrect padding; confirm whether formulae.brew.sh emits padded or unpadded `protected`/`signature` fields so the Rust decoder is configured to match (likely standard padded URL_SAFE).
- Bottle download URL construction (root_url + OCI manifest/blob path, HOMEBREW_ARTIFACT_DOMAIN replacement, GitHubPackages.root_url_if_match) lives in bottle.rb/bottle_specification.rb/github_packages.rb and is a separate subsystem from the JSON client — needs its own spec to actually fetch/extract bottles.
- The cask JSON install fields (artifacts: app/pkg/binary/zap/uninstall, sha256, url, version, depends_on) were not enumerated here; api/cask_struct.rb + Cask DSL define them and warrant a dedicated pass parallel to §11b.
- Service.from_hash, SPDX.string_to_license_expression, and DeprecateDisable.to_reason_string_or_symbol semantics are referenced by the struct generator but their exact output shapes (e.g. service run_type/keep_alive keys, SPDX expression tree node types) need separate reverse-engineering.
- The internal-API FormulaStruct/CaskStruct serialized on-disk shape (compact form) differs from the public JSON; if ferrobrew targets HOMEBREW_USE_INTERNAL_API it needs the full struct field/serialization spec from api/formula_struct.rb + api/cask_struct.rb (partially captured here for formula).

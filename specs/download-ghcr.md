# Bottle download from ghcr.io (OCI registry: manifest + blob fetch via curl)

## Key reference files
- `Library/Homebrew/download_strategy/curl_github_packages_download_strategy.rb`
- `Library/Homebrew/download_strategy/curl_download_strategy.rb`
- `Library/Homebrew/download_strategy/abstract_file_download_strategy.rb`
- `Library/Homebrew/download_strategy/abstract_download_strategy.rb`
- `Library/Homebrew/github_packages.rb`
- `Library/Homebrew/bottle.rb`
- `Library/Homebrew/bottle_specification.rb`
- `Library/Homebrew/utils/bottles.rb`
- `Library/Homebrew/resource.rb`
- `Library/Homebrew/downloadable.rb`
- `Library/Homebrew/utils/curl.rb`
- `Library/Homebrew/brew.sh`
- `Library/Homebrew/env_config.rb`
- `Library/Homebrew/global.rb`
- `Library/Homebrew/checksum.rb`
- `Library/Homebrew/extend/pathname.rb`
- `Library/Homebrew/utils.rb`

## Specification
# Bottle Download from ghcr.io — Implementation Spec

Homebrew downloads bottles from an OCI registry (default `ghcr.io`) using `curl` against the OCI Distribution v2 HTTP API. It does NOT use `docker`/`skopeo` for downloads (those are only used for *uploads*; ignore `github_packages.rb#upload_bottles`/`download`). The download path is plain HTTPS GET requests against `/v2/...` blob and manifest endpoints, plus content-addressed local caching.

## 1. Constants & literals (exact)

- `GitHubPackages::URL_DOMAIN = "ghcr.io"` (public).
- `URL_PREFIX = "https://ghcr.io/v2/"` (note trailing slash; private).
- `DOCKER_PREFIX = "docker://ghcr.io/"` (private; only for uploads).
- `URL_REGEX = %r{(?:https://ghcr\.io/v2/|docker://ghcr\.io/)([\w-]+)/([\w-]+)}` — capture group 1 = org, group 2 = repo. (`URL_PREFIX`/`DOCKER_PREFIX` are `Regexp.escape`d.)
- `HOMEBREW_BOTTLE_DEFAULT_DOMAIN = "https://ghcr.io/v2/homebrew/core"` (set in `brew.sh:638`, read from env in `global.rb:9`).
- `HOMEBREW_BOTTLES_EXTNAME_REGEX = /\.([a-z0-9_]+)\.bottle\.(?:(\d+)\.)?tar\.gz$/` — group 1 = tag (e.g. `arm64_sonoma`), group 2 = rebuild (optional).
- Anonymous Bearer token literal: **`Bearer QQ==`** (`QQ==` is base64 of ASCII `"A"`). This is the ghcr.io anonymous-access token. Set in `brew.sh:1131`.
- `GITHUB_PACKAGE_TYPE = "homebrew_bottle"` (annotation value, upload-only).
- Manifest Accept header literal: `"Accept: application/vnd.oci.image.index.v1+json"`.
- Default user agent header value: `HOMEBREW_USER_AGENT_CURL`, format `"<HOMEBREW_PRODUCT>/<version> (<system>; <processor> <os_user_agent_version>) <curl_name_and_version>"` (built in `brew.sh:809-812`). Passed via `--user-agent`.

## 2. HOMEBREW_GITHUB_PACKAGES_AUTH (Authorization header value)

Computed in `brew.sh:1118-1132` BEFORE Ruby runs; exported as env var `HOMEBREW_GITHUB_PACKAGES_AUTH`:

```
if HOMEBREW_DOCKER_REGISTRY_TOKEN set:
    HOMEBREW_GITHUB_PACKAGES_AUTH = "Bearer ${HOMEBREW_DOCKER_REGISTRY_TOKEN}"
elif HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN set:
    if value == "none":  unset HOMEBREW_GITHUB_PACKAGES_AUTH   # anonymous
    else:                HOMEBREW_GITHUB_PACKAGES_AUTH = "Basic ${...}"
else:
    HOMEBREW_GITHUB_PACKAGES_AUTH = "Bearer QQ=="              # anonymous default
```

The Rust port should replicate this exact precedence and produce the same string. The header line added to curl is literally `"Authorization: " + HOMEBREW_GITHUB_PACKAGES_AUTH`.

### When the Authorization header is actually attached (`CurlGitHubPackagesDownloadStrategy#initialize`)

Add `"Authorization: <HOMEBREW_GITHUB_PACKAGES_AUTH>"` to `meta[:headers]` IFF `HOMEBREW_GITHUB_PACKAGES_AUTH` is non-empty AND ( `HOMEBREW_ARTIFACT_DOMAIN` is empty OR `HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN` is non-empty OR `HOMEBREW_DOCKER_REGISTRY_TOKEN` is non-empty ). Rationale: when a private GHCR mirror is set via `HOMEBREW_ARTIFACT_DOMAIN` and no explicit token is given, do NOT send the default anonymous `Bearer QQ==` header.

NOTE: `meta[:headers]` is initialized to `[]` if absent, then the Authorization line is prepended via `<<`. The manifest resource also passes `headers: ["Accept: application/vnd.oci.image.index.v1+json"]`, so a manifest request ends up with both Accept and Authorization headers.

### Auth dropped on redirect

In `CurlDownloadStrategy#fetch`, after resolving the URL, if the resolve detected a redirection (`is_redirection == true`), every header in `meta[:headers]` starting with `"Authorization"` is deleted before the actual download. ghcr.io blob requests 302-redirect to a signed `*.blob.core.windows.net` / CDN URL that rejects the registry auth header; this must be honored. (Redirect detection: see §6.)

## 3. URL construction (root_url, manifest URL, blob URL)

### root_url resolution
`BottleSpecification#root_url`:
- If no explicit `root_url` set: `@root_url = GitHubPackages.root_url_if_match(HOMEBREW_BOTTLE_DOMAIN) || HOMEBREW_BOTTLE_DOMAIN`. `HOMEBREW_BOTTLE_DOMAIN` defaults to `HOMEBREW_BOTTLE_DEFAULT_DOMAIN = "https://ghcr.io/v2/homebrew/core"`.
- If explicit value `var` given (from formula `bottle do root_url "..."` block): `@root_url = GitHubPackages.root_url_if_match(var) || var`.

`GitHubPackages.root_url_if_match(url)`: match `url` against `URL_REGEX`; if org/repo extracted, return `root_url(org, repo)`; else nil. `root_url(org, repo, prefix="https://ghcr.io/v2/")` = `"<prefix><org.downcase>/<repo_without_prefix>"` where `repo_without_prefix(repo)` strips a leading `"homebrew-"`. So `https://ghcr.io/v2/homebrew/core` normalizes to itself (org=`homebrew`, repo=`core`).

### Bottle blob URL (the actual .tar.gz download)
In `Bottle#root_url` (`bottle.rb:264`): calls `Utils::Bottles.path_resolved_basename(root_url, name, checksum, filename)`.

`path_resolved_basename` (`utils/bottles.rb:110`):
- If `root_url` matches `GitHubPackages::URL_REGEX` (i.e. it's ghcr.io): returns `["<image_name>/blobs/sha256:<checksum_hexdigest>", filename.github_packages]` where:
  - `image_name = GitHubPackages.image_formula_name(name)` = formula name with `@`→`/` and `+`→`x` (`tr("@","/").tr("+","x")`).
  - `checksum` is the bottle's sha256 hex string (the OCI blob digest equals the bottle tarball's sha256).
  - `filename.github_packages` = `"<name>--<version><extname>"`, e.g. `wget--1.21.4.arm64_sonoma.bottle.tar.gz` (see Filename below).
- Else (non-ghcr mirror): returns `filename.url_encode` = `ERB::Util.url_encode("<name>-<version><extname>")` (URL-encoded, single dash before version, used as a path segment).

Then `@resource.url("<root_url>/<path>", ...)`. So the full bottle blob URL is:
```
https://ghcr.io/v2/<org>/<repo>/<image_name>/blobs/sha256:<sha256hex>
```
Example: `https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:abc123...`.

If `resolved_basename` was returned (the ghcr branch), it is assigned onto the downloader: `downloader.resolved_basename = filename.github_packages` (only when downloader is `CurlGitHubPackagesDownloadStrategy`). This sets the cached filename WITHOUT a HEAD request (see §5).

### Bottle::Filename (bottle.rb:7-62)
- `name` = `File.basename(formula.name)`, `version` = `PkgVersion`, `tag` = unstandardized tag symbol string, `rebuild` = Integer.
- `extname` = `".<tag>.bottle<rebuild_suffix>.tar.gz"` where `rebuild_suffix = ".<rebuild>"` if `rebuild > 0` else `""`. E.g. `.arm64_sonoma.bottle.tar.gz` or `.arm64_sonoma.bottle.2.tar.gz`.
- `to_str`/`to_s` = `"<name>--<version><extname>"` (double dash).
- `github_packages` = same as `to_str`: `"<name>--<version><extname>"` (double dash).
- `url_encode` = `ERB::Util.url_encode("<name>-<version><extname>")` (single dash; used for non-ghcr mirrors).
- `json` = `"<name>--<version>.<tag>.bottle.json"`.

### Manifest URL (the OCI image index / "tab" fetch)
`Bottle#github_packages_manifest_resource` (`bottle.rb:208`) builds a separate `Resource::BottleManifest` ONLY when `@resource.download_strategy == CurlGitHubPackagesDownloadStrategy`. URL:
```
<root_url>/<image_name>/manifests/<image_tag>
```
- `version_rebuild = GitHubPackages.version_rebuild(version, rebuild)` (see below).
- `image_name = GitHubPackages.image_formula_name(name)`.
- `image_tag = GitHubPackages.image_version_rebuild(version_rebuild)` (validates against `VALID_OCI_TAG_REGEX = /^[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}$/`; raises if invalid).
- Resource uses `CurlGitHubPackagesDownloadStrategy` with header `"Accept: application/vnd.oci.image.index.v1+json"`.
- `resolved_basename` set to `"<name>-<version_rebuild>.bottle_manifest.json"` (single dash) — this is the cache filename for the manifest JSON.

Full manifest URL example: `https://ghcr.io/v2/homebrew/core/wget/manifests/1.21.4`.

### version_rebuild (github_packages.rb:87)
`version_rebuild(version, rebuild, bottle_tag=nil)`:
- `bottle_tag` prefix: `".<bottle_tag>"` if present, else nothing.
- rebuild suffix: if `rebuild > 0`: `".<rebuild>"` when bottle_tag present, else `"-<rebuild>"`; if `rebuild == 0`: nothing.
- Result: `"<version><.bottle_tag?><rebuild_suffix?>"`. For manifest tag (no bottle_tag): e.g. `1.21.4` or `1.21.4-2`. For per-bottle ref name (with tag): e.g. `1.21.4.arm64_sonoma` or `1.21.4.arm64_sonoma.2`.

## 4. HOMEBREW_ARTIFACT_DOMAIN override + mirrors interleaving (CurlDownloadStrategy#fetch)

Build `urls = [url, *mirrors]` (mirrors come from `meta[:mirrors]`, e.g. fallback domains). Then if `HOMEBREW_ARTIFACT_DOMAIN` is set:
- For each url, rewrite: `u.sub(%r{^https?://ghcr\.io/}, "<HOMEBREW_ARTIFACT_DOMAIN_no_trailing_slash>/")`. Regex anchored at start, matches `http://ghcr.io/` or `https://ghcr.io/` literally (uses `GitHubPackages::URL_DOMAIN` interpolated). Domain has trailing `/` chomped before re-adding one. So `https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:..` → `<artifact_domain>/v2/homebrew/core/wget/blobs/sha256:..`.
- If `HOMEBREW_ARTIFACT_DOMAIN_NO_FALLBACK` is truthy: `urls = artifact_urls` only.
- Else: interleave — for each original url, push the rewritten artifact url first, then the original (only if different): `[artifact_url_1, original_1, artifact_url_2, original_2, ...]`. This tries the artifact domain first per-url, falling back to ghcr.io. (Only rewritten URLs get a fallback; non-ghcr URLs that don't match the regex are unchanged and appear only once.)

Then the download loop `shift`s the first url, tries it; on `CurlDownloadStrategyError` retries with the next url ("Trying a mirror..."); raises if `urls.empty?`.

`HOMEBREW_ARTIFACT_DOMAIN_NO_FALLBACK` is a boolean env var (`env_config.rb:80`). `HOMEBREW_ARTIFACT_DOMAIN` is a string prefix.

### Bottle-level fallback (Bottle#fallback_on_error?, bottle.rb:251)
Separate from artifact-domain interleaving. If a `DownloadError` is raised AND `@resource.url` starts with `HOMEBREW_BOTTLE_DOMAIN` AND `HOMEBREW_BOTTLE_DOMAIN != HOMEBREW_BOTTLE_DEFAULT_DOMAIN`: print "Bottle missing, falling back to the default domain...", call `root_url(HOMEBREW_BOTTLE_DEFAULT_DOMAIN)` (re-deriving URLs against the default ghcr.io), reset the cached manifest resource, return true → caller retries. This is the mechanism for `HOMEBREW_BOTTLE_DOMAIN` mirror → default ghcr.io fallback.

## 5. Cache filename scheme in HOMEBREW_CACHE

Two artifacts per cache: the `cached_location` (real file, content-addressed by URL hash) and a `symlink_location` (human-readable symlink). Both live in `HOMEBREW_CACHE/downloads/`.

### cached_location (AbstractFileDownloadStrategy#cached_location, line 33)
- `url_sha256 = Digest::SHA256.hexdigest(url)` — sha256 of the **download URL string** (NOT the file contents), lowercase hex, 64 chars.
- Glob `HOMEBREW_CACHE/downloads/<url_sha256>--*`, reject any path whose extname ends with `.incomplete`.
- If exactly one match: use it. Else: `HOMEBREW_CACHE/downloads/<url_sha256>--<Utils.safe_filename(resolved_basename)>`.
- `resolved_basename`: for ghcr bottles this is the pre-set `@resolved_basename` (the `Bottle::Filename#github_packages` value, e.g. `wget--1.21.4.arm64_sonoma.bottle.tar.gz`); for the manifest it's `<name>-<version_rebuild>.bottle_manifest.json`. If not pre-set, computed via a HEAD request (see §6).
- Pattern: `downloads/<sha256-of-url>--<safe-filename>`. Example: `downloads/3f2b...e1--wget--1.21.4.arm64_sonoma.bottle.tar.gz`.

### temporary_path (AbstractFileDownloadStrategy#temporary_path)
`"<cached_location>.incomplete"` — partial download target; renamed to `cached_location` on success.

### symlink_location (AbstractFileDownloadStrategy#symlink_location, line 21)
- `ext = Pathname(parse_basename(url)).extname` (double-extension aware, e.g. `.tar.gz`).
- `@symlink_location = @cache / Utils.safe_filename("<name>--<version><ext>")`.
- Created by `create_symlink_to_cached_download`: a relative symlink (`target.relative_path_from(symlink_dir)`) at `symlink_location` pointing to `cached_location`, `force: true` (overwrites existing). `mkpath` the parent first.
- NOTE: `name`/`version` here are the *download-strategy* name/version (Resource's `download_name`/`version`), giving e.g. `wget--1.21.4.arm64_sonoma.bottle.tar.gz` as the symlink, living directly under `HOMEBREW_CACHE` (cache root), pointing into `downloads/`.

### Utils.safe_filename (utils.rb:126)
`basename.gsub(/[[:cntrl:]\/<ALT_SEP>]/, "")` — strips control chars and path separators (`/` and OS alt-separator). `safe_filename?` returns true if none present.

### download_lock
`fetch` acquires a `DownloadLock` on `temporary_path` (a lockfile) before downloading; releases with `unlink: true` in `ensure`. Prevents concurrent downloads of the same file.

## 6. URL resolution / HEAD request (resolve_url_basename_time_file_size)

For ghcr bottles, `CurlGitHubPackagesDownloadStrategy#resolve_url_basename_time_file_size` is overridden: if `@resolved_basename` is set (the normal bottle/manifest case), it returns `[url, @resolved_basename, nil, nil, nil, false]` WITHOUT any HTTP request — no HEAD probe, no redirect detection here. So bottle downloads normally skip the preflight HEAD.

If `@resolved_basename` is blank, it falls back to the base `CurlDownloadStrategy#resolve_url_basename_time_file_size` (line 150), which:
- Runs `curl_headers(url, wanted_headers: ["content-disposition"])` → HEAD (or GET-retry) and parses responses.
- `final_url = curl_response_follow_redirections(responses, url)` — walks `location` headers joining via `URI.join`.
- Extracts filename from `Content-Disposition` (RFC 5987 `filename*` with `<enc>''<encoded>` decode, else `filename`), takes `File.basename`. Falls back to `parse_basename(final_url)`.
- Extracts `last-modified` (epoch-int or RFC date → Time), `content-length` (or `content-range` total as fallback), `content-type`.
- `is_redirection = (url != final_url)`.
- Returns `[final_url, basename, time, file_size, content_type, is_redirection]`, cached in `@resolved_info_cache` keyed by url.

The returned tuple type alias `URLMetadata = [String url, String basename, Time? time, Integer? file_size, String? content_type, Boolean is_redirection]`.

In `fetch`, the resolved `@file_size` and `@last_modified` drive cache-freshness checks (§7), and `is_redirection` triggers Authorization-header stripping (§2).

## 7. Cache freshness & download decision (CurlDownloadStrategy#fetch)

- `cached_location_valid = cached_location.exist?`.
- If valid AND content_type is not `text/*`:
  - If `last_modified && last_modified > cached_location.mtime`: invalidate (mtime older than server Last-Modified).
  - If `@file_size && @file_size != cached_location.size`: invalidate (size mismatch vs Content-Length).
- If still valid: print "Already downloaded: <path>" and skip download.
- Else: `_fetch` → on success `cached_location.dirname.mkpath` then `temporary_path.rename(cached_location)`.
- For ghcr bottles `last_modified`/`file_size` are nil (no preflight), so a freshly-existing cache file is used as-is.

`_fetch` (CurlDownloadStrategy#_fetch): prints "Downloading from <resolved_url>" if differs; calls `ensure_no_insecure_redirect!` (raises if HTTPS→HTTP and `HOMEBREW_NO_INSECURE_REDIRECT` set); then `_curl_download(resolved_url, temporary_path, timeout)` → `curl_download(resolved_url, to: temporary_path, try_partial: @try_partial, timeout:)`. `@try_partial` defaults to `true`.

## 8. curl invocation, resume, retry (utils/curl.rb)

### curl_download (line 283)
- `args = ["--location", *args]` (follow redirects).
- If `try_partial && destination.exist?`:
  - Do a HEAD via `curl_headers(..., wanted_headers: ["accept-ranges"])`; read last response headers.
  - `supports_partial = headers["accept-ranges"] (default "none") != "none"`.
  - `content_length = headers["content-length"].to_i`.
  - If `supports_partial`: if `destination.size == content_length` → return (already complete); else prepend `["--continue-at", "-"]` for resume.
- `args = ["--remote-time", "--output", <destination>, *args]` then `curl(*args)`.
- `--remote-time` sets file mtime from server; `--output` writes to the `.incomplete` temp path.

### curl_args (line 90) — base flags always applied
- `--disable` (skip .curlrc) unless `HOMEBREW_CURLRC` points to a path (then `--disable --config <path>`) or is a bare bool (legacy: omit `--disable`).
- `--cookie /dev/null` (or supplied cookies) — echo redirect cookies.
- `--globoff`.
- `--show-error` (default on).
- `--user-agent <HOMEBREW_USER_AGENT_CURL>` unless user_agent is `:curl`. `:browser`/`:fake` → `HOMEBREW_USER_AGENT_FAKE_SAFARI`.
- `--header "Accept-Language: en"`, plus any `header`/`headers` entries (each `--header <stripped>`).
- Unless `show_output`: `--fail`, `--progress-bar` (unless verbose), `--verbose` if `HOMEBREW_CURL_VERBOSE`, `--silent` if not a TTY or quiet.
- `--connect-timeout <n>` (DownloadStrategy sets 15 when mirrors present), `--max-time <n>` from remaining timeout.
- `--retry <n>` where `n = HOMEBREW_CURL_RETRIES.to_i` (only if positive). `--retry-max-time <n>` if set.
- `--referer <r>` if set.

### Per-strategy curl args (CurlDownloadStrategy#_curl_args, line 260)
Prepended to every curl/curl_output call: `-b <cookies>` (if `meta[:cookies]`), `-e <referer>` (if `meta[:referer]`), `--user <user>` (if `meta[:user]`), and **`--header <h.strip>` for each header in `meta[:headers]`** — this is how the `Authorization` and `Accept` headers reach curl. `_curl_opts` passes `meta[:user_agent]` through.

### curl_with_workarounds (line 211) — retry/workaround layer
- Applies `no_insecure_redirect_curl_args` (adds `--proto-redir =https` when `HOMEBREW_NO_INSECURE_REDIRECT` and `--location` present; strips caller `--proto-redir`).
- Runs curl; on success or if `--http1.1` already present, returns.
- Exit 28 + timeout → raise `Timeout::Error`.
- Exit 16 (HTTP/2 framing) → retry with `--http1.1`.
- Exit 56 (unexpected EOF) → if curl supports HTTP2 and version < 7.60.0, retry with `--http1.1`; else return result.
- `curl` (line 269) wraps and calls `result.assert_success!`.

### curl_headers (line 328) — used for resolve + partial-support probe
- Base args `["--fail", "--location", "--silent"]`; for non-POST adds `--head`, retry pass adds `--request GET`. POST adds `--dump-header -`.
- Workaround: adds `--http1.1` on retry pass when curl version in `[8.7, 8.10)`.
- Runs HEAD; if no wanted header found OR last status in 400–499, retries as GET. Accepts exit statuses for weird-server-reply / http-error / recv-error to still parse headers.

## 9. sha256 verification (Downloadable#verify_download_integrity → Pathname#verify_checksum)

After `downloader.fetch`, `Resource#fetch` (via `Downloadable#fetch`) calls `verify_download_integrity(cached_download)` unless disabled:
- `Pathname#verify_checksum(expected)` (`extend/pathname.rb:227`): raise `ChecksumMissingError` if no checksum; `actual = Checksum.new(Digest::SHA256.file(self).hexdigest.downcase)`; raise `ChecksumMismatchError` if `expected != actual`.
- `Checksum` stores lowercase hex; `==` compares case-insensitively (`other.downcase`). The expected checksum is the bottle's `tag_spec.checksum` (the `sha256` from the formula's `bottle do` block), which equals the OCI blob digest in the URL.
- `ChecksumMissingError` is non-fatal: prints "Cannot verify integrity..." + the computed `sha256 "..."` (unless `silence_checksum_missing_error?`).

### Manifest "verification" (Resource::BottleManifest)
The manifest resource has NO checksum. `verify_download_integrity` instead just parses the manifest (`tab`) to confirm validity. On `BottleManifest::Error` (corrupt/missing/unmatched), `Bottle#fetch_tab` retries once after `clear_cache`.

## 10. Manifest JSON structure (Resource::BottleManifest#manifest_annotations, resource.rb:444)

Cached manifest is a JSON OCI image index. Parse:
- Top-level `"manifests"` array (raise `Error "Missing 'manifests' section."` if blank).
- For each manifest entry, collect `entry["annotations"]` (raise if all blank).
- Find the annotation hash where `annotations["sh.brew.bottle.digest"] == bottle.checksum.hexdigest` AND `annotations["org.opencontainers.image.ref.name"] == GitHubPackages.version_rebuild(version, rebuild, tag.to_s)` (e.g. `1.21.4.arm64_sonoma`). Raise `Error "Couldn't find manifest matching bottle checksum."` if none.
- From that annotation hash extract:
  - `"sh.brew.tab"` → JSON string → parsed = the Tab (install receipt metadata). Missing/unparseable → `Error`.
  - `"sh.brew.bottle.size"` → Integer (bottle tarball size; used for `total_size`).
  - `"sh.brew.bottle.installed_size"` → Integer.
  - `"sh.brew.path_exec_files"` → comma-split → array of strings.

## 11. End-to-end flow (bottle install)

1. Resolve `root_url` (default `https://ghcr.io/v2/homebrew/core`).
2. Build bottle blob `Resource.url = <root_url>/<image_name>/blobs/sha256:<sha256>`, strategy `CurlGitHubPackagesDownloadStrategy`, with `resolved_basename = <name>--<version><extname>` (no HEAD needed).
3. (Optional, lazily) build manifest `Resource.url = <root_url>/<image_name>/manifests/<image_tag>` with `Accept: application/vnd.oci.image.index.v1+json`, `resolved_basename = <name>-<version_rebuild>.bottle_manifest.json`.
4. `fetch`: lock; compute `urls` (+artifact-domain interleave +mirrors); GET blob with `Authorization` (+`Accept` for manifest) header via `--location`; follow 302 to CDN dropping Authorization; resume via `--continue-at -` if partial+Accept-Ranges; write to `.incomplete`, rename to content-addressed `downloads/<sha256-of-url>--<filename>`; symlink `HOMEBREW_CACHE/<name>--<version><ext>` → cached file.
5. Verify sha256 of downloaded blob == expected bottle checksum.
6. On DownloadError: artifact-domain fallback (interleaved urls) then bottle-domain→default-domain fallback (`fallback_on_error?`).

## Rust implementation notes
## Rust implementation guidance

### Crates
- HTTP: prefer shelling out to `curl` to match behavior 1:1 (Homebrew relies on `--continue-at -`, exact retry/workaround exit-code handling, `--proto-redir =https`, `--fail`, redirect cookie echo). A native client (`reqwest`/`ureq`) is viable but you must reimplement: resume via `Range` headers gated on `Accept-Ranges != none`, drop `Authorization` after cross-origin redirect, exact retry counts, and HTTP/2→1.1 fallback. The exit-code workarounds (16, 28, 56) only matter if you shell to curl. Recommend a thin `curl` wrapper struct first, optimize later.
- Hashing: `sha2::Sha256` for both URL-hash (cache filename) and content verification. Hex-encode lowercase (`hex` crate or manual). URL hash = `hex(Sha256(url_string_bytes))`.
- JSON: `serde_json` for the manifest. Model the index as `{ manifests: Vec<{ annotations: HashMap<String,String> }> }`; pull the string-valued annotations by exact keys (`sh.brew.bottle.digest`, `org.opencontainers.image.ref.name`, `sh.brew.tab`, `sh.brew.bottle.size`, `sh.brew.bottle.installed_size`, `sh.brew.path_exec_files`).
- URL building: simple string concatenation matches Ruby exactly; do NOT percent-encode the ghcr blob path (the `sha256:` colon is intentionally literal in the path). For non-ghcr mirrors use a URL-encoder equivalent to `ERB::Util.url_encode` (encodes everything except unreserved `[A-Za-z0-9_.-]`; space→`%20`, NOT `+`).

### Types
- `struct BottleFilename { name, version, tag, rebuild }` with methods `extname()`, `to_string()` (double-dash), `github_packages()` (== to_string), `url_encode()` (single-dash, percent-encoded), `json()`.
- `enum CellarOrSymbol` for cellar; `struct TagSpecification { tag, checksum, cellar }`.
- `struct GhcrAuth` resolving `HOMEBREW_GITHUB_PACKAGES_AUTH` from the three env vars with the exact precedence (Token > BasicAuth(none→anonymous) > default `Bearer QQ==`). Keep the literal `"Bearer QQ=="`.
- `enum DownloadStrategy` with a `CurlGitHubPackages` variant carrying `resolved_basename: Option<String>` and `headers: Vec<String>`.

### Gotchas
- **URL-hash cache key**: the cache filename uses sha256 of the *URL string*, not the file. Two different artifact-domain rewrites of the same bottle produce different cache files. Hash the exact `url` string used (after artifact-domain rewrite if applied? — NO: the cache lookup uses `self.url` which is the original Resource url, not the per-attempt rewritten one; the rewrites only affect the network request, while `cached_location` is computed from the strategy's `url` field set at construction). Verify against `abstract_download_strategy.rb` `@url` vs the loop-local `url` shadow in `fetch`.
- **Glob-then-fallback**: `cached_location` first globs `downloads/<urlsha>--*` (excluding `*.incomplete`); if exactly one exists, reuse it regardless of basename. Replicate: read dir, filter prefix `"<urlsha>--"`, drop names ending `.incomplete`, if count==1 use it else construct deterministic name.
- **Symlink**: must be a *relative* symlink (`relative_path_from`) and force-overwrite. On creation, mkpath parent. macOS codesigning is irrelevant here (these are tarballs, not Mach-O), but the symlink must point correctly for later `stage`. Use `std::os::unix::fs::symlink` after computing the relative path; remove existing first to emulate `force: true`.
- **Atomicity**: download to `<cached>.incomplete`, then `rename` to final (atomic on same filesystem). `mkpath` the dirname before rename.
- **Authorization drop on redirect**: if you use a native client with manual redirect following, strip `Authorization` when the redirect target host differs (ghcr → blob storage). If shelling to curl with `--location`, curl ≥7.58 already drops auth on cross-host redirect, but Homebrew also proactively strips it from `meta[:headers]` only when its *own* preflight detected a redirect — which for bottles is skipped (resolved_basename set). So in practice the Authorization header IS sent on the initial blob GET and curl handles the redirect drop. Match: send Authorization on the registry request; rely on redirect-following to not leak it.
- **Anonymous token**: `Bearer QQ==` is required even for public bottles — ghcr.io rejects unauthenticated `/v2/` requests. Always send it unless a private artifact domain without explicit creds is configured (see §2 condition).
- **Manifest has no checksum**: validate by JSON-parsing and matching `sh.brew.bottle.digest` + `ref.name`; on failure clear cache and retry once.
- **`--continue-at -` resume**: only when a prior `.incomplete`/cached partial exists AND server `Accept-Ranges != none`; if `file.len() == content_length`, skip download entirely. Replicate the HEAD probe for `accept-ranges`/`content-length`.
- **Lock file**: implement a per-`temporary_path` advisory lock (flock) to avoid concurrent downloads; unlink on release.
- Env var trailing-slash handling: `HOMEBREW_ARTIFACT_DOMAIN` must have trailing `/` chomped then one re-added when rewriting (`<domain>/` + rest after `https://ghcr.io/`).

## Open questions
- The cache-key URL: in CurlDownloadStrategy#fetch the loop rebinds local `url` from the interleaved list, but `cached_location` is computed from the strategy's `@url` (the Resource url set at construction). Confirm in abstract_download_strategy.rb that `@url` is the original un-rewritten URL so artifact-domain rewrites do NOT change the cache filename — re-verify the `url` attr_reader vs the `url` local shadow in the fetch begin-block.
- Whether ghcr.io actually issues a redirect for blob GETs in all cases (and thus whether the proactive Authorization-strip path is ever exercised for bottles, given resolved_basename is pre-set and skips the preflight). Behavior depends on registry response, not testable from source alone.
- Exact value of HOMEBREW_USER_AGENT_CURL at runtime depends on brew.sh-computed product/version/system strings (global.rb fetches them from env); the Rust port must reconstruct the same components (HOMEBREW_PRODUCT, version, system, processor, OS version, curl version).
- manifest JSON 'sh.brew.bottle.size' annotation typing: Ruby does .to_i on a string; confirm the manifest stores these as JSON strings (per upload code they are written as `local_file_size.to_s`), so serde should deserialize as String then parse to integer, not as JSON numbers.

# dependency resolution

## Key reference files
- `Library/Homebrew/dependable.rb`
- `Library/Homebrew/dependency.rb`
- `Library/Homebrew/dependency/uses_from_macos_dependency.rb`
- `Library/Homebrew/dependency_collector.rb`
- `Library/Homebrew/dependencies.rb`
- `Library/Homebrew/requirement.rb`
- `Library/Homebrew/dependencies/requirements.rb`
- `Library/Homebrew/software_spec.rb`
- `Library/Homebrew/formula.rb`
- `Library/Homebrew/formula_installer.rb`
- `Library/Homebrew/tab.rb`
- `Library/Homebrew/tab/tab.rb`
- `Library/Homebrew/api/formula/formula_struct_generator.rb`
- `Library/Homebrew/formulary.rb`
- `Library/Homebrew/macos_version.rb`
- `Library/Homebrew/keg_only_reason.rb`
- `Library/Homebrew/simulate_system.rb`

## Specification
# Homebrew Dependency Resolution — Implementation Spec

## 1. Core data model

### 1.1 Dependable (mixin shared by Dependency + Requirement) — `dependable.rb`
Every dependency/requirement has a `tags` array of mixed `Symbol`/`String`/`Array` elements. Symbol tags drive type; String tags are option names (e.g. `"with-foo"`).

Reserved symbol tags (`RESERVED_TAGS`): `:build, :optional, :recommended, :run, :test, :linked, :implicit, :no_linkage`. (`:run`, `:linked` are dead-reserved — never emitted.)

Predicates (all just `tags.include?(:sym)`):
- `build?` → `:build`
- `optional?` → `:optional`
- `recommended?` → `:recommended`
- `test?` → `:test`
- `implicit?` → `:implicit`
- `no_linkage?` → `:no_linkage`
- `required?` → `!build? && !test? && !optional? && !recommended?` (NOTE: `:implicit` does NOT make a dep non-required; an implicit dep with no other type tag is "required").
- `option_tags` → `tags.grep(String)` (the String elements only)
- `options` → `Options.create(option_tags)`

Filter helpers used in install logic:
- `prune_from_option?(build)`: returns false unless `optional? || recommended?`; otherwise returns `build.without?(self)`.
- `prune_if_build_and_not_dependent?(dependent, formula=nil)`: returns false unless `build?`. If `formula` given → `dependent != formula`. Else (`dependent` is a Dependency) → `dependent.installed?`.

### 1.2 Dependency — `dependency.rb`
Fields: `name: String`, `tags: Array`, `tap: Option<Tap>` (derived from `Tap.with_formula_name(name)` — if name has a tap prefix like `user/repo/foo`).

Equality: `name == other.name && tags == other.tags`. `hash = [name, tags].hash`. `eql?` aliases `==`.

`option_names` → `[Utils.name_from_full_name(name)]` (strips tap prefix, e.g. `homebrew/core/foo` → `foo`).

`uses_from_macos?` → `false` (overridden to `true` in subclass).

`to_s` → `name`. `inspect` → `#<Dependency: "name" [tags]>`.

`dup_with_formula_name(formula)` → `self.class.new(formula.full_name, tags)` — used to canonicalize renamed/aliased names after resolving the formula.

`installed?(minimum_version:, minimum_revision:, minimum_compatibility_version:, bottle_os_version:)`: resolves formula via `Formulary.resolve(name)`; false if unavailable / opt_prefix missing. True if `latest_version_installed?`. Then version/revision/compatibility checks against installed keg's Tab (see §8 for full algorithm — relevant to install pruning, not to receipt serialization).

`satisfied?(...)` → `installed?(...) && missing_options.empty?`.

### 1.3 UsesFromMacOSDependency (subclass) — `dependency/uses_from_macos_dependency.rb`
Adds field `bounds: Hash<Symbol,Symbol>` (e.g. `{since: :catalina}` or `{since: :sequoia, until: :sonoma}`). Only `:since` is consulted by resolution logic; `:until` is stored/serialized but not used in `use_macos_install?`.

Equality adds `bounds`. `hash = [name, tags, bounds].hash`.
`uses_from_macos?` → `true`.
`dup_with_formula_name` → `self.class.new(formula.full_name, tags, bounds:)`.

**`use_macos_install?(bottle_os_version: nil) -> bool`** (the OS-bound resolution; see §6):
- Returns `false` unless simulating/running on macOS (`Homebrew::SimulateSystem.simulating_or_running_on_macos?`). On Linux always `false` → the dep is treated as a real brew formula dependency.
- If `bounds[:since]` is blank/absent → return `true` (dep is always provided by macOS).
- Compute `effective_os`:
  - if `bottle_os_version` present and starts with `"macOS "` → `MacOSVersion.new(bottle_os_version.delete_prefix("macOS "))` (the suffix is a numeric string like `"14"`).
  - elsif `SimulateSystem.current_os == :macos` (generic, no concrete version) → `Version::NULL` (treated as less than everything).
  - else → `MacOSVersion.from_symbol(SimulateSystem.current_os)`.
- Compute `since_os = MacOSVersion.from_symbol(bounds[:since])`; on parse error → `Version::NULL`.
- Return `true` if `effective_os >= since_os`, else fall through to `false`.

`installed?(...)` override: `use_macos_install?(bottle_os_version:) || super`.

### 1.4 Dependencies collection — `dependencies.rb`
Wraps an `Array<Dependency>` (delegates via SimpleDelegator). Selectors: `optional`, `recommended`, `build`, `required` (each `select(&:pred?)`), `default = build + required + recommended`.
`dup_without_system_deps` → new Dependencies rejecting deps where `dep.uses_from_macos? && dep.use_macos_install?` (i.e., drop uses_from_macos deps satisfied by the OS).

### 1.5 Requirement — `requirement.rb`
Non-formula constraint (compiler, OS, arch, xcode, codesign, etc.). Fields: `name` (inferred from class name minus `Dependency`/`Requirement` suffix, downcased), `tags`, `cask: Option<String>`, `download: Option<String>`. `build?` adds `:build` if class-level `build` set. Equality: `other.class == self.class && name == other.name && tags == other.tags`. `hash = [self.class, name, tags].hash`.

Concrete API-supported requirement classes (deserialized from JSON; see `DependencyCollector#parse_symbol_spec`): `:arch` (ArchRequirement), `:codesign` (CodesignRequirement), `:linux` (LinuxRequirement), `:macos` (MacOSRequirement), `:maximum_macos` (MacOSRequirement w/ comparator `"<="`), `:xcode` (XcodeRequirement). The API generator only round-trips `[:arch, :linux, :macos, :maximum_macos, :xcode]` (`API_SUPPORTED_REQUIREMENTS`).

### 1.6 Requirements collection — `dependencies/requirements.rb`
Backed by a `Set`. The `<<` operator deduplicates Comparable requirements: when inserting a requirement, it greps existing entries of the same class; if an existing `req > other` it keeps the existing and discards the new; otherwise deletes the existing and inserts the new (keeps the maximum, e.g. highest macOS version requirement).

## 2. DSL → dependency construction — `dependency_collector.rb` + `software_spec.rb`

`DependencyCollector` holds `deps: Dependencies` and `requirements: Requirements`, plus a class-level `Cache` keyed by spec.

`add(spec)`: `fetch(spec)` returns Array | Dependency | Requirement | nil. Array → push each compacted elem into `@deps`. Dependency → `@deps <<`. Requirement → `@requirements <<`. nil → no-op.

`parse_spec(spec, tags)` dispatch:
- `tags.include?(:implicit)` → raise ArgumentError "Implicit dependencies cannot be manually specified". (Implicit is added internally only.)
- String → `Dependency.new(spec, tags)`.
- Resource → resource_dep (adds download-tool deps like curl/git/xz; tags get `:build` + `:test`).
- Symbol → requirement (see §1.5 mapping).
- Requirement|Dependency instance → returned as-is.
- Class (Requirement subclass) → `spec.new(tags)`.

**`uses_from_macos(dep, bounds = {})`** in SoftwareSpec:
- If `dep` is a Hash: `bounds = dep.dup`; `dep, tags = bounds.shift` (first key/value pair becomes name + type tag(s)); `tags = [*tags]`; remaining hash entries are the actual bounds.
- Else: `tags = []`.
- → `depends_on UsesFromMacOSDependency.new(dep, tags, bounds:)`.

**`depends_on(spec)`**: `dep = dependency_collector.add(spec)`; records OS requirement constraints; `add_dep_option(dep)` (auto-adds `with-NAME`/`without-NAME` build options for optional/recommended deps).

A Hash spec like `{ "foo" => :build }` or `{ "foo" => [:build, :test] }` → `build` calls `spec.first` → `["foo", :build]` → `parse_spec("foo", [:build])`.

### 2.1 SoftwareSpec dep accessors
- `deps` → `dependency_collector.deps.dup_without_system_deps` (system-satisfied uses_from_macos dropped). **This is what Formula#deps returns.**
- `declared_deps` → `dependency_collector.deps` (everything, including uses_from_macos satisfied by OS). **This is what Formula#declared_deps returns and what the JSON `to_hash` serializes.**
- `requirements` → `dependency_collector.requirements`.

Formula delegates `deps`, `declared_deps`, `requirements` to `active_spec` (formula.rb:915-921).

## 3. Recursive expansion + topological install order — `dependency.rb` `Dependency.expand`

This is THE topological sort. There is no separate "topo sort" — install order falls out of the recursion order.

**Algorithm `expand(dependent, deps = dependent.deps, cache_key:, cache_timestamp:, &block) -> Array<Dependency>`:**
1. Maintain `@expand_stack` (class-level) of names to break cycles. Push `dependent.name`.
2. Optional cache lookup keyed by `cache_id(dependent) = "#{full_name}_#{class}"`.
3. For each `dep` in `deps` (in declaration order):
   - Skip if `dependent.name == dep.name` (self-dep guard).
   - Compute `action(dependent, dep, &block)`:
     - If block given → block result.
     - Else (default filter): if `dep.optional? || dep.recommended?` → return `PRUNE` unless `dependent.build.with?(dep)`. (i.e. optional/recommended pruned unless requested; required/build/test kept.)
   - Dispatch on action:
     - `PRUNE` (`:prune`) → skip this dep and its whole subtree.
     - `SKIP` (`:skip`) → skip emitting `dep` itself, but recurse into its children: `expanded_deps.concat(expand(dep.to_formula, ...))` (guarded by `@expand_stack.include?(dep.name)`).
     - `KEEP_BUT_PRUNE_RECURSIVE_DEPS` (`:keep_but_prune_recursive_deps`) → push `dep`, do NOT recurse.
     - else (nil/default → KEEP) → unless on stack: recurse children FIRST (`expand(dep_formula)` concatenated), THEN canonicalize name via `dep = dep.dup_with_formula_name(dep_formula)` and push `dep`. **Children are emitted before the parent → post-order DFS → installable order (deps before dependents).**
4. After the loop: `expanded_deps = merge_repeats(expanded_deps)` (dedup by name, preserving FIRST occurrence's position; see §3.1).
5. Cache result; pop stack (in `ensure`).

**Resulting order guarantee:** if A depends on B, B appears before A. Order among siblings follows declaration order. Duplicates are merged to first position.

### 3.1 `merge_repeats(all)` — dedup with tag merging
- Group by name; iterate `all.map(&:name).uniq` (preserves first-seen order).
- For each name, take first dep, compute merged tags via `merge_tags`, rebuild `dep.class.new(name, tags, **kwargs)` (kwargs carries `bounds:` if uses_from_macos).
- `merge_tags(deps)`:
  - `other_tags = deps.flat_map(&:option_tags).uniq`; append `:test` if any dep has `:test`.
  - result = `merge_necessity + merge_temporality + other_tags`.
  - `merge_necessity`: if any dep is neither recommended nor optional → `[]` (required wins). elsif any recommended → `[:recommended]`. else → `[:optional]`.
  - `merge_temporality`: `[:build]` only if ALL deps build?; `[:implicit]` only if ALL deps implicit?.

### 3.2 Formula#recursive_dependencies (formula.rb:2736)
`Dependency.expand(self, cache_key: "Formula#recursive_dependencies"[+"-#{full_name}" if block], cache_timestamp: Time.now if block, &block)`. With a block, uses timestamped cache cleared in `ensure`.

### 3.3 SoftwareSpec#recursive_dependencies (software_spec.rb:361)
Simpler manual variant (used outside install): collects `deps`, resolves each to a formula, then appends each formula's `recursive_dependencies` not already present. Dedup by `include?` (Dependency `==`). Does NOT guarantee post-order the way `Dependency.expand` does — order is: this spec's direct deps first, then recursive deps of each appended. Used by `Requirement.expand`.

## 4. JSON API serialization — `formula.rb` `dependencies_hash` (lines 3173-3236) + `internal_dependencies_hash`

Source: `Formula#dependencies_hash` builds per-spec dep arrays from `declared_deps` of each spec (`:stable`, `:head`). Implicit deps are always rejected first (`reject(&:implicit?)`). Output keys merged into the formula `to_hash` (defaults to empty arrays at to_hash lines 2955-2961).

For each spec (`:stable` goes to top-level hash; `:head` → nested under `"head_dependencies"` ONLY if head deps differ from stable; if `head == stable` it is omitted entirely):

- `"build_dependencies"`: `select(&:build?).reject(&:uses_from_macos?).map(&:name).uniq`
- `"dependencies"`: `reject(&:optional?).reject(&:recommended?).reject(&:build?).reject(&:test?).reject(&:uses_from_macos?).map(&:name).uniq` — i.e. plain required runtime deps.
- `"test_dependencies"`: `select(&:test?).reject(&:uses_from_macos?).map(&:name).uniq`
- `"recommended_dependencies"`: `select(&:recommended?).reject(&:uses_from_macos?).map(&:name).uniq`
- `"optional_dependencies"`: `select(&:optional?).reject(&:uses_from_macos?).map(&:name).uniq`
- `"uses_from_macos"`: from `select(&:uses_from_macos?).uniq`; each entry serialized as:
  - if `dep.tags.length >= 2` → `{ name => tags }` (tags is an array)
  - elsif `dep.tags.present?` (exactly 1) → `{ name => tags.first }` (single value)
  - else → `name` (bare string)
- `"uses_from_macos_bounds"`: `uses_from_macos_deps.map(&:bounds)` — **positionally aligned (zipped) with `uses_from_macos` array, same length/order**. Each element is a Hash like `{"since": "catalina"}` or `{}`.

**Key invariants for Rust:** the 5 type arrays contain ONLY non-uses_from_macos deps as bare name strings; uses_from_macos deps live exclusively in the two parallel arrays. A dep's "type" classification is mutually-derived from tags (a dep can be both build AND test → appears in both build_dependencies and test_dependencies; build_dependencies select is independent of dependencies select). `"dependencies"` = strictly required runtime.

### 4.1 internal_dependencies_hash (for non-API/internal v3 API)
Per declared dep (skip implicit): `{ name => metadata_or_nil }` where metadata = `{tags: dep.tags (if present), uses_from_macos: dep.bounds (if uses_from_macos? && present)}` or `nil` if empty.

## 5. JSON API deserialization — `api/formula/formula_struct_generator.rb` + `formulary.rb`

Reconstruction reverses §4. Per spec, builds `stable_dependencies`/`head_dependencies` (array of `depends_on` args) + `stable_uses_from_macos`/`head_uses_from_macos` (array of `uses_from_macos` args). `head_dependencies` defaults to stable's if absent (line 170).

**`process_dependencies(deps_hash)`** (generator:251):
`dependencies` (plain array of names) `+` for each type in `[:build, :test, :recommended, :optional]`: map each name → `{ name => type }`. Flattened. So a build dep `"cmake"` becomes `{ "cmake" => :build }` → `depends_on({"cmake" => :build})`.

**`process_uses_from_macos(deps_hash)`** (generator:299):
Zip `uses_from_macos` with `uses_from_macos_bounds` (positional). For each `(entry, bounds)`:
- `bounds ||= {}`, then `transform_keys(&:to_sym).transform_values(&:to_sym)`.
- If `entry` is a Hash (i.e. `{name => type}`): deep-symbolize values, then `entry = entry.merge(bounds)` (merge bounds keys into the same hash), emit `[entry, {}]`. → `uses_from_macos(entry, {})` where `entry = {name => type, since: :catalina, ...}`. NOTE: `uses_from_macos`'s Hash-handling shifts the first pair as name+tags and treats remaining keys as bounds.
- Else (`entry` is a bare name string): emit `[entry, bounds]` → `uses_from_macos("name", {since: :catalina})`.

`symbolize_dependency_hash` (generator:223): symbolizes `uses_from_macos_bounds` keys+values to symbols; for any dep Hash, transforms values (`Array → map(&:to_sym)`, scalar → `to_sym`) leaving the name key as-is.

`process_requirements` (generator:261): only for reqs whose `"specs"` includes the spec name; only `API_SUPPORTED_REQUIREMENTS`. `req_version`: for `:arch` → `version.to_sym`; for `:macos`/`:maximum_macos` → `MacOSVersion::SYMBOLS.key(version)` (reverse-lookup version string → symbol, e.g. `"10.15"` → `:catalina`); else raw. Tags = `[version?] + contexts(mapped: String→sym, Hash→deep-sym)`. Emit `req_name` (bare symbol) if no tags, else `{req_name => tags}`.

In `formulary.rb` (lines 276-298) the reconstructed args feed `depends_on dep` and `uses_from_macos(*args)` inside `stable do`/`head do` blocks.

## 6. uses_from_macos resolution summary (system lib vs brew formula)
At resolve/install time, a `UsesFromMacOSDependency` is satisfied by the OS (i.e. NOT installed as a brew formula) iff `use_macos_install?` is true (§1.3): on macOS, when no `:since` bound OR `effective_os >= since_os`. On Linux (or older macOS than `:since`), it becomes a normal brew formula dep. `Dependencies#dup_without_system_deps` (and thus `Formula#deps`) removes the OS-satisfied ones; `declared_deps` keeps them. The JSON serializer always emits all uses_from_macos in the parallel arrays regardless of current OS (serialization is OS-agnostic; resolution is per-OS).

## 7. INSTALL_RECEIPT.json `runtime_dependencies` array — `tab/tab.rb`

Filename constant: `FILENAME` (the install receipt). Written by `Tab#write` via `tabfile.atomic_write(to_json)` to `formula.prefix/FILENAME`.

**Computation (`Tab.create`, tab/tab.rb:64-101):**
1. `runtime_deps = formula.runtime_dependencies(undeclared: false)`.
2. `tab.runtime_dependencies = Tab.runtime_deps_hash(formula, runtime_deps)`.

**`runtime_dependencies(read_from_tab: true, undeclared: false)`** (formula.rb:2799) with `undeclared: false`:
- Skips tab-read branch (only used when `undeclared` true).
- `deps = declared_runtime_dependencies` (because `unless undeclared`).
- On `FormulaUnavailableError` → `[]`.

**`declared_runtime_dependencies`** (formula.rb:3396): `Dependency.expand` with a block:
- `PRUNE` if `dep.build?` (build deps excluded from runtime).
- keep (return nil) if `dep.required?`.
- if `build.any_args_or_options?` → `PRUNE` if `build.without?(dep)` (drop unselected optional/recommended); else (no build args) → `PRUNE` unless `dep.recommended?` (drop optional, keep recommended by default).
- → Result: required deps + default-on recommended deps + selected optional deps, recursively expanded, build/test excluded. Test deps are also excluded (only build/test/optional pruning; test deps aren't required/recommended so they're pruned by the `!dependency.recommended?` else branch... actually test deps fall through: not build, not required (test? makes required? false), not recommended → PRUNE). So **only required + active recommended/optional runtime deps**.

**`runtime_deps_hash(formula, deps)`** (tab/tab.rb:244): `deps.map { |dep| formula_to_dep_hash(dep.to_formula, formula.deps.map(&:name)) }`.

**`formula_to_dep_hash(formula, declared_deps)`** (tab.rb:160) — EXACT per-entry shape (`.compact` removes nil values, so nil fields are OMITTED):
```
{
  "full_name"             => formula.full_name,          # String
  "version"               => formula.version.to_s,       # String (version only, no revision)
  "revision"              => formula.revision,           # Integer (omitted if nil — but revision defaults to 0, present)
  "bottle_rebuild"        => formula.bottle&.rebuild,    # Integer or omitted if no bottle/nil
  "pkg_version"           => formula.pkg_version.to_s,   # String = "version_revision" (PkgVersion.new(version, revision))
  "declared_directly"     => declared_deps.include?(formula.full_name),  # Boolean
  "compatibility_version" => formula.compatibility_version,  # Integer or omitted if nil
}.compact
```
- `pkg_version` = `PkgVersion.new(version, revision).to_s` — if revision == 0 it is just the version string; if revision > 0 it is `"#{version}_#{revision}"`.
- `declared_directly`: true iff this dep's `full_name` is in the depending formula's **direct** `deps` (i.e. `formula.deps.map(&:name)`, which is the system-dep-pruned direct deps of the active spec — NOT recursive). Transitive-only deps get `declared_directly: false`.
- `version` is the bare version (no revision); `pkg_version` includes revision.
- `.compact` drops any key whose value is `nil` (so `bottle_rebuild`/`compatibility_version` absent when nil; `revision` is normally an integer incl. 0 so kept).

**Reading back** (`Tab#runtime_dependencies`, tab/tab.rb:303): returns `@runtime_dependencies` only if `parsed_homebrew_version >= "1.1.6"`, else nil (pre-1.1.6 tabs had buggy lists).

In `formula.rb to_hash` `"installed"` array (line 2999-3010) each installed keg emits `"runtime_dependencies" => tab.runtime_dependencies` (the stored array verbatim).

## 8. Dependency#installed? full version check (for install-time pruning, formula.rb:70-117)
Not needed for receipt serialization but needed for install resolution:
- false unless formula resolvable & `opt_prefix.exist?`.
- true if `latest_version_installed?`.
- false if `minimum_version` blank.
- get `installed_keg`; false unless present; false unless `formula.possible_names.include?(installed_keg.name)`.
- compatibility-version short-circuit: if both `minimum_compatibility_version` and `formula.compatibility_version` present, read installed tab `source.dig("versions","compatibility_version")`; return true if both equal `minimum_compatibility_version`.
- if `minimum_revision` present → `installed_version >= PkgVersion.new(minimum_version, minimum_revision)`.
- elsif `installed_version.version == minimum_version` → `formula.revision.zero?`.
- else → `installed_version.version > minimum_version`.

## 9. Install order in FormulaInstaller — `formula_installer.rb`
`compute_dependencies` → `check_requirements(expand_requirements)` then `expand_dependencies`.
`expand_dependencies_for_formula(formula)` (line 754) calls `Dependency.expand(formula, cache_key:)` with a block returning:
- `PRUNE` if `dep.prune_from_option?(build)` OR (`(dep.build? || dep.test?) && !keep_build_test`).
- `SKIP` (recurse children but don't install this dep) if `dep.satisfied?(minimum_version:, minimum_revision:, bottle_os_version:)`.
- else keep.
`keep_build_test` true if: (test dep && include_test && formula in `@include_test_formulae`) OR (build dep && not pouring bottle && (head || dependent not latest-installed)).
`minimum_version`/`minimum_revision` pulled from `@bottle_tab_runtime_dependencies.dig(dep.name, "version"/"revision")` (the bottle's recorded runtime deps), `bottle_os_version` from `@bottle_built_os_version`.
Returned array is already in installable (post-order) order; `install_dependencies` iterates it in order (special-casing bubblewrap on Linux to set `HOMEBREW_INSTALLING_BUBBLEWRAP=1` for itself and all deps up to/including the bubblewrap implicit dep).

`expand_requirements` (line 718) walks formula + recursive deps, collecting unsatisfied requirements per dependent, pruning satisfied/build/test/option-pruned ones. Requirement dedup keeps the max (Requirements `<<`).

## 10. keg_only representation (context for `dependencies` consumers)
`keg_only?` boolean; `keg_only_reason.to_hash` = `{"reason" => reason_string, "explanation" => explanation}`. `reason_string`: if reason is a Symbol → `@reason.inspect` (e.g. `:provided_by_macos` → `":provided_by_macos"`); else the string. keg_only does not change dependency *resolution* (deps of a keg_only formula are resolved identically); it affects linking only. Recognized symbol reasons: `:versioned_formula`, `:provided_by_macos`, `:shadowed_by_macos`.

## 11. MacOSVersion symbol table — `macos_version.rb:23` (`SYMBOLS`, ordered newest→oldest)
`tahoe:"26", sequoia:"15", sonoma:"14", ventura:"13", monterey:"12", big_sur:"11", catalina:"10.15"`.
`from_symbol(:catalina)` → `MacOSVersion.new("10.15")`. Comparison `<=>`: Symbol other resolved via SYMBOLS then numeric Version compare; `Version::NULL` (internal `"10.0"` tagged null) compares less than any real version. Used by uses_from_macos `since` bound check (`effective_os >= since_os`).

## Rust implementation notes
Recommended Rust shape:

```rust
enum DepKind { Required, Build, Test, Optional, Recommended } // derived, not stored
struct Dependency {
  name: String,                 // may include tap prefix "user/repo/foo"
  tags: Vec<Tag>,               // ordered, mixed
  tap: Option<Tap>,
  uses_from_macos: Option<UsesFromMacOS>, // None for plain dep
}
enum Tag { Build, Optional, Recommended, Test, Implicit, NoLinkage, Run, Linked, Option(String) }
struct UsesFromMacOS { bounds: BTreeMap<MacOsBoundKey, MacOsSymbol> } // {Since, Until} -> symbol
```
Use predicate methods mirroring Dependable. `required?` = none of build/test/optional/recommended (implicit irrelevant). Equality/hash must include name+tags (+bounds for uses_from_macos) — implement Hash/Eq matching Ruby exactly; tag ORDER matters for equality (Ruby compares arrays positionally), so preserve insertion order and replicate merge_tags ordering: necessity tags, then temporality (build, implicit), then option(String) tags (uniq, with :test appended to the string-tags bucket if any test). When merging, rebuild tags in that canonical order.

Topological expand: implement `Dependency.expand` as recursive post-order DFS over `formula.deps` with a name-based visited stack to break cycles, a per-call action enum {Prune, Skip, KeepButPruneRecursive, Keep}, and a final `merge_repeats` pass (group_by name preserving first-seen order, merge tags). The OUTPUT ORDER is the install order — do not add a separate topo sort. For SKIP, recurse children but don't emit the node. For default-no-block filter: prune optional/recommended unless `build.with?(dep)`.

Caching: Ruby uses a class-level cache keyed by `"{full_name}_{ClassName}"` with timestamped vs non-timestamped buckets. In Rust, prefer passing a `&mut HashMap` cache or per-resolution memo; the timestamp dance exists to avoid stale singleton cache across build-option variations — you can model it as "cache only when no per-formula build options influence pruning."

uses_from_macos resolution: model `SimulateSystem { os: OsTarget, arch }`. `OsTarget` = {GenericMacos, Macos(MacOsVersion), Linux, GenericLinux...}. `use_macos_install`: on macOS, true if no `since` bound, else `effective_os >= from_symbol(since)`. effective_os: bottle_os_version ("macOS NN") > generic-macos uses NULL (min) > concrete macOS symbol. Represent `Version::NULL` as a sentinel comparing less than all. Implement MacOSVersion as numeric dotted version with the SYMBOLS table (newest→oldest) for symbol<->string conversion; reverse-lookup (`SYMBOLS.key(v)`) needed for requirement version round-trip.

JSON (serde): For the public API, dependencies are flat `Vec<String>` for the 5 type arrays; `uses_from_macos` is `Vec<UfmEntry>` where `UfmEntry = String | { name: tag } | { name: [tags] }` (use `#[serde(untyped)]`/enum); `uses_from_macos_bounds` is a parallel `Vec<Map<String,String>>` aligned by INDEX with `uses_from_macos`. Deserialize by zipping the two arrays. Serialize: emit single-tag as scalar, >=2 tags as array, no-tags as bare string — match the length>=2 / present / else branching exactly. head deps omitted when identical to stable; on read, default head = stable. Implicit deps are NEVER serialized (filtered before serialize).

INSTALL_RECEIPT runtime_dependencies: serialize each as a serde struct with `#[serde(skip_serializing_if = "Option::is_none")]` to replicate Ruby `.compact` (omit nil fields). Field order in spec but JSON key order is not load-bearing for correctness, only `serde` round-trip. Compute via the `declared_runtime_dependencies` expand (build/test pruned, optional pruned unless selected, recommended kept by default) NOT the linkage/undeclared path (that path only runs with `undeclared: true`). `declared_directly` = membership test of dep.full_name in the parent's DIRECT pruned deps (`Formula#deps` = system-dep-stripped active-spec direct deps), not recursive. `version` = version string sans revision; `pkg_version` = `version` + (`_revision` if revision>0). `revision` is an i64 (default 0, always present). `bottle_rebuild` and `compatibility_version` are Option (omit when None).

Gotchas: (1) `:implicit` deps participate in expansion/install (used for source download tools like curl/git/xz) but are stripped from ALL API JSON and from internal_dependencies_hash; they DO appear in install ordering. (2) A dep can carry multiple type tags and thus appear in multiple JSON type arrays simultaneously. (3) `deps` (pruned) vs `declared_deps` (full) distinction is critical: serialization uses declared_deps; install/runtime uses deps. (4) Requirements collection keeps the maximum requirement of each class (semver-style max), unlike Dependencies which keep first + merge tags. (5) `dup_with_formula_name` rewrites a dep's name to the canonical full_name after resolving aliases/renames — do this post-recursion before emitting. (6) atomic_write for the receipt: write temp + rename for atomicity.

## Open questions
- Whether the Rust reimplementation will compute INSTALL_RECEIPT runtime_dependencies from formula definitions (the declared_runtime_dependencies path) or also support the linkage-checker undeclared path (Formula#undeclared_runtime_dependencies uses LinkageChecker on the installed keg's Mach-O/ELF libs — a large separate subsystem not covered here). Tab.create uses undeclared:false so the receipt path does NOT need linkage; confirm no other writer uses undeclared:true.
- The exact PkgVersion.to_s formatting for edge cases (e.g. head versions, version_scheme) — verified revision>0 yields 'version_revision' but head/special pkg_versions not exhaustively traced here.
- Whether `bottle_rebuild` is ever non-nil in receipts when installing from source vs bottle — it is `formula.bottle&.rebuild`, present only when a bottle is defined for the active spec; confirm install-from-source still records it.
- Linux-specific implicit deps (gcc/glibc/bubblewrap dep_if_needed methods are stubs returning nil in shared dependency_collector.rb; real logic is in extend/os/dependency_collector.rb which was not read here) — needs a follow-up read of Library/Homebrew/extend/os/dependency_collector.rb for full Linux dependency injection behavior.

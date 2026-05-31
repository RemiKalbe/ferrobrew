# Keg linking + HOMEBREW_PREFIX layout (link / unlink / optlink / link_overwrite)

## Key reference files
- `Library/Homebrew/keg.rb`
- `Library/Homebrew/extend/os/keg.rb`
- `Library/Homebrew/extend/os/mac/keg.rb`
- `Library/Homebrew/extend/os/linux/keg.rb`
- `Library/Homebrew/extend/pathname/observer_pathname_extension.rb`
- `Library/Homebrew/extend/pathname.rb`
- `Library/Homebrew/startup/config.rb`
- `Library/Homebrew/formula.rb`
- `Library/Homebrew/keg_only_reason.rb`
- `Library/Homebrew/unlink.rb`
- `Library/Homebrew/cmd/link.rb`
- `Library/Homebrew/formula_installer.rb`

## Specification
# Keg Linking + Prefix Layout Spec

A "keg" is one installed version of a formula: the directory `HOMEBREW_CELLAR/<name>/<version>/`. The "rack" is `HOMEBREW_CELLAR/<name>/` (parent of all versions). Linking symlinks selected contents of a keg into `HOMEBREW_PREFIX/{bin,lib,include,share,...}` so they appear on PATH etc. A stable `opt/<name>` symlink always points at the active keg.

## 1. Path constants (from startup/config.rb, resolved from env vars)

All are absolute `Pathname`s. `<name>` = formula name, `<version>` = `PkgVersion` string (e.g. `1.2.3`, `1.2.3_1`, `HEAD-abc123`).

- `HOMEBREW_PREFIX` = env `HOMEBREW_PREFIX` (e.g. `/opt/homebrew` ARM macOS, `/usr/local` Intel macOS, `/home/linuxbrew/.linuxbrew` Linux).
- `HOMEBREW_CELLAR` = env `HOMEBREW_CELLAR`. Normally `HOMEBREW_PREFIX/Cellar`, but when prefix==repository it is `HOMEBREW_REPOSITORY/Cellar`. Treat as independently configurable; do NOT assume it is under prefix.
- `HOMEBREW_CASKROOM` = env `HOMEBREW_CASKROOM` = `HOMEBREW_PREFIX/Caskroom`.
- `HOMEBREW_LINKED_KEGS` = `HOMEBREW_PREFIX/var/homebrew/linked` (directory of symlinks tracking which keg is linked).
- `HOMEBREW_PINNED_KEGS` = `HOMEBREW_PREFIX/var/homebrew/pinned`.
- `HOMEBREW_LOCKS` = `HOMEBREW_PREFIX/var/homebrew/locks`.
- `HOMEBREW_LOGS` = env `HOMEBREW_LOGS` (expanded).
- `HOMEBREW_CACHE` = env `HOMEBREW_CACHE`. Backups go to `HOMEBREW_CACHE/Backup`.
- `HOMEBREW_DEFAULT_PREFIX` = env `HOMEBREW_GENERIC_DEFAULT_PREFIX` (used to decide whether to refuse linking macOS-shadowed software).
- `HOMEBREW_ORIGINAL_BREW_FILE` = env `HOMEBREW_ORIGINAL_BREW_FILE`; its file `stat.uid` is the "Homebrew uid" used in overwrite checks.

## 2. Keg object model (keg.rb)

`Keg.new(path)`:
- If `path` starts with `"#{HOMEBREW_PREFIX}/opt/"`, first resolve the symlink (`path.resolved_path`) so an opt path maps to its real Cellar keg.
- Validate: `path.parent.parent.realpath == HOMEBREW_CELLAR.realpath` else raise "is not a valid keg"; and `path.directory?` else raise "is not a directory".
- Fields:
  - `name` = `path.parent.basename` (the rack name).
  - `path` = the keg dir (protected).
  - `rack` = `path.parent`.
  - `linked_keg_record` = `HOMEBREW_LINKED_KEGS/<name>` (a symlink-or-nothing).
  - `opt_record` = `HOMEBREW_PREFIX/opt/<name>` (a symlink-or-nothing).
  - `version` = `PkgVersion.parse(path.basename)`.
- `Keg.for(path)`: realpath-walk upward until `path.parent.parent == HOMEBREW_CELLAR.realpath`, returning the keg dir; raises `NotAKegError` if not under Cellar, `Errno::ENOENT` if path missing.

### linked? / optlinked?
- `linked?` is true iff `linked_keg_record` is a symlink AND resolves to a directory AND `path == linked_keg_record.resolved_path`. This is the canonical "is the active version this keg" check.
- `optlinked?` is true iff `opt_record` is a symlink AND `path == opt_record.resolved_path`.
- `resolved_path` for a symlink = `dirname.join(readlink)` (does NOT fully canonicalize; one-level resolve relative to the link's own directory). For a non-symlink, returns self.

## 3. Linkable top-level directories (`keg_link_directories`)

Class method `Keg.keg_link_directories` returns (order matters as a set, not iteration):
```
bin etc include lib sbin share var
```
On macOS the override appends `Frameworks` → `bin etc include lib sbin share var Frameworks`. On Linux it is the base list only.

Note: `var` is in `keg_link_directories` (so it is unlinked/scanned) but linking of `var` is NOT performed by `link` (see §5 — `link` only calls link_dir on etc, bin, sbin, include, share, lib, Frameworks; NOT var). `unlink` iterates ALL `keg_link_directories` including `var`.

### must_exist_subdirectories (created/kept, never rmdir'd on unlink)
`(keg_link_directories - [var]) + [opt, var/homebrew/linked]`, each prefixed with `HOMEBREW_PREFIX`, sorted+uniq. On macOS also add `HOMEBREW_PREFIX/Frameworks`. So on macOS:
`HOMEBREW_PREFIX/{Frameworks, bin, etc, include, lib, opt, sbin, share, var/homebrew/linked}`.
These are the directories that must always exist and that `unlink` will NOT attempt to rmdir.

## 4. ObserverPathnameExtension (observer_pathname_extension.rb)

A mixin extended onto individual `Pathname` instances (`dst.extend(ObserverPathnameExtension)`) to count operations and print verbose lines. Implement as a per-operation counter + optional logging wrapper.

- Module-level counters: `n` (links + unlinks created/removed) and `d` (rmdir count). `reset_counts!` zeroes both and clears `@put_verbose_trimmed_warning`. `total = n + d`.
- Wrapped ops (each calls the real op then optionally prints and increments):
  - `unlink`: print `"rm #{self}"`; `n += 1`.
  - `make_relative_symlink(src)`: print `"ln -s #{src.relative_path_from(dirname)} #{basename}"`; `n += 1`.
  - `mkpath`: print `"mkdir -p #{self}"`; (no counter).
  - `rmdir`: print `"rmdir #{self}"`; `d += 1`.
  - `install_info`: print `"info #{self}"`. `uninstall_info`: print `"uninfo #{self}"`.
- `verbose?`: normally the global verbose flag. When env `CI` is set and total >= 100 (`MAXIMUM_VERBOSE_OUTPUT`), suppress output after printing once: `"Only the first 100 operations were output."`.
- `link`/`unlink` return value = `ObserverPathnameExtension.n` (the count of symlink/unlink ops). `cmd/link.rb` prints `"<n> symlinks created."`.

## 5. The `link` algorithm (Keg#link)

Signature: `link(verbose:, dry_run:, overwrite:) -> Integer`. Steps:

1. **Already-linked guard**: if `linked_keg_record.directory?` raise `AlreadyLinkedError` (message names the conflicting `linked_keg_record.resolved_path`).
2. `ObserverPathnameExtension.reset_counts!`.
3. Unless `dry_run`: call `optlink(...)` (see §7) — creates opt symlink BEFORE prefix linking.
4. Call `link_dir` for each directory in this exact order, each with a strategy block returning a `Symbol`:
   - `etc` → always `:mkpath` (etc dirs are real dirs, files within symlinked individually).
   - `bin` → always `:skip_dir` (do not recurse into subdirs; only top-level files get symlinked; subdirs are pruned).
   - `sbin` → always `:skip_dir`.
   - `include` → `:mkpath` if the relative path matches `/^postgresql@\d+/`, else `:link`.
   - `share` → strategy by relative-path regex (see §6 share rules).
   - `lib` → strategy by relative-path regex (see §6 lib rules).
   - `Frameworks` (macOS only; on Linux this dir generally absent so harmless) → `:mkpath` if relative path matches `%r{[^/]*\.framework(/Versions)?$}` (i.e. `Foo.framework` and `Foo.framework/Versions`), else `:link`.
5. Unless `dry_run`: `make_relative_symlink(linked_keg_record, path, ...)` — creates `HOMEBREW_LINKED_KEGS/<name>` → keg path. THIS is what marks the keg linked.
6. On `LinkError` rescue: call `unlink(verbose:)` to roll back, then re-raise.
7. Return `ObserverPathnameExtension.n`.

### Strategy symbols (what link_dir does with each)
- `:link` — make a relative symlink `dst -> src`. For files: symlink the file. For directories: try to resolve conflicts; if none, symlink the whole directory as a single symlink and prune (don't recurse). THIS is "symlink-as-dir".
- `:mkpath` — create the dst directory as a real dir (`dst.mkpath`) and recurse into it (file-by-file). THIS is "real dir, recurse". For a file matched as mkpath (rare) it falls into the default branch and gets symlinked.
- `:skip_dir` — `Find.prune` (do not recurse into this directory at all). Used for bin/sbin subdirectories: only top-level files of bin/sbin are linked.
- `:skip_file` / `nil` — for a file, `Find.prune` (skip it). Used to skip specific files like `locale/locale.alias`, `charset.alias`, icon-theme caches.
- `:info` — symlink the file then run `install_info` (GNU info registration). `dir` files are skipped (`File.basename(src) == "dir"`).

## 6. link_dir traversal (Keg#link_dir) — the core walker

`link_dir(relative_dir, ...) { |relative_path| strategy_symbol }`:

- `root = path/relative_dir`. Return immediately unless `root.exist?`.
- Use `root.find` (Ruby `Find.find`, depth-first PRE-ORDER traversal). For each `src`:
  - Skip `src == root`.
  - `dst = HOMEBREW_PREFIX + src.relative_path_from(path)` (i.e. dst mirrors keg-relative path under prefix). Extend `dst` with ObserverPathnameExtension.
  - **If `src` is a symlink OR a regular file**:
    - `Find.prune` if basename is `.DS_Store`.
    - `Find.prune` if `src.resolved_path == dst` (already pointing at itself).
    - `Find.prune` if extension in `.pyc`/`.pyo` AND path contains `/site-packages/` (don't link Python cached objects).
    - Evaluate strategy `yield src.relative_path_from(root)`:
      - `:skip_file` or `nil` → `Find.prune`.
      - `:info` → if basename=="dir" skip (`next`); else `make_relative_symlink(dst, src)` then `dst.install_info`.
      - else (`:link`/`:mkpath`/`:skip_dir` on a file) → `make_relative_symlink(dst, src)`.
  - **If `src` is a directory**:
    - If `dst.directory? && !dst.symlink?` (dst already a real dir) → `next` (walk into it, do not prune; lets the tree merge).
    - `Find.prune` if `src.extname == ".app"` (never link .app bundles into prefix).
    - Evaluate strategy `yield src.relative_path_from(root)`:
      - `:skip_dir` → `Find.prune`.
      - `:mkpath` → `dst.mkpath` UNLESS `resolve_any_conflicts(dst,...)` returned true (i.e. a conflicting symlinked dir was expanded into a real dir).
      - else (`:link`) → unless `resolve_any_conflicts(dst,...)`: `make_relative_symlink(dst, src)` then `Find.prune` (symlink the whole subtree as one symlink and stop descending).

Key consequence: a subtree reached via `:link` becomes a single directory symlink; a subtree under `:mkpath` becomes a mirrored real-dir tree with file-level symlinks (so multiple formulae can co-own the dir).

### share strategy rules (relative path under `share/`)
- matches `INFOFILE_RX` = `%r{info/([^.].*?\.info(\.gz)?|dir)$}` → `:info`.
- `"locale/locale.alias"` OR matches `%r{^icons/.*/icon-theme\.cache$}` → `:skip_file`.
- matches any of: `LOCALEDIR_RX`, `%r{^icons/}`, `/^zsh/`, `/^fish/`, `%r{^lua/}`, `%r{^guile/}`, `/^postgresql@\d+/`, `/^pypy/`, OR any element of `SHARE_PATHS` → `:mkpath`.
- else → `:link`.

`LOCALEDIR_RX` = `%r{(locale|man)/([a-z]{2}|C|POSIX)(_[A-Z]{2})?(\.[a-zA-Z\-0-9]+(@.+)?)?}` (locale-style `lang[_TERR][.codeset][@mod]`).

`SHARE_PATHS` (always real dirs, never symlinks):
```
aclocal cps doc info java locale man
man/man1 man/man2 man/man3 man/man4 man/man5 man/man6 man/man7 man/man8
man/cat1 man/cat2 man/cat3 man/cat4 man/cat5 man/cat6 man/cat7 man/cat8
applications gnome gnome/help icons mime-info pixmaps sounds postgresql
```

### lib strategy rules (relative path under `lib/`)
- `"charset.alias"` → `:skip_file`.
- exactly one of `"cps"`, `"pkgconfig"`, `"cmake"`, `"dtrace"`, `"ghc"`, `"php"`, OR matches `/^gdk-pixbuf/`, `/^gio/`, `/^lua/`, `/^mecab/`, `/^node/`, `/^ocaml/`, `/^perl5/`, `/^postgresql@\d+/`, `/^pypy/`, `/^python[23]\.\d+/`, `/^R/`, `/^ruby/` → `:mkpath`.
- else → `:link`.

### include strategy
- `/^postgresql@\d+/` → `:mkpath`, else `:link`.

## 7. optlink (Keg#optlink) — the stable opt/<name> symlink

`optlink(verbose:, dry_run:, overwrite:)`:
1. If `opt_record` is a symlink or exists, `opt_record.delete`.
2. `make_relative_symlink(opt_record, path, ...)` → `HOMEBREW_PREFIX/opt/<name>` → keg path.
3. For each alias `a` in `tab.aliases`: delete-then-create `opt_record.parent/a` (i.e. `HOMEBREW_PREFIX/opt/<alias>`) symlink → keg path.
4. For each `oldname_opt_records`: delete and recreate symlink → keg path.

`opt_record` / `Formula#opt_prefix` = `HOMEBREW_PREFIX/opt/<name>`. This is the user-facing stable path (`opt_bin = opt_prefix/bin`, etc.).

`oldname_opt_records`: scan `HOMEBREW_PREFIX/opt` subdirs; pick those that are symlinks, != opt_record, AND `dir.resolved_path.parent == path.parent` (i.e. point at another keg in the SAME rack — renamed-formula aliases).

## 8. make_relative_symlink + conflict detection

`make_relative_symlink(dst, src, ...)`:
1. If `dst.symlink? && src == dst.resolved_path` → already linked; print "Skipping; link already exists" if verbose; return.
2. Dry-run + overwrite: print `"#{dst} -> #{dst.resolved_path}"` if dst is symlink, else `dst` if exists; return.
3. Dry-run only: print `dst`; return.
4. If `overwrite && (dst.exist? || dst.symlink?)`: `dst.delete`.
5. `dst.make_relative_symlink(src)` = `dst.dirname.mkpath` then `File.symlink(src.relative_path_from(dst.dirname), dst)` — i.e. the symlink target is RELATIVE.
6. Rescues:
   - `Errno::EEXIST`: if `dst.exist?` → raise `ConflictError(self, src.relative_path_from(path), dst, e)`. If `dst.symlink?` (broken symlink) → `dst.unlink` and `retry`.
   - `Errno::EACCES` → raise `DirectoryNotWritableError`.
   - other `SystemCallError` → raise `LinkError`.

`resolve_any_conflicts(dst, ...)` — only acts if `dst.symlink?`:
- `src = dst.resolved_path`; `lstat` it. If ENOENT (broken link) → unlink dst (unless dry_run), return nil.
- Return nil unless the link target is a directory.
- `Keg.for(src)`: if NotAKegError → (verbose msg) return nil (leave foreign symlink).
- Else: unlink dst (unless dry_run); call the OTHER keg's `link_dir(src, ...) { :mkpath }` to expand its previously-single dir-symlink into a real dir of file-symlinks; return true. This is how two formulae come to share a `:link`-strategy dir: the first one's whole-dir symlink is "exploded" into per-file symlinks so both can coexist.

### Error types
- `LinkError(keg, src, dst, cause)` — base; carries src (keg-relative), dst (prefix path).
- `ConflictError < LinkError` — `to_s` produces the "Could not symlink ... To force the link and overwrite all conflicting files: brew link --overwrite <name>" message; `suggestion` tells whether dst is a foreign keg's symlink (suggest `brew unlink <other>`) or a plain file (suggest `rm`).
- `DirectoryNotWritableError < LinkError` — "<dst.dirname> is not writable."
- `AlreadyLinkedError(keg)`.

## 9. The `unlink` algorithm (Keg#unlink)

`unlink(verbose:, dry_run:) -> Integer`:
1. `ObserverPathnameExtension.reset_counts!`.
2. `dirs = []`.
3. For each `dir` in `keg_link_directories.map { path/d }.select(&:exist?)` (NOTE: includes `var`):
   - `dir.find` over each `src`:
     - `dst = HOMEBREW_PREFIX + src.relative_path_from(path)`; extend observer.
     - If `dst.directory? && !dst.symlink?` → push to `dirs` (candidate for rmdir later).
     - `next` unless `dst.symlink?`.
     - `next` if `src != dst.resolved_path` (only remove links that point back into THIS keg).
     - dry_run → print dst; `Find.prune if src.directory?`; next.
     - If `dst` matches `INFOFILE_RX` → `dst.uninstall_info`.
     - `dst.unlink`; `Find.prune if src.directory?`.
4. Unless dry_run:
   - `remove_old_aliases`.
   - `remove_linked_keg_record if linked?` (removes `HOMEBREW_LINKED_KEGS/<name>` and rmdir-if-possible its parent).
   - `(dirs - must_exist_subdirectories).reverse_each(&:rmdir_if_possible)` — remove now-empty mirrored dirs deepest-first, but NEVER the must-exist dirs.
5. Return `ObserverPathnameExtension.n`.

`unlink` does NOT remove `opt_record` (opt symlink survives unlink; only `uninstall` removes it).

`rmdir_if_possible`: rmdir; on ENOTEMPTY, if the only child is `.DS_Store`, unlink it and retry; on EACCES/ENOENT/EBUSY/EPERM return false. Returns true if removed.

`remove_old_aliases`: removes stale alias symlinks under both `opt` and `linked` dirs for current `tab.aliases` (skipping versioned `@`-aliases via `/.+@./`), plus prunes `opt_record@*` glob entries not in current aliases. Also removes a bad `opt/<tap.user>` dir if present.

## 10. linked_keg tracking

The single source of truth for "which version is linked" is the symlink `HOMEBREW_LINKED_KEGS/<name>` (= `var/homebrew/linked/<name>`) pointing at the keg dir. Created in step 5 of `link` and removed in `unlink`. `Formula#linked_keg` returns the first of `possible_names.map { HOMEBREW_LINKED_KEGS/n }.find(&:directory?)` else `HOMEBREW_LINKED_KEGS/name`. `Keg.from_rack` chooses the active keg as: first `linked?`, else first `optlinked?`, else `max_by(&:scheme_and_version)` where `scheme_and_version = [version_scheme, version]`.

## 11. keg_only formulae (opt-only, not prefix)

`Formula#keg_only?` = has a `keg_only_reason` AND `keg_only_reason.applicable?`. `KegOnlyReason.applicable?` = `!by_macos?` on Linux/base; macOS override may differ. Reasons: symbols `:versioned_formula`, `:provided_by_macos`, `:shadowed_by_macos`, or an arbitrary String. `by_macos?` = provided_by_macos? || shadowed_by_macos?. `versioned_formula?` = reason == `:versioned_formula`.

Install/link behavior (FormulaInstaller#link, cmd/link.rb):
- During install, `link_keg = !formula.keg_only? || auto_link_versioned_keg_only?`. If link_keg is false → only `keg.optlink(...)` is called (creates `opt/<name>` + aliases, NO prefix symlinks). If link_keg true → full `keg.link(...)`.
- `auto_link_versioned_keg_only?`: true only when the formula is keg_only with reason `:versioned_formula` AND no related unversioned formula of the same name is also keg_only (i.e. versioned formulae auto-link unless an unversioned sibling owns the slot).
- `brew link` on a keg_only formula: refuses unless `--force`, EXCEPT versioned_formula reason which is allowed. Special-case: if `HOMEBREW_PREFIX == HOMEBREW_DEFAULT_PREFIX` and reason `by_macos?`, it refuses entirely ("Refusing to link macOS provided/shadowed software").
- After linking a keg_only formula, prints a PATH hint suggesting `opt/<name>/bin` (and `/sbin`) be prepended to PATH.

So: a keg_only formula has `opt/<name>` (+ aliases) symlinks but NO `bin/lib/include/share/etc` symlinks in the prefix — unless force-linked.

## 12. link_overwrite (formula.rb + unlink.rb + formula_installer.rb)

`link_overwrite "<glob>", ...` (formula DSL) populates a frozen `Set[String]` `link_overwrite_paths` (relative to prefix; supports literal `*` globs and `dir/` prefixes). Exposed in formula JSON as `"link_overwrite"` (array).

`Formula#link_overwrite?(path)` — true if linking should overwrite a conflicting `path`:
1. `keg_name = link_overwrite_keg_name(path)`:
   - If `path.stat.uid != HOMEBREW_ORIGINAL_BREW_FILE.stat.uid` → return nil (not Homebrew-owned, never overwrite).
   - `Keg.for(path)`; if its `tab.tap` is nil → return nil (DIY install). Else return the keg's `name`.
   - On `NotAKegError`/`ENOENT` → return `:missing` (file belongs to no keg).
2. If keg_name is a String: try `Formulary.factory(keg_name)`. If `FormulaUnavailableError` → defer to allowlist. If `TapFormulaAmbiguityError` → return false (belongs to another formula). Else if found formula's `possible_names` does NOT include keg_name → return false.
3. If `:missing` → defer to allowlist below.
4. Allowlist match: `to_check = path.relative_path_from(HOMEBREW_PREFIX)`; return true if any `p` in `link_overwrite_paths` satisfies: `p == to_check` OR `to_check.start_with?("#{p.chomp('/')}/")` OR regex `/^#{Regexp.escape(p).gsub('\*', ".*?")}$/` matches to_check (glob `*` → `.*?`).
5. Else: `implied_link_overwrite?(keg_name, link_overwrite_formulae)` — true if keg_name (non-missing) is a `possible_names` of any sibling overwrite formula.

`link_overwrite_formulae` / `_names`: BFS over the current formula's `link_overwrite_related_formula_names` = `[*versioned_formulae_names, *full_formulae_names, unversioned_formula_name]`, collecting the transitive family (sorted, uniq by full_name), excluding self.

`Unlink.unlink_link_overwrite_formulae(formula, verbose:)`: select related `link_overwrite_formulae` that are `linked?`; if the current formula is NOT keg_only, narrow to those that are keg_only; for each, find `any_installed_keg`, and `unlink` it. Called before `keg.link` during install and in `brew link`.

### Overwrite-with-backup flow (FormulaInstaller#link)
When `keg.link` raises `ConflictError`:
- If `formula.link_overwrite?(conflict_file)` and not already backed up: move `conflict_file` → `HOMEBREW_CACHE/Backup/<conflict_file relative to prefix>` (mkpath parent first), record in a hash, and `retry` the whole `keg.link`. Loop until no more overwritable conflicts.
- On non-overwritable ConflictError: fail, print possible conflicts via `keg.link(dry_run: true, overwrite: true)`.
- On any other exception during link: `keg.unlink`, then restore all backups (move back), re-raise.
- At the end, if backups were taken, warn that files were overwritten and backed up to `HOMEBREW_CACHE/Backup`.

`brew link --overwrite` (cmd/link.rb) passes `overwrite: true` straight into `keg.link`, where `make_relative_symlink` deletes any existing dst before linking.

## 13. Directory mkpath / chmod notes

- There is NO explicit chmod of created link-target directories inside keg.rb; `mkpath` uses default mode (umask). Directory creation happens via `dst.mkpath` (`:mkpath` strategy) and `dst.dirname.mkpath` (inside `make_relative_symlink`).
- `must_exist_directories` = `must_exist_subdirectories + [HOMEBREW_CELLAR]` (kept in sync with install.sh); `must_be_writable_directories` is a larger superset including specific share/man/zsh/pwsh completion dirs, etc/bash_completion.d, lib/{cps,pkgconfig}, var/log, plus CACHE/CELLAR/LOCKS/LOGS/REPOSITORY/python site-packages. These drive `brew doctor`-style permission checks, not linking itself.
- macOS `consistent_reproducible_symlink_permissions!`: `path.find { |f| f.lchmod 0777 if f.symlink? }` — makes symlink perms deterministic for reproducible bottles (no-op on Linux).

## 14. Complete HOMEBREW_PREFIX layout

```
HOMEBREW_PREFIX/
  Cellar/                 # (may live elsewhere; = HOMEBREW_CELLAR) racks: <name>/<version>/...
  Caskroom/               # casks (= HOMEBREW_CASKROOM)
  Frameworks/             # macOS only; .framework links (mkpath at Foo.framework + Versions)
  bin/                    # top-level keg bin files (subdirs NOT linked: :skip_dir)
  sbin/                   # top-level keg sbin files (:skip_dir)
  etc/                    # real dirs, files symlinked (:mkpath)
  include/                # symlinked, postgresql@N as mkpath
  lib/                    # mix of dir-symlinks (:link) and real dirs (:mkpath: cps,pkgconfig,cmake,perl5,ruby,python*,...)
  share/                  # man/info/locale/icons/zsh/... are real dirs; rest dir-symlinked
  var/
    homebrew/
      linked/             # HOMEBREW_LINKED_KEGS: <name> -> Cellar/<name>/<version>
      pinned/             # version-pinned kegs
      pinned_casks/
      locks/              # FormulaLock files
      tmp/.cellar
    log/
  opt/                    # <name> -> Cellar/<name>/<version> (stable); plus <alias> and oldname links
```

`opt/<name>`, `var/homebrew/linked`, and the seven/eight link dirs are the must-exist set never pruned on unlink.


## Rust implementation notes
Model the core as:

- `struct Keg { path: PathBuf, name: String, rack: PathBuf, linked_keg_record: PathBuf, opt_record: PathBuf, version: PkgVersion }`. Constructor must replicate the opt-resolve + Cellar-parent-parent validation (use `std::fs::canonicalize` for `realpath`). Beware: macOS `/usr/local` symlink chains and case-insensitive FS — canonicalize both sides before comparing.
- `enum LinkStrategy { Link, Mkpath, SkipDir, SkipFile, Info }` returned by a per-dir closure `Fn(&Path /* relative_to_root */) -> LinkStrategy`.
- Implement `link_dir` as a manual recursive walker, NOT `walkdir` with default recursion: you need Ruby `Find`'s pre-order semantics plus explicit `prune` (skip descending). `walkdir` supports this via `it.skip_current_dir()` on a depth-first iterator, or write your own recursion with an early-return-prune. Pre-order matters: parent decisions (`:link` => prune subtree) gate children.
- Symlinks: Ruby creates RELATIVE symlinks (`src.relative_path_from(dst.dirname)`). Use `pathdiff::diff_paths(src, dst.parent())` then `std::os::unix::fs::symlink`. Do NOT use absolute targets — Homebrew relies on relative links for relocatability.
- `resolved_path` is a ONE-LEVEL resolve relative to the link's own dir (`dirname.join(readlink)`), not full canonicalization. Implement as `let target = read_link(dst)?; if target.is_absolute() { target } else { dst.parent().join(target) }` WITHOUT canonicalizing — match Ruby exactly or `linked?`/`src == dst.resolved_path` comparisons will diverge.
- Counters: a thread-local or passed-in `struct LinkCounts { n: u32, d: u32 }` replacing the global `ObserverPathnameExtension`. `link`/`unlink` return `n`.
- Errors: an enum `LinkError { Conflict { src, dst }, DirNotWritable { src, dst }, AlreadyLinked, Other(io::Error) }`. Map `EEXIST`→Conflict (after re-checking `dst.exists()` to distinguish broken-symlink retry), `EACCES`→DirNotWritable, other `io::Error` from `symlink`→Other. `dst.exists()` in Rust follows symlinks (use `symlink_metadata` for lstat-style checks; distinguish `exists()` vs `symlink_metadata().is_ok()`). The retry-on-broken-symlink loop (`EEXIST` + `is_symlink()` => unlink + retry) is load-bearing.
- `Find.prune`-equivalent on `.app` dirs and `.DS_Store`, plus pyc/pyo-in-site-packages skip.
- GNU info: `install_info`/`uninstall_info` shell out to `install-info` (`which_install_info`) with `--quiet`. In Rust, locate `install-info` on PATH (or skip if absent) and run `install-info --quiet <file> <dir>/dir`.
- Conflict resolution `resolve_any_conflicts`: requires re-entrant `link_dir` on a DIFFERENT keg to explode its single dir-symlink into per-file symlinks. Factor `link_dir` to take an explicit keg/root so it can be invoked for a foreign keg.
- Glob matching for link_overwrite: replicate exactly — three branches: exact eq, `p.trim_end_matches('/') + "/"` prefix, and regex where literal `*` becomes `.*?` (anchored `^...$`) with everything else regex-escaped. Use the `regex` crate; escape with `regex::escape` then re-substitute `*`.
- Ownership check uses `stat.uid` compared to brew file's uid: `std::os::unix::fs::MetadataExt::uid()`.
- macOS-only: `Frameworks` link dir, `consistent_reproducible_symlink_permissions!` (`lchmod 0777` on symlinks — Rust has no stable `lchmod`; use `libc::lchmod` via FFI, or `fchmodat(AT_FDCWD, path, 0o777, AT_SYMLINK_NOFOLLOW)`), and codesigning of patched binaries (separate subsystem). Gate via `cfg(target_os)`.
- Atomicity: linking is NOT transactional in Ruby; rollback is best-effort (`unlink` on LinkError). Replicate the catch-unlink-rethrow rather than trying for true atomicity. The install-time backup-and-retry loop (move conflicts to `HOMEBREW_CACHE/Backup`, retry whole link, restore on failure) should be in the installer layer, not Keg::link.
- `rmdir_if_possible`: rmdir, on ENOTEMPTY check for lone `.DS_Store` and retry, swallow EACCES/ENOENT/EBUSY/EPERM. Reverse-order (deepest first) on the collected `dirs` minus must-exist set.
- Path constants: read from env (`HOMEBREW_PREFIX`, `HOMEBREW_CELLAR`, `HOMEBREW_CASKROOM`, `HOMEBREW_CACHE`, `HOMEBREW_LOGS`) at startup; derive `HOMEBREW_LINKED_KEGS = prefix/"var/homebrew/linked"`, `HOMEBREW_LOCKS = prefix/"var/homebrew/locks"`, etc. Do NOT assume Cellar is under prefix.

## Open questions
- tab.aliases / oldname_opt_records depend on the Tab (INSTALL_RECEIPT.json) subsystem and Formulary alias resolution, which are out of scope here; the Rust port needs those before optlink alias handling and remove_old_aliases can be fully implemented.
- Formulary.from_keg / Formulary.factory / possible_names (used by link_overwrite? and keg.to_formula) require the formula-loading subsystem; spec assumes those exist.
- macOS keg_only_reason override (extend/os/mac/keg_only_reason.rb) was not read — applicable?/by_macos? semantics on macOS for provided_by_macos/shadowed_by_macos may differ from the base (which treats macOS reasons as not-applicable on non-mac). Verify before implementing macOS keg_only linking refusal.
- FormulaLock (keg.lock) semantics (var/homebrew/locks flock files) are referenced but defined elsewhere; needed for safe concurrent link/unlink.
- consistent_reproducible_symlink_permissions! uses lchmod which is only invoked in bottle-building paths; confirm whether the Rust port needs it outside `brew bottle`.

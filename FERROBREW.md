# ferrobrew

A from-scratch **Rust reimplementation of Homebrew (`brew`)** that installs formulae the way real
brew does — from the JSON API and precompiled `ghcr.io` bottles.

> The cloned Homebrew Ruby under `Library/` (and `bin/brew`, `completions/`, `manpages/`, …) is
> **reference material only**. It is being replaced by the Rust workspace and will be removed as
> ferrobrew reaches parity. ferrobrew is standalone: it never shells back to Ruby.

## Layout

| Path | What |
| --- | --- |
| `crates/ferrobrew/` | The Rust crate (`ferrobrew` lib + binary). |
| `specs/` | Implementation-exact specs reverse-engineered from the Ruby, plus `architecture.md` (the porting blueprint). |
| `.github/workflows/ferrobrew-ci.yml` | Rust CI: `fmt`, `clippy -D warnings`, `build`, `test` on Linux + macOS. |
| `.github/workflows/ferrobrew-upstream-sync.yml` | The Claude-powered auto-port bot (below). |
| `Library/`, `bin/`, … | Upstream Homebrew Ruby — reference only, slated for removal. |

## Status

Working today (`cargo run -p ferrobrew -- <cmd>`):

- `ferrobrew install <formula>` — resolves runtime dependencies and install order, downloads the
  bottle from `ghcr.io`, verifies its sha256, extracts it into the Cellar, relocates it
  (`@@HOMEBREW_*@@` placeholders + Mach-O `install_name_tool`/ad-hoc codesign on macOS, ELF
  `patchelf` on Linux), links it into the prefix, and writes a byte-exact `INSTALL_RECEIPT.json`.
  Verified end-to-end into a throwaway sandbox prefix for `any_skip_relocation` and `:any`
  (relocated + codesigned) bottles — the installed binaries run.
- `ferrobrew uninstall <formula>` — unlinks every installed version and removes the keg + opt link.
- `ferrobrew list` — installed formula names.
- `ferrobrew info <formula>` — version, dependencies, and the selected bottle for the platform.
- `ferrobrew config` — the resolved `HOMEBREW_*` layout and current bottle tag.

Unit-tested subsystems (126 tests): config/path derivation, platform/bottle-tag detection, JSON API
client, formula/bottle model, dependency resolution & ordering, bottle download + sha256 + extract,
relocation (text + Mach-O/ELF), keg linking, and `INSTALL_RECEIPT.json`.

Known limitation: a bottle built for a concrete cellar (e.g. `/opt/homebrew/Cellar`) installs only
to a matching prefix; cross-prefix relocation of concrete-path (non-placeholder) bottles is not yet
implemented (it errors clearly rather than shipping a broken keg). Source builds, casks, services,
and the remaining commands (`upgrade`, `outdated`, `search`, …) are future work — see
`specs/architecture.md`.

## Build & test

```sh
cargo build            # build the workspace
cargo test             # run the unit tests
cargo run -p ferrobrew -- config
cargo run -p ferrobrew -- info wget
```

## Upstream-sync bot (Claude)

`.github/workflows/ferrobrew-upstream-sync.yml` keeps the port tracking upstream Homebrew. Daily
(and on manual dispatch) it:

1. Fetches `Homebrew/brew` and diffs the Ruby under `Library/` since the last synced commit
   (tracked in `.ferrobrew/sync-state.json`).
2. If there are changes, runs **`anthropics/claude-code-action@v1`** with a prompt to port the
   changes relevant to already-ported subsystems into the Rust, keep CI green, advance the sync
   marker, and **open a PR for human review** (it does not merge).

**To enable:** add an `ANTHROPIC_API_KEY` repository secret
(Settings → Secrets and variables → Actions). Until then the workflow is inert. The bot becomes
most valuable once the core install pipeline is in place — it is designed to maintain parity, not
to build the port from scratch.

Notes: PRs are review-gated by design. Branches pushed with the default `GITHUB_TOKEN` do not
re-trigger `ferrobrew-ci.yml`; re-run CI on the bot's PR manually, or supply a PAT, if you want
automatic checks on its PRs.

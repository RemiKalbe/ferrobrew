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

- `ferrobrew config` — derives the `HOMEBREW_*` layout for the host (verified against a real
  `/opt/homebrew` install) and reports the current bottle tag (e.g. `arm64_tahoe`).
- `ferrobrew info <formula>` — fetches a formula from the live JSON API and prints its version,
  dependencies, and the selected bottle URL + sha256 for the current platform.

Implemented & unit-tested subsystems: configuration/path derivation, platform & bottle-tag
detection, the formula/bottle JSON model, and a minimal API client.

Next, toward `ferrobrew install <formula>` (see `specs/architecture.md` for the full milestone
list): bottle download from `ghcr.io` (OCI blob + bearer auth) → sha256 verify → extract →
relocate `@@HOMEBREW_*@@` placeholders (Mach-O + codesign on macOS, ELF/patchelf on Linux) → keg
linking into the prefix → `INSTALL_RECEIPT.json` → dependency resolution & ordering.

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

# AGENTS.md — Repo hygiene

Scope: this file covers *repo hygiene* — branching, remotes, CI — plus a
short map of the binaries. It is not user-facing project documentation.

This repo follows the branch/release workflow documented in `CONTRIBUTING.md`
— read and follow it for any git, branch, or release work here (the
single-trunk model, `feature/x`/`fix/x` branch naming, how RC tags work,
etc). Don't improvise a different workflow. The short version: there is one
long-lived branch, `main` — no `dev` or `beta` branch exists. `main`
auto-publishes a dev-track build on every push. "Beta" and "stable" are both
just tags, not branches: push a `vX.Y.Z-rc.N` tag to publish a beta-track
build, push a plain `vX.Y.Z` tag to cut the signed stable release.
"Freezing" for stabilization means pausing pushes to `main`, not moving a
branch. This replaced an earlier three-branch (`dev`/`beta`/`main`) model
after `main` was found to have silently rotted out of sync with `dev`/`beta`
across most repos in this ecosystem.

When starting work on a new feature, create branch `feature/<feature-name>`.
When working on a bug or issue, create branch `fix/<issue you are fixing>`.

## Remotes
- `origin` — Forgejo (`git.breadway.dev` via Hestia, SSH) — authoritative.
- `github` — GitHub mirror. Push `origin` only; GitHub auto-mirrors.

## CI
- `check.yml` — clippy + test, triggers on push to `feature/**`/`fix/**`.
- `dev-release.yml` — triggers on push to `main`.
- `rc-release.yml` — triggers on `vX.Y.Z-rc.N` tag push.
- `release.yml` — triggers on any other `v*` tag push.

All four run on a self-hosted runner (`hestia`) inside a pinned Arch
container — not the host's native environment. The Containerfile/build
script are shared across bread-ecosystem products and live in
`bread-ecosystem/ci/`; this repo's `ci/build.sh` clones that repo at the
sha in `ci/bread-ecosystem.rev` (deliberately pinned, not `main`) and
delegates to it. Nothing runs automatically on plain commits or PRs
beyond what's listed.

## Architecture

Three crates:

| Crate | Role |
|---|---|
| `breadclipd` | Clipboard-watch daemon; persists history to SQLite |
| `breadclip` | GTK4 Layer Shell popup (thin UI over the same DB) |
| `breadclip-core` | Shared history schema / DB access |

`--screenshot` (`breadclip/src/screenshot.rs`) captures the history panel
through `bread-screenshots`; do not rewrite it just to retarget the crate pin.

`EVENTS.md` is the bread-event contract. App id `clip`. Implemented:
`bread.clip.copied`, `bread.clip.clear.done`/`.failed`, and command
`bread.command.clip.clear`. There is no pin/select — the history schema has
no pinned column and the popup has no pin UI. Do not invent
`bread.command.clip.pin`/`.select` (or matching events) ahead of a real
product feature.

## Don't
- Don't embed credentials in remote URLs — SSH or a credential helper only.
- Don't invent pin/select on the event bus. See `EVENTS.md`.

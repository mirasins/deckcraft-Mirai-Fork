# Mirai fork release channel

The Mirai fork ships its own releases, separate from upstream `storytold/deckcraft`.

## Versioning

- The workspace version in the root `Cargo.toml` is the single source of truth
  (`cargo xtask version` reads and validates it).
- Fork versions keep upstream's `MAJOR.MINOR.PATCH` and add a `-mirai.N` pre-release tag:
  `0.12.0-mirai.1`, `0.12.0-mirai.2`, … Bump `N` for each fork release; bump the core triple
  only when upstream's version moves (after a merge).
- The tag `v<version>` (e.g. `v0.12.0-mirai.1`) is created by the release workflow.

## Update checks

`crates/ui-egui/src/lib.rs` defines `UPDATE_APP` with the repository `mirasins/deckcraft`.
The in-app updater and the self-update helper both use it, so update checks and downloads
only ever look at the fork's GitHub releases — never at upstream's. `crates/engine/src/links.rs`
(About, Backstage, MCP instructions) points at the fork too.

## Cutting a release

1. Land the work on `main` with `cargo xtask ci` green.
2. Push the commit to the `release` branch of `mirasins/deckcraft`
   (`.github/workflows/release.yml` builds and publishes on push to `release`), or run the
   workflow manually from GitHub Actions with an optional version override.
3. The reusable workflow (`storytold/craft-common-actions`) builds desktop packages, the
   web build and the release assets, and publishes them to the fork's GitHub releases.
   Signing secrets (Apple, Windows/Azure) are optional: without them the macOS and Windows
   builds are unsigned. Add the secrets to the fork's repository settings to sign releases.

## Install identity

`craft-update` derives the bundle id from the app id (`ai.storyteller.deckcraft`), shared with
upstream. A machine with upstream DeckCraft installed and the fork side by side shares that
bundle id for menus and update prompts; the update channel itself is fork-only.

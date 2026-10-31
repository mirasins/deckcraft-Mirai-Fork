# Mirai fork release channel

The Mirai fork ships its own releases, separate from upstream `storytold/deckcraft`.

## Versioning

- The workspace version in the root `Cargo.toml` is the single source of truth
  (`cargo xtask version` reads and validates it).
- Fork versions are `MAJOR.MINOR.PATCH-mirai.N`. **Every fork release raises `PATCH` (or more)**:
  `0.12.0-mirai.1`, then `0.12.1-mirai.1`, … `craft-update` treats a `-mirai` build as a local build and
  offers it only a strictly higher `MAJOR.MINOR.PATCH`, so a release that only bumps `N`
  (`0.12.0-mirai.2`) is never offered to installed copies. Windows' MSI `ProductVersion` keeps
  only the triple as well. `N` stays `1` unless the same triple must be rebuilt.
- The tag `v<version>` (e.g. `v0.12.1-mirai.1`) names the GitHub release.

## Update checks

`crates/ui-egui/src/lib.rs` defines `UPDATE_APP` with the repository `mirasins/deckcraft`.
The in-app updater and the self-update helper both use it, so update checks and downloads
only ever look at the fork's GitHub releases — never at upstream's. `crates/engine/src/links.rs`
(About, Backstage, MCP instructions) points at the fork too.

## Cutting a release

Fork releases are built locally; GitHub Actions is not used for them.

1. Land the work on `main` with `cargo xtask ci` green, raising the version as above.
2. Build the Windows packages from the repository root:
   `pwsh -File packaging/windows/package.ps1 -Arch x64` (MSI and portable zip in `dist/release`).
3. Publish them as the release `v<version>` on the fork, marked latest, targeting `main`:
   `gh release create v<version> dist/release/* --target main --latest`.
   GitHub publishes a SHA-256 digest for every asset; the updater refuses a download without one.
4. Packages are unsigned: Windows SmartScreen asks testers to confirm ("More info", "Run anyway").

## Install identity

`craft-update` derives the bundle id from the app id (`ai.storyteller.deckcraft`), shared with
upstream. A machine with upstream DeckCraft installed and the fork side by side shares that
bundle id for menus and update prompts; the update channel itself is fork-only.

# Mirai Assistant for DeckCraft

> **Status (2026-10-24):** backend and proposal CLI checked offline on Windows; all eight CI
> steps passed with `WGPU_BACKEND=gl`. Not an in-app assistant yet.

Mirai Assistant uses the official Claude Code CLI with the user's supported account sign-in.
No API-key, alternate-provider or private-service fallback is implemented. The app does not read,
copy or store account credentials. Authentication checks do not send presentation text.

## Current scope

- `assistant.context` reads text-bearing shapes on the current slide (restricted to the selection
  when shapes are selected), up to 256 targets and 64 KiB. It neither edits nor calls Claude.
- `assistant.apply` validates the document identity, global revision and original shape text,
  then applies indexed replacements as one undoable action, respecting locked shapes.
- Text replacement uses the existing `text.set` semantics: it retains supported paragraph and
  first-run formatting, not arbitrary mixed formatting across rewritten text.
- The standalone `deckcraft-assistant` crate handles bounded Claude Code processes, cancellation,
  model selection and structured output. Shape text is untrusted data; Claude tools are disabled.

The first CLI integration is a **proposal preview**. It never automatically applies or saves model
output. Explicit generation sends the chosen slide's text to the user's Claude account; avoid
confidential slide content unless you intend that transfer. Snapshots belong to the live session
that created them and cannot be reapplied after reopening the file in a separate process.

## Shared appearance requirement

The owner requested the same purple/lavender **Mirai Assistant** appearance across WordCraft,
GridCraft and DeckCraft (2026-10-24). Use WordCraft's current assistant as the visual reference:
lavender surfaces, purple actions, conversational message layout, a roomy multiline composer
and a dropdown model selector, with matching light and dark variants. Keep the host application's
own identity and theme intact. Coordinate with the separate WordCraft visual-design work;
do not overwrite it. This requirement is pending UI implementation in this fork.

## CLI preview

```text
cargo run -p deckcraft-cli -- mirai --in slides.pptx --slide 1 --instruction "Make the wording more concise" --model sonnet
```

`--slide` is optional and one-based. Output is JSON with the original snapshot, indexed text
replacements and an explanation. This preview neither edits the presentation nor saves a file.

## Still pending

The desktop conversation panel, visual design, accept/reject workflow and real-account end-to-end
testing have not been adapted. This slice does not generate slide layouts, images, speaker notes,
tables or charts, and does not recursively edit grouped shapes. The app remains DeckCraft;
**Mirai Assistant** names the assistant.

## Validation

Offline checks on Windows: 31 client tests, 8 engine tests and 6 CLI tests pass. All eight
`cargo xtask ci` steps passed with `WGPU_BACKEND=gl`: formatting, PDF dependencies, Clippy,
workspace tests, doctests, assets, layers and WASM (24 crates). No tests were disabled for this run.

The initial run with the default Windows GPU backend terminated the UI test process with
`0xc0000005` (`STATUS_ACCESS_VIOLATION`). No UI code or GPU dependencies changed for this
adaptation. The same UI test binary passes serially (277 passed, 4 already ignored), and passes
in parallel with OpenGL. Repeated module runs implicate native parallel wgpu rendering.
That test-environment limitation is **not fixed**; production graphics settings are unchanged.
No live Claude request has been made for this adaptation. Builds warn when the optional
`craft-fonts` checkout is absent; bundled-font rendering has not been assessed here.

## Revision history

- 2026-10-24: backend/CLI slice checked with all eight CI gates using OpenGL on Windows;
  document the native parallel-test crash, pending lavender UI and account-only policy.

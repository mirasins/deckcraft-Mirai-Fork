# Mirai Assistant

> **Last reviewed:** 2026-10-30 · **Last updated:** 2026-10-30 · **Change:** minor (shape geometry, fills and lines in slide proposals) · **Validation:** offline tests; no live-account request.

Mirai is a task pane in **Review › Mirai Assistant**. It proposes plain-text replacements in selected
text shapes, in the current slide, or across the presentation. It can also propose a short list of
slide operations (below). It does not generate full decks, pictures, charts, advanced formatting
or media. Presentation-wide scope edits captured text on any slide; layout operations remain limited
to the original slide and newly created slides. It does not run commands produced by the model.

## Use

1. The current-slide scope is selected by default; choose selection or entire presentation instead.
   Entire presentation sends eligible text from up to 256 slides, at most 256 shapes and 64 KiB.
2. Open **Review › Mirai Assistant**, write an instruction, confirm that specific request and Send.
   Confirmation is per request, not a persistent blanket permission.
3. Compare each original/proposed text and the explanation. The latest proposal also appears in an
   editable card beside its first text shape on the slide; edit its drafts there or dismiss it.
   Apply all changes as one undoable operation, or dismiss the proposal. A change to the document's
   content, to the current slide or to a target's text invalidates the proposal; read a new context
   rather than applying stale changes. Moving the selection or navigating slides and back does not.
4. Cancel or New Conversation stops the request. Changing presentations clears the panel's local
   state. Changing the selection clears consent. Changing slides clears consent and cancels an
   in-flight request; text already sent cannot be retracted. Proposals stay bound to the slide where
   they were created.

The engine commands `assistant.context`, `assistant.apply`, `assistant.presentationContext`,
`assistant.applyPresentation`, `assistant.geometryContext` and `assistant.applyDeck` own the snapshot
and validation.

## Slide operations

A proposal can also carry a **deck** list, together with or instead of text suggestions. Text
suggestions replace their shapes first; then the operations run in order. The card lists each
operation before you apply it, and a mixed answer applies as one step:

| Operation | What it does |
|---|---|
| New slide | Adds a slide with layout Blank, Title or Title and Content, optionally with a title |
| Add text box | Adds a text box at a rectangle, with plain text and an optional style (bold, italic, size in points, `#RRGGBB` colour) |
| Set text | Replaces the text of a captured text shape on the original slide |
| Set position and size | Moves and resizes a captured text shape on the original slide |
| Background | Sets a solid `#RRGGBB` background |

Rules:

- A proposal holds **1 to 100 operations** (an empty operation list is rejected) and its text is
  limited to **64 KiB** in total. Larger requests are rejected with a message to split them.
- A style is accepted only on an added text box, sets at least one of bold, italic, size and colour,
  the size is 1 to 400 pt and the colour is `#RRGGBB`. Existing shapes keep their formatting.
- Coordinates and sizes are points from the slide's top-left corner, between 0 and 10000, with
  positive width and height.
- Set text and set position may address only text shapes captured in the original slide's context,
  and they must come before any new slide. Operations after a new slide apply to that new slide.
- Only these five operations are accepted from a model answer. Model output is never executed, and it is never treated
  as Markdown or as commands.
- The whole list applies as **one undoable step**. Undo restores the slide and its shapes as they
  were before the apply.
- A proposal made from an older document content revision or a different current slide is
  **rejected as stale** before any change; read a new context and ask again. A text suggestion and a
  Set text operation may not address the same shape: the whole answer is then refused.

Masters, layouts, notes, handouts, groups, tables, pictures, charts, fields, line breaks and other
non-plain content are outside this version; explicit current-slide scope reports skipped shapes.
Plain text keeps the standard text-set paragraph/first-run formatting and fit behavior.
Programmatic requests require `confirm: true`.

### Shape geometry and fills (engine, first step)

The engine can read and change unlocked, text-free autoshapes on the current slide. For a
slide-scope request the pane sends this context to the model as `geometry`, so the model may
propose `set_shape_bounds`, `set_shape_fill` and `add_shape` alongside text edits; for selection
or presentation scope no geometry is sent. The same operations are always available through the
engine (MCP, CLI and the control channel).

- `assistant.geometryContext` is read-only. It lists up to 256 shapes with `index`, `shape`, `label`,
  `preset`, `rect` (`[x, y, w, h]` in points) and `fill` (`#RRGGBB` for a plain solid fill, otherwise
  `null`). Closed shapes, lines, arcs and connector presets are listed. Pictures, groups, attached
  connectors, tables, charts, media, placeholders, text boxes, shapes with text, locked shapes,
  presets whose name isn't plain letters and digits, and shapes past the 256th are not listed;
  `skipped` counts them. Labels have control characters and line or paragraph separators replaced
  by spaces. Its indices are a separate space from
  the text targets of `assistant.context`.
- Pass the snapshot as `geometry` next to `context` and `deck`. The operations are:
  - `set_shape_bounds {shape, rect}`: moves and resizes a listed shape;
  - `set_shape_fill {shape, color}`: sets a plain `#RRGGBB` fill; on a line, arc or connector preset
    it sets the line colour instead;
  - `add_shape {preset, rect, color?}`: creates a shape from the gallery, with an optional fill (the
    line colour for a line, arc or connector). It needs no geometry snapshot. The created shape is
    left unselected.
  - A line's box runs from its start to its end and may be flat (zero width or height, not both);
    every other shape needs a positive width and height.
- In the pane, a slide-scope request whose geometry can't be read still runs, without shape
  operations; an answer whose `add_shape` names a preset outside the gallery, or a flat closed shape,
  is not offered. The pane sends textual geometry (labels, presets, rectangles, plain fills) only:
  no slide image or screenshot is sent, and Claude Code never receives one. Changing the slide
  after a proposal cancels it, so a proposal is only applied with the user's explicit consent.
- As above, everything is validated before the first change; a snapshot that is stale, names a
  locked or changed shape, or repeats one is refused; the whole list is one undoable step; and a shape
  operation after a new slide is refused.

## Account and privacy

The native client runs only the **official Claude Code CLI** with the user's own signed-in account
(`claude auth login`, or its official OAuth-token mechanism). There is no API-key fallback, other
provider, HTTP server or token extraction. Child processes lose credential overrides; no credentials
are read by the app or logged. Tools and MCP servers are disabled, sessions are not persisted and
input, output, execution time and process lifetime are bounded. Browser builds report unavailable.

**Consent before sending.** Selected slide text, or the current slide's text when that scope is
chosen, and the instruction are sent only after you confirm the request. Enter in the instruction
field reviews the request (Shift+Enter adds a line); a second Enter, or Confirm and send, sends it.
Send and Confirm and send work the same way with the button. Any change to the document, slide,
selection, scope, instruction or model cancels the review. Enter is ignored while an IME composition
is in progress. Context size is bounded by the limits above.

Treat model output as untrusted data until you review it. Proposals remain in memory. No live-account
check was run for this port; tests use local fixtures, including headless panel screenshots without a
provider call. Nothing in this document is a claim about the real behaviour of Claude.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-30 | minor | Engine, protocol and pane: `assistant.geometryContext` lists unlocked text-free autoshapes (lines, arcs and connector presets included) with their own indices; `assistant.applyDeck` accepts `set_shape_bounds`, `set_shape_fill` and `add_shape`, validated before any change and undoable in one step; a line's colour is its stroke and its box may be flat. Offline tests only. |
| 2026-10-29 | minor | An add_text operation can carry a style (bold, italic, size 1 to 400 pt, #RRGGBB colour) applied to the whole text box; styles are validated at the protocol and in the engine, listed on the card, and undo stays one step. Existing shapes keep their formatting; no live-account request. |
| 2026-10-29 | minor | Fixed proposals that were rejected as stale after a selection change or slide navigation: the snapshot revision is now a content revision (selection and navigation don't bump it). Mixed answers (text suggestions and slide operations) are accepted and apply atomically in one undo step; a suggestion and a set-text on the same shape are refused. Presentation-wide snapshots record their slide and apply only from it. Offline tests only. |
| 2026-10-29 | minor | Added presentation-wide text context and atomic text replacements across slides; text-only, excludes unsupported objects and is bounded to 256 slides/shapes and 64 KiB. Offline tests only. |
| 2026-10-29 | minor | Added the original Mirai logo and two-step Enter confirmation, bound to the document revision; no live-account or real-IME check. |
| 2026-10-29 | minor | Slide operations in proposals (new Blank, Title or Title and Content slides, text boxes, solid background, text and position/size of captured text shapes), listed on the card in readable lines, applied in one undo step with undo and redo; limits of 1 to 100 operations and 64 KiB; an empty operation list, stale proposals and mixed text/slide answers are rejected. Review pass: engine, protocol and UI tests cover several slides with distributed content, an empty starting slide, edits to an existing slide, rejection without partial changes, and atomic undo/redo; the card’s operation lines are asserted by a UI test and a headless screenshot was generated. No live-account request. |
| 2026-10-28 | minor | Editable contextual proposal card on the slide, undoable apply and slide-switch consent reset. |
| 2026-10-26 | minor | First selected/current-slide Mirai pane, standalone official-Claude transport, revision-checked text apply and offline tests. |

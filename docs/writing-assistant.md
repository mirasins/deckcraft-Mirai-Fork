# Mirai Assistant

> **Last reviewed:** 2026-10-29 · **Last updated:** 2026-10-29 · **Change:** minor (proposals can create slides, add text boxes, set a solid background and move or resize captured text shapes; preview and undoable apply) · **Validation:** offline tests; no live-account request.

Mirai is a task pane in **Review › Mirai Assistant**. It proposes plain-text replacements in selected
text shapes or in the current slide's eligible text shapes. It can also propose a short list of
slide operations (below). It does not generate full decks, pictures, charts, advanced formatting
or media, does not edit slides other than the one it was created from, and does not run commands
produced by the model.

## Use

1. The current-slide scope is selected by default; choose selection to send only selected text shapes.
   At most 256 shapes and 64 KiB of source text are accepted.
2. Open **Review › Mirai Assistant**, write an instruction, confirm that specific request and Send.
   Confirmation is per request, not a persistent blanket permission.
3. Compare each original/proposed text and the explanation. The latest proposal also appears in an
   editable card beside its first text shape on the slide; edit its drafts there or dismiss it.
   Apply all changes as one undoable operation, or dismiss the proposal. A changed document revision,
   slide or target text invalidates the proposal; read a new context rather than applying stale changes.
4. Cancel or New Conversation stops the request. Changing presentations clears the panel's local
   state. Changing the selection clears consent. Changing slides clears consent and cancels an
   in-flight request; text already sent cannot be retracted. Proposals stay bound to the slide where
   they were created.

The engine commands `assistant.context`, `assistant.apply` and `assistant.applyDeck` own the snapshot
and validation.

## Slide operations

A proposal can instead carry a **deck** list (never mixed with text suggestions in the same answer).
Operations run in order, and the card lists each one before you apply it:

| Operation | What it does |
|---|---|
| New slide | Adds a slide with layout Blank, Title or Title and Content, optionally with a title |
| Add text box | Adds a text box at a rectangle, with plain text |
| Set text | Replaces the text of a captured text shape on the original slide |
| Set position and size | Moves and resizes a captured text shape on the original slide |
| Background | Sets a solid `#RRGGBB` background |

Rules:

- A proposal holds **1 to 100 operations** (an empty operation list is rejected) and its text is
  limited to **64 KiB** in total. Larger requests are rejected with a message to split them.
- Coordinates and sizes are points from the slide's top-left corner, between 0 and 10000, with
  positive width and height.
- Set text and set position may address only text shapes captured in the original slide's context,
  and they must come before any new slide. Operations after a new slide apply to that new slide.
- Only these five operations are accepted. Model output is never executed, and it is never treated
  as Markdown or as commands.
- The whole list applies as **one undoable step**. Undo restores the slide and its shapes as they
  were before the apply.
- A proposal made from an older document revision or a different slide is **rejected as stale**
  before any change; read a new context and ask again.

Masters, layouts, notes, handouts, groups, tables, pictures, charts, fields, line breaks and other
non-plain content are outside this version; explicit current-slide scope reports skipped shapes.
Plain text keeps the standard text-set paragraph/first-run formatting and fit behavior.
Programmatic requests require `confirm: true`.

## Account and privacy

The native client runs only the **official Claude Code CLI** with the user's own signed-in account
(`claude auth login`, or its official OAuth-token mechanism). There is no API-key fallback, other
provider, HTTP server or token extraction. Child processes lose credential overrides; no credentials
are read by the app or logged. Tools and MCP servers are disabled, sessions are not persisted and
input, output, execution time and process lifetime are bounded. Browser builds report unavailable.

**Consent before sending.** Selected slide text, or the current slide's text when that scope is
chosen, and the instruction are sent only after you confirm the request. Context size is bounded
by the limits above.

Treat model output as untrusted data until you review it. Proposals remain in memory. No live-account
check was run for this port; tests use local fixtures, including headless panel screenshots without a
provider call. Nothing in this document is a claim about the real behaviour of Claude.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-29 | minor | Slide operations in proposals (new Blank, Title or Title and Content slides, text boxes, solid background, text and position/size of captured text shapes), listed on the card in readable lines, applied in one undo step with undo and redo; limits of 1 to 100 operations and 64 KiB; an empty operation list, stale proposals and mixed text/slide answers are rejected. Review pass: engine, protocol and UI tests cover several slides with distributed content, an empty starting slide, edits to an existing slide, rejection without partial changes, and atomic undo/redo; the card’s operation lines are asserted by a UI test and a headless screenshot was generated. No live-account request. |
| 2026-10-28 | minor | Editable contextual proposal card on the slide, undoable apply and slide-switch consent reset. |
| 2026-10-26 | minor | First selected/current-slide Mirai pane, standalone official-Claude transport, revision-checked text apply and offline tests. |

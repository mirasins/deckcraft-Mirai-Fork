# Mirai Assistant

> **Last reviewed:** 2026-10-28 · **Last updated:** 2026-10-28 · **Change:** minor (editable on-slide review) · **Validation:** offline tests; no live-account request.

Mirai is a task pane in **Review › Mirai Assistant**. It proposes plain-text replacements in selected
text shapes or in the current slide's eligible text shapes. It does not generate decks, layouts,
animations or media, or run commands produced by the model.

## Use

1. Select text shapes, or explicitly choose the current-slide scope. At most 256 shapes and 64 KiB
   of source text are accepted.
2. Open **Review › Mirai Assistant**, write an instruction, confirm that specific request and Send.
   Confirmation is per request, not a persistent blanket permission.
3. Compare each original/proposed text and the explanation. The latest proposal also appears in an
   editable card beside its first text shape on the slide; edit its drafts there or dismiss it.
   Apply all changes as one undoable
   operation, or dismiss the proposal. A changed document revision, slide or target text invalidates
   the proposal; read a new context rather than applying stale changes.
4. Cancel or New Conversation stops the request. Changing presentations clears the panel's local
   state. Changing the selection clears consent. Changing slides clears consent and cancels an in-flight request; text already sent cannot
   be retracted. Proposals stay bound to the slide where they were created.

The engine commands `assistant.context` and `assistant.apply` own the snapshot and validation.
Masters, layouts, notes, handouts, groups, tables, pictures, charts, fields, line breaks and other
non-plain content are outside this version; explicit current-slide scope reports skipped shapes.
Plain text keeps the standard text-set paragraph/first-run formatting and fit behavior. Model output
is never executed or treated as Markdown commands. Programmatic requests require `confirm: true`.

## Account and privacy

The native client runs only the **official Claude Code CLI** with the user's own signed-in account
(`claude auth login`, or its official OAuth-token mechanism). There is no API-key fallback, other
provider, HTTP server or token extraction. Child processes lose credential overrides; no credentials
are read by the app or logged. Tools and MCP servers are disabled, sessions are not persisted and
input, output, execution time and process lifetime are bounded. Browser builds report unavailable.

Selected slide text is sent only after confirmation. Treat model output as untrusted data until you
review it. Proposals remain in memory. No live-account check was run for this port; tests use local
fixtures, including headless panel screenshots without a provider call.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-28 | minor | Editable contextual proposal card on the slide, undoable apply and slide-switch consent reset. |
| 2026-10-26 | minor | First selected/current-slide Mirai pane, standalone official-Claude transport, revision-checked text apply and offline tests. |

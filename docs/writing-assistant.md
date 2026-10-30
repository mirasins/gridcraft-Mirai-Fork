# Mirai Assistant

> **Last reviewed:** 2026-10-29 · **Last updated:** 2026-10-30 · **Change:** minor (fixed the truncated output schema that made every live request fail as invalid; one synthetic formula request verified end to end with the signed-in account) · **Validation:** offline transport, engine and panel tests; one live synthetic formula request.

Mirai is a right-hand panel in **Review › Mirai Assistant**. It proposes three kinds of changes:
literal text replacements for selected ordinary cells, spreadsheet changes (cell inputs at absolute
addresses, plain values or formulas starting with `=`), and typed sheet-design operations (cell styles,
column widths or AutoFit, row heights, and Excel tables), plus an explicitly requested new sheet.
It does not analyze a whole workbook, create charts, or execute commands produced by the model. For formula
work, an optional bounded window of cells around the selection (default 15, at most 60 rows and columns away, at
most 32 KiB of text, formulas as typed) can be sent too, after the same confirmation.

## Use

1. Select at most 256 cells (64 KiB of source text at most). Only this selection is sent to Claude.
2. Open **Review › Mirai Assistant**.
3. Write an instruction. Enter or Send shows a notice of what is sent and where changes may apply;
   Enter or Send again confirms. Shift+Enter inserts a line. Confirmation is required for every
   request and is never persisted as a blanket permission. A checkbox sends the cells around the
   selection too, for formula work; the notice says so when it is on.
4. Review the proposal on the sheet: the proposed cell inputs are painted over their cells, and
   an “Apply?” banner sits over the grid. Enter keeps the changes; Esc cancels them; the banner has
   Apply and Cancel buttons too. A proposal may change cells outside the selection; the banner says
   so. The chat keeps only a short summary and the explanation. Dismissed proposals change nothing at
   all; applied changes are one Undo step (Ctrl+Z).
5. A proposal is rejected when the workbook revision or target text has changed since the context was
   taken; ask again for a new proposal.
6. Cancel or New Conversation stops the request. Changing workbooks clears the panel's local state.

The engine commands `assistant.context`, `assistant.apply` (text), `assistant.applyGrid` and `assistant.formulaContext`
(spreadsheet and design changes) implement the bounded snapshot and validated apply. Text output is always
literal: leading `=`, `+`, `-` and `@` are refused, and replacing a numeric/date cell changes it to
text, as the preview indicates. Spreadsheet proposals are limited to 256 cells and 64 KiB of addresses
and inputs; addresses must be absolute A1 (for example `B12`), duplicates are refused, and a new sheet
name is checked before anything changes. Design proposals are limited to 64 operations: cell-style patches,
column widths or AutoFit, row heights, and at most one table per proposal; style patches are validated
against the active cell style. Selected formula, merged, picture and spilled cells and
protected sheets are refused as context. Grid changes can write formulas but refuse merged, picture
and spilled cells. Programmatic requests still require `confirm: true`; no cloud
request happens on opening the panel.

## Account and privacy

The native client runs only the **official Claude Code CLI** with the user's own signed-in account
(`claude auth login`, or its official OAuth-token mechanism). Install/sign in to Claude Code if
necessary. No API-key fallback, alternate provider, HTTP server or credential copying exists.
Credential overrides are removed from the child process; credentials are never logged.
Requests run without tools or MCP servers, with no session persistence, bounded input/output and
timeouts, cancellation and process reaping. The browser build reports the assistant unavailable.

Only the selected cell text is sent, and only after confirmation; cells outside the selection are not
sent. Treat model output as untrusted data until you review it. Local proposal history is in memory only. One live check ran with the signed-in account: a synthetic formula
request through the real Claude CLI. It found and fixed a truncated output schema (one missing
closing brace since the design-proposal change) that made every live request fail as invalid; the
returned proposal now parses. The full pane flow with a real account is still to be exercised.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-30 | minor | Fixed the truncated output schema (one missing closing brace since the design-proposal change) that made every live Claude Code request fail as an invalid response. Schema-validity regression test, a captured real-envelope fixture, and one synthetic formula request verified end to end with the signed-in account. |
| 2026-10-29 | minor | On-sheet proposal preview: proposed cell inputs painted over the sheet (an isolated overlay; nothing is edited until applied), an “Apply?” banner, Enter applies and Esc cancels, and the chat keeps only a short summary. Stale proposals hide; cancelling never touches the workbook or the undo history. Offline tests only. |
| 2026-10-29 | minor | Formula work: an optional bounded window of cells around the selection (default 15, at most 60 rows and columns away, 32 KiB, formulas as typed) is sent after the same confirmation; new `assistant.formulaContext` engine command, pane checkbox and notice line. Fixed contexts over selected numeric cells failing every apply as changed. Offline tests only. |
| 2026-10-29 | minor | Added the original Mirai logo, two-step Enter confirmation, and atomic mixed text and cell proposals; no live-account or real-IME check. |
| 2026-10-29 | minor | Spreadsheet change proposals (cell inputs, explicit new sheet), previewed and applied as one undo step; 256-cell limit; stale proposals rejected; overwrite warning. |
| 2026-10-29 | minor | Added typed sheet-design proposals (cell styles, column widths/AutoFit, row heights, tables) to `assistant.applyGrid`/`assistant.applyDesign`, with preview, validation, atomicity and undo. |
| 2026-10-26 | minor | First selected-cell Mirai panel, standalone official-Claude transport, revision-checked literal apply and offline tests. |

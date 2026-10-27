# Mirai Assistant

> **Last reviewed:** 2026-10-26 · **Last updated:** 2026-10-26 · **Change:** minor (first text-assistance panel) · **Validation:** offline transport, engine and panel tests; no live-account request.

Mirai is a right-hand panel in **Review › Mirai Assistant**. It proposes literal text replacements for
selected ordinary cells. It does not create formulas, workbooks or charts, analyze a whole workbook,
or execute commands produced by the model.

## Use

1. Select at most 256 ordinary cells (64 KiB of source text at most).
2. Open **Review › Mirai Assistant**.
3. Write an instruction, confirm sending that request and choose Send. Confirmation is required for
   every request and is never persisted as a blanket permission.
4. Compare the original and proposed text. Dismiss it or apply all changes with one Undo step.
   A changed workbook revision or target text invalidates the proposal; ask for a new context.
5. Cancel or New Conversation stops the request. Changing workbooks clears the panel's local state.

The engine commands `assistant.context` and `assistant.apply` implement the bounded snapshot and
validated apply. Formula, merged, table, picture, spilled and protected-sheet cells are refused,
not silently skipped. Output is always literal text: leading `=`, `+`, `-` and `@` are refused.
Replacing a numeric/date cell changes it to text, as the preview indicates. Programmatic requests
still require `confirm: true`; no cloud request happens on opening the panel.

## Account and privacy

The native client runs only the **official Claude Code CLI** with the user's own signed-in account
(`claude auth login`, or its official OAuth-token mechanism). Install/sign in to Claude Code if
necessary. No API-key fallback, alternate provider, HTTP server or credential copying exists.
Credential overrides are removed from the child process; credentials are never logged.
Requests run without tools or MCP servers, with no session persistence, bounded input/output and
timeouts, cancellation and process reaping. The browser build reports the assistant unavailable.

Selected cell text is sent only after confirmation. Treat model output as untrusted data until you
review it. Local proposal history is in memory only. No live-account check was run for this port;
all new tests use fake local fixtures/processes.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-26 | minor | First selected-cell Mirai panel, standalone official-Claude transport, revision-checked literal apply and offline tests. |

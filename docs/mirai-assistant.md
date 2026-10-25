# Mirai Assistant for GridCraft

> **Status (2026-10-24):** experimental backend and proposal CLI, published at the owner's
> request with full-CI failures documented below. Not an in-app assistant yet.

Mirai Assistant uses the official Claude Code CLI and the user's supported account sign-in.
It does not implement an API-key, HTTP-service or alternate-provider fallback, and does not
read or copy account credentials. Checking authentication sends no workbook text.

## Current scope

- `assistant.context` reads one selected or explicitly supplied range, up to 256 cells and
  64 KiB of data. It does not contact Claude or change the workbook.
- `assistant.apply` accepts indexed changes against that snapshot. It checks workbook identity,
  revision, original contents and protection before editing. The batch is one undoable action.
- String values are literal text, including a leading `=`. Formula changes are a separate,
  explicit opt-in engine operation; the Claude text-suggestion client does not propose formulas.
- The standalone `gridcraft-assistant` crate handles bounded Claude Code processes, cancellation,
  model selection and structured responses. Document strings are data, not instructions, and
  Claude tools are disabled.

The first CLI integration is a **proposal preview**, not automatic application. Only an explicit
generation request sends the selected cell text to Claude. Do not include sensitive cells unless
you intend to send them through your Claude account. A snapshot belongs to its live document
session: it is not an importable patch for reopening a workbook in another process.

## Shared appearance requirement

The owner requested the same purple/lavender **Mirai Assistant** appearance across WordCraft,
GridCraft and DeckCraft (2026-10-24). Use WordCraft's current assistant as the visual reference:
lavender surfaces, purple actions, conversational message layout, a roomy multiline composer
and a dropdown model selector, with matching light and dark variants. Keep the host application's
own identity and theme intact. Coordinate with the separate WordCraft visual-design work;
do not overwrite it. This requirement is pending UI implementation in this fork.

## CLI preview

```text
cargo run -p gridcraft-cli -- mirai --in book.xlsx --range A1:B2 --instruction "Translate to English" --model sonnet
```

Optional `--sheet NAME` selects the worksheet. Output is JSON with the original snapshot,
indexed literal-text changes and an explanation; `preview` is true and `applied` is false.
There is no save or automatic-apply option.

## Still pending

The desktop conversation panel, visual design, review/accept workflow and end-to-end checks with
a real signed-in account have not been adapted. Notes here do not imply spreadsheet-wide analysis,
formula generation, chart editing or a finished UI. The existing workbook and app names stay
GridCraft; **Mirai Assistant** is the assistant's name.

## Validation

Offline checks on Windows: 31 client tests, 15 engine assistant tests and 8 CLI tests pass.
Workspace formatting and Clippy, workspace WASM, doctests, layer and asset checks pass.
Generated parity and keytip checks pass after regeneration (no semantic changes).
Full-workspace CI
is **not green**: the command-sequence fuzz test found an incremental-versus-full recalculation
mismatch after `range.fill` (seed 0, step 38). A two-command reproduction also fails on the
unmodified baseline `3099df93`; Mirai's additional registry entries exposed a preexisting bug.
the separate non-engine run also found a header-autofit UI assertion at 50% zoom (768 of
769 UI integration tests passed). Neither failure is being suppressed. The calculation semantics
were not changed as part of this adaptation; the UI failure's baseline attribution is incomplete.
The owner requested publication of this experimental branch despite these documented failures.
No live Claude request has been made for this adaptation.

## Revision history

- 2026-10-24: document the initial backend boundary and account-only authentication policy;
  publish the experimental slice at the owner's request with known full-CI failures and pending UI.

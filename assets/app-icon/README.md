# Mirai Calc app icon

The **Mirai Calc icon** ("Cal-Q"): a calculator on a glossy turquoise tile, from the Mirai icon set
supplied by the fork's owner (MIT OR Apache-2.0). It replaces upstream's placeholder ledger page.

## Geometry

A 512 × 512 viewBox holding a rounded square of 418 px (`rx=116`) centred with a transparent
margin, so it already sits on Apple's icon grid: every platform, macOS included, uses it as is.

## Files

| File | Use |
|---|---|
| `gridcraft.svg` | Master vector, as supplied; also `hicolor/scalable` |
| `gridcraft-1024.png` | 1024 px master, as supplied; every smaller PNG, the `.ico` and the `.icns` are scaled from it |
| `gridcraft-macos-512.png` | Runtime Dock icon on macOS (for `apps/gridcraft` to embed) |
| `gridcraft.icns` | macOS bundle icon (`CFBundleIconFile`) |
| `gridcraft.ico` | Windows exe icon, 16–256 px (embedded by `apps/gridcraft/build.rs`) |
| `hicolor/<size>/apps/ai.storyteller.gridcraft.png` | Linux icon theme, 16–512 px; the 256 px one is also the runtime window icon on Windows and Linux |
| `hicolor/scalable/apps/ai.storyteller.gridcraft.svg` | Linux scalable icon |
| `LICENSE.txt` | MIT OR Apache-2.0 |

The app ID is `ai.storyteller.gridcraft`. It's used for the hicolor icon names, the Wayland app
ID and `packaging/linux/ai.storyteller.gridcraft.desktop`.

## Regenerate

Replace the masters (`gridcraft.svg`, `gridcraft-1024.png`), then run `packaging/icons.sh`. It needs
Python 3 with Pillow.

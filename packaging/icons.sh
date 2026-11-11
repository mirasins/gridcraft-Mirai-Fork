#!/usr/bin/env bash
# Regenerate every Mirai Calc app icon from its masters in assets/app-icon/: gridcraft-1024.png (and
# the SVG, copied as the Linux scalable icon). The masters are the Mirai icon set supplied by the
# fork's owner (MIT OR Apache-2.0); the tile already sits on Apple's icon grid, so every platform
# uses it as is.
#
# Needs: Python 3 with Pillow. The outputs are committed, so builds and packaging never need it.
#
#   packaging/icons.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/assets/app-icon"
APP="gridcraft"
ID="ai.storyteller.$APP"

python3 - "$DIR" "$APP" <<'PY'
import os, struct, sys
from io import BytesIO
from PIL import Image

d, app = sys.argv[1:3]
ident = "ai.storyteller." + app
master = Image.open(os.path.join(d, app + "-1024.png")).convert("RGBA")

def scaled(size):
    return master.resize((size, size), Image.LANCZOS)

for s in (16, 24, 32, 48, 64, 128, 256, 512):
    out = os.path.join(d, "hicolor", f"{s}x{s}", "apps", ident + ".png")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    scaled(s).save(out, optimize=True)
macos = os.path.join(d, app + "-macos-512.png")
if os.path.exists(macos):
    scaled(512).save(macos, optimize=True)

# Windows .ico: PNG-compressed entries, 16-256 px.
blobs = []
for s in (16, 20, 24, 32, 40, 48, 64, 128, 256):
    buf = BytesIO()
    scaled(s).save(buf, "PNG", optimize=True)
    blobs.append((s, buf.getvalue()))
head = struct.pack("<HHH", 0, 1, len(blobs))
entries, data, offset = b"", b"", 6 + 16 * len(blobs)
for s, b in blobs:
    entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(b), offset)
    data += b
    offset += len(b)
open(os.path.join(d, app + ".ico"), "wb").write(head + entries + data)

# macOS .icns, 16-1024 px.
master.save(os.path.join(d, app + ".icns"), "ICNS")
PY

mkdir -p "$DIR/hicolor/scalable/apps"
cp "$DIR/$APP.svg" "$DIR/hicolor/scalable/apps/$ID.svg"
echo "icons written to $DIR"

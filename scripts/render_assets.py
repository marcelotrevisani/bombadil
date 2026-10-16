#!/usr/bin/env python3
"""Render every image asset from the two SVGs in assets/.

`assets/icon.svg` and `assets/banner.svg` are the sources of truth; every
other file in `assets/` is a build product that happens to be committed, so a
checkout builds and runs without a Python toolchain. Run `just icon` after
editing either SVG.

`icon-64.rgba` is raw sRGB RGBA, the shape `iced::window::icon::from_rgba`
takes -- shipping it decoded is what keeps an image decoder out of the binary.
It is written into the GUI crate, not here: `cargo package` only takes files
under the crate root, so an asset outside it packages cleanly and then fails to
compile for whoever installs the published crate.

The banner sets its own text in the application's own two typefaces, which are
in the repo but not installed on any particular machine. Rather than ask a
contributor to install them, this points fontconfig at
`crates/bombadil-gui/fonts` for the length of this process only -- a temporary
config file in a temp directory, nothing written to the user's font settings.
"""

import base64
import os
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
ICON_SVG = ASSETS / "icon.svg"
BANNER_SVG = ASSETS / "banner.svg"
FONTS = ROOT / "crates" / "bombadil-gui" / "fonts"
SIZES = (512, 256, 128, 64, 32)
RGBA_SIDE = 64
BANNER_SIZE = (1200, 340)
MACOS_SIDE = 1024
MACOS_ART = 824


def with_repo_fonts() -> None:
    """Re-exec with fontconfig pointed at the repo's own typefaces.

    Done by re-exec rather than in-process because fontconfig reads
    FONTCONFIG_FILE once, when it initialises -- which importing cairosvg
    already does.
    """
    if os.environ.get("BOMBADIL_FONTS_READY"):
        return
    tmp = Path(tempfile.mkdtemp(prefix="bombadil-fonts-"))
    # Only directories that exist: fontconfig warns about a <dir> it cannot
    # resolve, and a Windows path on Linux is exactly that.
    system = [d for d in ("/usr/share/fonts", "/System/Library/Fonts", "C:/Windows/Fonts")
              if Path(d).is_dir()]
    dirs = "".join(f"<dir>{d}</dir>" for d in [FONTS, *system])
    (tmp / "fonts.conf").write_text(
        "<?xml version='1.0'?>"
        "<!DOCTYPE fontconfig SYSTEM 'fonts.dtd'>"
        f"<fontconfig>{dirs}<cachedir>{tmp / 'cache'}</cachedir></fontconfig>"
    )
    env = dict(os.environ, FONTCONFIG_FILE=str(tmp / "fonts.conf"), BOMBADIL_FONTS_READY="1")
    raise SystemExit(subprocess.call([sys.executable, __file__], env=env))


with_repo_fonts()

import cairosvg  # noqa: E402  -- imported only after fontconfig is pointed at the repo fonts


def png_to_rgba(data: bytes) -> tuple[bytes, int, int]:
    """Decode an 8-bit RGBA PNG. Only the subset cairosvg emits."""
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat, width, height, depth, colour = 8, b"", 0, 0, 0, 0
    while pos < len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        kind = data[pos + 4 : pos + 8]
        body = data[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            width, height, depth, colour = struct.unpack(">IIBB", body[:10])
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    assert (depth, colour) == (8, 6), f"expected 8-bit RGBA, got {depth}/{colour}"

    raw = zlib.decompress(idat)
    stride = width * 4
    out = bytearray()
    prev = bytearray(stride)
    i = 0
    for _ in range(height):
        filter_type = raw[i]
        i += 1
        line = bytearray(raw[i : i + stride])
        i += stride
        for x in range(stride):
            left = line[x - 4] if x >= 4 else 0
            up = prev[x]
            up_left = prev[x - 4] if x >= 4 else 0
            if filter_type == 1:
                line[x] = (line[x] + left) & 0xFF
            elif filter_type == 2:
                line[x] = (line[x] + up) & 0xFF
            elif filter_type == 3:
                line[x] = (line[x] + (left + up) // 2) & 0xFF
            elif filter_type == 4:
                p = left + up - up_left
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - up_left)
                nearest = left if (pa <= pb and pa <= pc) else (up if pb <= pc else up_left)
                line[x] = (line[x] + nearest) & 0xFF
        out += line
        prev = line
    return bytes(out), width, height


def main() -> None:
    for size in SIZES:
        target = ASSETS / f"icon-{size}.png"
        cairosvg.svg2png(url=str(ICON_SVG), write_to=str(target), output_width=size, output_height=size)
        print(f"wrote {target.relative_to(ROOT)}")

    banner = ASSETS / "banner.png"
    cairosvg.svg2png(
        url=str(BANNER_SVG),
        write_to=str(banner),
        output_width=BANNER_SIZE[0],
        output_height=BANNER_SIZE[1],
    )
    print(f"wrote {banner.relative_to(ROOT)}")

    png = cairosvg.svg2png(url=str(ICON_SVG), output_width=RGBA_SIDE, output_height=RGBA_SIDE)
    rgba, width, height = png_to_rgba(png)
    expected = RGBA_SIDE * RGBA_SIDE * 4
    assert (width, height) == (RGBA_SIDE, RGBA_SIDE), (width, height)
    assert len(rgba) == expected, f"{len(rgba)} bytes, expected {expected}"
    target = ROOT / "crates" / "bombadil-gui" / "assets" / "icon-64.rgba"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(rgba)
    print(f"wrote {target.relative_to(ROOT)} ({len(rgba)} bytes)")

    # macOS draws Dock icons on a 1024 grid with the artwork in the middle
    # 824 (Apple's template), so a full-bleed icon looks a size too big next
    # to every other app. This one is padded to match. The GUI embeds it to
    # set the Dock icon at runtime, and the release workflow builds the
    # .app bundle's .icns from it.
    art = base64.b64encode(
        cairosvg.svg2png(url=str(ICON_SVG), output_width=MACOS_ART, output_height=MACOS_ART)
    ).decode()
    pad = (MACOS_SIDE - MACOS_ART) // 2
    padded = (
        f'<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" '
        f'width="{MACOS_SIDE}" height="{MACOS_SIDE}">'
        f'<image x="{pad}" y="{pad}" width="{MACOS_ART}" height="{MACOS_ART}" '
        f'xlink:href="data:image/png;base64,{art}"/></svg>'
    )
    macos = ROOT / "crates" / "bombadil-gui" / "assets" / "icon-macos.png"
    cairosvg.svg2png(bytestring=padded.encode(), write_to=str(macos))
    print(f"wrote {macos.relative_to(ROOT)}")

    # The desktop entry Bombadil writes for itself at startup embeds this one;
    # it has to sit under the crate for the same `cargo package` reason.
    themed = ROOT / "crates" / "bombadil-gui" / "assets" / "icon-256.png"
    themed.write_bytes((ASSETS / "icon-256.png").read_bytes())
    print(f"wrote {themed.relative_to(ROOT)}")


if __name__ == "__main__":
    main()

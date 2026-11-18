#!/usr/bin/env python3
"""Compare decoded PNG pixels. Pillow is available in the WSL Python user site.

Run with python3 -B. --ignore takes x,y,w,h in pixels or "footer". For footer,
use a full screenshot with a sibling .regions.json from --screenshot --regions.
Bounding boxes are [left, top, right, bottom] with exclusive right/bottom edges.
Exit 0 for equal outside the ignores, 1 for different sizes or pixels, 2 for errors.
"""

import argparse
import json
import sys
from pathlib import Path

try:
    from PIL import Image, ImageChops, ImageDraw, ImageOps
except ImportError:
    print("Pillow is required. Run with the WSL python3 that has Pillow in its user site.", file=sys.stderr)
    sys.exit(2)


def rectangle(value):
    try:
        x, y, width, height = map(int, value.split(","))
        if width > 0 and height > 0:
            return x, y, width, height
    except ValueError:
        pass
    raise argparse.ArgumentTypeError("use footer or x,y,w,h with positive width and height")


def read_png(path):
    with Image.open(path, formats=["PNG"]) as image:
        return image.convert("RGBA")


def footer(path, size):
    sidecar = path.with_suffix(".regions.json")
    try:
        data = json.loads(sidecar.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    try:
        x, y, width, height = values = data["regions"]["footer"]
        if (
            data["size"] == list(size)
            and all(type(v) is int for v in values)
            and 0 <= x < x + width <= size[0]
            and 0 <= y < y + height <= size[1]
        ):
            return tuple(values)
    except (KeyError, TypeError, ValueError):
        pass
    raise ValueError(f"{sidecar} needs a valid footer and the full PNG size")


def output_path(path, inputs):
    target = path.resolve()
    if target in (p.resolve() for p in inputs) or (
        target.exists() and any(target.samefile(p) for p in inputs)
    ):
        raise ValueError("--out must be a different file from both inputs")
    for p in (path.absolute(), target):
        if p.suffix.lower() in (".tak", ".takm"):
            raise ValueError("--out cannot write a TAK or TAKM path")
        for parent in p.parents:
            if parent.name.lower() == "input" and (parent.parent / "apps" / "AGENTS.md").is_file():
                raise ValueError("--out cannot write inside the repo input/ folder")
    return path


def fill_rect(draw, rect, size):
    x, y, width, height = rect
    left, top = max(x, 0), max(y, 0)
    right, bottom = min(x + width, size[0]), min(y + height, size[1])
    if left < right and top < bottom:
        draw.rectangle((left, top, right - 1, bottom - 1), fill=255)


def compare(a, b, rects, out):
    size = max(a.width, b.width), max(a.height, b.height)
    ignored = Image.new("L", size)
    draw = ImageDraw.Draw(ignored)
    for rect in rects:
        fill_rect(draw, rect, size)
    # Put both images on one canvas. Missing pixels count even if their counterpart is transparent.
    first, second = a, b
    if a.size != b.size:
        first, second = Image.new("RGBA", size), Image.new("RGBA", size)
        first.paste(a)
        second.paste(b)
    red, green, blue, alpha = ImageChops.difference(first, second).split()
    changed = ImageChops.lighter(ImageChops.lighter(red, green), ImageChops.lighter(blue, alpha))
    changed = changed.point([0] + [255] * 255)
    if a.size != b.size:
        extents = [Image.new("L", size), Image.new("L", size)]
        for image, extent in zip((a, b), extents):
            fill_rect(ImageDraw.Draw(extent), (0, 0, *image.size), size)
        changed = ImageChops.lighter(changed, ImageChops.difference(*extents))
    outside = ImageChops.subtract(changed, ignored)
    inside = ImageChops.darker(changed, ignored)
    if out is not None:
        diff = ImageOps.grayscale(first)
        diff.paste(ImageOps.grayscale(b))
        diff = diff.point(lambda n: n // 3).convert("RGB")
        diff.paste((255, 190, 70), mask=inside)
        diff.paste((255, 40, 110), mask=outside)
        diff.save(out, format="PNG")
    return {
        "size_a": a.size,
        "size_b": b.size,
        "size_mismatch": a.size != b.size,
        "different_pixels": changed.histogram()[255],
        "different_pixels_outside": outside.histogram()[255],
        "different_pixels_ignored": inside.histogram()[255],
        "bbox_all": changed.getbbox(),
        "bbox_outside": outside.getbbox(),
        "bbox_ignored": inside.getbbox(),
    }


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("a", type=Path)
    parser.add_argument("b", type=Path)
    parser.add_argument("--ignore", action="append", default=[], metavar="footer|X,Y,W,H")
    parser.add_argument("--out", type=Path, metavar="DIFF.png")
    parser.add_argument("--json", action="store_true")
    # argparse treats negative comma-separated coordinates as an option without "=".
    argv = list(argv)
    for i in range(len(argv) - 2, -1, -1):
        if argv[i] == "--ignore" and argv[i + 1].startswith("-") and "," in argv[i + 1]:
            argv[i:i + 2] = ["--ignore=" + argv[i + 1]]
    args = parser.parse_args(argv)
    try:
        a, b = read_png(args.a), read_png(args.b)
        rects = [rectangle(value) for value in args.ignore if value != "footer"]
        if "footer" in args.ignore:
            footers = [rect for rect in (footer(args.a, a.size), footer(args.b, b.size)) if rect]
            if not footers:
                raise ValueError("--ignore footer needs a sibling .regions.json from --screenshot --regions")
            rects.extend(footers)
        out = output_path(args.out, (args.a, args.b)) if args.out is not None else None
        result = compare(a, b, rects, out)
    except (OSError, ValueError, RuntimeError, argparse.ArgumentTypeError, Image.DecompressionBombError) as error:
        parser.error(str(error))
    if args.json:
        print(json.dumps(result))
    else:
        if result["size_mismatch"]:
            print(f"Size mismatch {a.width}x{a.height} vs {b.width}x{b.height}")
        for label, count, box in (
            ("All", "different_pixels", "bbox_all"),
            ("Outside ignores", "different_pixels_outside", "bbox_outside"),
            ("Inside ignores", "different_pixels_ignored", "bbox_ignored"),
        ):
            print(f"{label}: {result[count]} differing pixels, bounds {result[box]}")
    return int(result["size_mismatch"] or result["different_pixels_outside"] > 0)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

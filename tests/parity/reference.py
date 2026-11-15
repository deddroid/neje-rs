#!/usr/bin/env python3
"""Reference data for tests/parity.rs, produced by the Python prototype (neje-linux).

The Rust library must give the same bytes as the prototype, which was itself checked bit
for bit against the decompiled NEJE V3.6 code. This script runs the prototype on a set of
inputs and writes `manifest.tsv`: one line per case with the inputs and the SHA-256 of the
expected output. The Rust test recomputes every case and compares.

    python reference.py --prototype ~/Code/Neje/neje-linux --out /tmp/parity \
        [--clipart DIR] [--samples DIR] [--limit N] [--jobs N] \
        [--optimizer ottimizza_lavori.py] [--drawings DIR]

`--optimizer` adds the print-mode cases: the bands chosen by the reference tool
`ottimizza_lavori.py` (needs numpy) and its time estimates, on synthetic drawings and on the
490x490 PNG files of `--drawings`.

Needs Pillow (the prototype's dependency). Nothing from NEJE is copied into the repository:
the image folders are only read, and only their paths and hashes end up in the manifest.
"""
from __future__ import annotations

import argparse
import hashlib
import math
import os
import random
import struct
import sys
from multiprocessing import Pool
from pathlib import Path

from PIL import Image, ImageDraw

SYNTHETIC_SIZES = [
    (491, 491), (980, 490), (490, 980), (1000, 500), (500, 1000), (800, 800), (1234, 777),
    (640, 2000), (2000, 640), (3000, 300), (300, 3000), (513, 700), (4000, 3000), (600, 491),
    (491, 600), (490, 490), (489, 491), (2048, 2047), (495, 1481), (1481, 495), (120, 90),
]
MODES = ["bw", "dither", "gray-threshold"]
# (version, hardware version, base size)
PROTOCOLS = [("V10", 0, 490), ("V20", 0, 550), ("V21", 0, 490), ("V30", 11, 490), ("V30", 12, 550), ("V30", 102, 490)]


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def protocol(version: str, hw: int):
    from neje import protocol as P
    return P.BY_NAME[version](hw_version=hw or None)


# --- synthetic inputs ---------------------------------------------------------------------

def smooth_noise(rng: random.Random, size, mode: str) -> Image.Image:
    """Blobs, gradients and sharp shapes: exercises the resize filter and both thresholds."""
    bands = len(mode)
    small = (max(2, size[0] // rng.choice([7, 13, 31])), max(2, size[1] // rng.choice([7, 13, 31])))
    im = Image.frombytes(mode, small, rng.randbytes(small[0] * small[1] * bands))
    im = im.resize(size, rng.choice([Image.BICUBIC, Image.NEAREST, Image.BILINEAR]))
    draw = ImageDraw.Draw(im)
    for _ in range(12):
        x0, y0 = rng.randrange(size[0]), rng.randrange(size[1])
        x1, y1 = x0 + rng.randrange(1, size[0] // 3 + 2), y0 + rng.randrange(1, size[1] // 3 + 2)
        fill = tuple(rng.randrange(256) for _ in range(bands))
        if rng.random() < 0.5:
            draw.ellipse((x0, y0, x1, y1), fill=fill)
        else:
            draw.rectangle((x0, y0, x1, y1), fill=fill)
    return im


def make_inputs(out: Path, rng: random.Random) -> list[Path]:
    folder = out / "inputs"
    folder.mkdir(parents=True, exist_ok=True)
    sizes = SYNTHETIC_SIZES + [(rng.randrange(491, 1500), rng.randrange(491, 1500)) for _ in range(30)]
    paths = []
    for i, size in enumerate(sizes):
        mode = ["RGB", "RGBA", "L", "LA"][i % 4]
        path = folder / f"synthetic-{i:02d}-{mode}-{size[0]}x{size[1]}.png"
        smooth_noise(rng, size, mode).save(path)
        paths.append(path)
    # Palette images, with and without a transparent colour, and a 1-bit image.
    pal = smooth_noise(rng, (700, 520), "RGB").quantize(64)
    pal.save(folder / "palette.png")
    pal.save(folder / "palette-transparent.png", transparency=3)
    smooth_noise(rng, (900, 600), "L").convert("1").save(folder / "onebit.png")
    smooth_noise(rng, (640, 480), "RGB").save(folder / "plain.bmp")
    paths += [folder / "palette.png", folder / "palette-transparent.png", folder / "onebit.png", folder / "plain.bmp"]
    return paths


GCODE_PROGRAMS = {
    "square.nc": "G21 G90\nG0 X0 Y0\nM3 S100\nG1 X10 Y0\nG1 X10 Y10\nG1 X0 Y10\nG1 X0 Y0\nM5\nG0 X20 Y20\n",
    "inch-relative-arc.nc": "G20 G91\nM3\nG1 X0.1\nG2 X0 Y0 I-0.05 J0\nG3 X0.2 Y0.1 R0.3\nG1 Y-0.05\n",
    "comments.nc": "(header) G21\n; full line comment\nM4 S0\nG1 X5 Y5 (no power)\nS255\nG1 X30.5 Y12.25 F600\n"
                   "G92 X0 Y0\nG1 X3 Y-2\nG28\nG1 X36.75 Y36.75\nM5\nG1 X1 Y1\nm3\ng1 x2.5e1 y+.5\n",
    "arcs.nc": "G90 G21 M3 S10\nG0 X18 Y18\nG2 X18 Y18 I5 J0\nG3 X25 Y25 R-7\nG2 X5 Y30 R40\nG3 X30 Y5 I-3.3 J-8.1\n"
               "G2 X30 Y5 R1\nG1 X-4 Y50\n",
}


def random_gcode(rng: random.Random) -> str:
    lines = [rng.choice(["G21", "G20"]), "G90", "M3 S%d" % rng.randrange(1, 255)]
    unit = 1.0 if lines[0] == "G21" else 1 / 25.4
    for _ in range(rng.randrange(20, 120)):
        x, y = rng.uniform(-3, 40) * unit, rng.uniform(-3, 40) * unit
        kind = rng.random()
        if kind < 0.15:
            lines.append(rng.choice(["M5", "M3", "S0", "S80", "G91", "G90"]))
        elif kind < 0.6:
            lines.append(f"G{rng.choice([0, 1, 1, 1])} X{x:.4f} Y{y:.4f}")
        elif kind < 0.8:
            lines.append(f"G{rng.choice([2, 3])} X{x:.3f} Y{y:.3f} I{rng.uniform(-9, 9) * unit:.3f} J{rng.uniform(-9, 9) * unit:.3f}")
        else:
            lines.append(f"G{rng.choice([2, 3])} X{x:.3f} Y{y:.3f} R{rng.uniform(-30, 30) * unit:.3f}")
    return "\n".join(lines) + "\n"


def make_gcode(out: Path, rng: random.Random) -> list[Path]:
    folder = out / "gcode"
    folder.mkdir(parents=True, exist_ok=True)
    programs = dict(GCODE_PROGRAMS)
    programs.update({f"random-{i:02d}.nc": random_gcode(rng) for i in range(25)})
    paths = []
    for name, text in programs.items():
        (folder / name).write_text(text)
        paths.append(folder / name)
    return paths


# --- cases (run in worker processes) ------------------------------------------------------

def base_of(path: str, base_size: int, mode: str) -> Image.Image:
    from neje import imaging as I
    return I.center_on(I.new_base(base_size), I.prepare(I.load_image(path), base_size, mode))


def case_prepare(args):
    path, base_size, mode = args
    return f"prepare\t{path}\t{base_size}\t{mode}\t{sha(base_of(path, base_size, mode).tobytes())}"


def case_payload(args):
    path, version, hw, mode = args
    proto = protocol(version, hw)
    payload = proto.build_payload(base_of(path, proto.base_size, mode))
    return f"payload\t{path}\t{version}\t{hw}\t{mode}\t{sha(payload)}"


def case_resize(args):
    from neje import imaging as I
    path, max_border = args
    out = I.to_equal_small(I.load_image(path), max_border)
    return f"resize\t{path}\t{max_border}\t{out.width}\t{out.height}\t{sha(out.tobytes())}"


def case_ops(args):
    from neje import imaging as I
    path, mode, rotate, shrink, stamp, cx, cy = args
    base = base_of(path, 490, mode)
    for _ in range(rotate):
        base = I.rotate90(base)
    for _ in range(shrink):
        base = I.shrink(base)
    if stamp:
        base = I.place(base, I.prepare(I.load_image(stamp), 200, "bw"), cx, cy)
    return f"ops\t{path}\t{mode}\t{rotate}\t{shrink}\t{stamp}\t{cx}\t{cy}\t{sha(base.tobytes())}"


def case_gcode(args):
    from neje import gcode
    path, base_size, fit, line = args
    im = gcode.rasterize(Path(path).read_text(), base_size, fit=bool(fit), line_px=line)
    return f"gcode\t{path}\t{base_size}\t{fit}\t{line}\t{sha(im.tobytes())}"


def case_segments(path):
    from neje import gcode
    segs = gcode.parse_segments(Path(path).read_text())
    packed = b"".join(struct.pack("<4d", *s) for s in segs)
    return f"segments\t{path}\t{len(segs)}\t{sha(packed)}"


def case_decode(path):
    from neje import imaging as I
    im = I.load_image(path)
    return f"decode\t{path}\t{im.width}\t{im.height}\t{sha(im.tobytes())}"


def make_drawings(out: Path, rng: random.Random) -> list[Path]:
    """Black and white 490x490 drawings with parts far apart, close together, or alone."""
    folder = out / "drawings"
    folder.mkdir(parents=True, exist_ok=True)
    paths = []
    for i in range(10):
        im = Image.new("L", (490, 490), 255)
        draw = ImageDraw.Draw(im)
        for _ in range(rng.randrange(1, 6)):
            x0, y0 = rng.randrange(0, 440), rng.randrange(0, 440)
            w, h = rng.randrange(1, 120), rng.randrange(1, 200)
            if rng.random() < 0.4:
                draw.ellipse((x0, y0, x0 + w, y0 + h), outline=0, width=rng.randrange(1, 9))
            elif rng.random() < 0.5:
                draw.rectangle((x0, y0, x0 + w, y0 + h), fill=0)
            else:
                draw.line((x0, y0, x0 + w, y0 + h), fill=0, width=rng.randrange(1, 6))
        path = folder / f"drawing-{i:02d}.png"
        im.save(path)
        paths.append(path)
    Image.new("L", (490, 490), 255).save(folder / "empty.png")
    return paths + [folder / "empty.png"]


_optimizer = None


def use_optimizer(path: str) -> None:
    """Loads the reference tool as a module (also in the worker processes)."""
    import importlib.util
    global _optimizer
    spec = importlib.util.spec_from_file_location("ottimizza_lavori", path)
    _optimizer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(_optimizer)


def case_bands(args):
    import numpy as np
    path, burn = args
    black = np.asarray(Image.open(path).convert("L")) < 128
    bands = _optimizer.tagli_migliori(black, burn)
    total = 0.0
    for a, b in bands:
        job = np.zeros_like(black)
        job[:, a:b] = black[:, a:b]
        total += _optimizer.secondi(job, burn)
    single = _optimizer.secondi(black, burn)
    text = ",".join(f"{a}-{b}" for a, b in bands)
    return f"bands\t{path}\t{burn}\t{float(single).hex()}\t{float(total).hex()}\t{text}"


def run(case):
    fn, args = case
    return fn(args)


def prepare_worker(prototype: str, optimizer: str | None) -> None:
    use_prototype(prototype)
    if optimizer:
        use_optimizer(optimizer)


def use_prototype(folder: str) -> None:
    """Makes the prototype importable (also in the worker processes)."""
    sys.path.insert(0, folder)


# --- protocol cases (fast, run inline) ----------------------------------------------------

COMMANDS = [
    ("handshake", ()), ("start", (30,)), ("start", (1,)), ("start", (120,)), ("pause", ()), ("reset", ()),
    ("home", ()), ("center", ()), ("box", ()), ("move", ("up",)), ("move", ("down",)), ("move", ("left",)),
    ("move", ("right",)), ("forward", ()), ("backward", ()), ("redo", ()), ("erase", ()), ("power_on", ()),
    ("power_off", ()), ("move_to", (0, 0)), ("move_to", (123, 405)), ("move_to", (489, 99)),
    ("move_to", (549, 500)), ("set_burn", (0,)), ("set_burn", (1,)), ("set_burn", (30,)), ("set_burn", (120,)),
    ("set_burn", (121,)),
]


def command_lines():
    for version, hw, _ in PROTOCOLS:
        proto = protocol(version, hw)
        for name, args in COMMANDS:
            try:
                data = getattr(proto, name)(*args)
                result = "none" if data is None else data.hex()
            except ValueError:
                result = "error"
            yield f"command\t{version}\t{hw}\t{name}\t{','.join(map(str, args))}\t{result}"


def event_text(ev) -> str:
    if ev.kind == "progress":
        return f"progress:{ev.x},{ev.y}"
    if ev.kind in ("connected", "unknown", "version", "debug", "battery", "burn_value"):
        return f"{ev.kind}:{ev.value}"
    return ev.kind


def random_stream(rng: random.Random, version: str) -> bytes:
    out = bytearray()
    for _ in range(rng.randrange(5, 60)):
        kind = rng.random()
        if version == "V10":
            if kind < 0.4:
                out += bytes([0xFF, rng.randrange(6), rng.randrange(100), rng.randrange(6), rng.randrange(100)])
            elif kind < 0.8:
                out.append(rng.choice([0x65, 0x66, 0x78, 0x79]))
            else:
                out.append(rng.randrange(256))
        elif kind < 0.7:
            out += bytes([0xFF, rng.randrange(1, 13), rng.choice([0, 1, 2, 5, 11, 12, 13, 100, rng.randrange(256)]),
                          rng.choice([0, 0, 1, 50, 99, rng.randrange(256)])])
        elif kind < 0.85:
            out += rng.randbytes(rng.randrange(1, 4))
        else:
            out.append(0xFF)
    return bytes(out)


def parse_lines(rng: random.Random):
    for version, hw, _ in PROTOCOLS:
        for _ in range(200):
            stream = random_stream(rng, version)
            cuts = sorted(rng.sample(range(len(stream) + 1), k=min(len(stream), rng.randrange(0, 6))))
            proto = protocol(version, hw)
            events, start = [], 0
            for cut in cuts + [len(stream)]:
                events += proto.feed(stream[start:cut])
                start = cut
            text = ";".join(event_text(e) for e in events)
            yield f"parse\t{version}\t{hw}\t{stream.hex()}\t{','.join(map(str, cuts))}\t{text}"


def recognise_lines(rng: random.Random):
    replies = [bytes([0x65]), bytes([0x79, 0x65]), bytes([255, 1, 0, 0]), bytes([255, 2, 100, 0]), bytes([255, 2, 11, 1]),
               bytes([255, 2, 12, 1]), bytes([7, 255, 2, 13, 9]), bytes([255, 2, 14, 1]), b"", bytes([255, 1, 0])]
    replies += [rng.randbytes(rng.randrange(1, 8)) for _ in range(100)]
    for version, _, _ in PROTOCOLS[:4]:
        from neje import protocol as P
        for reply in replies:
            hw = P.BY_NAME[version].match_ack(reply)
            yield f"recognise\t{version}\t{reply.hex()}\t{'none' if hw is None else hw}"


def hypot_lines(rng: random.Random):
    for _ in range(5000):
        scale = 10.0 ** rng.randrange(-6, 7)
        a, b = rng.uniform(-50, 50) * scale, rng.uniform(-50, 50) * rng.choice([scale, 1.0, 1e-9])
        yield f"hypot\t{a.hex()}\t{b.hex()}\t{math.hypot(a, b).hex()}"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--prototype", required=True, help="folder of the Python prototype (contains the `neje` package)")
    ap.add_argument("--out", required=True, help="output folder for the manifest and the synthetic inputs")
    ap.add_argument("--clipart", help="folder of the NEJE clipart (PNG files named .bmp)")
    ap.add_argument("--samples", help="folder of the NEJE sample pictures (JPEG)")
    ap.add_argument("--limit", type=int, help="use at most N clipart files")
    ap.add_argument("--jobs", type=int, default=os.cpu_count())
    ap.add_argument("--optimizer", help="path of ottimizza_lavori.py, the reference for the print modes")
    ap.add_argument("--drawings", help="folder of 490x490 black and white PNG files to split in jobs")
    a = ap.parse_args()

    prototype = str(Path(a.prototype).expanduser().resolve())
    use_prototype(prototype)
    out = Path(a.out).expanduser().resolve()
    out.mkdir(parents=True, exist_ok=True)
    rng = random.Random(20261114)

    synthetic = make_inputs(out, rng)
    gcodes = make_gcode(out, rng)
    clipart = sorted(Path(a.clipart).expanduser().iterdir())[: a.limit] if a.clipart else []
    samples = sorted(p for p in Path(a.samples).expanduser().iterdir() if p.suffix.lower() in (".jpg", ".jpeg")) if a.samples else []

    # JPEG decoders differ between libraries: compare the pipeline on the decoded pixels
    # (saved as PNG) and record the JPEG decoding separately.
    decoded = []
    for p in samples:
        png = out / "inputs" / f"sample-{p.stem}.png"
        Image.open(p).convert("RGB").save(png)
        decoded.append(png)

    cases = []
    for p in synthetic + decoded:
        cases += [(case_prepare, (str(p), size, mode)) for size in (490, 550) for mode in MODES]
        cases += [(case_resize, (str(p), border)) for border in (490, 441, 100)]
    for p in clipart:
        cases += [(case_prepare, (str(p), 490, mode)) for mode in ("bw", "dither")]
    for p in synthetic + decoded + clipart + samples:
        cases.append((case_decode, str(p)))
    payload_inputs = synthetic[:12] + decoded + clipart[::60]
    for i, p in enumerate(payload_inputs):
        for version, hw, _ in PROTOCOLS:
            cases.append((case_payload, (str(p), version, hw, MODES[i % 2])))
    ops_inputs = synthetic[:16] + decoded[:8] + clipart[::200]
    for i, p in enumerate(ops_inputs):
        stamp = str(clipart[(i * 37) % len(clipart)]) if clipart and i % 2 else ""
        cases.append((case_ops, (str(p), MODES[i % 2], i % 4, i % 3, stamp, rng.randrange(-20, 510), rng.randrange(-20, 510))))
    for p in gcodes:
        cases.append((case_segments, str(p)))
        cases += [(case_gcode, (str(p), size, fit, line)) for size in (490, 550) for fit in (0, 1) for line in (1, 2, 3, 5)]

    optimizer = str(Path(a.optimizer).expanduser().resolve()) if a.optimizer else None
    if optimizer:
        drawings = make_drawings(out, rng)
        if a.drawings:
            drawings += [p for p in sorted(Path(a.drawings).expanduser().glob("*.png")) if Image.open(p).size == (490, 490)]
        cases += [(case_bands, (str(p), burn)) for p in drawings for burn in (10, 40, 120)]

    lines = list(command_lines()) + list(parse_lines(rng)) + list(recognise_lines(rng)) + list(hypot_lines(rng))
    with Pool(a.jobs, initializer=prepare_worker, initargs=(prototype, optimizer)) as pool:
        for n, line in enumerate(pool.imap_unordered(run, cases, chunksize=8), 1):
            lines.append(line)
            if n % 500 == 0:
                print(f"  {n}/{len(cases)}", file=sys.stderr)
    (out / "manifest.tsv").write_text("\n".join(sorted(lines)) + "\n")
    print(f"{len(lines)} cases written to {out / 'manifest.tsv'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

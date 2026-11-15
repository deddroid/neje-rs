# Serial protocol of the NEJE DK-8-KZ

How `neje` talks to the engraver, and where each fact comes from.

- **decompiled**: read in the official Windows software NEJE V3.6 (.NET, decompiled for
  interoperability; its code is not part of this repository). It tells what the PC sends and
  expects, not what the firmware does with it.
- **external**: confirmed by an independent project ([EzGraver](https://github.com/camrein/EzGraver),
  [nejemojo](https://github.com/sthysel/nejemojo)) or by the manual.
- **measured**: seen on a real DK-8-KZ (V10 firmware, CH340 `1a86:7523`) on 14–15/11/2026,
  with every byte logged and the movements checked with a webcam and a caliper.
- **guess**: deduced, still to be tried.

The measurements were made with the Python prototype this program was ported from and with
scripts sending the same bytes; the Rust program produces the same bytes as the prototype
(see `tests/parity`). On 15/11/2026 the Rust program itself ran on the machine: `detect`,
`reset`, `upload`, `start`, `box`, `center`, and `engrave`: in line-art mode it sent and
engraved three jobs one after the other without a restart in 133 s (129 s estimated), in
split mode three jobs of 22 050 filled pixels in 441 s (439 s estimated).

## Physical layer

| Fact | Source |
|---|---|
| USB-serial converter WCH CH340 (`1a86:7523`), Linux driver `ch341`, port `/dev/ttyUSB0` | measured |
| 57600 baud, 8 data bits, no parity, 1 stop bit, no flow control, DTR and RTS low | decompiled, external; measured |
| Opening the port on Linux does not restart the machine | measured |
| The laser glows at low power from the moment the machine is powered | measured |
| With both USB cables on the computer's ports the board restarted during long engravings (2 out of 2); with the power cable on a 5 V supply it did not | measured (one run each way) |
| No limit switches: the machine finds its edges by running against the end stops | measured, external |

## Detecting the firmware generation

The original launcher opens every serial port and tries, in order:

| Sent | Reply looked for (anywhere in what arrives) | Protocol |
|---|---|---|
| `F6` | `65` | V10 |
| `FF 09 00 00` | `FF 01 00 00` | V20 |
| `FF 09 00 00` | `FF 02 64` | V21 |
| `FF 09 00 00` | `FF 02 0B` / `0C` / `0D` | V30, hardware 11 / 12 / 13 |

(decompiled.) A DK-8-KZ answers `F6` with `65 70` after about 20 ms (measured); the meaning
of `70` is unknown and the original ignores it.

`neje` differs from the launcher on purpose:

- by default it only speaks **V10**. A V10 firmware probably reads the lone byte `09` of the
  4-byte handshake as "burn time 9" (guess), so `FF 09 00 00` is sent only with
  `--protocol auto` or a 4-byte protocol, once, and only after `F6` got no reply for 500 ms;
- without `--port` it probes only CH340 converters, never unrelated serial devices.

## Protocol V10

### Commands, PC to engraver

| Function | Bytes | What it does | Source |
|---|---|---|---|
| Handshake | `F6` | answers `65 70` | measured |
| Start | `F1 tt` | engraves the image in memory from the beginning, laser at **full power**; after a pause it resumes from the next pixel | measured |
| Burn time (`tt` = 1–120, default 30) | `tt` | no visible reaction when idle | decompiled, external |
| Pause | `F2` | stops within 2 pixels, keeps the position | measured |
| Restart ("Reboot") | `F9` | the board restarts, finds its edges again (about 4 s) and answers `79`. The plate comes back about 2 mm off: the position is lost | measured |
| To the origin | `F3` | absolute origin of the work area: head left, plate towards the operator | measured |
| To the centre | `FB` | centre of the bounding box **of the image in memory**, not of the area; also stops `F4` | measured |
| Outline ("Box locating") | `F4` | traces the bounding box of the image **over and over** until `FB` | measured |
| Up / down / left / right | `F5 01` / `02` / `03` / `04` | shifts by **8 pixels** per command and moves the reference with it: the whole image lands elsewhere, and `FB` does not undo it. `F5 02` stalls almost every time | measured |
| Forward / back | `FC AA` / `FC 55` | during an engraving, skips 16 rows forward (they are not engraved) / goes back 16 rows (they are engraved again) | measured |
| Redo | `FC 77` | after a finished job, starts again from the beginning with the last burn time | measured |
| Erase the image memory | `FE` × 8 | laser off for about 2 s, no reply | measured |

After a finished job (`66`) the firmware ignores `F3` and `FB` until `F9`; `F1`, `FC 77` and
a new upload work at once (measured).

`F1` engraves whatever image is in memory, which survives power off: on a machine that has
just been plugged in that is the last image sent, or the factory one (external, manual).

### Messages, engraver to PC

| Bytes | Meaning | Source |
|---|---|---|
| `65` (followed by `70`) | reply to the handshake | measured |
| `66` | engraving finished, about 0.5 s after the last position | measured |
| `79` | initialisation completed: sent after `F9` and after a restart. **During an engraving it means the board restarted and the job is lost**: `66` will not come | measured |
| `78` | no power supply | decompiled, never seen |
| `FF xh xl yh yl` | position: X = xh·100 + xl, Y = yh·100 + yl. One packet **for every burnt pixel**, in pixels of the 490×490 image | measured |

A `FF` opens a 5-byte packet whose bytes are not interpreted individually; any other byte is
ignored (`neje monitor` shows it as "unknown byte").

### Sending an image

1. `FE` × 8.
2. Wait 3 s with no confirmation from the firmware (decompiled; EzGraver waits 6 s). `neje`
   waits 3.5 s (`--erase-wait`). Data sent during the erase is lost.
3. 32 830 bytes as one continuous stream (5.7 s at 57600 baud, measured): a complete 1-bit
   BMP file.
   - 512×512 canvas, white, with the 490×490 image at the top left;
   - the first data row is the **top** row (the original flips the image vertically and saves
     a bottom-up BMP: the two cancel out);
   - 64 bytes per row, most significant bit = leftmost pixel;
   - **bit 1 = white, bit 0 = burn**;
   - 62-byte header. Its resolution and colour fields are assumed (typical GDI+ values);
     EzGraver sends a different header and works, so the firmware almost certainly ignores them.
4. Within half a second the machine moves by itself to the centre of the new image (measured).
5. Start with `F1 tt` (a separate click in the original; `neje engrave` waits 2 s first).

### How the firmware engraves

All measured.

- Row by row from the top, in a **zig-zag**: the first row right to left, the next left to
  right. On each row the head goes from the first to the last black pixel, burning the black
  ones and crossing the white ones in between. Empty rows and the white margins are skipped.
- There is no "laser on" and no "go to X, Y" command: a vector path is impossible. What can
  be chosen is the image, which is what the print modes of `neje` do (see the README).
- Orientation: seen from the front, the engraving looks like the preview, upright and not
  mirrored. Image X grows to the operator's right, image Y towards the operator; row 0 is the
  farthest one.
- Pixel pitch: **0.068–0.070 mm**, not the 0.075 mm of the manual, so the work area is about
  33–34 mm wide, not 36.75. The two axes may differ slightly (not established).
- The first rows (Y below about 16) may be out of reach: after the machine finds its edges,
  the origin of the plate lies about 1 mm beyond the end stop (guess from the movements, not
  checked with an engraving).

### Where the time goes

| Item | Time | Source |
|---|---|---|
| Burnt pixel | 2.5 ms + 0.5 ms × burn time | measured at burn time 10, 20 and 40 |
| White pixel crossed between two black stretches of a row | 1.73 ms | measured (least squares over 320 rows) |
| Row change | about 1 ms | measured |
| Upload | 3.5 s of erase wait + 5.7 s of data | measured |
| From `F1` to the first position | 0.8 to 5.3 s, depending on how far the first pixel is from the centre of the image | measured |

`neje` uses these numbers (with 3 s for start and end of each job) to estimate the duration
and to decide when splitting an image in several jobs pays off. Check on the machine: a
drawing estimated at 199 s as one job and 143 s as two took about 200 s and 148 s, and the
two jobs lined up on the piece. A new upload right after `66` works without `F9`; the
engraver must not be restarted or switched off between the jobs.

A line one pixel wide is engraved as one continuous pass; an 80 px tall letter took 3.9 s
with a 1 px stroke, 7.4 s with 2 px and about 11 s with 3 px, all readable (burn time 40).

### Preparing the picture (as the original program does)

1. Shrink to fit 490×490 keeping the proportions (integer divisions, bilinear filter).
2. Grey: Y = 0.299 R + 0.587 G + 0.114 B.
3. Black and white with the **Otsu** threshold, or NEJE's own error diffusion ("dithering":
   3/8 right, 3/8 below, 2/8 diagonal, threshold (min+max)/2, pixels above 230 forced white).
4. Centre on the 490×490 area.

The Python prototype reproduces the decompiled C# bit for bit (including its quirks, such as
counting the row padding in the histogram); the Rust library reproduces the prototype byte
for byte (see `tests/parity`).

## Protocols V20, V21, V30 (later models)

4-byte frames `FF cc pp pp` (decompiled, partly external through EzGraver; never tried on
hardware by this project).

| Function | Bytes |
|---|---|
| Handshake | `FF 09 00 00` |
| Start / pause / start again | `FF 01 01 00` / `FF 01 02 00` / `FF 01 03 00` |
| Centre / outline | `FF 02 01 00` / `FF 02 02 00` |
| Up / down / left / right | `FF 03 01..04 00` |
| Power on (also "Reboot") / off | `FF 04 01 00` / `FF 04 02 00` |
| Burn time | `FF 05 tt 00` |
| Erase and get ready to receive | `FF 06 01 00`, then wait for `FF 05 01 00` |
| Go to X, Y | `FF 0A xh xl` + `FF 0B yh yl` |
| Forward / back (V20, V21) | `FF 07 02 00` / `FF 07 01 00` |

Replies: `FF 03 xh xl` and `FF 04 yh yl` position, `FF 05 01 xx` erase done, `FF 06 xx xx`
finished, `FF 07 01` / `02` sleeping (battery / idle), `FF 08` debug, `FF 09 nn` battery,
`FF 0A` burn time, `FF 0B` verification OK (V30).

Image: pixel data only, no header, **1 = burn**: 576×552 (V20, 39 744 bytes), 512×512
(V21 and V30, 32 768 bytes), 552×572 (V30 hardware 102).

The print modes that split an image in several jobs are only used with V10: nothing is known
about how these firmwares behave between two jobs.

## Never sent by this program

- `FA vv` (motor speed, mentioned only in nejemojo's README);
- anything that writes the firmware. The microcontroller (probably an STC IAP15F2K61S2) cannot
  be read back, so replacing its firmware cannot be undone.

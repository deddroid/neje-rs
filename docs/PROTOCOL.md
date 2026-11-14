# Serial protocol of the NEJE DK-8-KZ

How `neje` talks to the engraver, and where each fact comes from.

- **decompiled**: read in the official Windows software NEJE V3.6 (.NET, decompiled for
  interoperability; its code is not part of this repository). It tells what the PC sends and
  expects, not what the firmware does with it.
- **external**: confirmed by an independent project ([EzGraver](https://github.com/camrein/EzGraver),
  [nejemojo](https://github.com/sthysel/nejemojo)) or by the manual.
- **verified**: seen on a real DK-8-KZ (V10 firmware, CH340 `1a86:7523`), with the date.
- **guess**: deduced, still to be tried.

Hardware checks so far were made on 14/11/2026 with the Python prototype this program was
ported from. **The Rust program itself has not driven a real machine yet.**

## Physical layer

| Fact | Source |
|---|---|
| USB-serial converter WCH CH340 (`1a86:7523`), Linux driver `ch341`, port `/dev/ttyUSB0` | verified 14/11/2026 |
| 57600 baud, 8 data bits, no parity, 1 stop bit, no flow control, DTR and RTS low | decompiled, external (EzGraver) |
| Opening the port on Linux does not restart the machine | verified 14/11/2026 |
| The converter is powered by the 5 V supply: without it the port disappears | verified 14/11/2026 |
| The laser glows at low power from the moment the supply is plugged in | verified 14/11/2026 |

## Detecting the firmware generation

The original launcher opens every serial port and tries, in order:

| Sent | Reply looked for (anywhere in what arrives) | Protocol |
|---|---|---|
| `F6` | `65` | V10 |
| `FF 09 00 00` | `FF 01 00 00` | V20 |
| `FF 09 00 00` | `FF 02 64` | V21 |
| `FF 09 00 00` | `FF 02 0B` / `0C` / `0D` | V30, hardware 11 / 12 / 13 |

(decompiled.) A DK-8-KZ answers `F6` with `65 70` after about 20 ms (verified 14/11/2026);
the meaning of `70` is unknown and the original ignores it.

`neje` differs from the launcher on purpose:

- by default it only speaks **V10**. A V10 firmware probably reads the lone byte `09` of the
  4-byte handshake as "burn time 9" (guess), so `FF 09 00 00` is sent only with
  `--protocol auto` or a 4-byte protocol, once, and only after `F6` got no reply for 500 ms;
- without `--port` it probes only CH340 converters, never unrelated serial devices.

## Protocol V10

### Commands, PC to engraver

| Function | Bytes | Laser | Source |
|---|---|---|---|
| Handshake | `F6` | no | decompiled; verified 14/11/2026 |
| Start / resume engraving the image in memory | `F1 tt` | **full power** | decompiled, external |
| Burn time (`tt` = 1–120, default 30) | `tt` | no | decompiled, external |
| Pause | `F2` | off (guess) | decompiled, external |
| Restart ("Reboot") | `F9` | no (guess) | decompiled, external |
| To the origin | `F3` | weak light | decompiled; movement verified 14/11/2026 (head left, plate towards the operator) |
| To the centre | `FB` | weak light | decompiled; movement verified 14/11/2026 |
| Outline of the image ("Box locating") | `F4` | probably weak light | decompiled, external |
| Up / down / left / right | `F5 01` / `02` / `03` / `04` | weak light | decompiled; `F5 03`/`04` verified 14/11/2026 |
| Back / forward in the engraving | `FC 55` / `FC AA` | yes while engraving (guess) | decompiled |
| Redo | `FC 77` | probably yes (guess) | decompiled |
| Erase the image memory | `FE` × 8 | no | decompiled, external |

Notes from the machine (14/11/2026):

- the head only moves on X, the plate only on Y;
- `F5` moves the motors without changing the firmware's idea of the position: it shifts where
  the whole image will land on the piece, and `FB` does not undo it. Ten `F5 04` steps move
  the head by a few millimetres, so a step is not a pixel;
- there are no limit switches: steps against the end stop are executed anyway. Unplugging and
  plugging the 5 V supply makes the machine find its edges again;
- `F5 02` only made the plate vibrate in two attempts out of two (cause unknown).

`F1` engraves whatever image is in memory, which survives power off: on a machine that has
just been plugged in that is the last image sent, or the factory one (external, manual).

### Messages, engraver to PC

| Bytes | Meaning | Source |
|---|---|---|
| `65` | connected | decompiled; verified 14/11/2026 |
| `66` | engraving finished | decompiled |
| `78` | no power supply | decompiled (never seen: without the supply there is no port) |
| `79` | power supply present | decompiled |
| `FF xh xl yh yl` | position: X = xh·100 + xl, Y = yh·100 + yl, in pixels of the 490×490 image | decompiled, external |

A `FF` opens a 5-byte packet whose bytes are not interpreted individually; any other byte is
ignored (`neje monitor` shows it as "unknown byte").

### Sending an image

1. `FE` × 8.
2. Wait 3 s with no confirmation from the firmware (decompiled; EzGraver waits 6 s). `neje`
   waits 3.5 s (`--erase-wait`). Data sent during the erase is lost.
3. 32 830 bytes as one continuous stream: a complete 1-bit BMP file.
   - 512×512 canvas, white, with the 490×490 image at the top left;
   - the first data row is the **top** row (the original flips the image vertically and saves
     a bottom-up BMP: the two cancel out);
   - 64 bytes per row, most significant bit = leftmost pixel;
   - **bit 1 = white, bit 0 = burn**;
   - 62-byte header. Its resolution and colour fields are assumed (typical GDI+ values);
     EzGraver sends a different header and works, so the firmware almost certainly ignores them.
4. Start with `F1 tt` (a separate click in the original; `neje engrave` waits 2 s, a guess).

Work area: 490 px × 0.075 mm = 36.75 mm (manual).

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

## Never sent by this program

- `FA vv` (motor speed, mentioned only in nejemojo's README);
- anything that writes the firmware. The microcontroller (probably an STC IAP15F2K61S2) cannot
  be read back, so replacing its firmware cannot be undone.

# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-11-15

First release.

### Added

- Image preparation as in the original NEJE V3.6 program: shrink to the 490×490 work area,
  grey, black and white with the Otsu threshold or NEJE's dithering. Pictures, text
  (built-in font) and G-code drawings can be combined, rotated and shrunk.
- Serial control of the engraver: detection, erase, upload, start, pause, progress and end
  of job, for protocol V10 (DK-8-KZ) and, untested on hardware, V20, V21 and V30.
- Print modes: single job, split in vertical bands engraved one after the other, and line
  art with a stroke of 1 to 6 pixels. Each has a description, a picture of the plan (empty
  space crossed and saved, cuts) and a time estimate, in `neje preview` and in the window.
- `neje` command line and `neje-gui` window, in English and Italian, following the system
  theme. Commands that switch the laser on ask for confirmation.
- `neje doctor` and a banner in the window detect a missing driver binding or permission
  on the serial port and can install the udev rule.
- Virtual engraver (`neje simulate`) to try everything without the machine.
- Packages: AUR (`neje`, `neje-bin`), archives for Linux and Windows, `install.sh` for a
  per-user installation, udev rule with `uaccess`.

### Verified on hardware

- On a DK-8-KZ with V10 firmware (14–15/11/2026) the commands, the image format, the timing
  model and the split in jobs were measured with the Python prototype and scripts that send
  the same bytes as this program (see `docs/PROTOCOL.md`).
- This program itself has not driven a real engraver yet; the line art it generates has
  not been engraved; the Windows build has never been tried with an engraver.

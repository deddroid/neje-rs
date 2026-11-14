# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Library: commands, messages and image payload of protocols V10, V20, V21 and V30; image
  preparation (shrink, grey, Otsu, NEJE dithering); G-code and text to bitmap. Same bytes
  as the Python prototype (`tests/parity`).
- Serial session with detection, erase, upload, start and progress; virtual engraver on a
  pseudo-terminal.
- `neje` command line and `neje-gui` window (egui), in English and Italian.
- udev rule (`uaccess`), desktop launcher and `install.sh` for a per-user installation.

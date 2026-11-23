//! The fonts installed on the system, by name, for the font picker of the window.
//!
//! Only the table directory and the `name` table of each file are read: a system can hold
//! gigabytes of fonts (CJK collections are tens of megabytes each) and the scan must stay
//! quick.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::text::{self, FontSource};

const EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];
/// Folder levels followed under each font folder.
const MAX_DEPTH: u32 = 8;
/// Faces read from one collection, and bytes of one name table, at most. The face limit
/// only guards against a corrupt count; real collections hold far fewer.
const MAX_FACES: u32 = 1024;
const MAX_NAME_TABLE: u32 = 1 << 20;

/// One face of an installed font file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontEntry {
    pub family: String,
    pub style: String,
    pub path: PathBuf,
    /// Face in the file (0 unless the file is a collection).
    pub index: u32,
}

impl FontEntry {
    /// "Family Style", as shown in the list.
    pub fn label(&self) -> String {
        format!("{} {}", self.family, self.style)
    }

    /// The font to load this face with.
    pub fn source(&self) -> FontSource {
        FontSource::File { path: self.path.clone(), index: self.index }
    }
}

/// The faces in the system font folders, sorted by name, one per family and style.
pub fn scan() -> Vec<FontEntry> {
    scan_dirs(&text::font_dirs())
}

/// The faces under `dirs`, sorted by name, one per family and style.
pub fn scan_dirs(dirs: &[PathBuf]) -> Vec<FontEntry> {
    let mut found = Vec::new();
    for dir in dirs {
        walk(dir, MAX_DEPTH, &mut found);
    }
    found.sort_by_cached_key(|f| (f.family.to_lowercase(), f.style.to_lowercase(), f.path.clone(), f.index));
    found.dedup_by(|later, kept| {
        later.family.to_lowercase() == kept.family.to_lowercase()
            && later.style.to_lowercase() == kept.style.to_lowercase()
    });
    found
}

/// Adds the faces of the font files under `dir`. Links to folders are not followed, so a
/// loop of links cannot trap the scan, and only regular files (or links to them) are opened,
/// since opening a FIFO would block.
fn walk(dir: &Path, depth: u32, found: &mut Vec<FontEntry>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if depth > 0 {
                walk(&path, depth - 1, found);
            }
        } else if is_font_file(&path)
            && (kind.is_file() || (kind.is_symlink() && std::fs::metadata(&path).is_ok_and(|m| m.is_file())))
        {
            found.extend(faces(&path));
        }
    }
}

fn is_font_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Every face of the font file at `path` that has a name; nothing when it cannot be read.
fn faces(path: &Path) -> Vec<FontEntry> {
    let Ok(mut file) = File::open(path) else { return Vec::new() };
    let offsets = face_offsets(&mut file).unwrap_or_default();
    offsets
        .into_iter()
        .enumerate()
        .filter_map(|(index, offset)| {
            let (family, style) = names(&name_table(&mut file, offset)?)?;
            Some(FontEntry { family, style, path: path.to_path_buf(), index: index as u32 })
        })
        .collect()
}

fn read_at(file: &mut File, offset: u64, len: usize) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0; len];
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn be16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Where the table directory of each face starts: 0 for a single font, the list in the
/// header of a collection (`ttcf`).
fn face_offsets(file: &mut File) -> Option<Vec<u32>> {
    let head = read_at(file, 0, 12)?;
    if &head[..4] != b"ttcf" {
        return Some(vec![0]);
    }
    let count = be32(&head, 8).min(MAX_FACES) as usize;
    let table = read_at(file, 12, count * 4)?;
    Some((0..count).map(|i| be32(&table, i * 4)).collect())
}

/// The `name` table of the face whose table directory starts at `offset`. Table offsets
/// count from the start of the file, in collections too.
fn name_table(file: &mut File, offset: u32) -> Option<Vec<u8>> {
    let directory = read_at(file, u64::from(offset), 12)?;
    let tables = usize::from(be16(&directory, 4));
    let records = read_at(file, u64::from(offset) + 12, tables * 16)?;
    let record = records.as_chunks::<16>().0.iter().find(|r| &r[..4] == b"name")?;
    let (start, len) = (be32(record, 8), be32(record, 12));
    if len > MAX_NAME_TABLE {
        return None;
    }
    read_at(file, u64::from(start), len as usize)
}

/// Family and style, preferring the typographic names (16, 17) and US English.
fn names(table: &[u8]) -> Option<(String, String)> {
    use ttf_parser::name_id;
    let table = ttf_parser::name::Table::parse(table)?;
    let find = |id: u16| {
        let (mut english, mut other) = (None, None);
        for name in table.names {
            if name.name_id != id {
                continue;
            }
            let Some(text) = name.to_string().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) else {
                continue;
            };
            if name.language() == ttf_parser::Language::English_UnitedStates {
                english.get_or_insert(text);
            } else {
                other.get_or_insert(text);
            }
        }
        english.or(other)
    };
    let family = find(name_id::TYPOGRAPHIC_FAMILY).or_else(|| find(name_id::FAMILY))?;
    let style = find(name_id::TYPOGRAPHIC_SUBFAMILY)
        .or_else(|| find(name_id::SUBFAMILY))
        .unwrap_or_else(|| "Regular".to_owned());
    Some((family, style))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts")
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("neje-fonts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A collection whose two faces are both `font` (a single-face font file).
    fn collection_of_two(font: &[u8]) -> Vec<u8> {
        const HEADER: u32 = 20;
        let mut out = Vec::new();
        out.extend_from_slice(b"ttcf");
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&HEADER.to_be_bytes());
        out.extend_from_slice(&HEADER.to_be_bytes());
        let mut copy = font.to_vec();
        for i in 0..usize::from(be16(&copy, 4)) {
            let at = 12 + i * 16 + 8;
            let moved = be32(&copy, at) + HEADER;
            copy[at..at + 4].copy_from_slice(&moved.to_be_bytes());
        }
        out.extend_from_slice(&copy);
        out
    }

    #[test]
    fn finds_the_bundled_fonts_by_name() {
        let dir = scratch("scan");
        std::fs::create_dir_all(dir.join("nested/deeper")).unwrap();
        std::fs::copy(assets().join("Inter-Regular.ttf"), dir.join("Inter-Regular.ttf")).unwrap();
        std::fs::copy(assets().join("Inter-Regular.ttf"), dir.join("nested/Inter-copy.ttf")).unwrap();
        std::fs::copy(assets().join("Rajdhani-SemiBold.ttf"), dir.join("nested/deeper/RAJDHANI.TTF")).unwrap();
        std::fs::write(dir.join("notes.ttf"), b"not a font at all").unwrap();
        std::fs::write(dir.join("readme.txt"), b"hello").unwrap();

        let found = scan_dirs(&[dir.clone(), dir.join("missing")]);
        let labels: Vec<String> = found.iter().map(FontEntry::label).collect();
        assert_eq!(labels, ["Inter Regular", "Rajdhani SemiBold"]);
        assert_eq!(found[1].path, dir.join("nested/deeper/RAJDHANI.TTF"));
        for face in &found {
            text::load(&face.source()).unwrap();
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_face_of_a_collection_is_listed() {
        let dir = scratch("ttc");
        let inter = std::fs::read(assets().join("Inter-Regular.ttf")).unwrap();
        let path = dir.join("Pair.ttc");
        std::fs::write(&path, collection_of_two(&inter)).unwrap();

        let found = faces(&path);
        assert_eq!(found.iter().map(|f| f.index).collect::<Vec<_>>(), [0, 1]);
        assert!(found.iter().all(|f| f.family == "Inter" && f.style == "Regular"), "{found:?}");
        for face in &found {
            text::load(&face.source()).unwrap();
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn fifos_are_skipped() {
        use nix::sys::stat::Mode;
        let dir = scratch("fifo");
        std::fs::copy(assets().join("Inter-Regular.ttf"), dir.join("Inter-Regular.ttf")).unwrap();
        nix::unistd::mkfifo(&dir.join("pipe.ttf"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        std::os::unix::fs::symlink(dir.join("pipe.ttf"), dir.join("link.ttf")).unwrap();

        let labels: Vec<String> = scan_dirs(std::slice::from_ref(&dir)).iter().map(FontEntry::label).collect();
        assert_eq!(labels, ["Inter Regular"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

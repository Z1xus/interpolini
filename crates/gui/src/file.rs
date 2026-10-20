use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::Xxh3;

use interpolini_core::config::Config;

use crate::project::{Look, View};
use crate::state::{Saved, Tracks};

const MAGIC: &[u8; 4] = b"LINI";
const VERSION: u16 = 1;
const HEADER: usize = 6;
const LEVEL: i32 = 1;
const MOST: usize = 256 << 20;
// bytes of each end of a file that the hash reads
const EDGE: u64 = 64 << 10;

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Document {
    pub app: String,
    pub path: String,
    pub look: Look,
    pub draft: Saved,
    pub draft_config: u32,
    pub configs: Vec<Config>,
    pub media: Vec<Media>,
    pub clips: Vec<Clip>,
    pub tracks: Tracks,
    pub view: View,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Clip {
    pub media: u32,
    pub config: u32,
    pub saved: Saved,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Media {
    pub absolute: String,
    pub relative: String,
    pub bytes: u64,
    pub mtime: i64,
    pub hash: u64,
}

fn damaged(_: impl ToString) -> String {
    "the file is damaged".into()
}

pub fn encode(document: &Document) -> Result<Vec<u8>, String> {
    let body = rmp_serde::to_vec_named(document).map_err(|error| error.to_string())?;
    let mut bytes = MAGIC.to_vec();
    bytes.extend(VERSION.to_le_bytes());
    let mut packer = zstd::Encoder::new(bytes, LEVEL).map_err(|error| error.to_string())?;
    packer
        .include_checksum(true)
        .map_err(|error| error.to_string())?;
    packer.write_all(&body).map_err(|error| error.to_string())?;
    packer.finish().map_err(|error| error.to_string())
}

pub fn decode(bytes: &[u8]) -> Result<Document, String> {
    if bytes.len() < HEADER || &bytes[..4] != MAGIC {
        return Err("it is not a project".into());
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version > VERSION {
        return Err("it needs a newer version".into());
    }
    let unpacker = zstd::Decoder::new(&bytes[HEADER..]).map_err(damaged)?;
    let mut body = Vec::new();
    unpacker
        .take(MOST as u64)
        .read_to_end(&mut body)
        .map_err(damaged)?;
    rmp_serde::from_slice(&body).map_err(damaged)
}

pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".part");
    let part = path.with_file_name(name);
    let result = File::create(&part).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&part, path)
    });
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

pub fn modified(meta: &fs::Metadata) -> i64 {
    let time = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
    time.map_or(0, |time| time.as_secs() as i64)
}

fn edges(path: &Path, bytes: u64) -> io::Result<u64> {
    let mut file = File::open(path)?;
    let mut hash = Xxh3::new();
    let mut buffer = vec![0; EDGE as usize];
    let first = bytes.min(EDGE) as usize;
    file.read_exact(&mut buffer[..first])?;
    hash.update(&buffer[..first]);
    if bytes > EDGE {
        let start = (bytes - EDGE).max(EDGE);
        let last = (bytes - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..last])?;
        hash.update(&buffer[..last]);
    }
    Ok(hash.digest())
}

fn name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn relative(folder: &Path, file: &Path) -> String {
    if !folder.is_absolute() || !file.is_absolute() {
        return String::new();
    }
    let (from, to): (Vec<_>, Vec<_>) = (folder.components().collect(), file.components().collect());
    let same = from
        .iter()
        .zip(&to)
        .take_while(|pair| pair.0 == pair.1)
        .count();
    if same == 0 {
        return String::new();
    }
    let up = std::iter::repeat_n("..".to_owned(), from.len() - same);
    let down = to[same..]
        .iter()
        .map(|part| part.as_os_str().to_string_lossy().into_owned());
    up.chain(down).collect::<Vec<_>>().join("/")
}

impl Media {
    pub fn read(path: &Path, project: Option<&Path>) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        let folder = project.and_then(Path::parent);
        Ok(Self {
            absolute: path.to_string_lossy().into_owned(),
            relative: folder.map_or_else(String::new, |folder| relative(folder, path)),
            bytes: meta.len(),
            mtime: modified(&meta),
            hash: edges(path, meta.len())?,
        })
    }

    pub fn matches(&self, path: &Path) -> bool {
        let Ok(meta) = fs::metadata(path) else {
            return false;
        };
        if !meta.is_file() || meta.len() != self.bytes {
            return false;
        }
        modified(&meta) == self.mtime || edges(path, self.bytes).is_ok_and(|hash| hash == self.hash)
    }

    pub fn rebase(&mut self, project: &Path) {
        let folder = project.parent().unwrap_or(Path::new(""));
        self.relative = relative(folder, Path::new(&self.absolute));
    }
}

pub fn find(media: &Media, project: &Path, old: &str) -> Option<(PathBuf, bool)> {
    let folder = project.parent().unwrap_or(Path::new(""));
    let mut exact = Vec::new();
    if !media.relative.is_empty() {
        let parts = media
            .relative
            .split(['/', '\\'])
            .filter(|part| !part.is_empty());
        exact.push(parts.fold(folder.to_path_buf(), |path, part| path.join(part)));
    }
    exact.push(PathBuf::from(&media.absolute));
    let file = name(&media.absolute);
    let before = old.rsplit_once(['/', '\\']).map(|(before, _)| before);
    let near = [Some(folder.to_path_buf()), before.map(PathBuf::from)];
    let near = near.into_iter().flatten().map(|folder| folder.join(file));
    let mut changed = None;
    for path in exact {
        if path.is_file() && media.matches(&path) {
            return Some((path, false));
        }
        if path.is_file() {
            changed.get_or_insert(path);
        }
    }
    near.into_iter()
        .find(|path| path.is_file() && media.matches(path))
        .map(|path| (path, false))
        .or(changed.map(|path| (path, true)))
}

pub fn index(folder: &Path) -> HashMap<String, Vec<PathBuf>> {
    let mut found: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut folders = vec![(folder.to_path_buf(), 0)];
    let mut count = 0;
    while let Some((folder, depth)) = folders.pop() {
        for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
            count += 1;
            if count > 50_000 {
                return found;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth < 4 {
                folders.push((path, depth + 1));
            } else if kind.is_file() {
                let key = entry.file_name().to_string_lossy().to_lowercase();
                found.entry(key).or_default().push(path);
            }
        }
    }
    found
}

pub fn pick(index: &HashMap<String, Vec<PathBuf>>, media: &Media) -> Option<PathBuf> {
    let paths = index.get(&name(&media.absolute).to_lowercase())?;
    paths.iter().find(|path| media.matches(path)).cloned()
}

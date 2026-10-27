//! The music tag index on disk, in `tags.txt` next to the library state, so Songs, Albums,
//! Artists and Genres fill in at once on the next start. Each line is one file with its size and
//! modification time; a file that changed since is probed again.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::library::Track;

const FILE: &str = "tags.txt";
const HEADER: &str = "# myvlc tags v1";

/// A file's size and modification time (in FILETIME units): when either changes, its tags may
/// have too.
pub type Stamp = (u64, u64);

/// The stamp of the file at `path`, None when it can't be read.
pub fn stamp(path: &Path) -> Option<Stamp> {
    fs::metadata(path)
        .ok()
        .map(|m| (m.len(), m.last_write_time()))
}

/// The saved tags by path; a missing or unreadable file is an empty index.
pub fn load(dir: &Path) -> HashMap<PathBuf, (Stamp, Track)> {
    let text = fs::read_to_string(dir.join(FILE)).unwrap_or_default();
    let tag = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let [path, size, time, duration, title, artist, album, genre] = f[..] else {
                return None;
            };
            let path = PathBuf::from(path);
            let track = Track {
                path: path.clone(),
                title: tag(title),
                artist: tag(artist),
                album: tag(album),
                genre: tag(genre),
                duration: duration.parse().ok().filter(|d: &f64| d.is_finite())?,
            };
            Some((path, ((size.parse().ok()?, time.parse().ok()?), track)))
        })
        .collect()
}

/// Writes the index through a temporary file, so a crash mid-write keeps the old one.
pub fn save(dir: &Path, tracks: &HashMap<PathBuf, (Stamp, Track)>) -> io::Result<()> {
    // Tabs and line breaks in a tag would split its line.
    let clean = |s: &Option<String>| s.as_deref().unwrap_or("").replace(['\t', '\r', '\n'], " ");
    let mut out = format!("{HEADER}\n");
    for (path, ((size, time), t)) in tracks {
        let Some(path) = path.to_str() else { continue };
        _ = writeln!(
            out,
            "{path}\t{size}\t{time}\t{}\t{}\t{}\t{}\t{}",
            t.duration,
            clean(&t.title),
            clean(&t.artist),
            clean(&t.album),
            clean(&t.genre),
        );
    }
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    fs::write(&tmp, out)?;
    fs::rename(tmp, dir.join(FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_tags() {
        let dir = std::env::temp_dir().join(format!("myvlc-tags-{}", std::process::id()));
        let song = PathBuf::from(r"E:\Music\Album\01 Song.mp3");
        let track = Track {
            path: song.clone(),
            title: Some("A\tB".into()),
            artist: Some("Artist".into()),
            album: None,
            genre: Some("Rock".into()),
            duration: 201.5,
        };
        let tracks = HashMap::from([(song.clone(), ((4096, 133_000_000_000_000_000), track))]);
        save(&dir, &tracks).unwrap();
        // A broken line is skipped, not the whole file.
        let file = dir.join(FILE);
        let text = fs::read_to_string(&file).unwrap() + "C:\\half\t12\n";
        fs::write(&file, text).unwrap();

        let loaded = load(&dir);
        assert_eq!(loaded.len(), 1);
        let (stamp, t) = &loaded[&song];
        assert_eq!(*stamp, (4096, 133_000_000_000_000_000));
        assert_eq!(t.title.as_deref(), Some("A B"));
        assert_eq!((t.album.as_deref(), t.duration), (None, 201.5));
        fs::remove_dir_all(dir).unwrap();
        assert!(load(Path::new(r"Z:\nowhere")).is_empty());
    }
}

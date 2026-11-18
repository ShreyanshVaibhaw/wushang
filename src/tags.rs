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
/// Version 2 escapes the path, which version 1 wrote as it was.
const HEADER: &str = "# wushang tags v2";

/// `path` with backslashes, tabs and line breaks written as `\\`, `\t`, `\n` and `\r`, so any
/// file name (macOS allows all of them) stays on its own line and in its own field.
fn escape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// The path `escape` wrote, None for an escape it never writes.
fn unescape(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        out.push(match c {
            '\\' => match chars.next()? {
                '\\' => '\\',
                't' => '\t',
                'n' => '\n',
                'r' => '\r',
                _ => return None,
            },
            c => c,
        });
    }
    Some(out)
}

/// A file's size and modification time (in FILETIME units): when either changes, its tags may
/// have too.
pub type Stamp = (u64, u64);

/// The stamp of the file at `path`, None when it can't be read.
pub fn stamp(path: &Path) -> Option<Stamp> {
    fs::metadata(path)
        .ok()
        .map(|m| (m.len(), m.last_write_time()))
}

/// The saved tags by path; a missing or unreadable file is an empty index. A save still on its
/// way to disk lands first.
pub fn load(dir: &Path) -> HashMap<PathBuf, (Stamp, Track)> {
    crate::library::finish_writes();
    let text = fs::read_to_string(dir.join(FILE)).unwrap_or_default();
    let tag = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    // Version 1 files have their paths as they were.
    let escaped = text.lines().next() == Some(HEADER);
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let [path, size, time, duration, title, artist, album, genre] = f[..] else {
                return None;
            };
            let path = PathBuf::from(if escaped {
                unescape(path)?
            } else {
                path.to_owned()
            });
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

/// Writes the index through a temporary file, so a crash mid-write keeps the old one. The
/// writing happens on the library's writer thread: the render loop saves every 30 seconds, and
/// the index can run to megabytes.
pub fn save(dir: &Path, tracks: &HashMap<PathBuf, (Stamp, Track)>) -> io::Result<()> {
    // Tabs and line breaks in a tag would split its line.
    let clean = |s: &Option<String>| s.as_deref().unwrap_or("").replace(['\t', '\r', '\n'], " ");
    let mut out = format!("{HEADER}\n");
    for (path, ((size, time), t)) in tracks {
        let Some(path) = path.to_str() else { continue };
        _ = writeln!(
            out,
            "{}\t{size}\t{time}\t{}\t{}\t{}\t{}\t{}",
            escape(path),
            t.duration,
            clean(&t.title),
            clean(&t.artist),
            clean(&t.album),
            clean(&t.genre),
        );
    }
    fs::create_dir_all(dir)?;
    crate::library::write_later(dir.join(FILE), out.into_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_tags() {
        let dir = std::env::temp_dir().join(format!("wushang-tags-{}", std::process::id()));
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
        // Saving writes on the library's writer thread.
        crate::library::finish_writes();
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

    /// Tabs, line breaks and backslashes are legal in macOS file names. Such a name keeps its
    /// tags, and can't split its line into an entry for another file.
    #[test]
    fn any_file_name_keeps_its_line() {
        let dir = std::env::temp_dir().join(format!("wushang-tags-names-{}", std::process::id()));
        let track = |path: &Path| Track {
            path: path.to_owned(),
            title: Some("T".into()),
            artist: None,
            album: None,
            genre: None,
            duration: 1.0,
        };
        let names = [
            "/Music/a\tb.mp3",
            "/Music/c\\d\\t.mp3",
            "/Music/x\n/Music/forged.mp3\t1\t2\t3\tForged\t\t\t",
            "/Music/cr\r.mp3",
        ];
        let tracks: HashMap<_, _> = names
            .iter()
            .map(|n| (PathBuf::from(n), ((7, 8), track(Path::new(n)))))
            .collect();
        save(&dir, &tracks).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.len(), names.len(), "{:?}", loaded.keys());
        for name in names {
            let (stamp, t) = &loaded[Path::new(name)];
            assert_eq!((*stamp, t.path.as_path()), ((7, 8), Path::new(name)));
        }
        // A file from before escaping reads as it was written.
        let v1 = "# wushang tags v1\n/Music/old\\one.mp3\t1\t2\t3\tOld\t\t\t\n";
        fs::write(dir.join(FILE), v1).unwrap();
        assert!(load(&dir).contains_key(Path::new("/Music/old\\one.mp3")));
        fs::remove_dir_all(dir).unwrap();
    }
}

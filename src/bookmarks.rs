//! Bookmarks: named positions in media files, kept in `bookmarks.txt` next to the library state,
//! one per line as `path<TAB>seconds<TAB>name`. Lines that don't parse are skipped, so a hand
//! edit can't lose the rest.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const FILE: &str = "bookmarks.txt";
const HEADER: &str = "# myvlc bookmarks v1";

#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub path: PathBuf,
    pub seconds: f64,
    pub name: String,
}

#[derive(Debug, Default)]
pub struct Bookmarks {
    file: PathBuf,
    /// Every file's bookmarks, each file's in time order.
    items: Vec<Bookmark>,
}

impl Bookmarks {
    /// Reads `dir`\bookmarks.txt; a missing or unreadable file means no bookmarks.
    pub fn load(dir: &Path) -> Bookmarks {
        let file = dir.join(FILE);
        let items = fs::read_to_string(&file).map_or_else(|_| Vec::new(), |t| parse(&t));
        Bookmarks { file, items }
    }

    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut text = format!("{HEADER}\n");
        for b in &self.items {
            let name = b.name.replace(['\t', '\r', '\n'], " ");
            text += &format!("{}\t{:.3}\t{name}\n", b.path.display(), b.seconds);
        }
        // Write then rename, so a crash mid-write leaves the old file whole.
        let tmp = self.file.with_extension("txt.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.file)
    }

    /// `path`'s bookmarks, earliest first.
    pub fn for_file(&self, path: &Path) -> Vec<&Bookmark> {
        let path = key(path);
        self.items.iter().filter(|b| b.path == path).collect()
    }

    /// Adds "Bookmark N" at `seconds`, N one more than the highest so far for the file, and
    /// returns its index among the file's bookmarks.
    pub fn add(&mut self, path: &Path, seconds: f64) -> usize {
        let key = key(path);
        let path = key.as_path();
        let n = self
            .for_file(path)
            .iter()
            .filter_map(|b| b.name.strip_prefix("Bookmark ")?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let seconds = if seconds.is_finite() {
            seconds.max(0.0)
        } else {
            0.0
        };
        // After the file's last bookmark that isn't later, so for_file stays in time order.
        let at = match (self.items.iter()).rposition(|b| b.path == path && b.seconds <= seconds) {
            Some(i) => i + 1,
            None => (self.items.iter())
                .position(|b| b.path == path)
                .unwrap_or(self.items.len()),
        };
        self.items.insert(
            at,
            Bookmark {
                path: path.to_owned(),
                seconds,
                name: format!("Bookmark {n}"),
            },
        );
        self.items[..at].iter().filter(|b| b.path == path).count()
    }

    /// Removes the `i`th of `path`'s bookmarks.
    pub fn remove(&mut self, path: &Path, i: usize) -> Option<Bookmark> {
        let path = key(path);
        let at = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, b)| b.path == path)
            .nth(i)?
            .0;
        Some(self.items.remove(at))
    }
}

/// Files are keyed by absolute path, so a file opened from the command line by a relative
/// path finds its bookmarks too.
fn key(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

fn parse(text: &str) -> Vec<Bookmark> {
    let mut items: Vec<Bookmark> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.trim_end_matches('\r').splitn(3, '\t');
            let path = fields.next().filter(|p| !p.is_empty())?;
            let seconds: f64 = fields.next()?.trim().parse().ok()?;
            let name = fields.next()?.trim();
            (seconds.is_finite() && seconds >= 0.0).then(|| Bookmark {
                path: PathBuf::from(path),
                seconds,
                name: name.to_owned(),
            })
        })
        .collect();
    // A stable sort by time keeps each file's bookmarks in order wherever they sit.
    items.sort_by(|a, b| a.seconds.total_cmp(&b.seconds));
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("myvlc-bookmarks-{name}-{}", std::process::id()));
        _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn parse_skips_bad_lines() {
        let text = "# myvlc bookmarks v1\n\
                    C:\\v\\a.mp4\t75.5\tIntro\n\
                    not a bookmark\n\
                    C:\\v\\a.mp4\tsoon\tBad time\n\
                    C:\\v\\a.mp4\t-3\tNegative\n\
                    \t10\tNo path\n\
                    C:\\v\\a.mp4\t12\tEarlier\r\n\
                    C:\\v\\b.mkv\t1\tName\twith a tab\n";
        let items = parse(text);
        let summary: Vec<(&str, f64, &str)> = items
            .iter()
            .map(|b| (b.path.to_str().unwrap(), b.seconds, b.name.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (r"C:\v\b.mkv", 1.0, "Name\twith a tab"),
                (r"C:\v\a.mp4", 12.0, "Earlier"),
                (r"C:\v\a.mp4", 75.5, "Intro"),
            ]
        );
    }

    #[test]
    fn add_remove_save_and_load() {
        let dir = temp_dir("roundtrip");
        let mut b = Bookmarks::load(&dir);
        let (a, other) = (Path::new(r"C:\v\a.mp4"), Path::new(r"C:\v\b.mkv"));
        assert!(b.for_file(a).is_empty());
        assert_eq!(b.add(a, 754.0), 0);
        assert_eq!(b.add(other, 5.0), 0);
        assert_eq!(b.add(a, 30.25), 0, "earlier ones sort first");
        assert_eq!(b.add(a, 900.0), 2);
        let names: Vec<&str> = b.for_file(a).iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Bookmark 2", "Bookmark 1", "Bookmark 3"]);
        assert_eq!(b.remove(a, 1).unwrap().seconds, 754.0);
        assert!(b.remove(a, 5).is_none());
        assert_eq!(b.add(a, f64::NAN), 0);
        assert_eq!(b.for_file(a)[0].name, "Bookmark 4");
        b.save().unwrap();

        let loaded = Bookmarks::load(&dir);
        assert_eq!(loaded.for_file(a), b.for_file(a));
        assert_eq!(loaded.for_file(other)[0].seconds, 5.0);
        _ = fs::remove_dir_all(&dir);
    }
}

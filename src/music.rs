//! The music index behind Songs, Albums, Artists and Genres: every music file under the Music
//! roots with its tags. The tags saved in `tags.txt` cover the files unchanged since they were
//! probed; the rest are probed again, one file at a time, in the background. A folder added or
//! a Resync scans the roots again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::library::{self, Library, Track};
use crate::{tags, thumbs};

type Tracks = HashMap<PathBuf, (tags::Stamp, Track)>;

/// What a scan found: the files whose saved tags still hold, and the files to probe.
type Scan = (Tracks, Vec<PathBuf>);

/// How often probed tags reach the library's lists while probing goes on.
const SHOW_EVERY: Duration = Duration::from_secs(1);

pub struct Music {
    /// Where `tags.txt` is.
    dir: PathBuf,
    prober: thumbs::Prober,
    tracks: Tracks,
    scan: Option<mpsc::Receiver<Scan>>,
    /// Roots asked for while a scan ran, scanned once it is in.
    again: Option<Vec<PathBuf>>,
    /// Probed since the library last got the tracks.
    changed: bool,
    shown: Instant,
    /// Changed since `tags.txt` was written.
    unsaved: bool,
}

impl Music {
    /// An empty index keeping its tags in `dir`; `scan` fills it.
    pub fn new(dir: &Path) -> Music {
        Music {
            dir: dir.to_owned(),
            prober: thumbs::Prober::new(),
            tracks: HashMap::new(),
            scan: None,
            again: None,
            changed: false,
            shown: Instant::now(),
            unsaved: false,
        }
    }

    /// Looks through `roots` for music files, in the background: at start, after a Music folder
    /// is added, and on Resync. Files that left the roots leave the index, and files that
    /// changed since they were probed are probed again.
    pub fn scan(&mut self, roots: Vec<PathBuf>) {
        if self.scan.is_some() {
            self.again = Some(roots);
            return;
        }
        let (tx, rx) = mpsc::channel();
        let dir = self.dir.clone();
        let known = self.tracks.clone();
        thread::spawn(move || {
            // The tags probed this run are newer than the saved ones.
            let mut saved = tags::load(&dir);
            saved.extend(known);
            let (mut found, mut changed) = (HashMap::new(), Vec::new());
            for path in library::music_files(&roots) {
                match saved.remove(&path) {
                    Some((stamp, track)) if tags::stamp(&path) == Some(stamp) => {
                        found.insert(path, (stamp, track));
                    }
                    _ => changed.push(path),
                }
            }
            _ = tx.send((found, changed));
        });
        self.scan = Some(rx);
    }

    /// Takes in what a scan and the prober found, and hands the tracks to `library` when they
    /// changed (at most every `SHOW_EVERY` while probing). Returns whether it did, for the
    /// music pages to show them.
    pub fn poll(&mut self, library: &mut Library) -> bool {
        let mut show = false;
        if let Some(scan) = &self.scan
            && let Ok((found, changed)) = scan.try_recv()
        {
            self.scan = None;
            self.unsaved |= found.len() != self.tracks.len() || !changed.is_empty();
            self.tracks = found;
            // Files still to probe show by name until their tags come; the empty stamp has
            // them probed again next run if this one ends first.
            for path in &changed {
                let track = Track {
                    path: path.clone(),
                    ..Default::default()
                };
                self.tracks
                    .insert(path.clone(), (tags::Stamp::default(), track));
                // Probed before this run, it changed since: it is asked for again.
                self.prober.forget(path);
            }
            self.prober.request_all(changed);
            show = true;
            if let Some(roots) = self.again.take() {
                self.scan(roots);
            }
        }
        for (path, info) in self.prober.take_ready() {
            // A file a later scan no longer found stays out.
            if !self.tracks.contains_key(&path) {
                continue;
            }
            let info = info.unwrap_or_default();
            // A file that can't be read gets no stamp, so it is probed again next time.
            let stamp = tags::stamp(&path).unwrap_or_default();
            let track = Track {
                path: path.clone(),
                title: info.title,
                artist: info.artist,
                album: info.album,
                genre: info.genre,
                duration: info.duration,
            };
            self.tracks.insert(path, (stamp, track));
            (self.changed, self.unsaved) = (true, true);
        }
        show |= self.changed && self.shown.elapsed() > SHOW_EVERY;
        if show {
            library.set_tracks(self.tracks.values().map(|(_, t)| t.clone()).collect());
            (self.changed, self.shown) = (false, Instant::now());
        }
        show
    }

    /// Writes the tags to `tags.txt` when they changed since it was last written.
    pub fn save(&mut self) {
        if std::mem::take(&mut self.unsaved)
            && let Err(e) = tags::save(&self.dir, &self.tracks)
        {
            eprintln!("tags: cannot save ({e})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A music folder added while running joins Songs without a restart, and one Resync finds
    /// a file that appeared since.
    #[test]
    fn a_music_folder_added_later_is_indexed() {
        let dir = std::env::temp_dir().join(format!("wushang-music-{}", std::process::id()));
        _ = std::fs::remove_dir_all(&dir);
        let (state, folder) = (dir.join("state"), dir.join("Songs"));
        std::fs::create_dir_all(&folder).unwrap();
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_h264.mp4");
        // Named as sound, which is all the scan looks at.
        std::fs::copy(&clip, folder.join("one.m4a")).unwrap();
        let mut library = Library::open(&state, false);
        let mut music = Music::new(&state);
        let indexed = |music: &mut Music, library: &mut Library, n: usize| {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(10) {
                music.poll(library);
                let probed = music
                    .tracks
                    .values()
                    .filter(|(s, _)| *s != tags::Stamp::default());
                if music.tracks.len() == n && probed.count() == n {
                    return true;
                }
                thread::sleep(Duration::from_millis(10));
            }
            false
        };
        music.scan(Vec::new());
        assert!(indexed(&mut music, &mut library, 0));

        library.add_root(library::Section::Music, folder.clone());
        music.scan(library.roots(library::Section::Music));
        assert!(indexed(&mut music, &mut library, 1));
        std::fs::copy(&clip, folder.join("two.m4a")).unwrap();
        music.scan(library.roots(library::Section::Music));
        assert!(indexed(&mut music, &mut library, 2));

        // What was probed is saved, and read back by the next run.
        music.save();
        let mut next = Music::new(&state);
        next.scan(library.roots(library::Section::Music));
        let start = Instant::now();
        while next.scan.is_some() && start.elapsed() < Duration::from_secs(10) {
            next.poll(&mut library);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(next.tracks.len(), 2);
        assert!(
            next.tracks
                .values()
                .all(|(s, _)| *s != tags::Stamp::default())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

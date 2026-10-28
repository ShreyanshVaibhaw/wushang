//! Media library: root folders, folder browsing, the music index, playlists, recent files and
//! resume positions. A plain model with no UI or FFmpeg; the sidebar and pages draw its entries.
//!
//! State lives in one folder (see [`Library::default_dir`]): `state.txt` holds roots, the recent
//! list, the counters, playlist thumbnails and favourites, and `playlists\<name>.m3u` holds one
//! playlist each.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::ErrorKind;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::{env, fs, io};

use crate::net::{self, Feed, NetKind, Station};

pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "wmv", "flv", "ts", "m2ts", "mts", "mpg", "mpeg",
    "ogv", "3gp",
];
pub const AUDIO_EXTS: &[&str] = &[
    "mp3", "m4a", "aac", "flac", "wav", "ogg", "opus", "wma", "alac", "aiff", "ape", "mka",
];
/// Recent files kept. Resume positions live in the recent list, so they go with it.
const MAX_RECENT: usize = 200;
/// Folder levels searched, the folder itself included, when deciding whether it holds any media.
const MEDIA_SEARCH_DEPTH: usize = 4;
/// Folder levels walked below each Music root by [`Library::music_files`].
const MUSIC_SCAN_DEPTH: usize = 8;
/// A file between these watched fractions resumes where it stopped; from `FINISHED_AT` on it
/// counts as finished.
const RESUME_FROM: f64 = 0.02;
const FINISHED_AT: f64 = 0.95;
/// Items in each of Home's "Continue Watching" and "Listen Again".
const HOME_RECENT: usize = 8;
const STATE_FILE: &str = "state.txt";
const STATE_HEADER: &str = "# myvlc library state v1";
const M3U_HEADER: &str = "#EXTM3U";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Home,
    Videos,
    Music,
    Network,
    Playlists,
    Recent,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Home => "Home",
            Section::Videos => "Videos",
            Section::Music => "Music",
            Section::Network => "Network",
            Section::Playlists => "Playlists",
            Section::Recent => "Recent",
        }
    }

    /// The page the sidebar item opens.
    pub fn place(self) -> Place {
        match self {
            Section::Home => Place::Home,
            Section::Videos => Place::Videos,
            Section::Music => Place::Music,
            Section::Network => Place::Network,
            Section::Playlists => Place::Playlists,
            Section::Recent => Place::Recent,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Place {
    Home,
    Videos,
    Music,
    Network,
    Playlists,
    Recent,
    /// The podcast feeds subscribed to, and one feed's episodes by its address.
    Podcasts,
    Podcast(String),
    /// The radio directory.
    Radio,
    /// A folder under a Videos or Music root.
    Folder(Section, PathBuf),
    Songs,
    Albums,
    Album(String),
    Artists,
    Artist(String),
    Genres,
    Genre(String),
    Playlist(String),
}

impl Place {
    pub fn section(&self) -> Section {
        match self {
            Place::Home => Section::Home,
            Place::Videos => Section::Videos,
            Place::Music
            | Place::Songs
            | Place::Albums
            | Place::Album(_)
            | Place::Artists
            | Place::Artist(_)
            | Place::Genres
            | Place::Genre(_) => Section::Music,
            Place::Network | Place::Podcasts | Place::Podcast(_) | Place::Radio => Section::Network,
            Place::Playlists | Place::Playlist(_) => Section::Playlists,
            Place::Recent => Section::Recent,
            Place::Folder(section, _) => *section,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaKind {
    Video,
    Audio,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    AddFolder(Section),
    CreatePlaylist,
    AddNetworkStream,
    AddPodcast,
}

/// A stream or podcast feed saved on the Network page.
#[derive(Clone, PartialEq, Debug)]
pub struct Address {
    pub kind: NetKind,
    pub url: String,
    pub name: String,
}

/// The tracks a folder's or playlist's files start with, over Settings: an audio language
/// code, and a subtitle language code or "off". None leaves Settings in charge.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackPrefs {
    pub audio: Option<String>,
    pub subtitles: Option<String>,
}

/// The key track preferences are kept under, for a folder or a playlist.
pub fn folder_scope(dir: &Path) -> String {
    format!("folder:{}", abs(dir).display())
}

pub fn playlist_scope(name: &str) -> String {
    format!("playlist:{name}")
}

/// How old a feed may get before showing it fetches it again, for new episodes.
pub const FEED_REFRESH: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// A podcast feed or the radio directory as last fetched: None while the fetch runs.
pub type Fetched<T> = Option<Result<T, String>>;

#[derive(Clone, PartialEq, Debug)]
pub enum Entry {
    /// Home-page section title; `more` is where its ">" chevron leads.
    Header {
        label: String,
        more: Option<Place>,
    },
    Folder {
        label: String,
        place: Place,
    },
    Media {
        label: String,
        path: PathBuf,
        kind: MediaKind,
        subtitle: Option<String>,
        /// From the music index, for the album sorts.
        album: Option<String>,
        /// Watched fraction, 0..1, when the file has been played.
        progress: Option<f32>,
        /// Watched 95% or more.
        finished: bool,
    },
    Action {
        label: String,
        action: Action,
    },
}

impl Entry {
    pub fn label(&self) -> &str {
        match self {
            Entry::Header { label, .. }
            | Entry::Folder { label, .. }
            | Entry::Media { label, .. }
            | Entry::Action { label, .. } => label,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Crumb {
    pub label: String,
    pub place: Place,
}

/// Tag info for one audio track, filled in by a scanner elsewhere.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Track {
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub duration: f64,
}

/// A recently played file and where it stopped, in seconds.
#[derive(Clone, PartialEq, Debug)]
struct Recent {
    path: PathBuf,
    position: f64,
    duration: f64,
}

impl Recent {
    fn fraction(&self) -> Option<f64> {
        (self.duration > 0.0).then(|| (self.position / self.duration).clamp(0.0, 1.0))
    }
}

pub struct Library {
    dir: PathBuf,
    video_roots: Vec<PathBuf>,
    music_roots: Vec<PathBuf>,
    /// Most recent first.
    recent: Vec<Recent>,
    visits: HashMap<PathBuf, u32>,
    plays: HashMap<PathBuf, u32>,
    playlist_plays: HashMap<String, u32>,
    /// The picture each playlist shows, by playlist name.
    thumbnails: HashMap<String, PathBuf>,
    /// Favourite files, folders and playlist files, in the order they were added.
    favorites: Vec<PathBuf>,
    /// The music index from the last scan, in path order, and a path lookup into it.
    // ponytail: in memory only, so Albums/Artists/Genres stay empty until the first scan of each
    // run finishes; save it next to state.txt if that wait shows.
    tracks: Vec<Track>,
    track_at: HashMap<PathBuf, usize>,
    /// Saved streams and podcast feeds, most recent first.
    addresses: Vec<Address>,
    /// Feeds and the radio directory fetched this run, by feed address; they change too often
    /// to keep. Missing (None for stations) until first asked for.
    feeds: HashMap<String, Fetched<Feed>>,
    /// When each feed's last fetch started, so a stale one fetches again (`FEED_REFRESH`).
    feed_times: HashMap<String, std::time::Instant>,
    stations: Option<Fetched<Vec<Station>>>,
    /// Track preferences by `folder_scope` or `playlist_scope`.
    picks: HashMap<String, TrackPrefs>,
}

impl Library {
    /// %APPDATA%\myvlc, or the MYVLC_HOME environment variable when set (tests and dev runs use it).
    pub fn default_dir() -> PathBuf {
        match env::var_os("MYVLC_HOME") {
            Some(home) if !home.is_empty() => PathBuf::from(home),
            _ => PathBuf::from(env::var_os("APPDATA").unwrap_or_default()).join("myvlc"),
        }
    }

    /// Loads state from `dir` (missing files mean empty state). On first run (no state file),
    /// seeds Videos and Music with the user's Videos and Music folders, unless MYVLC_HOME is set.
    pub fn load(dir: &Path) -> Library {
        Self::open(dir, env::var_os("MYVLC_HOME").is_none())
    }

    fn open(dir: &Path, seed: bool) -> Library {
        let mut lib = Library {
            dir: dir.to_path_buf(),
            video_roots: Vec::new(),
            music_roots: Vec::new(),
            recent: Vec::new(),
            visits: HashMap::new(),
            plays: HashMap::new(),
            playlist_plays: HashMap::new(),
            thumbnails: HashMap::new(),
            favorites: Vec::new(),
            tracks: Vec::new(),
            track_at: HashMap::new(),
            addresses: Vec::new(),
            feeds: HashMap::new(),
            feed_times: HashMap::new(),
            stations: None,
            picks: HashMap::new(),
        };
        match fs::read(dir.join(STATE_FILE)) {
            Ok(bytes) => lib.parse(&String::from_utf8_lossy(&bytes)),
            Err(e) if seed && e.kind() == ErrorKind::NotFound => lib.seed_roots(),
            Err(_) => {}
        }
        lib
    }

    fn seed_roots(&mut self) {
        // ponytail: misses Videos and Music folders redirected to OneDrive or another drive;
        // SHGetKnownFolderPath(FOLDERID_Videos / FOLDERID_Music) finds those.
        let Some(home) = env::var_os("USERPROFILE").map(PathBuf::from) else {
            return;
        };
        for (section, name) in [(Section::Videos, "Videos"), (Section::Music, "Music")] {
            let dir = home.join(name);
            if dir.is_dir() {
                self.add_root(section, dir);
            }
        }
    }

    /// Reads state.txt: one record per line, tab-separated, with the record type first.
    /// Unknown, malformed and comment lines are skipped.
    fn parse(&mut self, text: &str) {
        let count = |n: &str| n.parse::<u32>().ok();
        let seconds = |s: &str| s.parse::<f64>().ok().filter(|s| s.is_finite() && *s >= 0.0);
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields[..] {
                ["root", "videos", p] if !p.is_empty() => self.add_root(Section::Videos, p.into()),
                ["root", "music", p] if !p.is_empty() => self.add_root(Section::Music, p.into()),
                ["recent", p, pos, dur] if !p.is_empty() => {
                    let (Some(position), Some(duration)) = (seconds(pos), seconds(dur)) else {
                        continue;
                    };
                    let path = PathBuf::from(p);
                    if self.recent.len() < MAX_RECENT && !self.recent.iter().any(|r| r.path == path)
                    {
                        self.recent.push(Recent {
                            path,
                            position,
                            duration,
                        });
                    }
                }
                ["visit", p, n] if !p.is_empty() => {
                    if let Some(n) = count(n) {
                        self.visits.insert(p.into(), n);
                    }
                }
                ["plays", p, n] if !p.is_empty() => {
                    if let Some(n) = count(n) {
                        self.plays.insert(p.into(), n);
                    }
                }
                ["playlist", name, n] if check_name(name).is_ok() => {
                    if let Some(n) = count(n) {
                        self.playlist_plays.insert(name.into(), n);
                    }
                }
                ["thumb", name, p] if check_name(name).is_ok() && !p.is_empty() => {
                    self.thumbnails.insert(name.into(), p.into());
                }
                ["address", kind, url, name] if net::is_url(url) => {
                    if let Some(kind) = NetKind::from_key(kind)
                        && !self
                            .addresses
                            .iter()
                            .any(|a| a.kind == kind && a.url == url)
                    {
                        self.addresses.push(Address {
                            kind,
                            url: url.into(),
                            name: name.into(),
                        });
                    }
                }
                ["tracks", scope, audio, subtitles] if !scope.is_empty() => {
                    let some = |v: &str| (v != "-" && !v.is_empty()).then(|| v.to_owned());
                    let prefs = TrackPrefs {
                        audio: some(audio),
                        subtitles: some(subtitles),
                    };
                    if prefs != TrackPrefs::default() {
                        self.picks.insert(scope.into(), prefs);
                    }
                }
                ["favorite", p] if !p.is_empty() => {
                    let p = PathBuf::from(p);
                    if !self.favorites.contains(&p) {
                        self.favorites.push(p);
                    }
                }
                _ => {}
            }
        }
    }

    /// Writes state.txt through a temporary file, so a crash mid-write keeps the old state.
    /// Paths that are not valid Unicode are left out.
    pub fn save(&self) -> io::Result<()> {
        let mut out = format!("{STATE_HEADER}\n");
        for (kind, roots) in [("videos", &self.video_roots), ("music", &self.music_roots)] {
            for p in roots.iter().filter_map(|p| p.to_str()) {
                writeln!(out, "root\t{kind}\t{p}").unwrap();
            }
        }
        for r in &self.recent {
            if let Some(p) = r.path.to_str() {
                writeln!(out, "recent\t{p}\t{}\t{}", r.position, r.duration).unwrap();
            }
        }
        for (kind, counts) in [("visit", &self.visits), ("plays", &self.plays)] {
            for (p, n) in counts {
                if let Some(p) = p.to_str() {
                    writeln!(out, "{kind}\t{p}\t{n}").unwrap();
                }
            }
        }
        for (name, n) in &self.playlist_plays {
            writeln!(out, "playlist\t{name}\t{n}").unwrap();
        }
        for (name, p) in &self.thumbnails {
            if let Some(p) = p.to_str() {
                writeln!(out, "thumb\t{name}\t{p}").unwrap();
            }
        }
        for p in self.favorites.iter().filter_map(|p| p.to_str()) {
            writeln!(out, "favorite\t{p}").unwrap();
        }
        for (scope, t) in &self.picks {
            let value = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
            let (audio, subtitles) = (value(&t.audio), value(&t.subtitles));
            writeln!(out, "tracks\t{scope}\t{audio}\t{subtitles}").unwrap();
        }
        for a in &self.addresses {
            let name = a.name.replace(['\t', '\n', '\r'], " ");
            writeln!(out, "address\t{}\t{}\t{name}", a.kind.key(), a.url).unwrap();
        }
        fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("state.txt.tmp");
        fs::write(&tmp, out)?;
        fs::rename(tmp, self.dir.join(STATE_FILE))
    }

    fn root_list(&self, section: Section) -> &[PathBuf] {
        match section {
            Section::Videos => &self.video_roots,
            Section::Music => &self.music_roots,
            _ => &[],
        }
    }

    pub fn roots(&self, section: Section) -> Vec<PathBuf> {
        self.root_list(section).to_vec()
    }

    /// Adds a Videos or Music root; other sections and duplicates are ignored.
    pub fn add_root(&mut self, section: Section, path: PathBuf) {
        let path = abs(&path);
        let roots = match section {
            Section::Videos => &mut self.video_roots,
            Section::Music => &mut self.music_roots,
            _ => return,
        };
        if !roots.contains(&path) {
            roots.push(path);
        }
    }

    /// What a page shows, in display order.
    pub fn entries(&self, place: &Place) -> Vec<Entry> {
        let mut entries = self.raw_entries(place);
        let watched: HashMap<&Path, f64> = self
            .recent
            .iter()
            .filter_map(|r| Some((r.path.as_path(), r.fraction()?)))
            .collect();
        for entry in &mut entries {
            if let Entry::Media {
                path,
                progress,
                finished,
                ..
            } = entry
                && let Some(&f) = watched.get(path.as_path())
            {
                *progress = Some(f as f32);
                *finished = f >= FINISHED_AT;
            }
        }
        entries
    }

    /// Entries without watch progress, which [`Library::entries`] fills in.
    fn raw_entries(&self, place: &Place) -> Vec<Entry> {
        match place {
            Place::Home => self.home(),
            Place::Videos | Place::Music => {
                let section = place.section();
                let mut out = vec![action("Add", Action::AddFolder(section))];
                for root in self.root_list(section) {
                    out.push(folder(&name_of(root), Place::Folder(section, root.clone())));
                }
                if section == Section::Music {
                    for (label, view) in music_views() {
                        out.push(folder(label, view));
                    }
                }
                out
            }
            Place::Network => {
                let mut out = vec![
                    action("Add Network Stream", Action::AddNetworkStream),
                    folder("Podcasts", Place::Podcasts),
                    folder("Radio", Place::Radio),
                ];
                for a in self.addresses_of(NetKind::Stream) {
                    let path = PathBuf::from(&a.url);
                    out.push(media(&path, a.name.clone(), net::host_of(&a.url), None));
                }
                out
            }
            Place::Podcasts => {
                let mut out = vec![action("Add Podcast", Action::AddPodcast)];
                for a in self.addresses_of(NetKind::Podcast) {
                    out.push(folder(&a.name, Place::Podcast(a.url.clone())));
                }
                out
            }
            Place::Podcast(url) => match self.feeds.get(url) {
                Some(Some(Ok(feed))) => (feed.episodes.iter())
                    .map(|e| stream_media(&e.url, &e.title, e.date.as_deref().map(net::short_date)))
                    .collect(),
                Some(Some(Err(e))) => vec![notice(e)],
                _ => vec![notice("Loading episodes...")],
            },
            Place::Radio => match &self.stations {
                Some(Some(Ok(stations))) => (stations.iter())
                    .map(|s| {
                        let about = [s.tags.split(',').next().unwrap_or(""), &s.country];
                        let about: Vec<&str> =
                            about.into_iter().filter(|t| !t.is_empty()).collect();
                        stream_media(&s.url, &s.name, Some(about.join(" - ")))
                    })
                    .collect(),
                Some(Some(Err(e))) => vec![notice(e)],
                _ => vec![notice("Loading stations...")],
            },
            Place::Playlists => {
                let mut out = vec![action("Create playlist", Action::CreatePlaylist)];
                for name in self.playlists() {
                    out.push(folder(&name, Place::Playlist(name.clone())));
                }
                out
            }
            Place::Recent => self
                .recent
                .iter()
                .map(|r| self.file_media(&r.path))
                .collect(),
            Place::Folder(section, dir) => {
                let (dirs, files) = list_dir(dir);
                let dirs = dirs
                    .into_iter()
                    .filter(|(_, d)| has_media(d, MEDIA_SEARCH_DEPTH))
                    .map(|(name, d)| folder(&name, Place::Folder(*section, d)));
                let files = files
                    .iter()
                    .filter(|(_, p)| media_kind(p).is_some())
                    .map(|(_, p)| self.file_media(p));
                dirs.chain(files).collect()
            }
            Place::Songs => {
                let mut out: Vec<Entry> = if self.tracks.is_empty() {
                    let files = self.music_files();
                    files
                        .iter()
                        .map(|p| media(p, stem_of(p), None, None))
                        .collect()
                } else {
                    self.tracks.iter().map(track_media).collect()
                };
                out.sort_by(|a, b| natural_cmp(a.label(), b.label()));
                out
            }
            Place::Albums | Place::Artists | Place::Genres => {
                let mut values: Vec<&str> =
                    self.tracks.iter().filter_map(|t| tag(t, place)).collect();
                values.sort_by(|a, b| natural_cmp(a, b));
                values.dedup();
                values
                    .into_iter()
                    .map(|v| {
                        let child = match place {
                            Place::Albums => Place::Album(v.into()),
                            Place::Artists => Place::Artist(v.into()),
                            _ => Place::Genre(v.into()),
                        };
                        folder(v, child)
                    })
                    .collect()
            }
            Place::Album(value) | Place::Artist(value) | Place::Genre(value) => self
                .tracks
                .iter()
                .filter(|t| tag(t, place) == Some(value.as_str()))
                .map(track_media)
                .collect(),
            Place::Playlist(name) => {
                let items = self.playlist_items(name);
                items.iter().map(|p| self.file_media(p)).collect()
            }
        }
    }

    fn home(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let favorites: Vec<Entry> = self
            .favorites
            .iter()
            .filter_map(|p| self.favorite_entry(p))
            .collect();
        if !favorites.is_empty() {
            out.push(Entry::Header {
                label: "Favorites".into(),
                more: None,
            });
            out.extend(favorites);
        }
        let mut section = |label: &str, more: Place, entries: Vec<Entry>| {
            if !entries.is_empty() {
                out.push(Entry::Header {
                    label: label.into(),
                    more: Some(more),
                });
                out.extend(entries);
            }
        };
        // A file deleted or on a drive since unplugged would only show a blank card.
        let of_kind = |kind| {
            let recent = self.recent.iter();
            recent.filter(move |r| media_kind(&r.path) == Some(kind))
        };

        // A video deleted, or on a drive since unplugged, would only show a blank thumbnail.
        let watching = of_kind(MediaKind::Video)
            .filter(|r| {
                r.fraction()
                    .is_some_and(|f| (RESUME_FROM..FINISHED_AT).contains(&f))
                    && r.path.exists()
            })
            .take(HOME_RECENT)
            .map(|r| self.file_media(&r.path))
            .collect();
        section("Continue Watching", Place::Recent, watching);

        let listened = of_kind(MediaKind::Audio)
            .take(HOME_RECENT)
            .map(|r| match self.track(&r.path) {
                Some(t) => track_media(t),
                None => media(&r.path, stem_of(&r.path), None, None),
            })
            .collect();
        section("Listen Again", Place::Recent, listened);

        let folders = top(self.visits.iter().map(|(p, n)| (p, *n)))
            .into_iter()
            .filter_map(|p| {
                let section = self.section_of(p)?;
                p.is_dir()
                    .then(|| folder(&name_of(p), Place::Folder(section, p.clone())))
            })
            .take(4)
            .collect();
        section("Frequently Visited Folders", Place::Videos, folders);

        let names = self.playlists();
        let counts = names
            .iter()
            .filter_map(|n| Some((n, *self.playlist_plays.get(n)?)));
        let playlists = top(counts)
            .into_iter()
            .take(3)
            .map(|n| folder(n, Place::Playlist(n.clone())))
            .collect();
        section("Frequently Visited Playlists", Place::Playlists, playlists);

        let mut by_artist: HashMap<&str, u32> = HashMap::new();
        for (path, n) in &self.plays {
            if let Some(artist) = self.track(path).and_then(|t| t.artist.as_deref()) {
                *by_artist.entry(artist).or_default() += n;
            }
        }
        let artists = top(by_artist)
            .into_iter()
            .take(4)
            .map(|a| folder(a, Place::Artist(a.into())))
            .collect();
        section("Frequently Listened Artists", Place::Artists, artists);

        out
    }

    /// The breadcrumb trail to `place`, starting with its section.
    pub fn crumbs(&self, place: &Place) -> Vec<Crumb> {
        let section = place.section();
        let mut out = vec![crumb(section.label(), section.place())];
        match place {
            Place::Folder(section, path) => {
                let root = self
                    .root_list(*section)
                    .iter()
                    .filter(|r| path.starts_with(r))
                    .max_by_key(|r| r.components().count());
                let Some(root) = root else {
                    out.push(crumb(&name_of(path), place.clone()));
                    return out;
                };
                let mut at = root.clone();
                out.push(crumb(&name_of(root), Place::Folder(*section, at.clone())));
                for part in path.strip_prefix(root).unwrap_or(Path::new("")) {
                    at.push(part);
                    out.push(crumb(
                        &part.to_string_lossy(),
                        Place::Folder(*section, at.clone()),
                    ));
                }
            }
            Place::Songs | Place::Albums | Place::Artists | Place::Genres => {
                out.extend(
                    music_views()
                        .into_iter()
                        .filter(|(_, v)| v == place)
                        .map(|(l, v)| crumb(l, v)),
                );
            }
            Place::Album(x) | Place::Artist(x) | Place::Genre(x) => {
                let (label, view) = match place {
                    Place::Album(_) => ("Albums", Place::Albums),
                    Place::Artist(_) => ("Artists", Place::Artists),
                    _ => ("Genres", Place::Genres),
                };
                out.push(crumb(label, view));
                out.push(crumb(x, place.clone()));
            }
            Place::Playlist(name) => out.push(crumb(name, place.clone())),
            Place::Podcasts => out.push(crumb("Podcasts", Place::Podcasts)),
            Place::Podcast(url) => {
                out.push(crumb("Podcasts", Place::Podcasts));
                out.push(crumb(
                    &self.address_name(NetKind::Podcast, url),
                    place.clone(),
                ));
            }
            Place::Radio => out.push(crumb("Radio", Place::Radio)),
            _ => {}
        }
        out
    }

    /// The track preferences set on a folder or playlist.
    pub fn track_prefs(&self, scope: &str) -> TrackPrefs {
        self.picks.get(scope).cloned().unwrap_or_default()
    }

    /// Sets a folder's or playlist's audio (or subtitle) language; None goes back to Settings.
    pub fn set_track_pref(&mut self, scope: &str, audio: bool, value: Option<String>) {
        let prefs = self.picks.entry(scope.to_owned()).or_default();
        *(if audio {
            &mut prefs.audio
        } else {
            &mut prefs.subtitles
        }) = value;
        if *prefs == TrackPrefs::default() {
            self.picks.remove(scope);
        }
    }

    /// The track preferences for playing `file`: each from the nearest folder above it that sets
    /// one, else from a playlist holding it.
    pub fn tracks_for(&self, file: &Path) -> TrackPrefs {
        let mut found = TrackPrefs::default();
        let mut take = |t: &TrackPrefs| {
            found.audio = found.audio.take().or_else(|| t.audio.clone());
            found.subtitles = found.subtitles.take().or_else(|| t.subtitles.clone());
        };
        let file = abs(file);
        for dir in file.ancestors().skip(1) {
            if let Some(t) = self.picks.get(&folder_scope(dir)) {
                take(t);
            }
        }
        for (scope, t) in &self.picks {
            if let Some(name) = scope.strip_prefix("playlist:")
                && self.playlist_items(name).iter().any(|p| abs(p) == file)
            {
                take(t);
            }
        }
        found
    }

    /// Saved addresses of one kind, most recent first.
    pub fn addresses_of(&self, kind: NetKind) -> impl Iterator<Item = &Address> {
        self.addresses.iter().filter(move |a| a.kind == kind)
    }

    /// Saves `url`, or moves it to the front with its new `name`.
    pub fn add_address(&mut self, kind: NetKind, url: &str, name: &str) {
        self.remove_address(kind, url);
        let address = Address {
            kind,
            url: url.into(),
            name: name.into(),
        };
        self.addresses.insert(0, address);
    }

    pub fn remove_address(&mut self, kind: NetKind, url: &str) {
        self.addresses.retain(|a| a.kind != kind || a.url != url);
    }

    /// The saved name of an address, else one made from the address.
    pub fn address_name(&self, kind: NetKind, url: &str) -> String {
        (self.addresses_of(kind).find(|a| a.url == url))
            .map_or_else(|| net::url_name(url), |a| a.name.clone())
    }

    /// Whether `place` shows something not fetched yet, or a feed fetched longer than
    /// `FEED_REFRESH` ago, and so needs a fetch.
    pub fn wants_fetch(&self, place: &Place) -> bool {
        match place {
            Place::Podcast(url) => match self.feeds.get(url) {
                None => true,
                Some(Some(Ok(_))) => {
                    (self.feed_times.get(url)).is_none_or(|t| t.elapsed() > FEED_REFRESH)
                }
                _ => false,
            },
            Place::Radio => self.stations.is_none(),
            _ => false,
        }
    }

    /// Marks a fetch as running, so `wants_fetch` asks only once. A feed being fetched again
    /// keeps showing its episodes meanwhile.
    pub fn fetching(&mut self, place: &Place) {
        match place {
            Place::Podcast(url) => {
                self.feed_times
                    .insert(url.clone(), std::time::Instant::now());
                self.feeds.entry(url.clone()).or_insert(None);
            }
            Place::Radio => self.stations = Some(None),
            _ => {}
        }
    }

    /// A feed fetch finished. A feed whose title is known now gets it as its name. A refresh
    /// that failed keeps the episodes fetched before.
    pub fn feed_fetched(&mut self, url: &str, result: Result<Feed, String>) {
        if let Err(e) = &result
            && matches!(self.feeds.get(url), Some(Some(Ok(_))))
        {
            eprintln!("podcast: can't refresh {}: {e}", net::redact(url));
            return;
        }
        if let Ok(feed) = &result
            && !feed.title.is_empty()
            && let Some(a) =
                (self.addresses.iter_mut()).find(|a| a.kind == NetKind::Podcast && a.url == url)
        {
            a.name = feed.title.clone();
        }
        self.feeds.insert(url.into(), Some(result));
    }

    pub fn stations_fetched(&mut self, result: Result<Vec<Station>, String>) {
        self.stations = Some(Some(result));
    }

    /// Forgets a fetch of `place` that failed, so showing it again tries again.
    pub fn retry_failed(&mut self, place: &Place) {
        match place {
            Place::Podcast(url) if matches!(self.feeds.get(url), Some(Some(Err(_)))) => {
                self.feeds.remove(url);
            }
            Place::Radio if matches!(self.stations, Some(Some(Err(_)))) => self.stations = None,
            _ => {}
        }
    }

    /// The playable files of a place in display order (the play queue for next and previous).
    pub fn media_in(&self, place: &Place) -> Vec<PathBuf> {
        let entries = self.raw_entries(place).into_iter();
        entries
            .filter_map(|e| match e {
                Entry::Media { path, .. } => Some(path),
                _ => None,
            })
            .collect()
    }

    /// Moves `path` to the top of the recent list with its position, both in seconds.
    /// A play counts when a file becomes the most recent one, so calling this every few seconds
    /// during one playback counts once.
    pub fn record_position(&mut self, path: &Path, position: f64, duration: f64) {
        let path = abs(path);
        let finite = |s: f64| if s.is_finite() { s.max(0.0) } else { 0.0 };
        if self.recent.first().is_none_or(|r| r.path != path) {
            *self.plays.entry(path.clone()).or_default() += 1;
        }
        self.recent.retain(|r| r.path != path);
        let (position, duration) = (finite(position), finite(duration));
        self.recent.insert(
            0,
            Recent {
                path,
                position,
                duration,
            },
        );
        self.recent.truncate(MAX_RECENT);
    }

    /// Where to resume `path`: only when 2% to 95% of it has been watched.
    pub fn resume_position(&self, path: &Path) -> Option<f64> {
        let path = abs(path);
        let r = self.recent.iter().find(|r| r.path == path)?;
        let f = r.fraction()?;
        (RESUME_FROM..FINISHED_AT)
            .contains(&f)
            .then_some(r.position)
    }

    /// Settings' "Clear history": forgets what was played, where, and how often.
    pub fn clear_history(&mut self) {
        self.recent.clear();
        self.plays.clear();
        self.visits.clear();
        self.playlist_plays.clear();
    }

    pub fn visit_folder(&mut self, path: &Path) {
        *self.visits.entry(abs(path)).or_default() += 1;
    }

    fn playlist_dir(&self) -> PathBuf {
        self.dir.join("playlists")
    }

    fn playlist_file(&self, name: &str) -> io::Result<PathBuf> {
        check_name(name)?;
        Ok(self.playlist_dir().join(format!("{name}.m3u")))
    }

    /// Playlist names, sorted.
    pub fn playlists(&self) -> Vec<String> {
        let entries = fs::read_dir(self.playlist_dir())
            .into_iter()
            .flatten()
            .flatten();
        let mut names: Vec<String> = entries
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("m3u")))
            .map(|p| stem_of(&p))
            .collect();
        names.sort_by(|a, b| natural_cmp(a, b));
        names
    }

    /// Creates an empty playlist; fails if the name is invalid or taken.
    pub fn create_playlist(&mut self, name: &str) -> io::Result<()> {
        let file = self.playlist_file(name)?;
        fs::create_dir_all(self.playlist_dir())?;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file)?;
        io::Write::write_all(&mut f, format!("{M3U_HEADER}\n").as_bytes())
    }

    /// Appends `path` to a playlist, creating the playlist if needed.
    pub fn add_to_playlist(&mut self, name: &str, path: &Path) -> io::Result<()> {
        let file = self.playlist_file(name)?;
        let path = abs(path);
        let line = path
            .to_str()
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "path is not valid Unicode"))?;
        let mut text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        if text.is_empty() {
            text = format!("{M3U_HEADER}\n");
        } else if !text.ends_with('\n') {
            text.push('\n');
        }
        writeln!(text, "{line}").unwrap();
        fs::create_dir_all(self.playlist_dir())?;
        fs::write(file, text)
    }

    /// The files of a playlist in order. Relative lines resolve against the playlists folder.
    // ponytail: URL and file:// lines come out as bogus paths; parse them when network streams land.
    pub fn playlist_items(&self, name: &str) -> Vec<PathBuf> {
        let Ok(file) = self.playlist_file(name) else {
            return Vec::new();
        };
        let Ok(bytes) = fs::read(file) else {
            return Vec::new();
        };
        let dir = self.playlist_dir();
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(|l| l.trim_start_matches('\u{feff}').trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| dir.join(l))
            .collect()
    }

    pub fn record_playlist_play(&mut self, name: &str) {
        if check_name(name).is_ok() {
            *self.playlist_plays.entry(name.into()).or_default() += 1;
        }
    }

    /// The playlist's .m3u file, which also stands for the playlist in the favourites.
    pub fn playlist_path(&self, name: &str) -> Option<PathBuf> {
        self.playlist_file(name).ok().map(|p| abs(&p))
    }

    /// Renames a playlist with its play count, thumbnail and favourite. Fails if either name is
    /// invalid or `new` is taken; a change of case only is allowed.
    pub fn rename_playlist(&mut self, old: &str, new: &str) -> io::Result<()> {
        let from = self.playlist_file(old)?;
        let to = self.playlist_file(new)?;
        if old == new {
            return Ok(());
        }
        // Windows file names ignore case, so "mix" -> "Mix" finds itself at the target.
        if to.exists() && old.to_lowercase() != new.to_lowercase() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                format!("a playlist called {new:?} already exists"),
            ));
        }
        let (old_path, new_path) = (abs(&from), abs(&to));
        fs::rename(from, to)?;
        if let Some(n) = self.playlist_plays.remove(old) {
            self.playlist_plays.insert(new.into(), n);
        }
        if let Some(p) = self.thumbnails.remove(old) {
            self.thumbnails.insert(new.into(), p);
        }
        for p in &mut self.favorites {
            if *p == old_path {
                p.clone_from(&new_path);
            }
        }
        Ok(())
    }

    /// Deletes a playlist file and what the library keeps about it.
    pub fn delete_playlist(&mut self, name: &str) -> io::Result<()> {
        let file = self.playlist_file(name)?;
        let path = abs(&file);
        fs::remove_file(file)?;
        self.playlist_plays.remove(name);
        self.thumbnails.remove(name);
        self.favorites.retain(|p| *p != path);
        Ok(())
    }

    /// Removes item `index` (as [`Library::playlist_items`] counts) and its `#EXTINF` line,
    /// keeping every other line of the file as it was.
    pub fn remove_from_playlist(&mut self, name: &str, index: usize) -> io::Result<()> {
        let file = self.playlist_file(name)?;
        let text = String::from_utf8_lossy(&fs::read(&file)?).into_owned();
        let lines: Vec<&str> = text.lines().collect();
        let is_item = |l: &str| {
            let l = l.trim_start_matches('\u{feff}').trim();
            !l.is_empty() && !l.starts_with('#')
        };
        let at = (0..lines.len())
            .filter(|&i| is_item(lines[i]))
            .nth(index)
            .ok_or_else(|| {
                io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("{name} has no item {index}"),
                )
            })?;
        let mut start = at;
        while start > 0 && lines[start - 1].trim().starts_with("#EXTINF") {
            start -= 1;
        }
        let mut out = String::new();
        for line in lines[..start].iter().chain(&lines[at + 1..]) {
            writeln!(out, "{line}").unwrap();
        }
        fs::write(file, out)
    }

    pub fn playlist_thumbnail(&self, name: &str) -> Option<&Path> {
        self.thumbnails.get(name).map(PathBuf::as_path)
    }

    /// Sets or, with None, clears the picture a playlist shows.
    pub fn set_playlist_thumbnail(&mut self, name: &str, image: Option<&Path>) {
        match image {
            Some(p) if check_name(name).is_ok() => _ = self.thumbnails.insert(name.into(), abs(p)),
            _ => _ = self.thumbnails.remove(name),
        }
    }

    /// Marks a file as watched to the end, or clears where it stopped. Marking a file never
    /// played adds it to the end of the recent list, without counting a play.
    pub fn mark_watched(&mut self, path: &Path, watched: bool) {
        let path = abs(path);
        match self.recent.iter_mut().find(|r| r.path == path) {
            Some(r) if watched => {
                // An unknown length still needs a fraction of 1.
                if r.duration <= 0.0 {
                    r.duration = 1.0;
                }
                r.position = r.duration;
            }
            Some(r) => r.position = 0.0,
            None if watched => {
                if self.recent.len() >= MAX_RECENT {
                    self.recent.pop();
                }
                self.recent.push(Recent {
                    path,
                    position: 1.0,
                    duration: 1.0,
                });
            }
            None => {}
        }
    }

    /// Adds `path` (a file, folder or playlist file) to the favourites, or takes it out.
    pub fn set_favorite(&mut self, path: &Path, favorite: bool) {
        let path = abs(path);
        let known = self.favorites.contains(&path);
        if favorite && !known {
            self.favorites.push(path);
        } else if !favorite {
            self.favorites.retain(|p| *p != path);
        }
    }

    pub fn is_favorite(&self, path: &Path) -> bool {
        self.favorites.contains(&abs(path))
    }

    /// How a favourite shows on Home: a playlist, a media file or a folder. Playlists and folders
    /// that are gone are left out.
    fn favorite_entry(&self, path: &Path) -> Option<Entry> {
        if path.parent() == Some(abs(&self.playlist_dir()).as_path()) {
            let name = stem_of(path);
            return path
                .is_file()
                .then(|| folder(&name, Place::Playlist(name.clone())));
        }
        if media_kind(path).is_some() {
            return Some(self.file_media(path));
        }
        let section = self.section_of(path).unwrap_or(Section::Videos);
        path.is_dir()
            .then(|| folder(&name_of(path), Place::Folder(section, path.to_owned())))
    }

    /// Replaces the music index with the result of a scan. Blank tags count as missing.
    pub fn set_tracks(&mut self, mut tracks: Vec<Track>) {
        for t in &mut tracks {
            for tag in [&mut t.title, &mut t.artist, &mut t.album, &mut t.genre] {
                *tag = tag
                    .take()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty());
            }
        }
        tracks.sort_by(|a, b| natural_cmp(&a.path.to_string_lossy(), &b.path.to_string_lossy()));
        self.track_at = tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.path.clone(), i))
            .collect();
        self.tracks = tracks;
    }

    /// Every audio file under the Music roots, for the scanner to probe.
    pub fn music_files(&self) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for root in &self.music_roots {
            walk(root, MUSIC_SCAN_DEPTH, &mut |p| {
                if media_kind(&p) == Some(MediaKind::Audio) && seen.insert(p.clone()) {
                    out.push(p);
                }
            });
        }
        out
    }

    pub fn track(&self, path: &Path) -> Option<&Track> {
        self.track_at.get(path).map(|&i| &self.tracks[i])
    }

    /// A file shown by name, with its artist and album when the music index knows them.
    fn file_media(&self, path: &Path) -> Entry {
        let track = self.track(path);
        let artist = track.and_then(|t| t.artist.clone());
        media(
            path,
            name_of(path),
            artist,
            track.and_then(|t| t.album.clone()),
        )
    }

    /// The root section a folder belongs to.
    fn section_of(&self, path: &Path) -> Option<Section> {
        [Section::Videos, Section::Music]
            .into_iter()
            .find(|&s| self.root_list(s).iter().any(|r| path.starts_with(r)))
    }
}

/// Video or audio, by extension, ignoring case.
pub fn media_kind(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if VIDEO_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else if AUDIO_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::Audio)
    } else {
        None
    }
}

fn music_views() -> [(&'static str, Place); 4] {
    [
        ("Songs", Place::Songs),
        ("Albums", Place::Albums),
        ("Artists", Place::Artists),
        ("Genres", Place::Genres),
    ]
}

/// The tag a music view groups by.
fn tag<'a>(t: &'a Track, view: &Place) -> Option<&'a str> {
    match view {
        Place::Albums | Place::Album(_) => t.album.as_deref(),
        Place::Artists | Place::Artist(_) => t.artist.as_deref(),
        Place::Genres | Place::Genre(_) => t.genre.as_deref(),
        _ => None,
    }
}

fn folder(label: &str, place: Place) -> Entry {
    Entry::Folder {
        label: label.into(),
        place,
    }
}

fn action(label: &str, action: Action) -> Entry {
    Entry::Action {
        label: label.into(),
        action,
    }
}

fn crumb(label: &str, place: Place) -> Crumb {
    Crumb {
        label: label.into(),
        place,
    }
}

fn media(path: &Path, label: String, subtitle: Option<String>, album: Option<String>) -> Entry {
    Entry::Media {
        label,
        path: path.to_path_buf(),
        kind: media_kind(path).unwrap_or(MediaKind::Video),
        subtitle,
        album,
        progress: None,
        finished: false,
    }
}

/// A podcast episode or radio station: played from its address, listed as audio.
fn stream_media(url: &str, title: &str, subtitle: Option<String>) -> Entry {
    Entry::Media {
        label: title.into(),
        path: PathBuf::from(url),
        kind: MediaKind::Audio,
        subtitle: subtitle.filter(|s| !s.is_empty()),
        album: None,
        progress: None,
        finished: false,
    }
}

/// A line in place of a list that isn't there: still loading, or why it failed.
fn notice(text: &str) -> Entry {
    Entry::Header {
        label: text.into(),
        more: None,
    }
}

fn track_media(t: &Track) -> Entry {
    let title = t.title.clone().unwrap_or_else(|| stem_of(&t.path));
    media(&t.path, title, t.artist.clone(), t.album.clone())
}

/// The file or folder name, or the whole path for a drive root like `D:\`.
fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map_or_else(|| name_of(path), |n| n.to_string_lossy().into_owned())
}

/// Absolute form of `path`, so lookups match however the file was opened. A stream address
/// stays as it is; Windows would read it as a relative path with backslashes.
pub fn abs(path: &Path) -> PathBuf {
    if crate::net::is_address(path) {
        return path.to_path_buf();
    }
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Keys with a nonzero count, highest count first, ties in key order.
fn top<K: Ord>(counts: impl IntoIterator<Item = (K, u32)>) -> Vec<K> {
    let mut v: Vec<(K, u32)> = counts.into_iter().filter(|c| c.1 > 0).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter().map(|c| c.0).collect()
}

/// Rejects playlist names that can't be a file name on Windows.
pub fn check_name(name: &str) -> io::Result<()> {
    let device = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (device.len() == 4
            && (device.starts_with("COM") || device.starts_with("LPT"))
            && device.ends_with(|c: char| c.is_ascii_digit()));
    let bad = name.trim().is_empty()
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c))
        || reserved;
    if bad {
        Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("invalid playlist name {name:?}"),
        ))
    } else {
        Ok(())
    }
}

type Named = Vec<(String, PathBuf)>;

/// Settings' "Show hidden files": folder listings include hidden and dot files.
pub static SHOW_HIDDEN: AtomicBool = AtomicBool::new(false);

/// The visible subfolders and files of `dir`, each sorted by name in natural order.
fn list_dir(dir: &Path) -> (Named, Named) {
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // The hidden attribute also covers $RECYCLE.BIN and System Volume Information on drive roots.
        let hidden = entry
            .metadata()
            .is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0);
        if (hidden || name.starts_with('.')) && !SHOW_HIDDEN.load(Relaxed) {
            continue;
        }
        let path = entry.path();
        // Follow symlinks and junctions to folders; the depth caps keep loops finite.
        let is_dir = entry
            .file_type()
            .is_ok_and(|t| t.is_dir() || t.is_symlink() && path.is_dir());
        if is_dir {
            dirs.push((name, path))
        } else {
            files.push((name, path))
        }
    }
    dirs.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    files.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    (dirs, files)
}

/// Whether `dir` or its subfolders, `depth` levels in all, hold any media file.
fn has_media(dir: &Path, depth: usize) -> bool {
    let (dirs, files) = list_dir(dir);
    files.iter().any(|(_, p)| media_kind(p).is_some())
        || depth > 1 && dirs.iter().any(|(_, d)| has_media(d, depth - 1))
}

/// Calls `found` for every visible file in `dir` and up to `depth` levels below it.
fn walk(dir: &Path, depth: usize, found: &mut impl FnMut(PathBuf)) {
    let (dirs, files) = list_dir(dir);
    for (_, p) in files {
        found(p);
    }
    if depth > 0 {
        for (_, d) in dirs {
            walk(&d, depth - 1, found);
        }
    }
}

/// Case-insensitive order with digit runs compared as numbers, so "E2" sorts before "E10".
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    let digits = |it: &mut std::iter::Peekable<std::str::Chars>| {
        let mut s = String::new();
        while let Some(c) = it.next_if(char::is_ascii_digit) {
            s.push(c);
        }
        s
    };
    loop {
        let ord = match (x.peek().copied(), y.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let (m, n) = (digits(&mut x), digits(&mut y));
                let (m, n) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                m.len().cmp(&n.len()).then_with(|| m.cmp(n))
            }
            (Some(c), Some(d)) => {
                x.next();
                y.next();
                c.to_lowercase().cmp(d.to_lowercase())
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    // Equal apart from case or leading zeros: fall back to a plain compare so the order is stable.
    a.cmp(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    /// A fresh folder under the system temp folder, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Relaxed);
            let dir = env::temp_dir().join(format!("myvlc-lib-test-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        /// Creates an empty file and its parent folders.
        fn touch(&self, rel: &str) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"").unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The entries' labels joined by "|", which keeps the expected lists on one line.
    fn labels(entries: &[Entry]) -> String {
        entries
            .iter()
            .map(Entry::label)
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["E10", "e2", "E1", "Season 10", "season 9", "a", "B", "E02"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            ["a", "B", "E1", "E02", "e2", "E10", "season 9", "Season 10"]
        );
    }

    #[test]
    fn kinds_by_extension() {
        assert_eq!(media_kind(Path::new(r"C:\a\b.MKV")), Some(MediaKind::Video));
        assert_eq!(media_kind(Path::new("song.Flac")), Some(MediaKind::Audio));
        assert_eq!(media_kind(Path::new("notes.txt")), None);
        assert_eq!(media_kind(Path::new("noext")), None);
    }

    #[test]
    fn folder_entries_sorted_and_empty_folders_skipped() {
        let tmp = TempDir::new();
        let shows = tmp.0.join("Shows");
        for f in [
            "Shows/E10.mp4",
            "Shows/E2.mp4",
            "Shows/e1.mkv",
            "Shows/readme.txt",
            "Shows/.secret.mp4",
            "Shows/Season 10/a.mp4",
            "Shows/Season 2/a.mp4",
            "Shows/season 1/a.mp4",
            "Shows/Empty/notes.txt",
            "Shows/.hidden/x.mp4",
            "Shows/Deep/a/b/c/d.mp3",
            "Shows/TooDeep/a/b/c/d/e.mp3",
        ] {
            tmp.touch(f);
        }
        let mut lib = Library::open(&tmp.0.join("state"), false);
        lib.add_root(Section::Videos, shows.clone());

        let place = Place::Folder(Section::Videos, shows.clone());
        let entries = lib.entries(&place);
        assert_eq!(
            labels(&entries),
            "Deep|season 1|Season 2|Season 10|e1.mkv|E2.mp4|E10.mp4"
        );
        assert_eq!(
            entries[1],
            folder(
                "season 1",
                Place::Folder(Section::Videos, shows.join("season 1"))
            )
        );
        assert_eq!(
            lib.media_in(&place),
            ["e1.mkv", "E2.mp4", "E10.mp4"].map(|f| shows.join(f))
        );

        let page = lib.entries(&Place::Videos);
        assert_eq!(page[0], action("Add", Action::AddFolder(Section::Videos)));
        assert_eq!(page[1], folder("Shows", place));
    }

    #[test]
    fn crumbs_for_nested_folder() {
        let tmp = TempDir::new();
        let mut lib = Library::open(&tmp.0, false);
        let root = PathBuf::from(r"C:\v\Shows");
        lib.add_root(Section::Videos, root.clone());
        let at = |p: &str| Place::Folder(Section::Videos, root.join(p));

        let crumbs = lib.crumbs(&at(r"The Wand\S01"));
        let expected = [
            crumb("Videos", Place::Videos),
            crumb("Shows", at("")),
            crumb("The Wand", at("The Wand")),
            crumb("S01", at(r"The Wand\S01")),
        ];
        assert_eq!(crumbs, expected);
        assert_eq!(lib.crumbs(&Place::Home), [crumb("Home", Place::Home)]);
        let album = Place::Album("Arrival".into());
        assert_eq!(
            lib.crumbs(&album),
            [
                crumb("Music", Place::Music),
                crumb("Albums", Place::Albums),
                crumb("Arrival", album.clone())
            ]
        );
    }

    #[test]
    fn state_roundtrip() {
        let tmp = TempDir::new();
        let videos = PathBuf::from(r"D:\Mÿ Vídeos\ドラマ");
        let film = PathBuf::from(r"D:\Mÿ Vídeos\a film.mkv");
        let song = PathBuf::from(r"E:\Music\b song.mp3");
        let mut a = Library::open(&tmp.0, false);
        a.add_root(Section::Videos, videos.clone());
        a.add_root(Section::Videos, videos.clone());
        a.add_root(Section::Music, PathBuf::from(r"E:\Music"));
        a.record_position(&film, 600.5, 5400.25);
        a.record_position(&song, 30.0, 200.0);
        a.visit_folder(&videos);
        a.visit_folder(&videos);
        a.record_playlist_play("Road Trip");
        a.save().unwrap();

        // Damage the file the way a crash or a hand edit might.
        let state = tmp.0.join(STATE_FILE);
        let mut text = fs::read_to_string(&state).unwrap();
        text.push_str("recent\tonly a path\nvisit\tC:\\x\tmany\n\t\t\nbogus\nroot\tvideos\t\n");
        fs::write(&state, text).unwrap();

        let b = Library::open(&tmp.0, false);
        assert_eq!(b.roots(Section::Videos), [videos]);
        assert_eq!(b.roots(Section::Music), [PathBuf::from(r"E:\Music")]);
        assert_eq!(b.recent, a.recent);
        assert_eq!(b.media_in(&Place::Recent), [song.clone(), film.clone()]);
        assert_eq!(b.resume_position(&film), Some(600.5));
        assert_eq!(b.resume_position(&song), Some(30.0));
        assert_eq!(b.visits, a.visits);
        assert_eq!(b.plays, a.plays);
        assert_eq!(b.playlist_plays, a.playlist_plays);
    }

    #[test]
    fn resume_only_between_2_and_95_percent() {
        let tmp = TempDir::new();
        let mut lib = Library::open(&tmp.0, false);
        let f = Path::new(r"C:\v\a.mp4");
        lib.record_position(f, 1.0, 100.0);
        assert_eq!(lib.resume_position(f), None);
        lib.record_position(f, 50.0, 100.0);
        assert_eq!(lib.resume_position(f), Some(50.0));
        lib.record_position(f, 96.0, 100.0);
        assert_eq!(lib.resume_position(f), None);
        assert_eq!(
            lib.plays[f], 1,
            "repeated updates of one playback count once"
        );
    }

    #[test]
    fn playlist_roundtrip_and_bad_names() {
        let tmp = TempDir::new();
        let mut lib = Library::open(&tmp.0, false);
        let a = PathBuf::from(r"C:\Music\Ünïcode song.mp3");
        let b = PathBuf::from(r"D:\v\with space.mkv");
        lib.create_playlist("Road Trip").unwrap();
        lib.add_to_playlist("Road Trip", &a).unwrap();
        lib.add_to_playlist("Road Trip", &b).unwrap();
        lib.add_to_playlist("Fresh", &b).unwrap();

        assert_eq!(lib.playlists(), ["Fresh", "Road Trip"]);
        assert_eq!(lib.playlist_items("Road Trip"), [a.clone(), b.clone()]);
        let text = fs::read_to_string(tmp.0.join(r"playlists\Road Trip.m3u")).unwrap();
        assert_eq!(text, format!("#EXTM3U\n{}\n{}\n", a.display(), b.display()));
        assert_eq!(
            lib.create_playlist("Road Trip").unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(
            labels(&lib.entries(&Place::Playlist("Road Trip".into()))),
            "Ünïcode song.mp3|with space.mkv"
        );
        assert_eq!(
            labels(&lib.entries(&Place::Playlists)),
            "Create playlist|Fresh|Road Trip"
        );

        let bad_names = ",  ,a/b,a\\b,c:d,why?,<x>,a|b,q\",star*,dot.,CON,com3,tab\there";
        for bad in bad_names.split(',') {
            let err = lib.create_playlist(bad).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::InvalidInput, "{bad:?}");
            assert!(lib.add_to_playlist(bad, &a).is_err(), "{bad:?}");
        }
        assert_eq!(lib.playlists().len(), 2);
    }

    #[test]
    fn rename_and_delete_playlists() {
        let tmp = TempDir::new();
        let mut lib = Library::open(&tmp.0, false);
        let song = PathBuf::from(r"C:\Music\a.mp3");
        let cover = PathBuf::from(r"C:\Pictures\cover.png");
        lib.add_to_playlist("Mix", &song).unwrap();
        lib.create_playlist("Other").unwrap();
        lib.record_playlist_play("Mix");
        lib.set_playlist_thumbnail("Mix", Some(&cover));
        let mix = lib.playlist_path("Mix").unwrap();
        lib.set_favorite(&mix, true);

        lib.rename_playlist("Mix", "Road Trip").unwrap();
        assert_eq!(lib.playlists(), ["Other", "Road Trip"]);
        assert_eq!(lib.playlist_items("Road Trip"), std::slice::from_ref(&song));
        assert_eq!(lib.playlist_plays.get("Road Trip"), Some(&1));
        assert!(!lib.playlist_plays.contains_key("Mix"));
        assert_eq!(lib.playlist_thumbnail("Road Trip"), Some(cover.as_path()));
        assert_eq!(lib.playlist_thumbnail("Mix"), None);
        let road = lib.playlist_path("Road Trip").unwrap();
        assert!(lib.is_favorite(&road) && !lib.is_favorite(&mix));

        let taken = lib.rename_playlist("Road Trip", "other").unwrap_err();
        assert_eq!(taken.kind(), ErrorKind::AlreadyExists);
        assert_eq!(
            lib.rename_playlist("Road Trip", "a/b").unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert!(lib.rename_playlist("Gone", "New").is_err());
        lib.rename_playlist("Road Trip", "road trip").unwrap();
        assert_eq!(lib.playlists(), ["Other", "road trip"]);

        // Thumbnails and favourites survive a save, and a damaged record is skipped.
        lib.save().unwrap();
        let state = tmp.0.join(STATE_FILE);
        let mut text = fs::read_to_string(&state).unwrap();
        text.push_str("thumb\tbad/name\tC:\\x.png\nthumb\tOther\t\nfavorite\t\nfavorite\n");
        fs::write(&state, text).unwrap();
        let reloaded = Library::open(&tmp.0, false);
        assert_eq!(reloaded.thumbnails, lib.thumbnails);
        assert_eq!(reloaded.favorites, lib.favorites);

        lib.delete_playlist("road trip").unwrap();
        assert_eq!(lib.playlists(), ["Other"]);
        assert!(lib.thumbnails.is_empty() && lib.favorites.is_empty());
        assert!(lib.playlist_plays.is_empty());
        assert_eq!(
            lib.delete_playlist("road trip").unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn remove_from_playlist_keeps_other_lines() {
        let tmp = TempDir::new();
        let mut lib = Library::open(&tmp.0, false);
        lib.create_playlist("P").unwrap();
        let file = tmp.0.join(r"playlists\P.m3u");
        let text = "#EXTM3U\n#EXTINF:10,One\nC:\\a.mp4\n\n# a note\n#EXTINF:20,Two\nC:\\b.mp4\nC:\\c.mp4\n";
        fs::write(&file, text).unwrap();

        lib.remove_from_playlist("P", 1).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "#EXTM3U\n#EXTINF:10,One\nC:\\a.mp4\n\n# a note\nC:\\c.mp4\n"
        );
        lib.remove_from_playlist("P", 0).unwrap();
        assert_eq!(lib.playlist_items("P"), [PathBuf::from(r"C:\c.mp4")]);
        let err = lib.remove_from_playlist("P", 1).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(lib.remove_from_playlist("Nope", 0).is_err());
    }

    #[test]
    fn mark_watched_and_unwatched() {
        let tmp = TempDir::new();
        let mut lib = Library::open(&tmp.0, false);
        let (half, fresh) = (Path::new(r"C:\v\half.mp4"), Path::new(r"C:\v\new.mp4"));
        lib.record_position(half, 50.0, 100.0);

        lib.mark_watched(half, true);
        lib.mark_watched(fresh, true);
        assert_eq!(lib.resume_position(half), None);
        let recent = lib.entries(&Place::Recent);
        assert_eq!(labels(&recent), "half.mp4|new.mp4");
        assert!(
            recent
                .iter()
                .all(|e| matches!(e, Entry::Media { finished: true, .. }))
        );
        assert!(!lib.plays.contains_key(fresh), "marking is not playing");

        lib.mark_watched(half, false);
        let Entry::Media {
            progress, finished, ..
        } = &lib.entries(&Place::Recent)[0]
        else {
            panic!()
        };
        assert_eq!((*progress, *finished), (Some(0.0), false));
        assert_eq!(lib.resume_position(half), None);

        // Playing the marked file counts a play, since it never became the most recent.
        lib.record_position(fresh, 5.0, 60.0);
        assert_eq!(lib.plays[fresh], 1);
    }

    #[test]
    fn favorites_lead_home() {
        let tmp = TempDir::new();
        let root = tmp.0.join("Videos");
        let clip = tmp.touch(r"Videos\Show\ep1.mp4");
        tmp.touch(r"Videos\Gone\x.mp4");
        tmp.touch(r"Videos\half.mp4");
        let mut lib = Library::open(&tmp.0.join("state"), false);
        lib.add_root(Section::Videos, root.clone());
        lib.create_playlist("Mix").unwrap();
        lib.record_position(&root.join("half.mp4"), 50.0, 100.0);

        lib.set_favorite(&clip, true);
        lib.set_favorite(&root.join("Show"), true);
        lib.set_favorite(&lib.playlist_path("Mix").unwrap(), true);
        lib.set_favorite(&root.join("Gone"), true);
        lib.set_favorite(&clip, true);
        fs::remove_dir_all(root.join("Gone")).unwrap();
        assert!(lib.is_favorite(&clip));

        let home = lib.entries(&Place::Home);
        assert_eq!(
            labels(&home[..5]),
            "Favorites|ep1.mp4|Show|Mix|Continue Watching"
        );
        assert_eq!(
            home[0],
            Entry::Header {
                label: "Favorites".into(),
                more: None
            }
        );
        assert_eq!(
            home[2],
            folder("Show", Place::Folder(Section::Videos, root.join("Show")))
        );
        assert_eq!(home[3], folder("Mix", Place::Playlist("Mix".into())));

        lib.set_favorite(&clip, false);
        lib.set_favorite(&root.join("Show"), false);
        lib.delete_playlist("Mix").unwrap();
        assert!(!lib.is_favorite(&clip));
        assert_eq!(
            labels(&lib.entries(&Place::Home)),
            "Continue Watching|half.mp4"
        );
    }

    #[test]
    fn folders_and_playlists_choose_tracks() {
        let tmp = TempDir::new();
        let anime = tmp.0.join("Anime");
        let ep = tmp.touch(r"Anime\Show\ep1.mkv");
        let film = tmp.touch(r"Films\film.mkv");
        let mut lib = Library::open(&tmp.0.join("state"), false);
        lib.set_track_pref(&folder_scope(&anime), true, Some("jpn".into()));
        lib.set_track_pref(&folder_scope(&anime), false, Some("eng".into()));
        // The nearer folder wins for what it sets; the rest comes from further up.
        lib.set_track_pref(
            &folder_scope(&anime.join("Show")),
            false,
            Some("off".into()),
        );
        lib.create_playlist("Dubbed").unwrap();
        lib.add_to_playlist("Dubbed", &ep).unwrap();
        lib.add_to_playlist("Dubbed", &film).unwrap();
        lib.set_track_pref(&playlist_scope("Dubbed"), true, Some("ger".into()));
        lib.save().unwrap();

        let lib = Library::open(&tmp.0.join("state"), false);
        let want = |audio: &str, subtitles: Option<&str>| TrackPrefs {
            audio: Some(audio.into()),
            subtitles: subtitles.map(str::to_owned),
        };
        assert_eq!(lib.tracks_for(&ep), want("jpn", Some("off")));
        // A file in no chosen folder takes its playlist's choice.
        assert_eq!(lib.tracks_for(&film), want("ger", None));
        let mut lib = lib;
        lib.set_track_pref(&playlist_scope("Dubbed"), true, None);
        assert_eq!(lib.tracks_for(&film), TrackPrefs::default());
        assert_eq!(
            lib.track_prefs(&playlist_scope("Dubbed")),
            TrackPrefs::default()
        );
    }

    #[test]
    fn network_pages_keep_streams_and_feeds() {
        let tmp = TempDir::new();
        let mut lib = Library::open(&tmp.0.join("state"), false);
        let feed = "https://example.com/feed.xml";
        lib.add_address(NetKind::Stream, "https://example.com/live.m3u8", "Live");
        lib.add_address(NetKind::Podcast, feed, "example.com");
        lib.add_address(NetKind::Stream, "udp://239.0.0.1:1234", "Multicast");
        lib.save().unwrap();

        let mut lib = Library::open(&tmp.0.join("state"), false);
        let network = lib.entries(&Place::Network);
        assert_eq!(
            labels(&network),
            "Add Network Stream|Podcasts|Radio|Multicast|Live"
        );
        // A feed lists its episodes once fetched, and takes the feed's title as its name.
        assert!(lib.wants_fetch(&Place::Podcast(feed.into())));
        lib.fetching(&Place::Podcast(feed.into()));
        assert!(!lib.wants_fetch(&Place::Podcast(feed.into())));
        let episodes = net::parse_feed(
            r#"<rss><channel><title>Cast</title><item><title>One</title>
            <enclosure url="one.mp3"/><pubDate>Mon, 26 Oct 2026 08:00:00 GMT</pubDate></item>
            </channel></rss>"#,
            feed,
        );
        lib.feed_fetched(feed, Ok(episodes));
        assert_eq!(labels(&lib.entries(&Place::Podcasts)), "Add Podcast|Cast");
        let page = lib.entries(&Place::Podcast(feed.into()));
        let Entry::Media { path, subtitle, .. } = &page[0] else {
            panic!("an episode")
        };
        assert_eq!(path, Path::new("https://example.com/one.mp3"));
        assert_eq!(subtitle.as_deref(), Some("26 Oct 2026"));
        // A feed fetched a while ago fetches again, showing its episodes meanwhile, and keeps
        // them when that fails.
        let podcast = Place::Podcast(feed.into());
        assert!(!lib.wants_fetch(&podcast));
        let long_ago = std::time::Instant::now() - FEED_REFRESH - std::time::Duration::from_secs(1);
        lib.feed_times.insert(feed.into(), long_ago);
        assert!(lib.wants_fetch(&podcast));
        lib.fetching(&podcast);
        assert!(!lib.wants_fetch(&podcast));
        assert_eq!(lib.entries(&podcast).len(), 1);
        lib.feed_fetched(feed, Err("offline".into()));
        assert_eq!(lib.entries(&podcast).len(), 1);
        // A failed directory fetch is tried again when the page shows again.
        lib.fetching(&Place::Radio);
        lib.stations_fetched(Err("offline".into()));
        assert_eq!(labels(&lib.entries(&Place::Radio)), "offline");
        lib.retry_failed(&Place::Radio);
        assert!(lib.wants_fetch(&Place::Radio));

        lib.remove_address(NetKind::Stream, "udp://239.0.0.1:1234");
        assert_eq!(lib.entries(&Place::Network).len(), 4);
    }

    #[test]
    fn stream_addresses_stay_as_given() {
        let url = Path::new("https://example.com/live/index.m3u8");
        assert_eq!(abs(url), url);
        assert!(abs(Path::new("clip.mp4")).is_absolute());
    }

    #[test]
    fn home_sections_from_recent_data() {
        let tmp = TempDir::new();
        let root = tmp.0.join("Videos");
        tmp.touch(r"Videos\Alpha\x.mp4");
        tmp.touch(r"Videos\Beta\x.mp4");
        tmp.touch(r"Videos\half.mp4");
        let mut lib = Library::open(&tmp.0.join("state"), false);
        lib.add_root(Section::Videos, root.clone());
        let v = |n: &str| root.join(n);
        let s = |n: &str| PathBuf::from(format!(r"E:\Music\{n}.mp3"));
        let t = |n: &str, title: &str, artist: &str| Track {
            path: s(n),
            title: Some(title.into()),
            artist: Some(artist.into()),
            ..Default::default()
        };
        lib.set_tracks(vec![
            t("1", "Bohemian Rhapsody", "Queen"),
            t("2", "Dancing Queen", "ABBA"),
        ]);

        lib.record_position(&v("half.mp4"), 50.0, 100.0);
        lib.record_position(&v("done.mp4"), 99.0, 100.0);
        lib.record_position(&v("barely.mp4"), 1.0, 100.0);
        lib.record_position(&s("1"), 10.0, 200.0);
        lib.record_position(&s("2"), 10.0, 200.0);
        lib.record_position(&s("1"), 20.0, 200.0);
        for dir in ["Alpha", "Alpha", "Beta", "Gone"] {
            lib.visit_folder(&root.join(dir));
        }
        for name in ["Mix", "Chill", "Unplayed"] {
            lib.create_playlist(name).unwrap();
        }
        for name in ["Mix", "Chill", "Mix"] {
            lib.record_playlist_play(name);
        }

        let home = lib.entries(&Place::Home);
        assert_eq!(
            labels(&home),
            "Continue Watching|half.mp4|\
             Listen Again|Bohemian Rhapsody|Dancing Queen|\
             Frequently Visited Folders|Alpha|Beta|\
             Frequently Visited Playlists|Mix|Chill|\
             Frequently Listened Artists|Queen|ABBA"
        );
        let more: Vec<Option<Place>> = home
            .iter()
            .filter_map(|e| match e {
                Entry::Header { more, .. } => Some(more.clone()),
                _ => None,
            })
            .collect();
        let expected = [
            Place::Recent,
            Place::Recent,
            Place::Videos,
            Place::Playlists,
            Place::Artists,
        ];
        assert_eq!(more, expected.map(Some));
        let Entry::Media {
            progress, finished, ..
        } = &home[1]
        else {
            panic!()
        };
        assert_eq!((*progress, *finished), (Some(0.5), false));
        assert_eq!(
            home[6],
            folder("Alpha", Place::Folder(Section::Videos, root.join("Alpha")))
        );

        let recent = lib.entries(&Place::Recent);
        assert_eq!(labels(&recent), "1.mp3|2.mp3|barely.mp4|done.mp4|half.mp4");
        let Entry::Media {
            finished, subtitle, ..
        } = &recent[3]
        else {
            panic!()
        };
        assert!(*finished);
        assert_eq!(*subtitle, None);
        let Entry::Media { subtitle, .. } = &recent[0] else {
            panic!()
        };
        assert_eq!(subtitle.as_deref(), Some("Queen"));

        // A fresh library has nothing to show.
        let empty = Library::open(&tmp.0.join("other"), false);
        assert!(empty.entries(&Place::Home).is_empty());
    }

    #[test]
    fn albums_grouped_from_tracks() {
        let tmp = TempDir::new();
        let mut lib = Library::open(&tmp.0, false);
        let t = |path: &str, album: Option<&str>, genre: &str| Track {
            path: PathBuf::from(path),
            album: album.map(Into::into),
            genre: Some(genre.into()),
            ..Default::default()
        };
        lib.set_tracks(vec![
            t(r"E:\M\Arrival\10 Why.mp3", Some("Arrival"), "Pop"),
            t(r"E:\M\Arrival\2 Money.mp3", Some("Arrival "), "Pop"),
            t(
                r"E:\M\Opera\1 Death.mp3",
                Some(" A Night at the Opera"),
                "Rock",
            ),
            t(r"E:\M\loose.mp3", None, " "),
            t(r"E:\M\blank.mp3", Some(""), "Rock"),
        ]);

        assert_eq!(
            labels(&lib.entries(&Place::Albums)),
            "A Night at the Opera|Arrival"
        );
        assert_eq!(
            lib.entries(&Place::Albums)[1],
            folder("Arrival", Place::Album("Arrival".into()))
        );
        let arrival = lib.entries(&Place::Album("Arrival".into()));
        assert_eq!(labels(&arrival), "2 Money|10 Why");
        assert_eq!(labels(&lib.entries(&Place::Genres)), "Pop|Rock");
        assert_eq!(lib.entries(&Place::Genre("Rock".into())).len(), 2);
        assert_eq!(lib.entries(&Place::Songs).len(), 5);
    }

    #[test]
    fn music_files_walk_roots() {
        let tmp = TempDir::new();
        for f in [
            r"M\b.mp3",
            r"M\a\c.FLAC",
            r"M\a\v.mp4",
            r"M\.hidden\d.mp3",
            r"M\x.txt",
        ] {
            tmp.touch(f);
        }
        let mut lib = Library::open(&tmp.0.join("state"), false);
        let m = tmp.0.join("M");
        lib.add_root(Section::Music, m.clone());
        lib.add_root(Section::Music, m.join("a"));
        assert_eq!(lib.music_files(), [m.join("b.mp3"), m.join(r"a\c.FLAC")]);
        // Before a scan, Songs lists the files by name.
        assert_eq!(labels(&lib.entries(&Place::Songs)), "b|c");
    }
}

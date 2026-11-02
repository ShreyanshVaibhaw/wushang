//! wushang: a small video player built in the shape of VLC's pipeline, with the interface from
//! Juxtopposed's VLC redesign.
//!
//! demux thread -> video and audio decoder threads -> Metal renderer + Core Graphics UI (this thread)
//!                                                 -> Core Audio callback (drives the clock)

mod ass;
mod assoc;
mod audio;
mod bookmarks;
mod captions;
mod capture;
mod codepage;
mod convert;
mod d2d;
mod disc;
mod dsp;
mod hdr;
mod library;
mod net;
mod nv12;
mod platform;
mod player;
mod prefs;
mod probe;
mod profiles;
mod queue;
mod record;
mod render;
mod session;
mod snapshot;
mod stream;
mod subs;
mod tags;
mod thumbs;
mod tonemap;
mod ui;
mod update;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use d2d::{
    IDC_HAND, IDC_IBEAM, VIRTUAL_KEY, VK_BACK, VK_D, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F,
    VK_F1, VK_HOME, VK_I, VK_LEFT, VK_M, VK_N, VK_O, VK_OEM_5, VK_OEM_COMMA, VK_P, VK_Q, VK_R,
    VK_RETURN, VK_RIGHT, VK_S, VK_SPACE, VK_UP,
};
use ffmpeg_next as ff;
use platform::{Event, Rect, Window};

use bookmarks::Bookmarks;
use captions::CaptionStyle;
use dsp::{EQ_PRESETS, StereoMode};
use library::{Entry, Library, MediaKind, Place, Section, Track};
use net::NetKind;
use player::{HwDevice, Shared, VideoFrame};
use prefs::Prefs;
use queue::{Queue, Repeat};
use render::{DeviceLock, Flip, Renderer, VideoAdjust, Zoom};
use session::{Session, TrackChoice, TrackKind};
use subs::Cues;
use ui::panels::{ASPECT, CROP};
use ui::{
    Action, Category, Dialog, DialogAction, Hit, Layout, MenuAction, Page, Panel, PanelAction,
    PanelData, PrefsAction, QueueAction, QueueData, QueueItem, Scene, TrackTab, Ui,
};

/// The window's event handler is a plain function, so it reaches shared state through statics.
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
/// The open file's length in seconds, as f64 bits, for seek clicks.
static DURATION: AtomicU64 = AtomicU64::new(0);
/// A file dropped on the window; the render loop opens it.
static OPEN: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Files opened from Finder or the Dock while running, in order; the render loop plays them, as
/// a second launch did on Windows.
static OPEN_PLAY: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static RESIZED: AtomicBool = AtomicBool::new(false);
/// Screens changed (HDR mode, brightness, layout) or the window moved: check for HDR again.
static DISPLAY_CHANGED: AtomicBool = AtomicBool::new(false);
/// The full window's restored frame and whether it was zoomed, saved while it is the mini
/// player.
static MINI: Mutex<Option<(Rect, bool)>> = Mutex::new(None);
/// Thumbnail size in pixels: the grid tile at 125% scaling, 16:9.
const THUMB_SIZE: (u32, u32) = (400, 225);
/// Pixels across the seek bar preview's still; it shows 200 DIPs wide, so this stays sharp up to
/// 200% scaling.
const SEEK_STILL_WIDTH: u32 = 400;
/// How often playback positions go into the library while playing.
const RECORD_EVERY: Duration = Duration::from_secs(5);
const SAVE_EVERY: Duration = Duration::from_secs(30);
/// Smallest window in DIPs.
const MIN_SIZE: (f32, f32) = (800.0, 450.0);

thread_local! {
    /// Pointer state, shared by the window procedure and the render loop on the UI thread.
    static UI: RefCell<Ui> = RefCell::new(Ui::new());
    /// Clicks the render loop handles, because they need the library or open files.
    static PENDING: RefCell<Vec<Hit>> = const { RefCell::new(Vec::new()) };
    /// Changes from the tracks and settings popups, applied by the render loop.
    static PANEL_ACTIONS: RefCell<Vec<PanelAction>> = const { RefCell::new(Vec::new()) };
    /// Changes from the queue panel, applied by the render loop.
    static QUEUE_ACTIONS: RefCell<Vec<QueueAction>> = const { RefCell::new(Vec::new()) };
    /// Menus to open (what was clicked, and where) and menu choices, handled by the render
    /// loop because they need the library and the open file.
    static MENU_REQUESTS: RefCell<Vec<(Hit, f32, f32)>> = const { RefCell::new(Vec::new()) };
    static MENU_ACTIONS: RefCell<Vec<MenuAction>> = const { RefCell::new(Vec::new()) };
    /// Requests from the open dialog, applied by the render loop.
    static DIALOG_ACTIONS: RefCell<Vec<DialogAction>> = const { RefCell::new(Vec::new()) };
    /// Ctrl+I asked for the Properties dialog; the render loop knows which file.
    static WANT_PROPERTIES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The render loop saves the frame on screen, which only it holds.
    static WANT_SNAPSHOT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The settings in force. The window procedure reads the keys', wheel's and clicks'; the
    /// render loop applies the rest.
    static PREFS: RefCell<Prefs> = RefCell::new(Prefs::default());
    /// Requests from the Settings window, applied by the render loop.
    static PREFS_ACTIONS: RefCell<Vec<PrefsAction>> = const { RefCell::new(Vec::new()) };
    /// Requests from the first-run welcome, applied by the render loop.
    static ONBOARDING_ACTIONS: RefCell<Vec<ui::onboarding::OnboardingAction>> =
        const { RefCell::new(Vec::new()) };
    /// Into the mini player (None) or out of it to a page. The render loop does it, since
    /// moving the window sends messages to the window procedure, which borrows `UI`.
    static MINI_REQUEST: std::cell::Cell<Option<Option<Page>>> = const { std::cell::Cell::new(None) };
    /// Where the mini player last was: its left and bottom edges on screen, in points.
    static MINI_SPOT: std::cell::Cell<Option<(f64, f64)>> = const { std::cell::Cell::new(None) };
    /// The mini player's card in DIPs, as the user last sized it (the design's at first).
    static MINI_CARD: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((ui::mini::W, ui::mini::H)) };
    /// Whether the mini player window is sized with its queue open.
    static MINI_QUEUE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn prefs() -> Prefs {
    PREFS.with_borrow(Clone::clone)
}

/// Sleep timer choices in minutes, by index into the panel's list; the last one is "End of file".
const SLEEP_MINUTES: [u64; 5] = [0, 15, 30, 45, 60];
const SLEEP_END_OF_FILE: usize = 5;
/// The zoom every video opens with, by index into `ui::panels::ZOOM`: Fill, so the picture
/// covers the window with no black bars.
const DEFAULT_ZOOM: usize = 1;

fn main() {
    let result = run();
    update::finish();
    platform::finish();
    if let Err(e) = result {
        eprintln!("wushang: {e}");
        std::process::exit(1);
    }
}

/// What the render loop owns besides the window: the library and the file playing.
struct App {
    shared: Arc<Shared>,
    /// The GPU decoder device; Settings can turn its use off for the next file.
    hw: Option<HwDevice>,
    audio: Arc<audio::Device>,
    library: Library,
    library_changed: bool,
    /// An update downloading: what it saved, or why it failed.
    downloading: Option<mpsc::Receiver<Result<update::Saved, String>>>,
    session: Option<Session>,
    /// The frame on screen. It holds a decoder surface, so it goes before its session.
    current: Option<VideoFrame>,
    /// Files previous and next step through: the place the file was opened from, then what
    /// the queue panel changed.
    queue: Queue,
    /// Lengths of the queue's files, probed in the background.
    durations: HashMap<PathBuf, f64>,
    queue_prober: thumbs::Prober,
    /// The queue's files watched to the end, dimmed in the panel.
    watched: HashSet<PathBuf>,
    bookmarks: Bookmarks,
    recorded: Instant,
    /// Flip, rotation, zoom, crop and colour, from the settings panel's Video tab.
    adjust: VideoAdjust,
    /// The panel's zoom, aspect and crop choices, kept as list indices.
    zoom: usize,
    aspect: usize,
    crop: usize,
    /// The embedded subtitle track's decoder thread and cues.
    embedded: Option<(thread::JoinHandle<()>, Arc<Cues>)>,
    /// Subtitle files next to the open file, loaded when first chosen, and the one shown.
    external: Vec<(PathBuf, Option<Arc<Cues>>)>,
    external_selected: Option<usize>,
    /// Positive shows subtitles later.
    sub_delay: f64,
    /// How fast the subtitles' clock runs (1 = as written), and how many times longer each line
    /// stays (0 = as written).
    sub_speed: f64,
    sub_duration: f64,
    /// Playing backwards: the position last stepped back to, and when.
    reverse: Option<(f64, Instant)>,
    caption: CaptionStyle,
    devices: Vec<String>,
    /// Index into the panel's sleep list, and when a timed one runs out.
    sleep: usize,
    sleep_at: Option<Instant>,
    show_statistics: bool,
    /// The file just opened wants fullscreen ("Start videos in fullscreen"); the render loop
    /// has the window.
    enter_fullscreen: bool,
    /// When the settings changed last without being saved; saved a moment later, so a dragged
    /// slider doesn't write the file at every step.
    prefs_changed: Option<Instant>,
    profiles: profiles::Profiles,
    /// The line over the video and when it appeared.
    notice: Option<(String, Instant)>,
    /// Lines for the notice from other threads, such as the snapshot writer.
    notices: (mpsc::Sender<String>, mpsc::Receiver<String>),
    /// A stream address connecting on a thread of its own, so the window never waits on it.
    opening: Option<Opening>,
    /// Podcast feeds and the radio directory, fetched in the background.
    fetcher: net::Fetcher,
    /// The library page shown last, to notice when the Network pages are entered.
    shown_place: Option<Place>,
    /// Names for stream addresses played from a library page: a station or episode name.
    stream_titles: HashMap<PathBuf, String>,
    /// Where the last recording reports back; it outlives the session it recorded.
    record_control: Option<record::Control>,
    /// The capture menu's devices, found on a thread of their own.
    capture_devices: Arc<Mutex<Vec<capture::Device>>>,
    /// The conversion running, one at a time.
    converting: Option<convert::Job>,
    /// The playlist the queue came from, for the bars' playlist name; the next `play` takes
    /// `next_playlist`, which only playing from a playlist sets.
    playing_playlist: Option<String>,
    next_playlist: Option<String>,
    /// The HTTP server players connect to while streaming out, and where its errors come back.
    stream_server: Option<stream::Server>,
    stream_control: Option<record::Control>,
    /// The seek bar being dragged; see `App::scrub`.
    scrub: Option<Scrub>,
}

/// A drag of the seek bar, which shows the video at the pointer as it moves, as YouTube does.
struct Scrub {
    /// Playing when the drag began, so it plays again when it ends.
    resume: bool,
    /// The last target sent, when, and the serial the seek before it had.
    sent: f64,
    at: Instant,
    serial: u64,
    /// Whether that seek was exact.
    exact: bool,
}

/// While dragging, a quick seek goes out once the last one's frame is on screen, or after this
/// long if it never comes.
const SCRUB_WAIT: Duration = Duration::from_millis(250);
/// A pointer that rests this long gets an exact seek, for the frame under it.
const SCRUB_REST: Duration = Duration::from_millis(150);

struct Opening {
    path: PathBuf,
    ready: mpsc::Receiver<Result<ff::format::context::Input, String>>,
    cancel: Arc<AtomicBool>,
}

impl App {
    /// Plays `path` from where it was left, with `queue` for previous and next. Repeat and
    /// shuffle carry over from the last queue, as in VLC.
    /// Counts a play of playlist `name` and marks the next `play` as coming from it.
    fn mark_playlist(&mut self, name: &str) {
        self.library.record_playlist_play(name);
        self.next_playlist = Some(name.to_owned());
    }

    fn play(&mut self, path: &Path, queue: Vec<PathBuf>) -> bool {
        self.playing_playlist = self.next_playlist.take();
        let (repeat, shuffle) = (self.queue.repeat(), self.queue.shuffle());
        self.queue = Queue::new(queue, Some(path));
        self.queue.set_repeat(repeat);
        self.queue.set_shuffle(shuffle);
        self.queue_changed();
        self.open(path)
    }

    /// Opens `path` from where it was left, keeping the queue as it is. A stream address
    /// connects in the background and starts playing once it answers.
    fn open(&mut self, path: &Path) -> bool {
        self.stop();
        let p = prefs();
        let options = session::NetOptions {
            timeout: p.net_timeout,
            reconnect: p.net_reconnect,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        if net::is_address(path) {
            let (done, ready) = mpsc::channel();
            let (url, flag) = (path.to_owned(), cancel.clone());
            thread::spawn(move || _ = done.send(session::open_input(&url, options, &flag)));
            // Disc and capture addresses have "://" too, but no host.
            let host = net::is_url_path(path)
                .then(|| net::host_of(&path.to_string_lossy()))
                .flatten();
            let line = match host {
                Some(host) => format!("Connecting to {host}..."),
                None if disc::is_disc_path(path) => "Opening the disc...".to_owned(),
                None => "Opening the capture device...".to_owned(),
            };
            self.notice = Some((line, Instant::now()));
            self.opening = Some(Opening {
                path: path.to_owned(),
                ready,
                cancel,
            });
            return true;
        }
        let input = session::open_input(path, options, &cancel);
        self.start(path, input.map(|input| (input, cancel)))
    }

    /// Starts playing what `open` opened at `path`.
    fn start(
        &mut self,
        path: &Path,
        input: Result<(ff::format::context::Input, Arc<AtomicBool>), String>,
    ) -> bool {
        // A new file plays forwards.
        self.reverse = None;
        // A folder's or playlist's track choices go over Settings' for its files.
        let mut p = prefs();
        let tracks = self.library.tracks_for(path);
        if let Some(audio) = tracks.audio {
            p.audio_language = audio;
        }
        match tracks.subtitles.as_deref() {
            Some("off") => p.subtitles = false,
            Some(language) => {
                p.subtitles = true;
                p.sub_language = language.to_owned();
            }
            None => {}
        }
        let start = if p.resume == 0 {
            self.library.resume_position(path)
        } else {
            None
        };
        let choice = TrackChoice {
            audio_language: p.audio_language.clone(),
            sub_language: p.sub_language.clone(),
            subtitles: p.subtitles,
        };
        let hw = self.hw.as_ref().filter(|_| p.hw_decoding);
        let opened = input.map_err(Into::into).and_then(|(input, cancel)| {
            Session::open_with(
                input,
                cancel,
                session::NetOptions {
                    timeout: p.net_timeout,
                    reconnect: p.net_reconnect,
                },
                path,
                start,
                &choice,
                &self.shared,
                hw,
                &self.audio,
            )
        });
        match opened {
            Ok(mut s) => {
                if let Some(title) = self.stream_titles.get(path) {
                    s.title = title.clone();
                }
                if s.duration > 0.0 {
                    self.durations.insert(path.to_owned(), s.duration);
                }
                self.enter_fullscreen = p.start_fullscreen && s.video_size.is_some();
                self.session = Some(s);
                // Each file starts at the default speed and aspect ratio, as in VLC.
                let effects = self.audio.effects();
                effects.set_speed(p.speed);
                self.shared.clock.set_speed(effects.speed());
                self.aspect = ASPECT.iter().position(|a| *a == p.aspect).unwrap_or(0);
                self.adjust.aspect = ratio(ASPECT.get(self.aspect).copied());
                // Opening left it playing.
                if !p.autoplay {
                    self.shared.toggle_pause();
                }
                self.start_subtitles(&p);
                self.record();
                self.refresh_watched();
                true
            }
            Err(e) => {
                eprintln!("wushang: {e}");
                if net::is_address(path) {
                    let host = net::is_url_path(path)
                        .then(|| net::host_of(&path.to_string_lossy()))
                        .flatten();
                    let line = match host {
                        Some(host) => format!("Can't play the stream from {host}"),
                        None if disc::is_disc_path(path) => {
                            "Can't play the disc (an encrypted one needs AACS or CSS keys)"
                                .to_owned()
                        }
                        None => "Can't open the capture device".to_owned(),
                    };
                    self.notice = Some((line, Instant::now()));
                }
                false
            }
        }
    }

    /// Plays a file opened from outside the library, queued with the rest of its folder.
    fn play_file(&mut self, path: &Path) -> bool {
        if net::is_address(path) {
            return self.play(path, vec![path.to_owned()]);
        }
        // A DVD or Blu-ray folder, or a disc image, opened or dropped like a file.
        if (path.is_dir()
            || path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("iso")))
            && let Some(address) = disc::address(path)
        {
            return self.play(&address, vec![address.clone()]);
        }
        let queue = path.parent().map_or_else(Vec::new, |dir| {
            self.library
                .media_in(&Place::Folder(section_of(path), dir.to_owned()))
        });
        self.play(path, queue)
    }

    /// Starts recording the open file (into ~/Movies/wushang unless Settings says otherwise), or
    /// stops; the notice says where it went.
    fn toggle_record(&mut self) {
        // A notice about the last recording would otherwise be lost under the new one's.
        self.take_recordings();
        let Some(session) = &self.session else {
            return;
        };
        if session.recording() {
            session.set_recording(None);
            return;
        }
        let (dir, ext) = PREFS.with_borrow(|p| {
            let key = prefs::RECORD_DIR.get(p.record_dir).map_or("", |c| c.0);
            let own = snapshot::Folder::Videos;
            let ext = prefs::RECORD_FORMAT
                .get(p.record_format)
                .map_or("mkv", |c| c.0);
            (snapshot::output_dir(key, own, &session.path), ext)
        });
        let dir = match dir {
            Ok(dir) => dir,
            Err(e) => {
                self.notice = Some((format!("Can't record ({e})"), Instant::now()));
                return;
            }
        };
        let stem = snapshot::stem(&session.path, &session.title);
        let seconds = session.position().unwrap_or(0.0);
        let path = snapshot::free_path(&dir, &snapshot::file_name(&stem, seconds), ext);
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        session.set_recording(Some(path));
        self.record_control = Some(session.record_control());
        self.notice = Some((format!("Recording to {name}"), Instant::now()));
    }

    /// Converts `path` into ~/Movies/wushang (MP4) or ~/Music/wushang (MP3) in the background.
    fn convert(&mut self, path: &Path, target: convert::Target) {
        if let Some(job) = &self.converting {
            let name = job.output.file_name().unwrap_or_default().to_string_lossy();
            self.notice = Some((format!("Still converting {name}"), Instant::now()));
            return;
        }
        let folder = match target {
            convert::Target::Mp4 => snapshot::Folder::Videos,
            convert::Target::Mp3 => snapshot::Folder::Music,
        };
        let dir = snapshot::known_folder(folder)
            .map(|d| d.join("wushang"))
            .and_then(|dir| std::fs::create_dir_all(&dir).map(|()| dir));
        match dir {
            Ok(dir) => {
                let stem = snapshot::stem(path, "");
                let output = snapshot::free_path(&dir, &stem, target.extension());
                self.converting = Some(convert::Job::start(path, target, output));
            }
            Err(e) => self.notice = Some((format!("Can't convert ({e})"), Instant::now())),
        }
    }

    /// Keeps the notice on the running conversion's progress, and says how it ended.
    fn watch_conversion(&mut self) {
        let Some(job) = &self.converting else { return };
        let name = job
            .output
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let line = match job.finished() {
            None => format!("Converting to {name}: {:.0}%", job.progress() * 100.0),
            Some(result) => {
                self.converting = None;
                match result {
                    Ok(path) => format!("Converted to {}", path.display()),
                    Err(e) if e == "cancelled" => "Conversion stopped".to_owned(),
                    Err(e) => format!("Can't convert {name}: {e}"),
                }
            }
        };
        // While it runs, the notice stays up; the final line then lasts the usual while.
        if self.notice.as_ref().is_none_or(|(text, _)| *text != line) {
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        self.notice = Some((line, Instant::now()));
    }

    /// Shows how the last recording ended, once its file is closed.
    fn take_recordings(&mut self) {
        if let Some(control) = &self.record_control {
            let done = std::mem::take(&mut control.lock().unwrap().done);
            for result in done {
                let line = match result {
                    Ok(path) => format!("Recording saved to {}", path.display()),
                    Err(e) => format!("Can't record: {e}"),
                };
                self.notice = Some((line, Instant::now()));
                UI.with_borrow_mut(|ui| ui.dirty = true);
            }
        }
        if let Some(control) = &self.stream_control {
            let done = std::mem::take(&mut control.lock().unwrap().done);
            for e in done.into_iter().filter_map(Result::err) {
                self.notice = Some((format!("Can't stream: {e}"), Instant::now()));
                UI.with_borrow_mut(|ui| ui.dirty = true);
            }
        }
        // The stream ended with its media, or failed: let the players go.
        if self.stream_server.is_some() && !self.session.as_ref().is_some_and(Session::streaming) {
            self.stream_server = None;
        }
    }

    /// Starts streaming the open media to other devices over HTTP, or stops.
    fn toggle_stream(&mut self) {
        self.take_recordings();
        let Some(session) = &self.session else {
            return;
        };
        if session.streaming() {
            session.set_streaming(None);
            self.stream_server = None;
            self.notice = Some(("Streaming stopped".to_owned(), Instant::now()));
            return;
        }
        // Every network interface, so other devices reach it; the macOS firewall may ask to let
        // it through.
        let port = PREFS.with_borrow(|p| p.stream_port);
        let server = match stream::Server::start(std::net::Ipv4Addr::UNSPECIFIED.into(), port) {
            Ok(server) => server,
            Err(e) => {
                self.notice = Some((format!("Can't stream ({e})"), Instant::now()));
                return;
            }
        };
        session.set_streaming(Some(server.broadcast.clone()));
        self.stream_control = Some(session.stream_control());
        let line = format!(
            "Streaming at {}: open it in a player on another device",
            server.url()
        );
        self.notice = Some((line, Instant::now()));
        self.stream_server = Some(server);
    }

    fn stop(&mut self) {
        self.reverse = None;
        if let Some(o) = self.opening.take() {
            o.cancel.store(true, Relaxed);
        }
        self.record();
        self.current = None;
        self.session = None;
        // The session's stop flag ends the subtitle decoder too.
        if let Some((thread, _)) = self.embedded.take() {
            _ = thread.join();
        }
        self.external.clear();
        self.external_selected = None;
    }

    /// The file's starting subtitles: the embedded track the session picked (a default+forced
    /// one, or one in the preferred language), else a matching subtitle file next to it, as VLC
    /// loads them automatically. Settings can turn off subtitles, or just the files.
    fn start_subtitles(&mut self, p: &Prefs) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        // Fonts the file carries, which its styled subtitles ask for by name, unless Settings'
        // Extensions turned them off. The last file's go first.
        let fonts = session.take_fonts();
        ass::clear_fonts();
        if p.embedded_fonts {
            for (name, data) in fonts {
                ass::add_font(&name, &data);
            }
        }
        let initial = session.take_initial_subtitles();
        self.external = subs::find_external(&session.path)
            .into_iter()
            .map(|p| (p, None))
            .collect();
        match initial {
            Some(stream) => self.decode_embedded(stream),
            None if p.subtitles && p.auto_subs && !self.external.is_empty() => {
                self.show_external(Some(0))
            }
            None => {}
        }
    }

    /// The canvas styled (ASS) subtitles are drawn on: the video's stored and shown sizes, for
    /// audio a 1080p one.
    fn subtitle_canvas(&self) -> ass::Canvas {
        let session = self.session.as_ref();
        match session.and_then(|s| Some((s.video_size?, s.display_size()?))) {
            Some((stored, shown)) => ass::Canvas { stored, shown },
            None => (1920, 1080).into(),
        }
    }

    fn decode_embedded(&mut self, stream: session::SubtitleStream) {
        let decoder = subs::spawn_embedded(
            stream.parameters,
            stream.time_base,
            stream.packets,
            self.shared.clone(),
            self.subtitle_canvas(),
        );
        if let Some((old, _)) = self.embedded.replace(decoder) {
            _ = old.join();
        }
    }

    fn show_external(&mut self, i: Option<usize>) {
        self.external_selected = None;
        let canvas = self.subtitle_canvas();
        let Some((path, cues)) = i.and_then(|i| self.external.get_mut(i)) else {
            return;
        };
        if cues.is_none() {
            match subs::load_external(path, canvas) {
                Ok(c) => *cues = Some(c),
                Err(e) => {
                    eprintln!("subtitles: {e}");
                    return;
                }
            }
        }
        self.external_selected = i;
    }

    /// Follows a drag of the seek bar, `fraction` along it (None once it ends): playback pauses,
    /// and the video shows the keyframe under the pointer as it moves, then the exact frame
    /// where it rests. The drop's own exact seek comes from the bar; playing resumes after it.
    fn scrub(&mut self, fraction: Option<f64>) {
        let shared = &self.shared;
        let duration = self.session.as_ref().map_or(0.0, |s| s.duration);
        let Some(fraction) = fraction.filter(|_| duration > 0.0) else {
            if let Some(scrub) = self.scrub.take()
                && scrub.resume
                && shared.paused.load(Relaxed)
            {
                shared.toggle_pause();
            }
            return;
        };
        let target = fraction * duration;
        let serial = shared.serial.load(Relaxed);
        let scrub = self.scrub.get_or_insert_with(|| {
            let resume = !shared.paused.load(Relaxed);
            if resume {
                shared.toggle_pause();
            }
            Scrub {
                resume,
                sent: f64::NAN,
                at: Instant::now(),
                serial,
                exact: false,
            }
        });
        // The last seek is done once the demuxer has made it and its first frame is up.
        let shown =
            serial != scrub.serial && self.current.as_ref().is_some_and(|c| c.serial == serial);
        let ready = shown || scrub.at.elapsed() > SCRUB_WAIT;
        if target != scrub.sent && ready {
            shared.seek_quick(target);
        } else if target == scrub.sent && !scrub.exact && ready && scrub.at.elapsed() > SCRUB_REST {
            shared.seek_to(target);
            scrub.exact = true;
            scrub.at = Instant::now();
            scrub.serial = serial;
            return;
        } else {
            return;
        }
        scrub.sent = target;
        scrub.exact = false;
        scrub.at = Instant::now();
        scrub.serial = serial;
    }

    /// The cues to show at media time `t`.
    fn cues_at(&self, t: f64) -> Vec<subs::Cue> {
        let t = (t - self.sub_delay) * self.sub_speed;
        self.subtitles()
            .map_or_else(Vec::new, |c| c.active_stretched(t, self.sub_duration))
    }

    /// The subtitle track on screen.
    fn subtitles(&self) -> Option<&Arc<Cues>> {
        match self.external_selected {
            Some(i) => self.external[i].1.as_ref(),
            None => self.embedded.as_ref().map(|(_, c)| c),
        }
    }

    /// Where the subtitles have lines, in media time, for the seek bar's timeline.
    fn subtitle_spans(&self) -> Vec<(f64, f64)> {
        let at = |t: f64| t / self.sub_speed + self.sub_delay;
        let stretch = |(s, e): (f64, f64)| match self.sub_duration {
            f if f > 0.0 => (s, s + (e - s) * f),
            _ => (s, e),
        };
        let spans = self.subtitles().map_or_else(Vec::new, |c| c.spans());
        (spans.into_iter().map(stretch))
            .map(|(s, e)| (at(s), at(e)))
            .collect()
    }

    /// What the tracks and settings popups show.
    fn panel_data(&self) -> PanelData {
        let s = self.session.as_ref();
        let labels = |k| {
            s.map_or_else(Vec::new, |s| {
                s.tracks(k).iter().map(|t| t.label.clone()).collect()
            })
        };
        let selected = |k| {
            s.and_then(|s| {
                let stream = s.selected(k)?;
                s.tracks(k).iter().position(|t| t.stream == stream)
            })
        };
        let effects = self.audio.effects();
        let eq = effects.eq();
        let current = self.audio.current_output();
        let p = prefs();
        PanelData {
            video_tracks: labels(TrackKind::Video),
            video_selected: selected(TrackKind::Video),
            audio_tracks: labels(TrackKind::Audio),
            audio_selected: selected(TrackKind::Audio),
            sub_tracks: labels(TrackKind::Subtitle),
            sub_selected: if self.external_selected.is_some() {
                None
            } else {
                selected(TrackKind::Subtitle)
            },
            external_subs: self
                .external
                .iter()
                .map(|(p, _)| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
            external_selected: self.external_selected,
            devices: self.devices.clone(),
            device_selected: current
                .and_then(|c| self.devices.iter().position(|d| *d == c))
                .unwrap_or(0),
            speed: effects.speed(),
            audio_delay: effects.delay(),
            sub_delay: self.sub_delay,
            eq_presets: EQ_PRESETS.iter().map(|p| p.name.to_owned()).collect(),
            eq_preset: eq.enabled.then_some(eq.preset).flatten(),
            eq_custom: eq.enabled && eq.preset.is_none(),
            eq_bands: eq.bands,
            // The panel shows the preamp around 0; VLC's scale has unity at 12 dB.
            eq_preamp: eq.preamp - dsp::PREAMP_UNITY_DB,
            two_pass: eq.two_pass,
            recording: self.session.as_ref().is_some_and(Session::recording),
            // The address to open, and how many players have it open.
            streaming: (self.stream_server.as_ref())
                .filter(|_| self.session.as_ref().is_some_and(Session::streaming))
                .map(|s| (s.url(), s.broadcast.players())),
            stereo: StereoMode::ALL
                .iter()
                .position(|m| *m == effects.stereo())
                .unwrap_or(0),
            flip: match self.adjust.flip {
                Flip::None => 0,
                Flip::Horizontal => 1,
                Flip::Vertical => 2,
            },
            rotate: i32::from(self.adjust.rotate),
            zoom: self.zoom,
            aspect: self.aspect,
            crop: self.crop,
            brightness: self.adjust.brightness,
            contrast: self.adjust.contrast,
            hue: self.adjust.hue,
            saturation: self.adjust.saturation,
            sleep_timer: self.sleep,
            show_statistics: self.show_statistics,
            sub_size: self.caption.size_ratio / CaptionStyle::default().size_ratio,
            video_sizes: s.map_or_else(Vec::new, |s| {
                s.tracks(TrackKind::Video).iter().map(|t| t.size).collect()
            }),
            reverse: self.reverse.is_some(),
            visualizer: p.visualizer,
            sub_timeline: p.sub_timeline,
            sub_styles: p.sub_styles,
            hdr: p.hdr,
            sub_speed: self.sub_speed as f32,
            sub_duration: self.sub_duration as f32,
            sub_font: p.sub_font,
            sub_color: p.sub_color,
            outline_color: p.outline_color,
            profiles: self.profiles.names().to_vec(),
            profile: (self.profiles.names().iter())
                .position(|n| n == self.profiles.current())
                .unwrap_or(0),
        }
    }

    /// Applies one change from the popups.
    fn apply(&mut self, win: Window, action: PanelAction) {
        let effects = self.audio.effects();
        match action {
            PanelAction::SelectTrack(tab, i) => self.select_track(tab, i),
            PanelAction::SelectExternal(i) => self.show_external(i),
            PanelAction::AddSubtitleFile => pick_subtitle_file(Picked::SubtitleFile),
            PanelAction::AddAudioFile => pick_audio_file(),
            PanelAction::SelectDevice(i) => {
                // The list starts with the system default, which also follows the system's choice.
                let name = (i > 0).then(|| self.devices.get(i)).flatten();
                self.audio.switch_to(name.map(String::as_str));
                // The choice sticks, as Settings' "Output device".
                let device = name.cloned().unwrap_or_default();
                PREFS.with_borrow_mut(|p| p.device = device);
                self.prefs_changed = Some(Instant::now());
            }
            PanelAction::RefreshDevices => self.devices = audio::output_devices(),
            PanelAction::SetSpeed(v) => {
                effects.set_speed(v);
                self.shared.clock.set_speed(effects.speed());
            }
            PanelAction::SetAudioDelay(v) => effects.set_delay(v),
            PanelAction::SetSubDelay(v) => self.sub_delay = v.clamp(-10.0, 10.0),
            PanelAction::SetEqPreset(i) => effects.set_eq(i),
            PanelAction::SetEqBand(band, db) => {
                let eq = effects.eq();
                let mut bands = eq.bands;
                bands[band] = db;
                effects.set_eq_bands(bands, eq.preamp);
            }
            PanelAction::SetPreamp(db) => {
                effects.set_eq_bands(effects.eq().bands, db + dsp::PREAMP_UNITY_DB)
            }
            PanelAction::SetTwoPass(on) => effects.set_two_pass(on),
            PanelAction::SetStereo(i) => effects.set_stereo(StereoMode::ALL[i.min(4)]),
            PanelAction::SetFlip(f) => {
                self.adjust.flip = match f {
                    1 => Flip::Horizontal,
                    2 => Flip::Vertical,
                    _ => Flip::None,
                }
            }
            PanelAction::SetRotate(deg) => self.adjust.rotate = deg.rem_euclid(360) as u16,
            PanelAction::SetZoom(i) => self.zoom = i,
            PanelAction::SetAspect(i) => {
                self.aspect = i;
                self.adjust.aspect = ratio(ASPECT.get(i).copied());
            }
            PanelAction::SetCrop(i) => {
                self.crop = i;
                self.adjust.crop = ratio(CROP.get(i).copied());
            }
            PanelAction::SetBrightness(v) => self.adjust.brightness = v,
            PanelAction::SetContrast(v) => self.adjust.contrast = v,
            PanelAction::SetHue(v) => self.adjust.hue = v,
            PanelAction::SetSaturation(v) => self.adjust.saturation = v,
            PanelAction::ResetVideo => {
                self.adjust = VideoAdjust::default();
                (self.zoom, self.aspect, self.crop) = (DEFAULT_ZOOM, 0, 0);
            }
            PanelAction::SetSleepTimer(i) => {
                self.sleep = i;
                self.sleep_at = SLEEP_MINUTES
                    .get(i)
                    .filter(|m| **m > 0)
                    .map(|m| Instant::now() + Duration::from_secs(m * 60));
            }
            PanelAction::SetShowStatistics(on) => self.show_statistics = on,
            PanelAction::SetSubSize(v) => {
                self.caption.size_ratio = CaptionStyle::default().size_ratio * v.clamp(0.5, 2.0)
            }
            PanelAction::SetReverse(on) => self.set_reverse(on),
            PanelAction::SetVisualizer(i) => self.change_prefs(win, |p| p.visualizer = i),
            PanelAction::SetSubTimeline(on) => self.change_prefs(win, |p| p.sub_timeline = on),
            PanelAction::SetHdr(on) => self.change_prefs(win, |p| p.hdr = on),
            PanelAction::SetResolution(i) => {
                let i = i.or_else(|| ui::panels::best_video(&self.panel_data().video_sizes));
                if i.is_some() {
                    self.select_track(TrackTab::Video, i);
                }
            }
            PanelAction::SetStreaming(on) => {
                if on != self.session.as_ref().is_some_and(Session::streaming) {
                    self.toggle_stream();
                }
            }
            PanelAction::TestStereo => self.test_stereo(),
            PanelAction::SetSubSpeed(v) => self.sub_speed = f64::from(v.clamp(0.5, 2.0)),
            PanelAction::SetSubDuration(v) => self.sub_duration = f64::from(v.clamp(0.0, 4.0)),
            PanelAction::SetSubStyles(on) => self.change_prefs(win, |p| p.sub_styles = on),
            PanelAction::SetSubFont(i) => {
                if let Some(font) = prefs::FONTS.get(i) {
                    self.change_prefs(win, |p| p.sub_font = (*font).to_owned());
                }
            }
            PanelAction::SetSubColor(i) => {
                if let Some((rgb, _)) = prefs::COLORS.get(i) {
                    self.change_prefs(win, |p| p.sub_color = *rgb);
                }
            }
            PanelAction::SetOutlineColor(i) => {
                if let Some((rgb, _)) = prefs::COLORS.get(i) {
                    self.change_prefs(win, |p| p.outline_color = *rgb);
                }
            }
            PanelAction::SetProfile(i) => {
                if let Some(name) = self.profiles.names().get(i).cloned()
                    && let Err(e) = self.switch_profile(win, &name, false)
                {
                    eprintln!("profiles: cannot switch to {name}: {e}");
                }
            }
            PanelAction::EditSubtitle => self.edit_subtitle(win),
        }
    }

    fn select_track(&mut self, tab: TrackTab, i: Option<usize>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let kind = match tab {
            TrackTab::Video => TrackKind::Video,
            TrackTab::Audio => TrackKind::Audio,
            TrackTab::Subtitles => TrackKind::Subtitle,
        };
        let stream = i
            .and_then(|i| session.tracks(kind).get(i))
            .map(|t| t.stream);
        match session.select(kind, stream) {
            Ok(Some(subtitles)) => {
                self.external_selected = None;
                self.decode_embedded(subtitles);
                // The demuxer reads ahead, so the line on screen now was already read past; seek
                // in place (exact, so nothing jumps) to read it again.
                self.shared.seek_by(0.0);
            }
            Ok(None) => match kind {
                TrackKind::Subtitle => {
                    self.external_selected = None;
                    if let Some((old, _)) = self.embedded.take() {
                        _ = old.join();
                    }
                }
                // A turned-off video track leaves no picture behind.
                TrackKind::Video if stream.is_none() => self.current = None,
                _ => {}
            },
            Err(e) => eprintln!("tracks: {e}"),
        }
    }

    /// One line per fact, for the statistics overlay.
    fn statistics(&self, shown: u64, dropped: u64) -> Option<String> {
        let s = self.session.as_ref().filter(|_| self.show_statistics)?;
        let video = s
            .selected(TrackKind::Video)
            .and_then(|v| s.tracks(TrackKind::Video).iter().find(|t| t.stream == v));
        let audio = s
            .selected(TrackKind::Audio)
            .and_then(|a| s.tracks(TrackKind::Audio).iter().find(|t| t.stream == a));
        let decoding = match self.current.as_ref().map(|c| c.frame.format()) {
            Some(ff::format::Pixel::VIDEOTOOLBOX) => "GPU (VideoToolbox)",
            Some(_) => "software",
            None => "-",
        };
        let (w, h) = s.video_size.unwrap_or((0, 0));
        Some(format!(
            "Video  {} {w}x{h}, {decoding}\nAudio  {}\nFrames  {shown} shown, {dropped} dropped\nSpeed  {:.2}x",
            video.map_or("none", |t| t.codec.as_str()),
            audio.map_or("none", |t| t.codec.as_str()),
            self.audio.effects().speed(),
        ))
    }

    /// Saves where the open file is, for Continue Watching and resuming.
    fn record(&mut self) {
        self.recorded = Instant::now();
        let Some(s) = &self.session else { return };
        // A camera has no place to resume from.
        if !PREFS.with_borrow(|p| p.save_recent) || capture::is_capture_path(&s.path) {
            return;
        }
        let position = if s.finished() {
            s.duration
        } else {
            s.position().unwrap_or(0.0)
        };
        self.library.record_position(&s.path, position, s.duration);
        self.library_changed = true;
    }

    /// Plays the next (`delta` > 0) or previous file in the queue's order.
    fn step(&mut self, delta: isize) -> bool {
        let path = if delta > 0 {
            self.queue.next()
        } else {
            self.queue.previous()
        };
        path.is_some_and(|p| self.open(&p))
    }

    /// At the end of a file: the same one again under Repeat One, else the next.
    fn advance(&mut self) -> bool {
        self.queue.on_end().is_some_and(|p| self.open(&p))
    }

    /// Appends files to the queue, for the panel's + and the menus' "Add to Queue".
    pub fn add_to_queue(&mut self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.queue.add(paths);
            self.queue_changed();
        }
    }

    /// Asks for the lengths of files new to the queue, and which of them were watched.
    fn queue_changed(&mut self) {
        let unknown = (self.queue.items().iter())
            .filter(|p| !self.durations.contains_key(*p))
            .cloned()
            .collect();
        self.queue_prober.request_all(unknown);
        self.refresh_watched();
    }

    /// Files finished before, from the library's recent list.
    fn refresh_watched(&mut self) {
        let finished: HashSet<PathBuf> = (self.library.entries(&Place::Recent).into_iter())
            .filter_map(|e| match e {
                Entry::Media {
                    path,
                    finished: true,
                    ..
                } => Some(path),
                _ => None,
            })
            .collect();
        // The recent list keeps absolute paths; the queue keeps them as listed.
        self.watched = (self.queue.items().iter())
            .filter(|p| finished.contains(&library::abs(p)))
            .cloned()
            .collect();
    }

    /// What the queue panel shows.
    fn queue_data(&self) -> QueueData {
        let session = self.session.as_ref();
        QueueData {
            items: (self.queue.items().iter())
                .map(|p| QueueItem {
                    name: p
                        .file_name()
                        .unwrap_or(p.as_os_str())
                        .to_string_lossy()
                        .into_owned(),
                    duration: self.durations.get(p).copied().filter(|d| *d > 0.0),
                    watched: self.watched.contains(p),
                })
                .collect(),
            current: self.queue.current(),
            repeat: self.queue.repeat(),
            shuffle: self.queue.shuffle(),
            bookmarks: session.map_or_else(Vec::new, |s| {
                (self.bookmarks.for_file(&s.path).into_iter())
                    .map(|b| (b.name.clone(), b.seconds))
                    .collect()
            }),
            chapters: session.map_or_else(Vec::new, |s| s.chapters().to_vec()),
        }
    }

    /// Applies one change from the queue panel.
    fn apply_queue(&mut self, action: QueueAction) {
        match action {
            QueueAction::Play(i) => {
                if let Some(path) = self.queue.play_at(i) {
                    self.open(&path);
                }
            }
            QueueAction::Move(from, to) => self.queue.move_item(from, to),
            QueueAction::PlayNext(i) => self.queue.play_next(i),
            QueueAction::Remove(i) => _ = self.queue.remove(i),
            QueueAction::AddFiles => pick_media_files(Picked::QueueFiles),
            QueueAction::Sort => self.queue.sort_by_name(),
            QueueAction::CycleRepeat => self.queue.cycle_repeat(),
            QueueAction::ToggleShuffle => self.queue.toggle_shuffle(),
            QueueAction::AddBookmark | QueueAction::RemoveBookmark(_) => {
                let Some(s) = &self.session else { return };
                if let QueueAction::RemoveBookmark(i) = action {
                    self.bookmarks.remove(&s.path, i);
                } else {
                    self.bookmarks.add(&s.path, s.position().unwrap_or(0.0));
                }
                if let Err(e) = self.bookmarks.save() {
                    eprintln!("bookmarks: cannot save ({e})");
                }
            }
            QueueAction::SeekTo(t) => self.shared.seek_to(t),
        }
    }

    fn save(&mut self) {
        if self.library_changed {
            if let Err(e) = self.library.save() {
                eprintln!("library: cannot save ({e})");
            }
            self.library_changed = false;
        }
    }

    /// Settings from the Settings window. What changed takes effect now, except the per-file
    /// defaults (speed, aspect ratio, tracks, decoding, fullscreen), which the next file uses.
    fn apply_prefs(&mut self, win: Window, new: Prefs) {
        let old = prefs();
        if new.on_top != old.on_top {
            set_topmost(win, new.on_top);
        }
        UI.with_borrow_mut(|ui| {
            ui.auto_hide = new.auto_hide;
            ui.hide_after = Duration::from_secs_f32(new.hide_after);
            if new.show_hidden != old.show_hidden {
                ui.browse.stale = true;
            }
        });
        library::SHOW_HIDDEN.store(new.show_hidden, Relaxed);
        ass::STYLES.store(new.sub_styles, Relaxed);
        let restyle = new.sub_styles != old.sub_styles;
        if (new.repeat, new.shuffle) != (old.repeat, old.shuffle) {
            self.queue.set_repeat(REPEAT[new.repeat.min(2)]);
            self.queue.set_shuffle(new.shuffle);
        }
        if new.device != old.device {
            let name = (!new.device.is_empty()).then_some(new.device.as_str());
            self.audio.switch_to(name);
        }
        apply_effects(&self.audio, Some(&old), &new);
        self.caption = new.caption();
        ui::theme::set(theme_of(&new));
        PREFS.set(new);
        self.prefs_changed = Some(Instant::now());
        if restyle {
            self.reload_subtitles();
        }
    }

    /// Applies `change` to the settings, as the Settings window would.
    fn change_prefs(&mut self, win: Window, change: impl FnOnce(&mut Prefs)) {
        let mut p = prefs();
        change(&mut p);
        self.apply_prefs(win, p);
    }

    /// Opens the subtitles on screen again, so "Embedded Sub Format" applies to them.
    fn reload_subtitles(&mut self) {
        for (_, cues) in &mut self.external {
            *cues = None;
        }
        if let Some(i) = self.external_selected {
            self.show_external(Some(i));
        } else if self.embedded.is_some() {
            let selected = self.panel_data().sub_selected;
            self.select_track(TrackTab::Subtitles, selected);
        }
    }

    /// Plays backwards (VLC's "Reverse"), or forwards again. Backwards, playback stays paused
    /// and the frame loop steps the picture back with exact seeks, as fast as they decode.
    fn set_reverse(&mut self, on: bool) {
        if on == self.reverse.is_some() {
            return;
        }
        let paused = self.shared.paused.load(Relaxed);
        if on {
            let Some(at) = self.session.as_ref().and_then(Session::position) else {
                return;
            };
            if !paused {
                self.shared.toggle_pause();
            }
            self.reverse = Some((at, Instant::now()));
        } else {
            self.reverse = None;
            if paused {
                self.shared.toggle_pause();
            }
        }
    }

    /// One step of reverse playback, from the frame loop: once the last step's picture is up,
    /// seek back by the time since. Play or pause while reversing stops, paused.
    fn step_reverse(&mut self) {
        let Some((at, since)) = self.reverse else {
            return;
        };
        if self.session.is_none() {
            self.reverse = None;
            return;
        }
        if !self.shared.paused.load(Relaxed) {
            self.reverse = None;
            self.shared.toggle_pause();
            return;
        }
        let serial = self.shared.serial.load(Relaxed);
        let landed = self.current.as_ref().is_none_or(|c| c.serial == serial);
        if !landed || since.elapsed() < REVERSE_STEP {
            return;
        }
        let to = at - since.elapsed().as_secs_f64() * f64::from(self.audio.effects().speed());
        self.shared.seek_to(to.max(0.0));
        self.reverse = (to > 0.0).then(|| (to, Instant::now()));
    }

    /// The stereo "Test": a tone on the left, then the right, as the stereo mode plays them.
    // ponytail: the system plays it on the default device at its own volume; mix it into the
    // player's output if the chosen device or volume should count.
    fn test_stereo(&mut self) {
        platform::play_sound(dsp::test_tone(self.audio.effects().stereo()));
    }

    /// Opens the chosen subtitle file in the user's text editor.
    fn edit_subtitle(&self, _win: Window) {
        if let Some((path, _)) = self.external_selected.and_then(|i| self.external.get(i)) {
            platform::edit_text_file(path);
        }
    }

    /// Switches to a settings profile, adding it first when `create`, and applies its settings.
    fn switch_profile(&mut self, win: Window, name: &str, create: bool) -> std::io::Result<()> {
        let live = prefs();
        let new = if create {
            self.profiles.create(name, &live)?
        } else {
            self.profiles.switch(name, &live)?
        };
        self.apply_prefs(win, new);
        // Saved now, along with profile.txt, rather than a moment later.
        self.prefs_changed = None;
        prefs().save(&Library::default_dir())
    }

    /// Changes the panels, queue and menus make to things Settings also holds, so the
    /// equalizer, stereo mode, subtitle size, repeat and shuffle stick across launches.
    fn sync_prefs(&mut self) {
        let effects = self.audio.effects();
        let eq = effects.eq();
        let custom = eq.enabled && eq.preset.is_none();
        let preset = match eq.preset.and_then(|i| EQ_PRESETS.get(i)) {
            _ if !eq.enabled => String::new(),
            Some(e) => e.name.to_owned(),
            None => prefs::EQ_CUSTOM.to_owned(),
        };
        let stereo = (StereoMode::ALL.iter())
            .position(|m| *m == effects.stereo())
            .unwrap_or(0);
        let size = self.caption.size_ratio / CaptionStyle::default().size_ratio;
        let repeat = REPEAT.iter().position(|r| *r == self.queue.repeat());
        let shuffle = self.queue.shuffle();
        let changed = PREFS.with_borrow_mut(|p| {
            let old = p.clone();
            p.eq_preset = preset;
            if custom {
                p.eq_bands[0] = eq.preamp;
                p.eq_bands[1..].copy_from_slice(&eq.bands);
            }
            p.two_pass = eq.two_pass;
            p.stereo = stereo;
            p.sub_size = (size * 20.0).round() / 20.0;
            p.repeat = repeat.unwrap_or(0);
            p.shuffle = shuffle;
            *p != old
        });
        if changed {
            self.prefs_changed = Some(Instant::now());
        }
    }
}

/// Settings' "Repeat" choices, by `prefs::REPEAT`.
const REPEAT: [Repeat; 3] = [Repeat::Off, Repeat::All, Repeat::One];

/// The equalizer, its two-pass mode and the stereo mode from settings; with `old`, only what
/// changed, so a hand-tuned equalizer survives turning two-pass on.
fn apply_effects(audio: &audio::Device, old: Option<&Prefs>, new: &Prefs) {
    let effects = audio.effects();
    let custom = new.eq_preset == prefs::EQ_CUSTOM;
    if custom && old.is_none_or(|o| o.eq_preset != new.eq_preset || o.eq_bands != new.eq_bands) {
        let [preamp, bands @ ..] = new.eq_bands;
        effects.set_eq_bands(bands, preamp);
    } else if !custom && old.is_none_or(|o| o.eq_preset != new.eq_preset) {
        effects.set_eq(EQ_PRESETS.iter().position(|e| e.name == new.eq_preset));
    }
    if old.is_none_or(|o| o.two_pass != new.two_pass) {
        effects.set_two_pass(new.two_pass);
    }
    if old.is_none_or(|o| o.stereo != new.stereo) {
        effects.set_stereo(StereoMode::ALL[new.stereo.min(StereoMode::ALL.len() - 1)]);
    }
}

/// "Keep window above other windows". The mini player stays above them either way.
fn set_topmost(win: Window, on: bool) {
    win.set_floating(on || is_mini());
}

/// The colours `p` asks for; "System" follows the system's light or dark appearance.
fn theme_of(p: &Prefs) -> ui::theme::Theme {
    ui::theme::Theme {
        light: match p.theme {
            1 => true,
            2 => ui::theme::system_light(),
            _ => false,
        },
        accent: p.accent,
    }
}

/// The settings window opened at `category`, with the output devices as the tracks panel
/// lists them.
fn open_preferences(app: &App, category: Category) {
    UI.with_borrow_mut(|ui| ui.open_preferences(prefs(), app.devices.clone(), category));
}

fn section_of(path: &Path) -> Section {
    match library::media_kind(path) {
        Some(MediaKind::Audio) => Section::Music,
        _ => Section::Videos,
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    const USAGE: &str = "usage: wushang [--sw] [--play-and-exit] [file or stream address]";
    let mut software = false;
    let mut play_and_exit = false;
    let mut path = None;
    for arg in std::env::args_os().skip(1) {
        if arg == "--sw" {
            software = true;
        } else if arg == "--play-and-exit" {
            play_and_exit = true;
        } else if arg.to_string_lossy().starts_with("--") {
            return Err(format!("unknown option {arg:?}\n{USAGE}").into());
        } else {
            path = Some(PathBuf::from(arg));
        }
    }
    if play_and_exit && path.is_none() {
        return Err(USAGE.into());
    }
    // Off the window's thread.
    thread::spawn(assoc::register);

    ff::init()?;
    // Sets up TLS once, instead of on every stream opened.
    unsafe { ff::ffi::avformat_network_init() };
    ff::util::log::set_level(ff::util::log::Level::Error);
    let shared = Arc::new(Shared::new());
    _ = SHARED.set(shared.clone());
    // The audio device can take a moment to open, so start now, while the window comes up.
    let audio = audio::Device::open(shared.clone());
    // A file opened from Finder (Open With, a double-click, a drop on the Dock icon) arrives
    // as AppKit starts, as a command-line file does on Windows.
    let mut opened = platform::init();
    if path.is_none() && !opened.is_empty() {
        path = Some(opened[0].clone());
    } else {
        opened.clear();
    }
    // No settings yet means wushang hasn't run here before: the welcome shows.
    let first_run = !Library::default_dir().join("settings.txt").exists();
    let settings = Prefs::load(&Library::default_dir());
    if !settings.device.is_empty() {
        audio.switch_to(Some(&settings.device));
    }
    apply_effects(&audio, None, &settings);
    shared.set_volume(settings.startup_volume());
    library::SHOW_HIDDEN.store(settings.show_hidden, Relaxed);
    ass::STYLES.store(settings.sub_styles, Relaxed);
    ui::theme::set(theme_of(&settings));
    UI.with_borrow_mut(|ui| ui.toolbar = ui::Toolbar::load(&Library::default_dir()));
    UI.with_borrow_mut(|ui| {
        ui.auto_hide = settings.auto_hide;
        ui.hide_after = Duration::from_secs_f32(settings.hide_after);
    });
    PREFS.set(settings.clone());

    let device = render::create_device()?;
    let hw = if software {
        None
    } else {
        HwDevice::videotoolbox()
    };
    let mut app = App {
        shared: shared.clone(),
        hw,
        audio,
        library: Library::load(&Library::default_dir()),
        library_changed: false,
        downloading: None,
        session: None,
        current: None,
        queue: Queue::default(),
        durations: HashMap::new(),
        queue_prober: thumbs::Prober::new(),
        watched: HashSet::new(),
        bookmarks: Bookmarks::load(&Library::default_dir()),
        recorded: Instant::now(),
        adjust: VideoAdjust::default(),
        zoom: DEFAULT_ZOOM,
        aspect: 0,
        crop: 0,
        embedded: None,
        external: Vec::new(),
        external_selected: None,
        sub_delay: 0.0,
        sub_speed: 1.0,
        sub_duration: 0.0,
        reverse: None,
        caption: settings.caption(),
        devices: audio::output_devices(),
        sleep: 0,
        sleep_at: None,
        show_statistics: false,
        enter_fullscreen: false,
        prefs_changed: None,
        profiles: profiles::Profiles::load(&Library::default_dir()),
        notice: None,
        notices: mpsc::channel(),
        opening: None,
        fetcher: net::Fetcher::new(),
        shown_place: None,
        stream_titles: HashMap::new(),
        record_control: None,
        capture_devices: Arc::default(),
        converting: None,
        playing_playlist: None,
        next_playlist: None,
        stream_server: None,
        stream_control: None,
        scrub: None,
    };
    refresh_devices(&app.capture_devices);
    app.queue.set_repeat(REPEAT[settings.repeat.min(2)]);
    app.queue.set_shuffle(settings.shuffle);
    if let Some(path) = &path {
        let opened = if play_and_exit {
            // Measurement runs play exactly this file from the start.
            app.session = Some(Session::open(
                path,
                None,
                &TrackChoice::default(),
                &shared,
                app.hw.as_ref(),
                &app.audio,
            )?);
            true
        } else if opened.len() > 1 {
            // Several files opened together play in that order.
            app.play(path, opened)
        } else {
            app.play_file(path)
        };
        if !opened {
            return Err(format!("cannot play {path:?}").into());
        }
        UI.with_borrow_mut(|ui| ui.show(Page::Player));
    }

    let mut shown_title = window_title(app.session.as_ref());
    let size = app.session.as_ref().and_then(|s| s.video_size);
    let win = create_window(&shown_title, size.unwrap_or((1600, 900)));
    let maximized = restore_placement(win, &settings);
    if settings.on_top {
        set_topmost(win, true);
    }
    platform::set_handler(on_event);
    let lock = app.hw.as_ref().and_then(DeviceLock::from_hw);
    let mut renderer = Renderer::new(win.layer(), win.size_px(), device, lock)?;
    // Show the window the way the launcher asked: `open -j` starts wushang hidden, and then it
    // stays out of the way and isn't zoomed. The layer already has its size, so a window that
    // starts hidden still gets full-size drawables.
    let hidden = platform::launched_hidden();
    win.show(!hidden);
    if maximized && !hidden && !win.is_zoomed() {
        win.zoom();
    }
    // libdav1d keeps every core busy in bursts, and at normal priority this render loop then woke
    // up 40 ms late and skipped frames. VLC runs its video output thread above normal for the
    // same reason; here the main thread asks for the user-interactive class.
    platform::raise_thread_priority();

    // Started on a file, it plays; the welcome waits for a launch without one.
    if first_run && path.is_none() {
        let folder = videos_folder(&app.library);
        UI.with_borrow_mut(|ui| ui.open_onboarding(settings.clone(), folder));
    }

    let thumbnailer = thumbs::Thumbnailer::new(2, THUMB_SIZE.0, THUMB_SIZE.1);
    // The seek bar's preview: the time last asked for, and the file the still on hand is from.
    let previewer = thumbs::SeekPreviewer::new(SEEK_STILL_WIDTH);
    let mut preview_asked: Option<(PathBuf, f64)> = None;
    let mut still_for: Option<PathBuf> = None;
    // Tags for Songs, Albums, Artists and Genres come from probing every music file once; the
    // index on disk covers the files that haven't changed since.
    let prober = thumbs::Prober::new();
    let mut tracks: HashMap<PathBuf, (tags::Stamp, Track)> = HashMap::new();
    let mut music_scan = (!play_and_exit).then(|| scan_music(app.library.roots(Section::Music)));
    update::clean_up();
    let mut update_check = (!play_and_exit && prefs().check_updates).then(update::check);
    let mut update_offer: Option<update::Release> = None;
    let mut tracks_changed = false;
    let mut tags_unsaved = false;
    let mut tracks_set = Instant::now();
    let mut saved = Instant::now();

    let (mut shown, mut dropped) = (0u64, 0u64);
    let mut last_position = 0.0;
    // Whether the system is asked to keep the screen on.
    let mut awake = false;
    // Whether the pointer was hidden over the video, so it hides once each time the bars do.
    let mut pointer_hidden = false;
    // One pass of the render loop; false ends playback (--play-and-exit). See `run_frames`.
    let mut frame = || -> Result<bool, Box<dyn Error>> {
        // Nothing to show in the mini player once playback ends.
        if app.session.is_none() && is_mini() {
            MINI_REQUEST.set(Some(Some(Page::Library)));
        }
        match MINI_REQUEST.take() {
            Some(None) if app.session.is_some() => enter_mini(win),
            Some(Some(page)) => exit_mini(win, page),
            _ => {}
        }
        if is_mini() {
            fit_mini(win, UI.with_borrow(|ui| ui.queue.open));
        }
        if let Some(path) = OPEN.lock().unwrap().take() {
            // Dropped on an open playlist, a file joins it; anywhere else it plays.
            let playlist = UI.with_borrow(|ui| match &ui.browse.place {
                Place::Playlist(name) if ui.page == Page::Library => Some(name.clone()),
                _ => None,
            });
            if let Some(name) = playlist {
                match app.library.add_to_playlist(&name, &path) {
                    Ok(()) => UI.with_borrow_mut(|ui| ui.browse.stale = true),
                    Err(e) => eprintln!("library: cannot add to {name}: {e}"),
                }
            } else if app.play_file(&path) {
                UI.with_borrow_mut(|ui| ui.show(Page::Player));
            }
        }
        let opened = std::mem::take(&mut *OPEN_PLAY.lock().unwrap());
        if let Some(first) = opened.first().cloned() {
            let played = if opened.len() > 1 {
                app.play(&first, opened)
            } else {
                app.play_file(&first)
            };
            if played {
                if is_mini() {
                    MINI_REQUEST.set(Some(Some(Page::Player)));
                }
                UI.with_borrow_mut(|ui| ui.show(Page::Player));
            }
        }
        // What the last file picker was opened for.
        for (then, paths) in PICKED.with_borrow_mut(std::mem::take) {
            use_picked(&mut app, then, paths);
            app.sync_prefs();
        }
        for action in PANEL_ACTIONS.with_borrow_mut(std::mem::take) {
            app.apply(win, action);
            app.sync_prefs();
            // Video adjustments change the picture without a new frame.
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        for action in PREFS_ACTIONS.with_borrow_mut(std::mem::take) {
            match action {
                PrefsAction::Changed(p) => app.apply_prefs(win, *p),
                PrefsAction::ClearHistory => {
                    app.library.clear_history();
                    app.library_changed = true;
                    UI.with_borrow_mut(|ui| ui.browse.stale = true);
                }
                PrefsAction::ShowShortcuts => {
                    UI.with_borrow_mut(|ui| ui.open_dialog(Dialog::Shortcuts))
                }
            }
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        if app
            .prefs_changed
            .is_some_and(|t| t.elapsed() > Duration::from_secs(1))
        {
            app.prefs_changed = None;
            if let Err(e) = prefs().save(&Library::default_dir()) {
                eprintln!("settings: cannot save ({e})");
            }
        }
        if std::mem::take(&mut app.enter_fullscreen) && !is_fullscreen() {
            toggle_fullscreen(win);
        }
        for action in ONBOARDING_ACTIONS.with_borrow_mut(std::mem::take) {
            match action {
                ui::onboarding::OnboardingAction::PickFolder => {
                    pick_folder(Picked::OnboardingFolder)
                }
            }
        }
        for action in DIALOG_ACTIONS.with_borrow_mut(std::mem::take) {
            apply_dialog(&mut app, win, action);
        }
        if WANT_PROPERTIES.take() {
            show_properties(&app);
        }
        if WANT_SNAPSHOT.take() {
            take_snapshot(&app, &mut renderer);
        }
        fetch_network(&mut app);
        app.take_recordings();
        app.watch_conversion();
        if let Some(input) = app.opening.as_ref().and_then(|o| o.ready.try_recv().ok()) {
            let o = app.opening.take().unwrap();
            app.notice = None;
            app.start(&o.path, input.map(|input| (input, o.cancel)));
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        // While a dropped stream opens again, the notice says so; it fades once it plays.
        if shared.reconnecting.load(Relaxed) {
            let fresh = app.notice.as_ref().is_none_or(|(t, _)| t != RECONNECTING);
            app.notice = Some((RECONNECTING.to_owned(), Instant::now()));
            if fresh {
                UI.with_borrow_mut(|ui| ui.dirty = true);
            }
        }
        if let Some(text) = app.notices.1.try_iter().last() {
            app.notice = Some((text, Instant::now()));
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        // "Connecting..." stays until the stream answers.
        if app.opening.is_none()
            && app
                .notice
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() > NOTICE_FOR)
        {
            app.notice = None;
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        for action in QUEUE_ACTIONS.with_borrow_mut(std::mem::take) {
            app.apply_queue(action);
            app.sync_prefs();
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        for (path, info) in app.queue_prober.take_ready() {
            let duration = info.map_or(0.0, |i| i.duration);
            app.durations.insert(path, duration);
        }
        if app.sleep_at.is_some_and(|at| Instant::now() >= at) {
            app.sleep_at = None;
            app.sleep = 0;
            if !shared.paused.load(Relaxed) {
                shared.toggle_pause();
            }
        }
        for hit in PENDING.with_borrow_mut(std::mem::take) {
            // A click queued behind a navigation must see the new page's entries.
            UI.with_borrow_mut(|ui| {
                if ui.browse.stale {
                    ui.browse.refresh(&app.library);
                    ui.relayout = true;
                }
            });
            handle(&mut app, win, hit);
        }
        for (hit, x, y) in MENU_REQUESTS.with_borrow_mut(std::mem::take) {
            open_menu(&mut app, hit, (x, y));
        }
        for action in MENU_ACTIONS.with_borrow_mut(std::mem::take) {
            menu_action(&mut app, win, action);
            app.sync_prefs();
        }
        DURATION.store(
            app.session.as_ref().map_or(0.0, |s| s.duration).to_bits(),
            Relaxed,
        );

        // A still for the seek bar's preview at the pointer, for a video file on disk. It moves
        // in steps of a 400th of the file, at least half a second, so a sweep asks only so often.
        let hover = UI.with_borrow(Ui::seek_hover);
        if let (Some(fraction), Some(s)) = (hover, app.session.as_ref())
            && s.video_size.is_some()
            && s.duration > 0.0
            && !net::is_address(&s.path)
        {
            let at = fraction * s.duration;
            let step = (s.duration / 400.0).max(0.5);
            let ask = preview_asked
                .as_ref()
                .is_none_or(|(path, asked)| *path != s.path || (asked - at).abs() >= step);
            if ask {
                previewer.request(&s.path, at);
                preview_asked = Some((s.path.clone(), at));
            }
        }
        if let Some((path, still)) = previewer.take_ready()
            && app.session.as_ref().is_some_and(|s| s.path == path)
        {
            renderer.set_seek_still(still.as_ref().map(|t| (t.width, t.height, &t.bgra[..])));
            still_for = Some(path);
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        // The next file starts without the last one's still.
        if still_for.is_some()
            && still_for.as_deref() != app.session.as_ref().map(|s| s.path.as_path())
        {
            renderer.set_seek_still(None);
            still_for = None;
        }
        for (path, thumb) in thumbnailer.take_ready() {
            renderer.add_thumbnail(
                path,
                thumb.as_ref().map(|t| (t.width, t.height, &t.bgra[..])),
            );
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        if let Some(scan) = &music_scan
            && let Ok((known, changed)) = scan.try_recv()
        {
            music_scan = None;
            tracks.extend(known);
            // Files still to probe show by name until their tags come; the empty stamp has
            // them probed again next run if this one ends first.
            for path in &changed {
                let track = Track {
                    path: path.clone(),
                    ..Default::default()
                };
                tracks.insert(path.clone(), (tags::Stamp::default(), track));
            }
            prober.request_all(changed);
            app.library
                .set_tracks(tracks.values().map(|(_, t)| t.clone()).collect());
            tracks_set = Instant::now();
            UI.with_borrow_mut(|ui| {
                if ui.browse.place.section() == Section::Music || ui.browse.place == Place::Home {
                    ui.browse.stale = true;
                }
            });
        }
        // A newer release waits for a quiet moment: no dialog, no welcome, nothing playing.
        if let Some(check) = &update_check
            && let Ok(release) = check.try_recv()
        {
            update_check = None;
            update_offer = Some(release);
        }
        let playing = app.session.is_some() && !app.shared.paused.load(Relaxed);
        if !playing
            && UI.with_borrow(|ui| ui.dialog.is_none() && ui.onboarding.is_none())
            && let Some(release) = update_offer.take()
        {
            open_dialog(&app, Dialog::Update(release));
        }
        if let Some(downloading) = &app.downloading
            && let Ok(result) = downloading.try_recv()
        {
            app.downloading = None;
            match result {
                // Quit as usual, saving everything, and let msiexec update and start wushang.
                // With the dialog closed meanwhile, nothing happens until the next offer.
                Ok(update::Saved::Installer(msi)) => {
                    if UI.with_borrow(|ui| {
                        ui.dialog
                            .as_ref()
                            .is_some_and(|d| matches!(d.dialog, Dialog::Update(_)))
                    }) {
                        update::install_on_exit(msi);
                        destroy(win);
                    }
                }
                // The download is selected in Finder already, to drag into Applications.
                Ok(update::Saved::Zip(zip)) => open_dialog(&app, Dialog::Downloaded(zip)),
                Err(e) => UI.with_borrow_mut(|ui| ui.dialog_error(e)),
            }
        }
        for (path, info) in prober.take_ready() {
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
            tracks.insert(path, (stamp, track));
            (tracks_changed, tags_unsaved) = (true, true);
        }
        if tracks_changed && tracks_set.elapsed() > Duration::from_secs(1) {
            app.library
                .set_tracks(tracks.values().map(|(_, t)| t.clone()).collect());
            (tracks_changed, tracks_set) = (false, Instant::now());
            UI.with_borrow_mut(|ui| {
                if ui.browse.place.section() == Section::Music || ui.browse.place == Place::Home {
                    ui.browse.stale = true;
                }
            });
        }
        if app.session.is_some() && app.recorded.elapsed() > RECORD_EVERY {
            app.record();
        }
        if saved.elapsed() > SAVE_EVERY {
            app.save();
            if std::mem::take(&mut tags_unsaved) {
                save_tags(&tracks);
            }
            saved = Instant::now();
        }

        let resized = RESIZED.swap(false, Relaxed);
        if resized {
            let (w, h) = win.size_px();
            renderer.resize(w, h)?;
        }
        let hdr = PREFS.with_borrow(|p| p.hdr);
        renderer.update_display(&win.ns_window(), hdr, DISPLAY_CHANGED.swap(false, Relaxed));

        app.step_reverse();
        app.scrub(UI.with_borrow(Ui::scrubbing));
        let serial = shared.serial.load(Relaxed);
        let fresh = app.current.as_ref().is_none_or(|c| c.serial != serial);
        let paused = shared.paused.load(Relaxed);
        // "Stop the screensaver during playback": only while a video plays.
        let want_awake = PREFS.with_borrow(|p| p.keep_awake)
            && !paused
            && app.session.as_ref().is_some_and(|s| s.video_size.is_some());
        if want_awake != awake {
            awake = want_awake;
            platform::keep_awake(awake);
        }
        // Without audio (or once its device failed), video starts the system clock from the frame on
        // screen, after opening and after every seek.
        if !shared.audio_master.load(Relaxed)
            && shared.clock.get().is_none()
            && let Some(c) = app.current.as_ref().filter(|c| c.serial == serial)
        {
            shared.clock.set(c.pts);
        }
        let now = shared.clock.get();
        let next = app.session.as_ref().and_then(|s| {
            s.frames.with(|q| {
                q.retain(|f| f.serial == serial);
                let front = q.front()?;
                // First frame after opening or seeking: show it at once instead of waiting for the clock.
                if fresh {
                    return q.pop_front();
                }
                let now = now?;
                if paused || front.pts > now {
                    return None;
                }
                // Behind schedule: skip to the newest frame that is already due, like VLC drops late pictures.
                let mut frame = q.pop_front();
                while q.front().is_some_and(|f| f.pts <= now) {
                    dropped += 1;
                    frame = q.pop_front();
                }
                frame
            })
        });

        // Lay out for the current size, page and bar visibility; a change means a repaint. The
        // layout's DIPs are AppKit's points.
        let dpi = (96.0 * win.scale()) as f32;
        let (w, h) = win.size();
        let (w, h) = (w as f32, h as f32);
        let fullscreen = win.is_fullscreen();
        let has_video = app.session.as_ref().is_some_and(|s| s.video_size.is_some());
        let card = app.session.is_some();
        let panel_data = app.panel_data();
        let queue_data = UI.with_borrow(|ui| ui.queue.open).then(|| app.queue_data());
        // The Window menu, Mission Control and the Dock name the file playing.
        let title = window_title(app.session.as_ref());
        if title != shown_title {
            win.set_title(&title);
            shown_title = title;
        }
        let needs_paint = UI.with_borrow_mut(|ui| {
            // Nothing to show on the player page once playback ends or fails, unless its bars
            // are being customized. A stream connecting, or the notice saying why it couldn't,
            // keeps it up.
            let waiting = app.opening.is_some() || app.notice.is_some();
            if app.session.is_none() && !waiting && ui.customize.is_none() {
                ui.show(Page::Library);
            }
            ui.menu_tick();
            // An open context menu would close with the refresh; it waits for that.
            if ui.browse.context.is_none() && app.library.media_folders_changed() {
                ui.browse.stale = true;
            }
            if ui.browse.stale {
                ui.browse.refresh(&app.library);
                ui.relayout = true;
            }
            let old = &ui.layout;
            let layout = match ui.page {
                Page::Player => {
                    let bars = !has_video || ui.bars_visible(paused, fullscreen);
                    let changed = ui.relayout
                        || old.page != Page::Player
                        || bars != old.bars
                        || ui.overlay != old.has_overlay()
                        || ui.panel_stale(&panel_data)
                        || ui.queue_stale(queue_data.as_ref())
                        || (w, h) != (old.width, old.height);
                    changed.then(|| {
                        let toolbar = ui.customize.as_ref().map_or(&ui.toolbar, |c| &c.draft);
                        let mut layout = Layout::player(w, h, bars, fullscreen, toolbar);
                        if let Some(c) = &ui.customize {
                            layout.add_customize(c);
                        }
                        if ui.overlay {
                            renderer.add_overlay(&mut layout, &ui.browse);
                        }
                        layout.add_panel(&ui.panels, &panel_data);
                        if let Some(q) = &queue_data {
                            layout.add_queue(&ui.queue, q);
                        }
                        layout
                    })
                }
                Page::Library => {
                    let changed = ui.relayout
                        || old.page != Page::Library
                        || (w, h) != (old.width, old.height)
                        || card != (old.video.w() > 0.0)
                        || (card && ui.queue_stale(queue_data.as_ref()));
                    changed.then(|| {
                        let queue = card.then_some(ui.queue.open);
                        let mut layout = renderer.library_layout(w, h, &ui.browse, queue);
                        if let Some(q) = &queue_data {
                            layout.add_mini_queue(&ui.queue, q);
                        }
                        layout
                    })
                }
                Page::Mini => {
                    let changed = ui.relayout
                        || old.page != Page::Mini
                        || (w, h) != (old.width, old.height)
                        || ui.queue_stale(queue_data.as_ref());
                    changed.then(|| {
                        let card = MINI_CARD.get().1;
                        let mut layout = Layout::mini(w, h, card, ui.queue.open);
                        if let Some(q) = &queue_data {
                            layout.add_mini_queue(&ui.queue, q);
                        }
                        layout
                    })
                }
            };
            if let Some(mut layout) = layout {
                if let Some(p) = &ui.preferences {
                    layout.add_preferences(p);
                }
                if let Some(o) = &ui.onboarding {
                    layout.add_onboarding(o);
                }
                if let Some(d) = &ui.dialog {
                    renderer.add_dialog(&mut layout, d);
                }
                ui.layout = layout;
                ui.relayout = false;
                ui.dirty = true;
                ui.sync_menu();
            }
            if ui.settle_seek_preview() || ui.dialog_tick() || ui.preferences_tick() {
                ui.dirty = true;
            }
            // The menu bar's commands wait while something in the window takes the keys, as
            // an open dialog took every key on Windows.
            platform::set_menu_enabled(
                ui.dialog.is_none()
                    && ui.preferences.is_none()
                    && ui.onboarding.is_none()
                    && ui.customize.is_none()
                    && !ui.menu.is_open(),
            );
            std::mem::take(&mut ui.dirty)
        });

        app.adjust.zoom = match app.zoom {
            1 => Zoom::Fill,
            2 => Zoom::Scale(2.0),
            _ => Zoom::Original,
        };

        if let Some(f) = &next {
            last_position = f.pts;
        } else if let Some(t) = now {
            last_position = t;
        }
        // The mini player's progress bar moves while the library shows.
        let tick = card && now.is_some() && !paused && last_position.fract() < 0.02;
        // The audio visualizer draws in place of the cover, every pass while sound plays.
        let visualizer = PREFS.with_borrow(|p| p.visualizer);
        let visual_on = visualizer > 0
            && UI.with_borrow(|ui| ui.page == Page::Player)
            && app.session.as_ref().is_some_and(|s| s.video_size.is_none());
        if next.is_some() || needs_paint || resized || tick || (visual_on && !paused) {
            let new_frame = next.is_some();
            if let Some(f) = next {
                app.current = Some(f);
            }
            let session = app.session.as_ref();
            let cues = app.cues_at(session.and_then(Session::position).unwrap_or(last_position));
            let marks = if PREFS.with_borrow(|p| p.sub_timeline) {
                app.subtitle_spans()
            } else {
                Vec::new()
            };
            let visual = visual_on.then(|| {
                let (rate, samples) = shared.scope.lock().unwrap().clone();
                match visualizer {
                    1 => dsp::spectrum(&samples, rate, VISUAL_BANDS),
                    _ => samples,
                }
            });
            let stats = app.statistics(shown, dropped);
            let file = session.map_or(String::new(), |s| {
                s.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
            UI.with_borrow(|ui| {
                let duration = session.map_or(0.0, |s| s.duration);
                let scene = Scene {
                    layout: &ui.layout,
                    hover: ui.hover,
                    position: ui.seek_preview().map_or(last_position, |f| f * duration),
                    duration,
                    live: duration <= 0.0 && session.is_some_and(|s| net::is_address(&s.path)),
                    time_left: PREFS.with_borrow(|p| p.time_left),
                    // Reverse playback pauses underneath, but it is playing.
                    paused: paused && app.reverse.is_none(),
                    volume: shared.volume(),
                    muted: shared.muted.load(Relaxed),
                    title: session.map_or("", |s| &s.title),
                    folder: session.map_or("", |s| &s.folder),
                    file: &file,
                    art: session
                        .filter(|s| s.video_size.is_none())
                        .map(|s| s.path.as_path()),
                    media: session
                        .filter(|s| !net::is_address(&s.path))
                        .map(|s| s.path.as_path()),
                    artist: session
                        .and_then(|s| app.library.track(&s.path))
                        .and_then(|t| t.artist.as_deref())
                        .unwrap_or_default(),
                    playlist: app.playing_playlist.as_deref().unwrap_or_default(),
                    fullscreen,
                    has_frame: app.current.is_some(),
                    browse: &ui.browse,
                    panel: Some(&panel_data),
                    queue: queue_data.as_ref(),
                    dialog: ui.dialog.as_ref(),
                    preferences: ui.preferences.as_ref(),
                    onboarding: ui.onboarding.as_ref(),
                    customize: ui.customize.as_ref(),
                    repeat: app.queue.repeat(),
                    shuffle: app.queue.shuffle(),
                    favorite: session.is_some_and(|s| app.library.is_favorite(&s.path)),
                    recording: session.is_some_and(Session::recording),
                    cues: &cues,
                    sub_marks: &marks,
                    visual: visual.as_deref().map(|v| (visualizer, v)),
                    caption_style: &app.caption,
                    stats: stats.as_deref(),
                    notice: app.notice.as_ref().map(|(text, _)| text.as_str()),
                    seek_hover: ui.seek_hover().map(|f| f * duration),
                };
                renderer.present(
                    app.current.as_ref().map(|c| &c.frame),
                    &scene,
                    dpi,
                    &app.adjust,
                )
            })?;
            for path in renderer.take_wanted() {
                thumbnailer.request(&path);
            }
            // Count video frames, not repaints the interface asked for.
            if new_frame {
                if shown == 0 {
                    eprintln!(
                        "first frame on screen {} ms after launch",
                        started.elapsed().as_millis()
                    );
                }
                shown += 1;
            }
        } else {
            // Nothing to show yet: wait a moment, or until the user does something.
            platform::wait_for_event(Duration::from_millis(2));
        }
        // Hide the pointer over the video while the bars are hidden, like VLC.
        let hide = UI.with_borrow(|ui| {
            ui.page == Page::Player && !ui.layout.bars && ui.hover == Some(Hit::Video)
        });
        if hide && !pointer_hidden {
            platform::hide_cursor_until_moved();
        }
        pointer_hidden = hide;
        // Checked on painting passes too: the visualizer paints every one.
        if !paused && app.session.as_ref().is_some_and(Session::finished) {
            if play_and_exit {
                return Ok(false);
            }
            // Like VLC's playlist: carry on with the next file, or stop at the end (or where
            // the sleep timer says).
            if app.sleep == SLEEP_END_OF_FILE || !app.advance() {
                // The sleep timer stops; the end of the queue may quit, per Settings.
                let quit = app.sleep != SLEEP_END_OF_FILE && PREFS.with_borrow(|p| p.at_end == 1);
                app.sleep = 0;
                app.stop();
                if quit {
                    destroy(win);
                }
            }
            last_position = 0.0;
            UI.with_borrow_mut(|ui| {
                ui.browse.stale = true;
                ui.dirty = true;
            });
        }
        Ok(true)
    };
    // Saved below even when rendering failed for good.
    let rendered = run_frames(&mut frame, &shared.quit);

    shared.quit.store(true, Relaxed);
    if !play_and_exit {
        app.stop();
        app.save();
        if tags_unsaved {
            save_tags(&tracks);
        }
        PREFS.with_borrow_mut(|p| p.volume = shared.volume());
        if let Err(e) = prefs().save(&Library::default_dir()) {
            eprintln!("settings: cannot save ({e})");
        }
    }
    platform::keep_awake(false);
    drop(app);
    eprintln!("shown {shown} frames, dropped {dropped}");
    rendered
}

/// The shortest gap between reverse playback's steps back.
const REVERSE_STEP: Duration = Duration::from_millis(33);
/// The spectrum visualizer's bars.
const VISUAL_BANDS: usize = 32;

/// A pass of the render loop.
type Frame<'a> = dyn FnMut() -> Result<bool, Box<dyn Error>> + 'a;

thread_local! {
    /// The pass `run_frames` is running, for the window procedure.
    static FRAME: std::cell::Cell<Option<*mut Frame<'static>>> = const { std::cell::Cell::new(None) };
    /// A pass is under way, so another can't start inside it.
    static IN_FRAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Passes in a row that may fail, about two seconds' worth, before wushang gives up. One failed
/// pass only costs a frame.
const MAX_FAILED_PASSES: u32 = 120;

/// Runs `frame` until the window closes, `quit` is set or the pass returns false.
///
/// Resizing the window, an open menu or a file picker makes AppKit run an event loop of its own
/// until it is done, so this loop stops and the video would freeze. While that loop runs, a timer
/// in its run loop modes runs passes instead (`modal_frame`).
fn run_frames(frame: &mut Frame<'_>, quit: &AtomicBool) -> Result<(), Box<dyn Error>> {
    // The lifetime is erased for the thread-local; the pointer is gone before `frame` is, and
    // every pass goes through it, one at a time.
    let ptr: *mut Frame<'static> = unsafe { std::mem::transmute(frame as *mut Frame<'_>) };
    FRAME.set(Some(ptr));
    platform::set_modal_tick(modal_frame);
    let mut failed = 0;
    let result = loop {
        if !platform::pump_messages() || quit.load(Relaxed) {
            break Ok(());
        }
        let pass = call_frame(ptr);
        // Between passes, so modal_frame can run them while the picker is open.
        run_picker();
        match pass {
            Ok(true) => failed = 0,
            Ok(false) => break Ok(()),
            Err(e) => {
                failed += 1;
                if failed == 1 {
                    eprintln!("render: {e}");
                }
                if failed == MAX_FAILED_PASSES {
                    break Err(e);
                }
            }
        }
    };
    FRAME.set(None);
    result
}

fn call_frame(ptr: *mut Frame<'static>) -> Result<bool, Box<dyn Error>> {
    if IN_FRAME.replace(true) {
        return Ok(true);
    }
    // A pass's drawable and other autoreleased objects go when it ends, not when the app does.
    let result = platform::autoreleasepool(|_| unsafe { (*ptr)() });
    IN_FRAME.set(false);
    result
}

/// A pass from the timer in AppKit's own loops.
fn modal_frame() {
    if let Some(ptr) = FRAME.get()
        && let Err(e) = call_frame(ptr)
    {
        eprintln!("render: {e}");
    }
}

/// Walks the Music folders `roots` on a thread, since a big library or a slow drive would hold
/// up the window. Sends the files whose saved tags still match (by size and time) with those
/// tags, and the files to probe.
fn scan_music(roots: Vec<PathBuf>) -> mpsc::Receiver<MusicScan> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut saved = tags::load(&Library::default_dir());
        let (mut known, mut changed) = (HashMap::new(), Vec::new());
        for path in library::music_files(&roots) {
            match saved.remove(&path) {
                Some((stamp, track)) if tags::stamp(&path) == Some(stamp) => {
                    known.insert(path, (stamp, track));
                }
                _ => changed.push(path),
            }
        }
        _ = tx.send((known, changed));
    });
    rx
}

type MusicScan = (HashMap<PathBuf, (tags::Stamp, Track)>, Vec<PathBuf>);

fn save_tags(tracks: &HashMap<PathBuf, (tags::Stamp, Track)>) {
    if let Err(e) = tags::save(&Library::default_dir(), tracks) {
        eprintln!("tags: cannot save ({e})");
    }
}

/// A click that needs the library or opens files.
fn handle(app: &mut App, win: Window, hit: Hit) {
    let go = |place: Place| UI.with_borrow_mut(|ui| ui.browse.go(place));
    match hit {
        Hit::Nav(section) => go(section.place()),
        Hit::AddItem => {
            let section = UI.with_borrow(|ui| ui.browse.place.section());
            add_folder(section);
        }
        Hit::AddHere => match UI.with_borrow(|ui| ui.browse.place.clone()) {
            Place::Playlists => create_playlist(app),
            Place::Playlist(name) => pick_media_files(Picked::PlaylistFiles(name)),
            place => add_folder(place.section()),
        },
        Hit::Back => UI.with_borrow_mut(|ui| ui.browse.back()),
        Hit::Forward => UI.with_borrow_mut(|ui| ui.browse.forward()),
        Hit::Crumb(i) => {
            if let Some(c) = UI.with_borrow(|ui| ui.browse.crumbs.get(i).cloned()) {
                go(c.place);
            }
        }
        Hit::Entry(i) => {
            let Some((entry, place)) = UI.with_borrow(|ui| {
                Some((ui.browse.entries.get(i)?.clone(), ui.browse.place.clone()))
            }) else {
                return;
            };
            match entry {
                Entry::Header { more: Some(p), .. } => go(p),
                Entry::Header { more: None, .. } => {}
                Entry::Folder { place, .. } => {
                    // Visits are history too, kept only when Settings says to.
                    if let Place::Folder(_, dir) = &place
                        && PREFS.with_borrow(|p| p.save_recent)
                    {
                        app.library.visit_folder(dir);
                        app.library_changed = true;
                    }
                    go(place);
                }
                Entry::Media { path, .. } => {
                    if let Place::Playlist(name) = &place {
                        app.mark_playlist(name);
                    }
                    let queue = app.library.media_in(&place);
                    // Streams have no tags to name them by; the page's names do, for the whole
                    // queue.
                    if net::is_url_path(&path) {
                        for entry in app.library.entries(&place) {
                            if let Entry::Media { path, label, .. } = entry {
                                app.stream_titles.insert(path, label);
                            }
                        }
                    }
                    if app.play(&path, queue) {
                        UI.with_borrow_mut(|ui| ui.show(Page::Player));
                    }
                }
                Entry::Action { action, .. } => match action {
                    library::Action::AddFolder(section) => add_folder(section),
                    library::Action::CreatePlaylist => create_playlist(app),
                    library::Action::AddNetworkStream => {
                        open_dialog(app, Dialog::Address(NetKind::Stream));
                    }
                    library::Action::AddPodcast => {
                        open_dialog(app, Dialog::Address(NetKind::Podcast))
                    }
                },
            }
        }
        Hit::Card => UI.with_borrow_mut(|ui| ui.show(Page::Player)),
        Hit::Tool(item) => use_tool(app, win, item),
        Hit::MiniExpand if is_mini() => MINI_REQUEST.set(Some(Some(Page::Player))),
        Hit::MiniExpand => UI.with_borrow_mut(|ui| ui.show(Page::Player)),
        Hit::MiniClose => {
            app.stop();
            if is_mini() {
                MINI_REQUEST.set(Some(Some(Page::Library)));
            }
        }
        Hit::Folder => {
            let file = app.session.as_ref().filter(|s| !net::is_address(&s.path));
            if let Some(dir) = file.and_then(|s| s.path.parent()) {
                let place = Place::Folder(
                    section_of(&app.session.as_ref().unwrap().path),
                    dir.to_owned(),
                );
                UI.with_borrow_mut(|ui| {
                    ui.browse.go(place);
                    ui.show(Page::Library);
                });
            }
        }
        Hit::Next => _ = app.step(1),
        Hit::Previous => {
            // Like VLC: back to the start, or to the previous file when already near it.
            let near_start = app
                .session
                .as_ref()
                .and_then(Session::position)
                .is_none_or(|t| t < 3.0);
            if !(near_start && app.step(-1)) {
                app.shared.seek_to(0.0);
            }
        }
        _ => {}
    }
    UI.with_borrow_mut(|ui| ui.dirty = true);
}

/// Builds and opens the menu for a click on `hit`.
fn open_menu(app: &mut App, hit: Hit, at: (f32, f32)) {
    use ui::menu::{self, Kind};
    let (kind, items) = match hit {
        Hit::More => {
            let recent: Vec<PathBuf> = app
                .library
                .entries(&Place::Recent)
                .into_iter()
                .filter_map(|e| match e {
                    Entry::Media { path, .. } => Some(path),
                    _ => None,
                })
                .collect();
            let p = &app.profiles;
            let devices = app.capture_devices.lock().unwrap().clone();
            // Look again for next time, in case a camera was plugged in.
            refresh_devices(&app.capture_devices);
            let converting = match &app.session {
                _ if app.converting.is_some() => menu::Converting::Running,
                Some(s) if !net::is_address(&s.path) => menu::Converting::File(&s.path),
                _ => menu::Converting::Nothing,
            };
            let items = menu::main_menu(&recent, p.names(), p.current(), &devices, converting);
            (Kind::Main, items)
        }
        Hit::Sort => UI.with_borrow(|ui| {
            let b = &ui.browse;
            (
                Kind::Sort,
                menu::sort_menu(b.sort, b.has_artists(), b.has_albums()),
            )
        }),
        Hit::Video => {
            let paused = app.shared.paused.load(Relaxed);
            (Kind::Player, menu::player_menu(&app.panel_data(), paused))
        }
        Hit::Queue(ui::queue::Target::More(i)) => {
            let Some(path) = app.queue.items().get(i).cloned() else {
                return;
            };
            let playlists = app.library.playlists();
            let items = menu::queue_menu(i, &path, app.queue.current(), &playlists);
            (Kind::File, items)
        }
        Hit::Entry(i) => {
            let Some(entry) = UI.with_borrow(|ui| ui.browse.entries.get(i).cloned()) else {
                return;
            };
            let items = match &entry {
                Entry::Folder {
                    place: Place::Playlist(name),
                    ..
                } => app.library.playlist_items(name),
                _ => Vec::new(),
            };
            let place = UI.with_borrow(|ui| ui.browse.place.clone());
            let scope = match &entry {
                Entry::Folder {
                    place: Place::Folder(_, dir),
                    ..
                } => Some(library::folder_scope(dir)),
                Entry::Folder {
                    place: Place::Playlist(name),
                    ..
                } => Some(library::playlist_scope(name)),
                _ => None,
            };
            let tracks = scope.map(|s| {
                let prefs = app.library.track_prefs(&s);
                (s, prefs)
            });
            let playlists = app.library.playlists();
            match menu::entry_menu(&entry, &playlists, &items, &place, tracks) {
                Some(menu) => menu,
                None => return,
            }
        }
        _ => return,
    };
    UI.with_borrow_mut(|ui| ui.open_menu(kind, items, at, hit));
}

/// Carries out a menu choice.
fn menu_action(app: &mut App, win: Window, action: MenuAction) {
    let show_player = || UI.with_borrow_mut(|ui| ui.show(Page::Player));
    match action {
        MenuAction::OpenFiles => pick_media_files(Picked::OpenFiles),
        MenuAction::OpenFolder => pick_folder(Picked::OpenFolder),
        MenuAction::PlayRecent(path) => {
            if app.play_file(&path) {
                show_player();
            }
        }
        MenuAction::Play(path) => {
            // The page it was on is the queue, as when the entry is clicked.
            let place = UI.with_borrow(|ui| ui.browse.place.clone());
            let mut queue = app.library.media_in(&place);
            if !queue.contains(&path) {
                queue = vec![path.clone()];
            }
            if let Place::Playlist(name) = &place {
                app.mark_playlist(name);
            }
            if app.play(&path, queue) {
                show_player();
            }
        }
        MenuAction::Settings(tab) => {
            if app.session.is_some() {
                UI.with_borrow_mut(|ui| {
                    ui.show(Page::Player);
                    ui.panels
                        .show(Some(Panel::Settings(tab)), Some(Hit::Settings));
                    ui.relayout = true;
                });
            }
        }
        MenuAction::Preferences(category) => open_preferences(app, category),
        MenuAction::Quit => destroy(win),
        MenuAction::TogglePause => app.shared.toggle_pause(),
        MenuAction::Stop => app.stop(),
        MenuAction::Previous => handle(app, win, Hit::Previous),
        MenuAction::Next => handle(app, win, Hit::Next),
        MenuAction::HideUi => UI.with_borrow_mut(Ui::hide_bars),
        MenuAction::MiniPlayer if is_mini() => MINI_REQUEST.set(Some(Some(Page::Player))),
        MenuAction::MiniPlayer => MINI_REQUEST.set(Some(None)),
        MenuAction::CustomizePlayer => {
            if is_mini() {
                MINI_REQUEST.set(Some(Some(Page::Player)));
            }
            UI.with_borrow_mut(|ui| {
                ui.show(Page::Player);
                ui.open_customize();
            });
        }
        MenuAction::Profile(name) => {
            if let Err(e) = app.switch_profile(win, &name, false) {
                eprintln!("profiles: cannot switch to {name} ({e})");
            }
        }
        MenuAction::NewProfile => open_dialog(app, Dialog::NewProfile),
        MenuAction::OpenNetwork => open_dialog(app, Dialog::Address(NetKind::Stream)),
        MenuAction::Snapshot => WANT_SNAPSHOT.set(true),
        MenuAction::Record => app.toggle_record(),
        MenuAction::SetTracks(scope, audio, value) => {
            app.library.set_track_pref(&scope, audio, value);
            app.library_changed = true;
        }
        MenuAction::Stream => app.toggle_stream(),
        MenuAction::StreamFile(path) => {
            if app.session.as_ref().is_none_or(|s| s.path != path) {
                let place = UI.with_borrow(|ui| ui.browse.place.clone());
                let mut queue = app.library.media_in(&place);
                if !queue.contains(&path) {
                    queue = vec![path.clone()];
                }
                if !app.play(&path, queue) {
                    return;
                }
                show_player();
            }
            if !app.session.as_ref().is_some_and(Session::streaming) {
                app.toggle_stream();
            }
        }
        MenuAction::Convert(path, target) => app.convert(&path, target),
        MenuAction::ConvertOpen => {
            if let Some(path) = app.session.as_ref().map(|s| s.path.clone())
                && !net::is_address(&path)
            {
                app.convert(&path, convert::Target::Mp4);
            }
        }
        MenuAction::CancelConvert => {
            if let Some(job) = &app.converting {
                job.cancel();
            }
        }
        MenuAction::OpenDisc(Some(folder)) => open_disc(app, &folder),
        MenuAction::OpenDisc(None) => pick_folder(Picked::Disc),
        MenuAction::OpenDiscImage => {
            let images = vec![("Disc images", "*.iso".to_owned())];
            pick(Picker::Files(images, false), Picked::Disc);
        }
        MenuAction::OpenCapture(address, name) => {
            let path = PathBuf::from(address);
            app.stream_titles.insert(path.clone(), name);
            if app.play(&path, vec![path.clone()]) {
                show_player();
            }
        }
        MenuAction::Panel(a) => app.apply(win, a),
        MenuAction::Queue(a) => {
            app.apply_queue(a);
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        // Files join the queue, or play when nothing is open.
        MenuAction::AddToQueue(paths) => {
            if app.session.is_some() {
                app.add_to_queue(paths);
            } else if let Some(first) = paths.first().cloned()
                && app.play(&first, paths)
            {
                show_player();
            }
        }
        MenuAction::AddToPlaylist(name, paths) => {
            for path in paths {
                if let Err(e) = app.library.add_to_playlist(&name, &path) {
                    eprintln!("library: cannot add to {name}: {e}");
                }
            }
            UI.with_borrow_mut(|ui| ui.browse.stale = true);
        }
        MenuAction::ShowInExplorer(path) => platform::reveal_in_finder(&path),
        MenuAction::GoToSource(path) => {
            if let Some(dir) = path.parent() {
                let place = Place::Folder(section_of(&path), dir.to_owned());
                UI.with_borrow_mut(|ui| ui.browse.go(place));
            }
        }
        MenuAction::AddSubtitleTo(path) => pick_subtitle_file(Picked::SubtitleFor(path)),
        MenuAction::AddAudioTo(path) => {
            if app.session.as_ref().is_none_or(|s| s.path != path) {
                let place = UI.with_borrow(|ui| ui.browse.place.clone());
                let mut queue = app.library.media_in(&place);
                if !queue.contains(&path) {
                    queue = vec![path.clone()];
                }
                if !app.play(&path, queue) {
                    return;
                }
                show_player();
            }
            pick_audio_file();
        }
        MenuAction::ForgetAddress(kind, url) => {
            app.library.remove_address(kind, &url);
            app.library_changed = true;
            UI.with_borrow_mut(|ui| {
                if ui.browse.place == Place::Podcast(url) {
                    ui.browse.go(Place::Podcasts);
                }
                ui.browse.stale = true;
            });
        }
        MenuAction::AddFolder(section) => add_folder(section),
        MenuAction::Resync => {
            app.library.forget_media_folders();
            UI.with_borrow_mut(|ui| ui.browse.stale = true);
        }
        MenuAction::PlayPlaylist(name) => play_playlist(app, &name, false),
        MenuAction::ShufflePlaylist(name) => play_playlist(app, &name, true),
        MenuAction::ExportPlaylist(name) => {
            let file = format!("{name}.m3u");
            pick(Picker::Save(file), Picked::ExportPlaylist(name));
        }
        MenuAction::Sort(mode) => UI.with_borrow_mut(|ui| {
            ui.browse.set_sort(mode);
            ui.browse.scroll = 0.0;
            ui.relayout = true;
        }),
        MenuAction::ShowQueue => UI.with_borrow_mut(|ui| {
            if !ui.queue.open {
                ui.toggle_queue();
            }
        }),
        MenuAction::ToggleRepeat => app.queue.cycle_repeat(),
        MenuAction::ToggleShuffle => app.queue.toggle_shuffle(),
        MenuAction::ShowShortcuts => open_dialog(app, Dialog::Shortcuts),
        MenuAction::ShowAbout => open_dialog(app, Dialog::About),
        MenuAction::NewPlaylist(files) => open_dialog(app, Dialog::NewPlaylist { files }),
        MenuAction::Properties(path) => open_dialog(app, Dialog::Properties(path)),
        MenuAction::RenamePlaylist(name) => open_dialog(app, Dialog::RenamePlaylist(name)),
        // Both toggle: a watched file becomes unwatched, a favourite stops being one.
        MenuAction::MarkWatched(path) => {
            let watched =
                app.library.entries(&Place::Recent).iter().any(
                    |e| matches!(e, Entry::Media { path: p, finished: true, .. } if *p == path),
                );
            app.library.mark_watched(&path, !watched);
            app.library_changed = true;
            UI.with_borrow_mut(|ui| ui.browse.stale = true);
        }
        MenuAction::Favorite(path) => toggle_favorite(app, &path),
        // Deleting asks first; the dialog's Delete comes back as DialogAction::DeletePlaylist.
        MenuAction::DeletePlaylist(name) => open_dialog(app, Dialog::DeletePlaylist(name)),
        MenuAction::RemoveFromPlaylist(name, path) => {
            let index = app
                .library
                .playlist_items(&name)
                .iter()
                .position(|p| *p == path);
            if let Some(i) = index
                && let Err(e) = app.library.remove_from_playlist(&name, i)
            {
                eprintln!("library: cannot remove from {name}: {e}");
            }
            UI.with_borrow_mut(|ui| ui.browse.stale = true);
        }
        MenuAction::FavoritePlaylist(name) => {
            if let Some(path) = app.library.playlist_path(&name) {
                toggle_favorite(app, &path);
            }
        }
    }
    UI.with_borrow_mut(|ui| ui.dirty = true);
}

fn toggle_favorite(app: &mut App, path: &Path) {
    let favorite = app.library.is_favorite(path);
    app.library.set_favorite(path, !favorite);
    app.library_changed = true;
    UI.with_borrow_mut(|ui| ui.browse.stale = true);
}

/// Picks an audio file to play with the open file, as a track of its own.
fn pick_audio_file() {
    let filters = vec![
        ("Audio files", file_spec(library::AUDIO_EXTS)),
        ("Video files", file_spec(library::VIDEO_EXTS)),
        ("All files", "*.*".to_owned()),
    ];
    pick(Picker::Files(filters, false), Picked::AudioFile);
}

/// Plays the sound of the audio file at `path` with the open file, as a track of its own.
fn add_audio_track(app: &mut App, path: &Path) {
    let Some(session) = app.session.as_mut() else {
        return;
    };
    match session.add_audio_file(path) {
        Ok(stream) => {
            let audio = session.tracks(TrackKind::Audio);
            let i = audio.iter().position(|t| t.stream == stream);
            app.select_track(TrackTab::Audio, i);
        }
        Err(e) => eprintln!("audio: {e}"),
    }
}

/// Shows the subtitle file `subs` picked for `path`: on the open file, or on `path` once it
/// plays.
fn add_subtitle_to(app: &mut App, path: &Path, subs: PathBuf) {
    let open = app.session.as_ref().is_some_and(|s| s.path == path);
    if !open {
        let place = UI.with_borrow(|ui| ui.browse.place.clone());
        let mut queue = app.library.media_in(&place);
        if !queue.iter().any(|p| p == path) {
            queue = vec![path.to_owned()];
        }
        if !app.play(path, queue) {
            return;
        }
        UI.with_borrow_mut(|ui| ui.show(Page::Player));
    }
    let i = match app.external.iter().position(|(p, _)| *p == subs) {
        Some(i) => i,
        None => {
            app.external.push((subs, None));
            app.external.len() - 1
        }
    };
    app.show_external(Some(i));
}

/// Plays a playlist from its first file, or in a shuffled order.
fn play_playlist(app: &mut App, name: &str, shuffle: bool) {
    let mut items = app.library.playlist_items(name);
    if shuffle {
        shuffle_in_place(&mut items);
    }
    let Some(first) = items.first().cloned() else {
        return;
    };
    app.mark_playlist(name);
    if app.play(&first, items) {
        UI.with_borrow_mut(|ui| ui.show(Page::Player));
    }
}

/// Fisher-Yates with a xorshift seeded from the clock; good enough for a play order.
fn shuffle_in_place<T>(items: &mut [T]) {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
        | 1;
    for i in (1..items.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        items.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

/// Writes a playlist to the M3U file the user picked, with absolute paths so it works anywhere.
fn export_playlist(app: &App, name: &str, file: &Path) {
    let mut text = String::from("#EXTM3U\n");
    for item in app.library.playlist_items(name) {
        text.push_str(&item.to_string_lossy());
        text.push('\n');
    }
    if let Err(e) = std::fs::write(file, text) {
        eprintln!("playlist: cannot export to {}: {e}", file.display());
    }
}

/// Picks a folder for the library's Videos, or its Music for `Section::Music`.
fn add_folder(section: Section) {
    let section = match section {
        Section::Music => Section::Music,
        _ => Section::Videos,
    };
    pick_folder(Picked::Folder(section));
}

/// Opens the New Playlist dialog.
fn create_playlist(app: &mut App) {
    open_dialog(app, Dialog::NewPlaylist { files: Vec::new() });
}

/// Opens a dialog, filling in what it needs from the library: Rename Playlist shows the
/// playlist's current thumbnail.
fn open_dialog(app: &App, dialog: Dialog) {
    let thumbnail = match &dialog {
        Dialog::RenamePlaylist(name) => app.library.playlist_thumbnail(name).map(Path::to_owned),
        _ => None,
    };
    UI.with_borrow_mut(|ui| {
        ui.open_dialog(dialog);
        ui.dialog_show_thumbnail(thumbnail);
    });
}

/// Does what the open dialog asked for.
fn apply_dialog(app: &mut App, win: Window, action: DialogAction) {
    // Library errors in words for the dialog's error line.
    let explain = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::AlreadyExists => "A playlist with this name already exists.".to_owned(),
        _ => format!("Can't save the playlist: {e}"),
    };
    match action {
        DialogAction::OpenAddress(kind, typed) => match net::normalize_url(&typed) {
            Err(e) => UI.with_borrow_mut(|ui| ui.dialog_error(e)),
            Ok(url) => {
                UI.with_borrow_mut(Ui::close_dialog);
                let name = app.library.address_name(kind, &url);
                app.library.add_address(kind, &url, &name);
                app.library_changed = true;
                UI.with_borrow_mut(|ui| ui.browse.stale = true);
                if kind == NetKind::Podcast {
                    UI.with_borrow_mut(|ui| {
                        ui.browse.go(Place::Podcast(url));
                        ui.show(Page::Library);
                    });
                } else {
                    let path = PathBuf::from(url);
                    if app.play(&path, vec![path.clone()]) {
                        UI.with_borrow_mut(|ui| ui.show(Page::Player));
                    }
                }
            }
        },
        DialogAction::DownloadUpdate(release) => app.downloading = Some(update::download(&release)),
        DialogAction::DeletePlaylist(name) => {
            if let Err(e) = app.library.delete_playlist(&name) {
                eprintln!("library: cannot delete {name}: {e}");
            }
            // Leave the deleted playlist's page.
            UI.with_borrow_mut(|ui| {
                if ui.browse.place == Place::Playlist(name) {
                    ui.browse.go(Place::Playlists);
                }
                ui.browse.stale = true;
            });
        }
        DialogAction::CreateProfile(name) => match app.switch_profile(win, &name, true) {
            Ok(()) => UI.with_borrow_mut(Ui::close_dialog),
            Err(e) => {
                let message = match e.kind() {
                    std::io::ErrorKind::AlreadyExists => e.to_string(),
                    _ => format!("Can't save the profile: {e}"),
                };
                UI.with_borrow_mut(|ui| ui.dialog_error(message));
            }
        },
        DialogAction::PickFiles => pick_media_files(Picked::DialogFiles),
        DialogAction::PickThumbnail => {
            let images = vec![(
                "Images",
                "*.png;*.jpg;*.jpeg;*.bmp;*.gif;*.webp;*.tif;*.tiff".to_owned(),
            )];
            pick(Picker::Files(images, false), Picked::Thumbnail);
        }
        DialogAction::CreatePlaylist {
            name,
            thumbnail,
            files,
        } => {
            if let Err(e) = app.library.create_playlist(&name) {
                UI.with_borrow_mut(|ui| ui.dialog_error(explain(e)));
                return;
            }
            for file in &files {
                if let Err(e) = app.library.add_to_playlist(&name, file) {
                    eprintln!("library: cannot add {file:?} to {name}: {e}");
                }
            }
            app.library
                .set_playlist_thumbnail(&name, thumbnail.as_deref());
            app.library_changed = true;
            app.save();
            UI.with_borrow_mut(|ui| {
                ui.close_dialog();
                ui.browse.stale = true;
            });
        }
        DialogAction::RenamePlaylist {
            from,
            to,
            thumbnail,
        } => {
            if let Err(e) = app.library.rename_playlist(&from, &to) {
                UI.with_borrow_mut(|ui| ui.dialog_error(explain(e)));
                return;
            }
            // The thumbnail moved with the playlist; only a new pick replaces it.
            if let Some(image) = thumbnail {
                app.library.set_playlist_thumbnail(&to, Some(&image));
            }
            app.library_changed = true;
            app.save();
            UI.with_borrow_mut(|ui| {
                ui.close_dialog();
                // The open playlist page follows its new name.
                if ui.browse.place == Place::Playlist(from) {
                    ui.browse.place = Place::Playlist(to);
                }
                ui.browse.stale = true;
            });
        }
        DialogAction::OpenLink(url) => platform::open_with_default_app(Path::new(&url)),
    }
    UI.with_borrow_mut(|ui| ui.dirty = true);
}

/// Properties of the file playing, else of the library file under the pointer.
fn show_properties(app: &App) {
    let path = app.session.as_ref().map(|s| s.path.clone()).or_else(|| {
        UI.with_borrow(|ui| match ui.hover {
            Some(Hit::Entry(i)) => match ui.browse.entries.get(i) {
                Some(Entry::Media { path, .. }) => Some(path.clone()),
                _ => None,
            },
            _ => None,
        })
    });
    if let Some(path) = path {
        open_dialog(app, Dialog::Properties(path));
    }
}

/// "*.a;*.b" for a file picker filter.
fn file_spec(exts: &[&str]) -> String {
    exts.iter()
        .map(|e| format!("*.{e}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// The system file picker for video and audio files, several at once.
fn pick_media_files(then: Picked) {
    let (video, audio) = (
        file_spec(library::VIDEO_EXTS),
        file_spec(library::AUDIO_EXTS),
    );
    let filters = vec![
        ("Media files", format!("{video};{audio}")),
        ("Video files", video),
        ("Audio files", audio),
        ("All files", "*.*".to_owned()),
    ];
    pick(Picker::Files(filters, true), then);
}

/// "16:9" or "2.39:1" as a whole-number ratio; None for "Default".
fn ratio(label: Option<&str>) -> Option<(u32, u32)> {
    let (w, h) = label?.split_once(':')?;
    let (w, h): (f64, f64) = (w.parse().ok()?, h.parse().ok()?);
    Some(((w * 100.0).round() as u32, (h * 100.0).round() as u32))
}

/// The system file picker, for loading a subtitle file by hand.
fn pick_subtitle_file(then: Picked) {
    let filters = vec![
        (
            "Subtitles",
            "*.srt;*.ass;*.ssa;*.vtt;*.sub;*.idx;*.smi;*.sami;*.sup".to_owned(),
        ),
        ("All files", "*.*".to_owned()),
    ];
    pick(Picker::Files(filters, false), then);
}

/// The system folder picker.
fn pick_folder(then: Picked) {
    pick(Picker::Folder, then);
}

/// A system file picker to show: files with (name, "*.a;*.b") filters, one or several, a
/// folder, or the Save dialog for an M3U file named so at first.
enum Picker {
    Files(Vec<(&'static str, String)>, bool),
    Folder,
    Save(String),
}

/// What the files picked are for. Pickers are modal, and one run inside a pass would hold the
/// render loop up until it closed, so a pass only asks for one (`pick`); `run_frames` shows it
/// between passes, while `modal_frame` keeps the video and the interface going, and the next
/// pass does this with the answer (`use_picked`).
enum Picked {
    /// Shown on the open file.
    SubtitleFile,
    /// Played as the open file's audio track.
    AudioFile,
    /// Shown on this file, played first if it isn't open.
    SubtitleFor(PathBuf),
    QueueFiles,
    OpenFiles,
    OpenFolder,
    /// A DVD or Blu-ray folder or image.
    Disc,
    /// The welcome's media folder.
    OnboardingFolder,
    /// For the New Playlist dialog's list.
    DialogFiles,
    /// The playlist dialog's picture.
    Thumbnail,
    /// A library folder for the section.
    Folder(Section),
    /// Appended to the playlist.
    PlaylistFiles(String),
    /// Where the playlist is exported to.
    ExportPlaylist(String),
}

thread_local! {
    /// The picker a pass asked for; the first one asked wins.
    static PICK: RefCell<Option<(Picker, Picked)>> = const { RefCell::new(None) };
    /// Pickers' answers, for the next pass. Cancelled ones leave nothing.
    static PICKED: RefCell<Vec<(Picked, Vec<PathBuf>)>> = const { RefCell::new(Vec::new()) };
}

/// Asks for `picker`, to do `then` with its answer.
fn pick(picker: Picker, then: Picked) {
    PICK.with_borrow_mut(|p| {
        if p.is_none() {
            *p = Some((picker, then));
        }
    });
}

/// Shows the picker the last pass asked for, if any. Modal: AppKit runs its own loop until it
/// closes, and the modal timer runs passes meanwhile.
fn run_picker() {
    let Some((picker, then)) = PICK.take() else {
        return;
    };
    let paths = match picker {
        Picker::Files(filters, multiple) => platform::pick_files(&filters, multiple),
        Picker::Folder => platform::pick_folder().into_iter().collect(),
        Picker::Save(name) => platform::pick_save_file(&name, ("M3U playlist", "*.m3u"))
            .into_iter()
            .collect(),
    };
    if !paths.is_empty() {
        PICKED.with_borrow_mut(|p| p.push((then, paths)));
    }
}

/// Does what the files were picked for; `paths` holds at least one.
fn use_picked(app: &mut App, then: Picked, mut paths: Vec<PathBuf>) {
    let show_player = || UI.with_borrow_mut(|ui| ui.show(Page::Player));
    let first = paths[0].clone();
    match then {
        Picked::SubtitleFile => {
            let i = match app.external.iter().position(|(p, _)| *p == first) {
                Some(i) => i,
                None => {
                    app.external.push((first, None));
                    app.external.len() - 1
                }
            };
            app.show_external(Some(i));
        }
        Picked::AudioFile => add_audio_track(app, &first),
        Picked::SubtitleFor(media) => add_subtitle_to(app, &media, first),
        Picked::QueueFiles => app.add_to_queue(paths),
        Picked::OpenFiles => {
            if app.play(&first, paths) {
                show_player();
            }
        }
        Picked::OpenFolder => {
            let queue = app.library.media_in(&Place::Folder(Section::Videos, first));
            if let Some(first) = queue.first().cloned()
                && app.play(&first, queue)
            {
                show_player();
            }
        }
        Picked::Disc => open_disc(app, &first),
        Picked::OnboardingFolder => {
            app.library.add_root(Section::Videos, first);
            app.library_changed = true;
            app.save();
            let folder = videos_folder(&app.library);
            UI.with_borrow_mut(|ui| {
                ui.browse.stale = true;
                ui.onboarding_folder(folder);
            });
        }
        Picked::DialogFiles => UI.with_borrow_mut(|ui| ui.dialog_add_files(paths)),
        Picked::Thumbnail => {
            let image = paths.pop();
            UI.with_borrow_mut(|ui| ui.dialog_set_thumbnail(image));
        }
        Picked::Folder(section) => {
            app.library.add_root(section, first);
            app.library_changed = true;
            app.save();
            UI.with_borrow_mut(|ui| ui.browse.stale = true);
        }
        Picked::PlaylistFiles(name) => {
            for file in paths {
                if let Err(e) = app.library.add_to_playlist(&name, &file) {
                    eprintln!("library: cannot add {file:?} to {name}: {e}");
                }
            }
            UI.with_borrow_mut(|ui| ui.browse.stale = true);
        }
        Picked::ExportPlaylist(name) => export_playlist(app, &name, &first),
    }
    UI.with_borrow_mut(|ui| ui.dirty = true);
}

/// The window's own title, for the Window menu, Mission Control and the Dock: the file playing,
/// then the app.
fn window_title(session: Option<&Session>) -> String {
    match session {
        // A stream's own name: its address could carry a user name and password.
        Some(s) if net::is_address(&s.path) => format!("{} - wushang", s.title),
        Some(s) => match s.path.file_name() {
            Some(name) => format!("{} - wushang", name.to_string_lossy()),
            None => "wushang".to_owned(),
        },
        None => "wushang".to_owned(),
    }
}

/// Creates the window hidden; `run` shows it once the renderer is ready. The window draws its own
/// title bar; AppKit keeps the shadow, rounded corners, resizing, fullscreen and the
/// minimize animation.
fn create_window(title: &str, (video_w, video_h): (u32, u32)) -> Window {
    // Open with the video at its own size in pixels, shrunk to fit 80% of the screen, plus the
    // bars.
    let (screen_w, screen_h, scale) = platform::main_screen();
    let (screen_w, screen_h) = (screen_w * scale * 0.8, screen_h * scale * 0.8);
    let (chrome_w, chrome_h) = (20.0 * scale, 95.0 * scale);
    let fit = ((screen_w - chrome_w) / f64::from(video_w))
        .min((screen_h - chrome_h) / f64::from(video_h))
        .min(1.0);
    let width = ((f64::from(video_w) * fit + chrome_w) / scale).max(f64::from(MIN_SIZE.0));
    let height = ((f64::from(video_h) * fit + chrome_h) / scale).max(f64::from(MIN_SIZE.1));
    let min = (f64::from(MIN_SIZE.0), f64::from(MIN_SIZE.1));
    platform::create_window(title, (width.round(), height.round()), min)
}

/// Puts the window where it was when wushang last closed, if Settings remember it and that spot
/// is still on a screen. Returns whether it was zoomed.
fn restore_placement(win: Window, p: &Prefs) -> bool {
    let Some(([x, y, w, h], maximized)) = p.window.filter(|_| p.remember_window) else {
        return false;
    };
    let rect = Rect {
        x: f64::from(x),
        y: f64::from(y),
        w: f64::from(w).max(f64::from(MIN_SIZE.0)),
        h: f64::from(h).max(f64::from(MIN_SIZE.1)),
    };
    if !Window::on_a_screen(rect) {
        return false;
    }
    win.set_frame(rect);
    maximized
}

/// Keeps the window's restored frame (in points, from the main screen's top left) and zoomed
/// state for next time, as it closes.
fn remember_placement(win: Window) {
    // In the mini player, the placement to come back to is the one saved on the way in; in
    // fullscreen, the window's own from before.
    let saved = *MINI.lock().unwrap();
    let (r, maximized) = saved.unwrap_or_else(|| win.placement());
    let rect = [r.x, r.y, r.w, r.h].map(|v| v.round() as i32);
    PREFS.with_borrow_mut(|prefs| prefs.window = Some((rect, maximized)));
}

fn is_fullscreen() -> bool {
    Window.is_fullscreen()
}

/// Closes the window and ends the render loop, as DestroyWindow and WM_DESTROY did.
fn destroy(win: Window) {
    remember_placement(win);
    if let Some(shared) = SHARED.get() {
        shared.quit.store(true, Relaxed);
    }
    win.close();
}

/// The pointer's shape over the open dialog: an I-beam on its field, a hand on links.
fn set_pointer() {
    let cursor = match UI.with_borrow(Ui::dialog_cursor) {
        Some(c) if c == IDC_IBEAM => platform::Cursor::IBeam,
        Some(c) if c == IDC_HAND => platform::Cursor::Hand,
        _ => platform::Cursor::Arrow,
    };
    platform::set_cursor(cursor);
}

/// What the window tells wushang: the window procedure, as it was on Windows.
fn on_event(event: Event) {
    let Some(shared) = SHARED.get() else {
        return;
    };
    let win = Window;
    match event {
        Event::Resized => {
            RESIZED.store(true, Relaxed);
            // The mini player keeps the size it is dragged to, queue apart.
            if is_mini() {
                let (w, h) = win.size();
                let queue = if MINI_QUEUE.get() {
                    ui::mini::QUEUE_H
                } else {
                    0.0
                };
                MINI_CARD.set((
                    (w as f32).max(ui::mini::W),
                    (h as f32 - queue).max(ui::mini::H),
                ));
            }
        }
        Event::DisplayChanged => DISPLAY_CHANGED.store(true, Relaxed),
        // The system switched between light and dark.
        Event::AppearanceChanged => {
            DISPLAY_CHANGED.store(true, Relaxed);
            if prefs().theme == 2 {
                ui::theme::set(theme_of(&prefs()));
                UI.with_borrow_mut(|ui| ui.dirty = true);
            }
        }
        Event::MouseMove { x, y } => {
            let action = UI.with_borrow_mut(|ui| ui.on_move(x, y));
            act(win, shared, action);
            set_pointer();
        }
        Event::MouseLeave => UI.with_borrow_mut(Ui::on_leave),
        Event::MouseDown { x, y, clicks } => {
            let hit = UI.with_borrow(|ui| ui.layout.hit(x, y));
            // The design's title bar moves the window, and a double-click zooms it, except the
            // mini player's, which would grow to fill the screen.
            if hit == Some(Hit::Caption) && !is_fullscreen() {
                if clicks % 2 == 0 {
                    if !is_mini() {
                        win.title_double_click();
                    }
                } else {
                    win.drag();
                }
                return;
            }
            let on_video = hit == Some(Hit::Video);
            let (click, double) = PREFS.with_borrow(|p| (p.click, p.double_click));
            if clicks % 2 == 0 && on_video && double == 0 {
                // The first click of the two paused or played; put that back.
                if click == 1 {
                    shared.toggle_pause();
                }
                toggle_fullscreen(win);
            } else {
                let action = UI.with_borrow_mut(|ui| ui.on_down(x, y));
                act(win, shared, action);
            }
        }
        Event::MouseUp { x, y } => {
            let action = UI.with_borrow_mut(|ui| ui.on_up(x, y));
            act(win, shared, action);
        }
        Event::RightUp { x, y } => {
            let action = UI.with_borrow_mut(|ui| ui.on_right(x, y));
            act(win, shared, action);
        }
        // The wheel scrolls the popups, the library and the sidebar panel, and sets the volume.
        Event::Wheel {
            x,
            y,
            notches,
            steps,
            physical,
        } => {
            if UI.with_borrow(|ui| ui.dialog.is_some()) {
                if steps != 0.0 {
                    UI.with_borrow_mut(|ui| ui.scroll_dialog(steps));
                }
            } else if UI.with_borrow(|ui| ui.preferences.is_some()) {
                UI.with_borrow_mut(|ui| ui.scroll_preferences(x, y, notches));
            } else if UI.with_borrow(|ui| ui.menu.is_open()) {
                // An open menu takes the wheel, so nothing scrolls away under it.
                if UI.with_borrow(|ui| ui.over_menu(x, y)) {
                    UI.with_borrow_mut(|ui| ui.scroll_menu(x, y, -60.0 * notches));
                }
            } else if UI.with_borrow(|ui| ui.over_panel(x, y)) {
                UI.with_borrow_mut(|ui| ui.scroll_panel(x, y, -60.0 * notches));
            } else if UI.with_borrow(|ui| ui.page == Page::Library || ui.over_overlay()) {
                UI.with_borrow_mut(|ui| ui.scroll(-120.0 * notches));
            } else if physical != 0.0 {
                // Over the video: volume, seeking or nothing, per Settings.
                let (wheel, reverse, skip) =
                    PREFS.with_borrow(|p| (p.wheel, p.reverse_wheel, p.skip[1]));
                let notches = if reverse { -physical } else { physical };
                match wheel {
                    0 => act(
                        win,
                        shared,
                        Some(Action::SetVolume(shared.volume() + 0.05 * notches)),
                    ),
                    1 => shared.seek_by(f64::from(notches) * f64::from(skip)),
                    _ => {}
                }
            }
        }
        Event::Drop(files) => {
            // The New Playlist dialog takes every dropped file into its list.
            if UI.with_borrow(Ui::dialog_takes_files) {
                UI.with_borrow_mut(|ui| {
                    ui.dialog_add_files(files);
                    ui.dirty = true;
                });
            } else if let Some(first) = files.into_iter().next() {
                *OPEN.lock().unwrap() = Some(first);
            }
        }
        // Opened from Finder or the Dock while running: it plays, whatever is on screen.
        Event::Open(paths) => OPEN_PLAY.lock().unwrap().extend(paths),
        Event::Char(c) => {
            // An open dialog takes typing before the search box, then the Settings window.
            if UI.with_borrow_mut(|ui| ui.dialog_char(c) || ui.preferences_char(c)) {
                return;
            }
            // Typing goes to the library's search box while it has focus.
            UI.with_borrow_mut(|ui| {
                let b = &mut ui.browse;
                if !b.searching {
                    return;
                }
                match char::from_u32(c) {
                    Some('\u{8}') => _ = b.search.pop(),
                    Some(c) if !c.is_control() => b.search.push(c),
                    _ => return,
                }
                b.scroll = 0.0;
                b.apply();
                ui.relayout = true;
            });
        }
        Event::Key {
            key,
            shift,
            ctrl,
            alt,
        } => {
            let typing = UI.with_borrow(|ui| ui.dialog.is_some() || ui.preferences.is_some());
            if alt {
                // Option + Left/Right is Alt's very short skip; in a text field it moves by
                // words and Option + Backspace deletes one, as on macOS. Other Option keys type
                // characters, as Alt keys on Windows did nothing here.
                let word = matches!(key, VK_LEFT | VK_RIGHT | VK_BACK | VK_DELETE);
                if typing && word {
                    on_key(win, shared, key, shift, true);
                } else if matches!(key, VK_LEFT | VK_RIGHT)
                    && !typing
                    && !UI.with_borrow(|ui| ui.menu.is_open())
                {
                    let skip = f64::from(PREFS.with_borrow(|p| p.skip[0]));
                    shared.seek_by(if key == VK_LEFT { -skip } else { skip });
                } else {
                    return;
                }
            } else if typing && ctrl && matches!(key, VK_LEFT | VK_RIGHT | VK_BACK) {
                // Command + Left/Right go to the field's start and end, and Command +
                // Backspace deletes back to the start, as on macOS.
                if key == VK_BACK {
                    on_key(win, shared, VK_HOME, true, false);
                    on_key(win, shared, VK_BACK, false, false);
                } else {
                    let to = if key == VK_LEFT { VK_HOME } else { VK_END };
                    on_key(win, shared, to, shift, false);
                }
            } else {
                on_key(win, shared, key, shift, ctrl);
            }
            UI.with_borrow_mut(|ui| {
                ui.wake();
                ui.dirty = true;
            });
        }
        Event::Menu(command) => {
            use platform::Command as C;
            let action = match command {
                C::About => MenuAction::ShowAbout,
                C::Settings => MenuAction::Preferences(Category::Playback),
                C::Quit => MenuAction::Quit,
                C::Open => MenuAction::OpenFiles,
                C::OpenFolder => MenuAction::OpenFolder,
                C::OpenNetwork => MenuAction::OpenNetwork,
                C::PlayPause => MenuAction::TogglePause,
                C::MiniPlayer => MenuAction::MiniPlayer,
                C::Minimize => {
                    win.minimize();
                    return;
                }
            };
            act(win, shared, Some(Action::Menu(action)));
        }
        Event::Close => destroy(win),
    }
}

fn act(win: Window, shared: &Shared, action: Option<Action>) {
    let Some(action) = action else { return };
    UI.with_borrow_mut(|ui| {
        ui.dirty = true;
        // A click anywhere else takes focus from an empty search box.
        if ui.browse.searching
            && ui.browse.search.is_empty()
            && !matches!(action, Action::Click(Hit::Search))
        {
            ui.browse.searching = false;
        }
    });
    match action {
        Action::Click(Hit::Play) => shared.toggle_pause(),
        Action::Click(Hit::Speaker) => shared.toggle_mute(),
        Action::Click(Hit::Duration) => PREFS.with_borrow_mut(|p| p.time_left = !p.time_left),
        Action::Click(Hit::Minimize) => win.minimize(),
        Action::Click(Hit::Maximize) => win.zoom(),
        Action::Click(Hit::Fullscreen) => toggle_fullscreen(win),
        Action::Click(Hit::Close) => destroy(win),
        Action::Click(Hit::Search) => UI.with_borrow_mut(|ui| {
            // The panel has no search field; its magnifier opens the library's.
            ui.overlay = false;
            ui.show(Page::Library);
            ui.browse.searching = true;
        }),
        Action::Click(Hit::OpenLibrary) => UI.with_borrow_mut(|ui| {
            ui.overlay = false;
            ui.show(Page::Library);
        }),
        // A click on the video closes the panel, or else plays or pauses if Settings say so.
        Action::Click(Hit::Video) => {
            let closed = UI.with_borrow_mut(|ui| {
                let open = ui.overlay;
                if open {
                    ui.overlay = false;
                    ui.browse.scroll = 0.0;
                }
                open
            });
            if !closed && PREFS.with_borrow(|p| p.click == 1) {
                shared.toggle_pause();
            }
        }
        // The panel is always a list.
        Action::Click(Hit::ViewMode) if UI.with_borrow(|ui| ui.page == Page::Player) => {}
        Action::Click(Hit::ViewMode) => UI.with_borrow_mut(|ui| {
            ui.browse.grid = !ui.browse.grid;
            ui.browse.scroll = 0.0;
            ui.relayout = true;
        }),
        // The menus, audio, subtitle, settings and playlist panels arrive in later stages.
        Action::Click(Hit::Sidebar) => UI.with_borrow_mut(|ui| {
            ui.overlay = !ui.overlay;
            ui.browse.scroll = 0.0;
        }),
        Action::Click(
            hit @ (Hit::Nav(_)
            | Hit::AddItem
            | Hit::AddHere
            | Hit::Back
            | Hit::Forward
            | Hit::Crumb(_)
            | Hit::Entry(_)
            | Hit::Card
            | Hit::MiniClose
            | Hit::MiniExpand
            | Hit::Tool(_)
            | Hit::Folder
            | Hit::Next
            | Hit::Previous),
        ) => PENDING.with_borrow_mut(|p| p.push(hit)),
        Action::Click(_) => {}
        Action::SeekTo(fraction) => {
            shared.seek_to(fraction * f64::from_bits(DURATION.load(Relaxed)))
        }
        Action::SetVolume(volume) => shared.set_volume(volume),
        Action::Panel(a) => PANEL_ACTIONS.with_borrow_mut(|p| p.push(a)),
        Action::Queue(a) => QUEUE_ACTIONS.with_borrow_mut(|q| q.push(a)),
        Action::Menu(a) => MENU_ACTIONS.with_borrow_mut(|m| m.push(a)),
        Action::OpenMenu(hit, x, y) => MENU_REQUESTS.with_borrow_mut(|m| m.push((hit, x, y))),
        Action::Dialog(a) => DIALOG_ACTIONS.with_borrow_mut(|p| p.push(a)),
        Action::Preferences(a) => PREFS_ACTIONS.with_borrow_mut(|p| p.push(a)),
        Action::Onboarding(a) => ONBOARDING_ACTIONS.with_borrow_mut(|o| o.push(a)),
        Action::SaveToolbar(t) => {
            if let Err(e) = t.save(&Library::default_dir()) {
                eprintln!("toolbar: cannot save ({e})");
            }
            UI.with_borrow_mut(|ui| {
                ui.toolbar = t;
                ui.relayout = true;
            });
        }
    }
}

/// A click on one of the bar items Customize Player adds.
fn use_tool(app: &mut App, win: Window, item: ui::ToolItem) {
    use ui::ToolItem as T;
    let queue = |a| QUEUE_ACTIONS.with_borrow_mut(|q| q.push(a));
    let queue_tab = |tab| {
        UI.with_borrow_mut(|ui| {
            ui.queue.tab = tab;
            ui.queue.scroll = 0.0;
            if !ui.queue.open {
                ui.toggle_queue();
            }
            ui.relayout = true;
        })
    };
    match item {
        T::Forward10 => app.shared.seek_by(10.0),
        T::Back10 => app.shared.seek_by(-10.0),
        T::Forward15 => app.shared.seek_by(15.0),
        T::Back15 => app.shared.seek_by(-15.0),
        T::Replay => app.shared.seek_to(0.0),
        T::Stop => app.stop(),
        T::Repeat => queue(QueueAction::CycleRepeat),
        T::Shuffle => queue(QueueAction::ToggleShuffle),
        T::Bookmark => queue(QueueAction::AddBookmark),
        T::Chapters => queue_tab(ui::queue::Tab::Chapters),
        T::Bookmarks => queue_tab(ui::queue::Tab::Bookmarks),
        T::MiniPlayer => MINI_REQUEST.set(Some(None)),
        T::Zoom => {
            PANEL_ACTIONS.with_borrow_mut(|p| p.push(PanelAction::SetZoom((app.zoom + 1) % 3)))
        }
        T::Hdr => {
            let on = PREFS.with_borrow(|p| p.hdr);
            PANEL_ACTIONS.with_borrow_mut(|p| p.push(PanelAction::SetHdr(!on)))
        }
        // The speed slider is on the settings popup's Playback tab.
        T::Speed => UI.with_borrow_mut(|ui| {
            let panel = ui::panels::Panel::Settings(ui::panels::SettingsTab::Playback);
            ui.panels.show(Some(panel), Some(Hit::Settings));
            ui.queue.open = false;
            ui.relayout = true;
        }),
        T::Quit => destroy(win),
        T::Preferences => open_preferences(app, Category::Playback),
        T::Home => UI.with_borrow_mut(|ui| ui.show(Page::Library)),
        T::Properties => WANT_PROPERTIES.set(true),
        T::Snapshot => WANT_SNAPSHOT.set(true),
        T::Record => app.toggle_record(),
        T::RightSidebar => UI.with_borrow_mut(Ui::toggle_queue),
        T::ArtistName => {
            let artist = (app.session.as_ref())
                .and_then(|s| app.library.track(&s.path))
                .and_then(|t| t.artist.clone());
            if let Some(artist) = artist {
                UI.with_borrow_mut(|ui| {
                    ui.browse.go(Place::Artist(artist));
                    ui.show(Page::Library);
                });
            }
        }
        T::PlaylistName => {
            if let Some(name) = app.playing_playlist.clone() {
                UI.with_borrow_mut(|ui| {
                    ui.browse.go(Place::Playlist(name));
                    ui.show(Page::Library);
                });
            }
        }
        T::Favorite => {
            if let Some(path) = app.session.as_ref().map(|s| s.path.clone()) {
                let on = !app.library.is_favorite(&path);
                app.library.set_favorite(&path, on);
                app.library_changed = true;
                UI.with_borrow_mut(|ui| ui.browse.stale = true);
            }
        }
        T::Playlists => UI.with_borrow_mut(|ui| {
            ui.browse.go(Place::Playlists);
            ui.show(Page::Library);
        }),
        _ => {}
    }
}

/// Plays the DVD or Blu-ray in `path` (a drive, a folder, or a disc image).
fn open_disc(app: &mut App, path: &Path) {
    match disc::address(path) {
        Some(address) => {
            if app.play(&address, vec![address.clone()]) {
                UI.with_borrow_mut(|ui| ui.show(Page::Player));
            }
        }
        None => {
            let line = format!("No DVD or Blu-ray video in {}", path.display());
            app.notice = Some((line, Instant::now()));
            UI.with_borrow_mut(|ui| ui.show(Page::Player));
        }
    }
}

/// Lists the capture devices into `devices` in the background; it takes a moment.
fn refresh_devices(devices: &Arc<Mutex<Vec<capture::Device>>>) {
    let devices = devices.clone();
    thread::spawn(move || *devices.lock().unwrap() = capture::devices());
}

/// Fetches the podcast feed or radio directory the library page shows, and takes in what
/// finished. Entering one of those pages again retries a fetch that failed.
fn fetch_network(app: &mut App) {
    let place = UI.with_borrow(|ui| ui.browse.place.clone());
    if app.shown_place.as_ref() != Some(&place) {
        app.library.retry_failed(&place);
        app.shown_place = Some(place.clone());
    }
    if app.library.wants_fetch(&place) {
        app.library.fetching(&place);
        let timeout = Duration::from_secs(prefs().net_timeout.into());
        match &place {
            Place::Podcast(url) => app.fetcher.feed(url, timeout),
            _ => app.fetcher.stations(timeout),
        }
    }
    let ready = app.fetcher.take_ready();
    for fetched in &ready {
        match fetched {
            net::Fetched::Feed { feed, result } => {
                app.library.feed_fetched(feed, result.clone());
                // The feed's own title replaces the address as its name.
                app.library_changed = true;
            }
            net::Fetched::Stations(result) => app.library.stations_fetched(result.clone()),
        }
    }
    if !ready.is_empty() {
        UI.with_borrow_mut(|ui| ui.browse.stale = true);
    }
}

const RECONNECTING: &str = "Reconnecting...";

/// How long a notice stays over the video.
const NOTICE_FOR: Duration = Duration::from_secs(3);

/// Saves the frame on screen as a PNG; the notice says where once it's written.
fn take_snapshot(app: &App, renderer: &mut render::Renderer) {
    let (Some(current), Some(session)) = (&app.current, &app.session) else {
        return;
    };
    let seconds = session.position().unwrap_or(current.pts);
    match renderer.snapshot(&current.frame, &app.adjust) {
        Ok(picture) => {
            let stem = snapshot::stem(&session.path, &session.title);
            let (dir, jpeg) = PREFS.with_borrow(|p| {
                let key = prefs::SNAP_DIR.get(p.snap_dir).map_or("", |c| c.0);
                let own = snapshot::Folder::Pictures;
                (
                    snapshot::output_dir(key, own, &session.path),
                    p.snap_format == 1,
                )
            });
            match dir {
                Ok(dir) => {
                    snapshot::save(picture, dir, &stem, seconds, jpeg, app.notices.0.clone())
                }
                Err(e) => _ = app.notices.0.send(format!("Can't save the snapshot ({e})")),
            }
        }
        Err(e) => _ = app.notices.0.send(format!("Can't take a snapshot ({e})")),
    }
}

/// The welcome's media folder: the library's newest Videos folder, the one Browse just added.
fn videos_folder(library: &Library) -> String {
    library
        .roots(Section::Videos)
        .last()
        .map_or_else(|| "No folder yet".to_owned(), |p| p.display().to_string())
}

/// A key down, with Shift and Ctrl (Command, or Control) as held when it was pressed.
fn on_key(win: Window, shared: &Shared, key: VIRTUAL_KEY, shift: bool, ctrl: bool) {
    let (skip, volume_step) = PREFS.with_borrow(|p| (p.skip, p.volume_step));
    let step = f64::from(match (ctrl, shift) {
        (true, _) => skip[3],
        (false, true) => skip[2],
        (false, false) => skip[1],
    });
    let volume_step = volume_step as f32 / 100.0;
    let page = UI.with_borrow(|ui| ui.page);
    // An open dialog takes every key, then the Settings window.
    if UI.with_borrow(|ui| ui.dialog.is_some()) {
        let action = UI.with_borrow_mut(|ui| ui.dialog_key(key, shift, ctrl));
        act(win, shared, action);
        return;
    }
    if UI.with_borrow(|ui| ui.customize.is_some()) {
        let action = UI.with_borrow_mut(|ui| ui.customize_key(key));
        act(win, shared, action);
        return;
    }
    if UI.with_borrow(|ui| ui.onboarding.is_some()) {
        let action = UI.with_borrow_mut(|ui| ui.onboarding_key(key));
        act(win, shared, action);
        return;
    }
    if UI.with_borrow(|ui| ui.preferences.is_some()) {
        UI.with_borrow_mut(|ui| ui.preferences_key(key, shift, ctrl));
        return;
    }
    // An open menu takes the keyboard.
    if UI.with_borrow(|ui| ui.menu.is_open()) {
        use ui::menu::Key;
        let key = match key {
            VK_UP => Some(Key::Up),
            VK_DOWN => Some(Key::Down),
            VK_LEFT => Some(Key::Left),
            VK_RIGHT => Some(Key::Right),
            VK_RETURN | VK_SPACE => Some(Key::Enter),
            VK_ESCAPE => Some(Key::Escape),
            _ => None,
        };
        if let Some(key) = key {
            let action = UI.with_borrow_mut(|ui| ui.menu_key(key));
            act(win, shared, action);
        }
        return;
    }
    // While searching, keys type; Esc clears the search and Enter keeps it.
    if UI.with_borrow(|ui| ui.browse.searching) {
        UI.with_borrow_mut(|ui| match key {
            VK_ESCAPE => {
                ui.browse.search.clear();
                ui.browse.searching = false;
                ui.browse.apply();
                ui.relayout = true;
            }
            VK_RETURN => ui.browse.searching = false,
            _ => {}
        });
        return;
    }
    // The menus' shortcuts.
    let menu = |action| act(win, shared, Some(Action::Menu(action)));
    let shortcut = match key {
        VK_O if ctrl => Some(MenuAction::OpenFiles),
        VK_R if ctrl => Some(MenuAction::ConvertOpen),
        VK_D if ctrl => Some(MenuAction::OpenDisc(None)),
        VK_N if ctrl => Some(MenuAction::OpenNetwork),
        VK_F if ctrl => Some(MenuAction::OpenFolder),
        VK_P | VK_OEM_COMMA if ctrl => Some(MenuAction::Preferences(Category::Playback)),
        VK_Q if ctrl => Some(MenuAction::Quit),
        VK_OEM_5 if ctrl && page == Page::Player => Some(MenuAction::HideUi),
        VK_M if ctrl => Some(MenuAction::MiniPlayer),
        VK_F1 if shift => Some(MenuAction::ShowAbout),
        VK_S if shift && page == Page::Player => Some(MenuAction::Snapshot),
        VK_S if page == Page::Player => Some(MenuAction::Stop),
        VK_R if page == Page::Player => Some(MenuAction::Record),
        _ => None,
    };
    if let Some(action) = shortcut {
        menu(action);
        return;
    }
    match key {
        VK_P => PENDING.with_borrow_mut(|p| p.push(Hit::Previous)),
        VK_N => PENDING.with_borrow_mut(|p| p.push(Hit::Next)),
        VK_BACK if page == Page::Library => UI.with_borrow_mut(|ui| ui.browse.back()),
        VK_SPACE => shared.toggle_pause(),
        VK_LEFT => shared.seek_by(-step),
        VK_RIGHT => shared.seek_by(step),
        VK_UP => shared.set_volume(shared.volume() + volume_step),
        VK_DOWN => shared.set_volume(shared.volume() - volume_step),
        VK_M => shared.toggle_mute(),
        VK_F if page == Page::Mini => {}
        VK_F => toggle_fullscreen(win),
        VK_ESCAPE if UI.with_borrow_mut(Ui::close_panel) => {}
        VK_ESCAPE if UI.with_borrow(|ui| ui.overlay && ui.page == Page::Player) => {
            UI.with_borrow_mut(|ui| ui.overlay = false)
        }
        VK_ESCAPE if is_fullscreen() => toggle_fullscreen(win),
        VK_ESCAPE if page == Page::Mini => MINI_REQUEST.set(Some(Some(Page::Player))),
        // Esc leaves the player for the library, which keeps playing in the mini player.
        VK_ESCAPE => UI.with_borrow_mut(|ui| ui.show(Page::Library)),
        VK_Q => destroy(win),
        VK_F1 => UI.with_borrow_mut(|ui| ui.open_dialog(Dialog::Shortcuts)),
        // VLC's "Media Information": the file playing, else the file under the pointer.
        VK_I if ctrl => WANT_PROPERTIES.set(true),
        _ => {}
    }
}

fn is_mini() -> bool {
    MINI.lock().unwrap().is_some()
}

/// The mini player's size in points: the card as last sized, and its queue when that's open.
fn mini_size(queue: bool) -> (f64, f64) {
    let (w, h) = MINI_CARD.get();
    let h = h + if queue { ui::mini::QUEUE_H } else { 0.0 };
    (f64::from(w), f64::from(h))
}

/// The mini player's smallest size: the design's card, and its queue when that's open.
fn mini_min_size(win: Window, queue: bool) {
    let queue = if queue { ui::mini::QUEUE_H } else { 0.0 };
    win.set_min_size(f64::from(ui::mini::W), f64::from(ui::mini::H + queue));
}

/// Shrinks the window to the mini player, above other windows: where it last was, or at the
/// bottom right of its screen's work area.
fn enter_mini(win: Window) {
    if is_mini() {
        return;
    }
    // Native fullscreen animates out first; the request waits for it to finish.
    if win.fullscreen() != platform::Fullscreen::Off {
        toggle_fullscreen(win);
        MINI_REQUEST.set(Some(None));
        return;
    }
    *MINI.lock().unwrap() = Some(win.placement());
    UI.with_borrow_mut(|ui| {
        ui.overlay = false;
        ui.show(Page::Mini);
    });
    MINI_QUEUE.set(false);
    let (w, h) = mini_size(false);
    let work = win.work_area();
    let margin = 16.0 * w / 391.0;
    let (l, b) = MINI_SPOT
        .get()
        .unwrap_or((work.right() - margin - w, work.bottom() - margin));
    mini_min_size(win, false);
    win.set_frame(Rect {
        x: l,
        y: b - h,
        w,
        h,
    });
    win.set_floating(true);
}

/// Back to the full window, where it was, on `page`.
fn exit_mini(win: Window, page: Page) {
    let Some((frame, zoomed)) = MINI.lock().unwrap().take() else {
        return;
    };
    let r = win.frame();
    MINI_SPOT.set(Some((r.x, r.bottom())));
    UI.with_borrow_mut(|ui| ui.show(page));
    win.set_min_size(f64::from(MIN_SIZE.0), f64::from(MIN_SIZE.1));
    win.set_frame(frame);
    if zoomed && !win.is_zoomed() {
        win.zoom();
    }
    set_topmost(win, prefs().on_top);
}

/// Grows the mini player up for its queue and back, keeping its bottom edge. Otherwise its
/// size is the user's.
fn fit_mini(win: Window, queue: bool) {
    if MINI_QUEUE.replace(queue) == queue {
        return;
    }
    mini_min_size(win, queue);
    let (w, h) = mini_size(queue);
    let r = win.frame();
    if (r.h - h).abs() < 0.5 {
        return;
    }
    win.set_frame(Rect {
        x: r.x,
        y: r.bottom() - h,
        w,
        h,
    });
}

/// Native fullscreen on the window's screen, and back.
fn toggle_fullscreen(win: Window) {
    win.toggle_fullscreen();
}

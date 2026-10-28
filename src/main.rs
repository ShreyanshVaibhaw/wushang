//! myvlc: a small video player built in the shape of VLC's pipeline, with the interface from
//! Juxtopposed's VLC redesign.
//!
//! demux thread -> video and audio decoder threads -> D3D11 renderer + Direct2D UI (this thread)
//!                                                 -> WASAPI callback (drives the clock)

mod ass;
mod audio;
mod bookmarks;
mod captions;
mod capture;
mod codepage;
mod convert;
mod disc;
mod dsp;
mod library;
mod net;
mod nv12;
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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use ffmpeg_next as ff;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, GetMonitorInfoW, GetStockObject, HBRUSH, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromRect, MonitorFromWindow, ScreenToClient,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};
use windows::Win32::System::Threading::{
    GetCurrentThread, GetStartupInfoW, STARTF_USESHOWWINDOW, STARTUPINFOW, SetThreadPriority,
    THREAD_PRIORITY_ABOVE_NORMAL,
};
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
    FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog, HDROP, IFileOpenDialog, IFileSaveDialog,
    SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, w};

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

/// The window procedure has no user data pointer here, so it reaches shared state through statics.
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
/// The open file's length in seconds, as f64 bits, for seek clicks.
static DURATION: AtomicU64 = AtomicU64::new(0);
/// A file dropped on the window; the render loop opens it.
static OPEN: Mutex<Option<PathBuf>> = Mutex::new(None);
static RESIZED: AtomicBool = AtomicBool::new(false);
/// Window style and placement saved while in borderless fullscreen.
static WINDOWED: Mutex<Option<(isize, WINDOWPLACEMENT)>> = Mutex::new(None);
/// The full window's placement, saved while it is the mini player.
static MINI: Mutex<Option<WINDOWPLACEMENT>> = Mutex::new(None);
/// Thumbnail size in pixels: the grid tile at 125% scaling, 16:9.
const THUMB_SIZE: (u32, u32) = (400, 225);
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
    /// Where the mini player last was: its left and bottom edges on screen, in pixels.
    static MINI_SPOT: std::cell::Cell<Option<(i32, i32)>> = const { std::cell::Cell::new(None) };
}

fn prefs() -> Prefs {
    PREFS.with_borrow(Clone::clone)
}

/// Sleep timer choices in minutes, by index into the panel's list; the last one is "End of file".
const SLEEP_MINUTES: [u64; 5] = [0, 15, 30, 45, 60];
const SLEEP_END_OF_FILE: usize = 5;

fn main() {
    if let Err(e) = run() {
        eprintln!("myvlc: {e}");
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
    /// The stereo test's sound, kept while Windows plays it.
    test_tone: Vec<u8>,
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
}

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
                eprintln!("myvlc: {e}");
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
        // A DVD or Blu-ray folder, or a Blu-ray image, opened or dropped like a file.
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

    /// Starts recording the open file (into Videos\myvlc unless Settings says otherwise), or
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
            let own = &windows::Win32::UI::Shell::FOLDERID_Videos;
            let ext = prefs::RECORD_FORMAT
                .get(p.record_format)
                .map_or("mkv", |c| c.0);
            (snapshot::output_dir(key, own, &session.path), ext)
        });
        let dir = match dir.map_err(|e| e.message()) {
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

    /// Converts `path` into Videos\myvlc (MP4) or Music\myvlc (MP3) in the background.
    fn convert(&mut self, path: &Path, target: convert::Target) {
        if let Some(job) = &self.converting {
            let name = job.output.file_name().unwrap_or_default().to_string_lossy();
            self.notice = Some((format!("Still converting {name}"), Instant::now()));
            return;
        }
        let folder = match target {
            convert::Target::Mp4 => &windows::Win32::UI::Shell::FOLDERID_Videos,
            convert::Target::Mp3 => &windows::Win32::UI::Shell::FOLDERID_Music,
        };
        let dir = snapshot::known_folder(folder)
            .map(|d| d.join("myvlc"))
            .map_err(|e| e.message())
            .and_then(|dir| {
                std::fs::create_dir_all(&dir)
                    .map(|()| dir)
                    .map_err(|e| e.to_string())
            });
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
        // Every network interface, so other devices reach it; Windows may ask to let it through.
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
        // Extensions turned them off.
        let fonts = session.take_fonts();
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

    /// The canvas styled (ASS) subtitles are drawn on: the video's size, for audio a 1080p one.
    // ponytail: anamorphic video (non-square pixels) draws them stretched with the picture;
    // give libass the display size and the stored size apart if that shows.
    fn subtitle_canvas(&self) -> (u32, u32) {
        (self.session.as_ref())
            .and_then(|s| s.video_size)
            .unwrap_or((1920, 1080))
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
    fn apply(&mut self, hwnd: HWND, action: PanelAction) {
        let effects = self.audio.effects();
        match action {
            PanelAction::SelectTrack(tab, i) => self.select_track(tab, i),
            PanelAction::SelectExternal(i) => self.show_external(i),
            PanelAction::AddSubtitleFile => {
                if let Some(path) = pick_subtitle_file(hwnd) {
                    let i = match self.external.iter().position(|(p, _)| *p == path) {
                        Some(i) => i,
                        None => {
                            self.external.push((path, None));
                            self.external.len() - 1
                        }
                    };
                    self.show_external(Some(i));
                }
            }
            PanelAction::AddAudioFile => add_audio_file(self, hwnd),
            PanelAction::SelectDevice(i) => {
                // The list starts with the system default, which also follows Windows' choice.
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
                (self.zoom, self.aspect, self.crop) = (0, 0, 0);
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
            PanelAction::SetVisualizer(i) => self.change_prefs(hwnd, |p| p.visualizer = i),
            PanelAction::SetSubTimeline(on) => self.change_prefs(hwnd, |p| p.sub_timeline = on),
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
            PanelAction::SetSubStyles(on) => self.change_prefs(hwnd, |p| p.sub_styles = on),
            PanelAction::SetSubFont(i) => {
                if let Some(font) = prefs::FONTS.get(i) {
                    self.change_prefs(hwnd, |p| p.sub_font = (*font).to_owned());
                }
            }
            PanelAction::SetSubColor(i) => {
                if let Some((rgb, _)) = prefs::COLORS.get(i) {
                    self.change_prefs(hwnd, |p| p.sub_color = *rgb);
                }
            }
            PanelAction::SetOutlineColor(i) => {
                if let Some((rgb, _)) = prefs::COLORS.get(i) {
                    self.change_prefs(hwnd, |p| p.outline_color = *rgb);
                }
            }
            PanelAction::SetProfile(i) => {
                if let Some(name) = self.profiles.names().get(i).cloned()
                    && let Err(e) = self.switch_profile(hwnd, &name, false)
                {
                    eprintln!("profiles: cannot switch to {name}: {e}");
                }
            }
            PanelAction::EditSubtitle => self.edit_subtitle(hwnd),
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
            Some(ff::format::Pixel::D3D11) => "GPU (D3D11VA)",
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
    fn apply_queue(&mut self, hwnd: HWND, action: QueueAction) {
        match action {
            QueueAction::Play(i) => {
                if let Some(path) = self.queue.play_at(i) {
                    self.open(&path);
                }
            }
            QueueAction::Move(from, to) => self.queue.move_item(from, to),
            QueueAction::PlayNext(i) => self.queue.play_next(i),
            QueueAction::Remove(i) => _ = self.queue.remove(i),
            QueueAction::AddFiles => {
                let files = pick_media_files(hwnd);
                self.add_to_queue(files);
            }
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
    fn apply_prefs(&mut self, hwnd: HWND, new: Prefs) {
        let old = prefs();
        if new.on_top != old.on_top {
            set_topmost(hwnd, new.on_top);
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
    fn change_prefs(&mut self, hwnd: HWND, change: impl FnOnce(&mut Prefs)) {
        let mut p = prefs();
        change(&mut p);
        self.apply_prefs(hwnd, p);
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
    // ponytail: Windows plays it on the default device at its own volume; mix it into the
    // player's output if the chosen device or volume should count.
    fn test_stereo(&mut self) {
        use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
        use windows::core::PCWSTR;
        unsafe {
            // Stops a test still playing before its sound is freed.
            _ = PlaySoundW(PCWSTR::null(), None, SND_ASYNC);
            self.test_tone = dsp::test_tone(self.audio.effects().stereo());
            let sound = PCWSTR(self.test_tone.as_ptr().cast());
            _ = PlaySoundW(sound, None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
        }
    }

    /// Opens the chosen subtitle file in the program Windows edits it with, else Notepad.
    fn edit_subtitle(&self, hwnd: HWND) {
        use windows::Win32::UI::Shell::ShellExecuteW;
        let Some((path, _)) = self.external_selected.and_then(|i| self.external.get(i)) else {
            return;
        };
        let file = HSTRING::from(path.as_os_str());
        unsafe {
            let done = ShellExecuteW(Some(hwnd), w!("edit"), &file, None, None, SW_SHOWNORMAL);
            // Above 32 means it started.
            if done.0 as usize <= 32 {
                let quoted = HSTRING::from(format!("\"{}\"", path.display()));
                ShellExecuteW(
                    Some(hwnd),
                    w!("open"),
                    w!("notepad.exe"),
                    &quoted,
                    None,
                    SW_SHOWNORMAL,
                );
            }
        }
    }

    /// Switches to a settings profile, adding it first when `create`, and applies its settings.
    fn switch_profile(&mut self, hwnd: HWND, name: &str, create: bool) -> std::io::Result<()> {
        let live = prefs();
        let new = if create {
            self.profiles.create(name, &live)?
        } else {
            self.profiles.switch(name, &live)?
        };
        self.apply_prefs(hwnd, new);
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

/// "Keep window above other windows".
fn set_topmost(hwnd: HWND, on: bool) {
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    unsafe { _ = SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, flags) };
}

/// The colours `p` asks for; "System" follows Windows' app mode.
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
    const USAGE: &str = "usage: myvlc [--sw] [--play-and-exit] [file or stream address]";
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

    ff::init()?;
    // Sets up Winsock and TLS once, instead of on every stream opened.
    unsafe { ff::ffi::avformat_network_init() };
    ff::util::log::set_level(ff::util::log::Level::Error);
    // The folder picker is a COM object.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let shared = Arc::new(Shared::new());
    _ = SHARED.set(shared.clone());
    // WASAPI takes seconds to open on some machines, so start now, while the window comes up.
    let audio = audio::Device::open(shared.clone());
    // No settings yet means myvlc hasn't run here before: the welcome shows.
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
        HwDevice::from_d3d11(&device)
    };
    let mut app = App {
        shared: shared.clone(),
        hw,
        audio,
        library: Library::load(&Library::default_dir()),
        library_changed: false,
        session: None,
        current: None,
        queue: Queue::default(),
        durations: HashMap::new(),
        queue_prober: thumbs::Prober::new(),
        watched: HashSet::new(),
        bookmarks: Bookmarks::load(&Library::default_dir()),
        recorded: Instant::now(),
        adjust: VideoAdjust::default(),
        zoom: 0,
        aspect: 0,
        crop: 0,
        embedded: None,
        external: Vec::new(),
        external_selected: None,
        sub_delay: 0.0,
        sub_speed: 1.0,
        sub_duration: 0.0,
        reverse: None,
        test_tone: Vec::new(),
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
        } else {
            app.play_file(path)
        };
        if !opened {
            return Err(format!("cannot play {path:?}").into());
        }
        UI.with_borrow_mut(|ui| ui.show(Page::Player));
    }

    let window_title = app.session.as_ref().map_or("myvlc".into(), |s| {
        s.path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    let size = app.session.as_ref().and_then(|s| s.video_size);
    let hwnd = create_window(&window_title, size.unwrap_or((1600, 900)))?;
    let maximized = restore_placement(hwnd, &settings);
    if settings.on_top {
        set_topmost(hwnd, true);
    }
    let lock = app.hw.as_ref().and_then(|h| DeviceLock::from_hw(h.d3d11()));
    let mut renderer = Renderer::new(hwnd, client_size(hwnd), device, lock)?;
    // Show the window the way the launcher asked (a "Run: Minimized" shortcut, or a script that
    // starts us minimized without focus). The swap chain already exists, so a window that starts
    // minimized still gets full-size buffers.
    let show = if maximized && !launched_minimized() {
        SW_SHOWMAXIMIZED
    } else {
        SW_SHOWDEFAULT
    };
    unsafe { _ = ShowWindow(hwnd, show) };
    unsafe { DragAcceptFiles(hwnd, true) };
    // libdav1d keeps every core busy in bursts, and at normal priority this render loop then woke
    // up 40 ms late and skipped frames. VLC runs its video output thread above normal for the
    // same reason. Only this thread is raised; Windows threads don't inherit priority.
    unsafe { _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) };

    // Started on a file, it plays; the welcome waits for a launch without one.
    if first_run && path.is_none() {
        let folder = videos_folder(&app.library);
        UI.with_borrow_mut(|ui| ui.open_onboarding(settings.clone(), folder));
    }

    let thumbnailer = thumbs::Thumbnailer::new(2, THUMB_SIZE.0, THUMB_SIZE.1);
    // Tags for Songs, Albums, Artists and Genres come from probing every music file once; the
    // index on disk covers the files that haven't changed since.
    let prober = thumbs::Prober::new();
    let mut tracks: HashMap<PathBuf, (tags::Stamp, Track)> = HashMap::new();
    if !play_and_exit {
        let mut saved_tags = tags::load(&Library::default_dir());
        let mut changed = Vec::new();
        // ponytail: one metadata read per music file on the UI thread at startup, like the
        // folder walk before it; move both to a thread if a large library delays the window.
        for path in app.library.music_files() {
            match saved_tags.remove(&path) {
                Some((stamp, track)) if tags::stamp(&path) == Some(stamp) => {
                    tracks.insert(path, (stamp, track));
                }
                _ => changed.push(path),
            }
        }
        if !tracks.is_empty() {
            app.library
                .set_tracks(tracks.values().map(|(_, t)| t.clone()).collect());
        }
        prober.request_all(changed);
    }
    let mut tracks_changed = false;
    let mut tags_unsaved = false;
    let mut tracks_set = Instant::now();
    let mut saved = Instant::now();

    let (mut shown, mut dropped) = (0u64, 0u64);
    let mut last_position = 0.0;
    // Whether Windows is asked to keep the screen on.
    let mut awake = false;
    // One pass of the render loop; false ends playback (--play-and-exit). See `run_frames`.
    let mut frame = || -> Result<bool, Box<dyn Error>> {
        // Nothing to show in the mini player once playback ends.
        if app.session.is_none() && is_mini() {
            MINI_REQUEST.set(Some(Some(Page::Library)));
        }
        match MINI_REQUEST.take() {
            Some(None) if app.session.is_some() => enter_mini(hwnd),
            Some(Some(page)) => exit_mini(hwnd, page),
            _ => {}
        }
        if is_mini() {
            fit_mini(hwnd, UI.with_borrow(|ui| ui.queue.open));
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
        for action in PANEL_ACTIONS.with_borrow_mut(std::mem::take) {
            app.apply(hwnd, action);
            app.sync_prefs();
            // Video adjustments change the picture without a new frame.
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        for action in PREFS_ACTIONS.with_borrow_mut(std::mem::take) {
            match action {
                PrefsAction::Changed(p) => app.apply_prefs(hwnd, *p),
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
            toggle_fullscreen(hwnd);
        }
        for action in ONBOARDING_ACTIONS.with_borrow_mut(std::mem::take) {
            match action {
                ui::onboarding::OnboardingAction::PickFolder => {
                    if let Some(dir) = pick_folder(hwnd) {
                        app.library.add_root(Section::Videos, dir);
                        app.library_changed = true;
                        app.save();
                        let folder = videos_folder(&app.library);
                        UI.with_borrow_mut(|ui| {
                            ui.browse.stale = true;
                            ui.onboarding_folder(folder);
                        });
                    }
                }
            }
        }
        for action in DIALOG_ACTIONS.with_borrow_mut(std::mem::take) {
            apply_dialog(&mut app, hwnd, action);
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
            app.apply_queue(hwnd, action);
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
            handle(&mut app, hwnd, hit);
        }
        for (hit, x, y) in MENU_REQUESTS.with_borrow_mut(std::mem::take) {
            open_menu(&mut app, hit, (x, y));
        }
        for action in MENU_ACTIONS.with_borrow_mut(std::mem::take) {
            menu_action(&mut app, hwnd, action);
            app.sync_prefs();
        }
        DURATION.store(
            app.session.as_ref().map_or(0.0, |s| s.duration).to_bits(),
            Relaxed,
        );

        for (path, thumb) in thumbnailer.take_ready() {
            renderer.add_thumbnail(
                path,
                thumb.as_ref().map(|t| (t.width, t.height, &t.bgra[..])),
            );
            UI.with_borrow_mut(|ui| ui.dirty = true);
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
            let (w, h) = client_size(hwnd);
            renderer.resize(w, h)?;
        }

        app.step_reverse();
        let serial = shared.serial.load(Relaxed);
        let fresh = app.current.as_ref().is_none_or(|c| c.serial != serial);
        let paused = shared.paused.load(Relaxed);
        // "Stop the screensaver during playback": only while a video plays.
        let want_awake = PREFS.with_borrow(|p| p.keep_awake)
            && !paused
            && app.session.as_ref().is_some_and(|s| s.video_size.is_some());
        if want_awake != awake {
            awake = want_awake;
            let flags = if awake {
                ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED
            } else {
                ES_CONTINUOUS
            };
            unsafe { SetThreadExecutionState(flags) };
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

        // Lay out for the current size, page and bar visibility; a change means a repaint.
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32;
        let (w, h) = client_size(hwnd);
        let (w, h) = (w as f32 * 96.0 / dpi, h as f32 * 96.0 / dpi);
        let fullscreen = WINDOWED.lock().unwrap().is_some();
        let has_video = app.session.as_ref().is_some_and(|s| s.video_size.is_some());
        let card = app.session.is_some();
        let panel_data = app.panel_data();
        let queue_data = UI.with_borrow(|ui| ui.queue.open).then(|| app.queue_data());
        let needs_paint = UI.with_borrow_mut(|ui| {
            // Nothing to show on the player page once playback ends or fails, unless its bars
            // are being customized. A stream connecting, or the notice saying why it couldn't,
            // keeps it up.
            let waiting = app.opening.is_some() || app.notice.is_some();
            if app.session.is_none() && !waiting && ui.customize.is_none() {
                ui.show(Page::Library);
            }
            ui.menu_tick();
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
                        let mut layout = Layout::mini(w, h, ui.queue.open);
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
            thread::sleep(Duration::from_millis(2));
        }
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
                    unsafe { _ = DestroyWindow(hwnd) };
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
    unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
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

/// The timer that runs passes while Windows' own move and size loop has the thread.
const MODAL_TIMER: usize = 1;
/// Passes in a row that may fail, about two seconds' worth, before myvlc gives up. One failed
/// pass only costs a frame.
const MAX_FAILED_PASSES: u32 = 120;

/// Runs `frame` until the window closes, `quit` is set or the pass returns false.
///
/// Dragging the window or its edge makes Windows run a message loop of its own until the mouse
/// lets go, so this loop stops and the video froze. While that loop runs, a timer has the window
/// procedure run passes instead (`modal_frame`).
fn run_frames(frame: &mut Frame<'_>, quit: &AtomicBool) -> Result<(), Box<dyn Error>> {
    // The lifetime is erased for the thread-local; the pointer is gone before `frame` is, and
    // every pass goes through it, one at a time.
    let ptr: *mut Frame<'static> = unsafe { std::mem::transmute(frame as *mut Frame<'_>) };
    FRAME.set(Some(ptr));
    let mut failed = 0;
    let result = loop {
        if !pump_messages() || quit.load(Relaxed) {
            break Ok(());
        }
        match call_frame(ptr) {
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
    let result = unsafe { (*ptr)() };
    IN_FRAME.set(false);
    result
}

/// A pass from the window procedure's timer during a window drag.
fn modal_frame() {
    if let Some(ptr) = FRAME.get()
        && let Err(e) = call_frame(ptr)
    {
        eprintln!("render: {e}");
    }
}

fn save_tags(tracks: &HashMap<PathBuf, (tags::Stamp, Track)>) {
    if let Err(e) = tags::save(&Library::default_dir(), tracks) {
        eprintln!("tags: cannot save ({e})");
    }
}

/// A click that needs the library or opens files.
fn handle(app: &mut App, hwnd: HWND, hit: Hit) {
    let go = |place: Place| UI.with_borrow_mut(|ui| ui.browse.go(place));
    match hit {
        Hit::Nav(section) => go(section.place()),
        Hit::AddItem => {
            let section = UI.with_borrow(|ui| ui.browse.place.section());
            add_folder(app, hwnd, section);
        }
        Hit::AddHere => match UI.with_borrow(|ui| ui.browse.place.clone()) {
            Place::Playlists => create_playlist(app),
            Place::Playlist(name) => add_files_to_playlist(app, hwnd, &name),
            place => add_folder(app, hwnd, place.section()),
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
                    if let Place::Folder(_, dir) = &place {
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
                    library::Action::AddFolder(section) => add_folder(app, hwnd, section),
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
        Hit::Tool(item) => use_tool(app, hwnd, item),
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
fn menu_action(app: &mut App, hwnd: HWND, action: MenuAction) {
    let show_player = || UI.with_borrow_mut(|ui| ui.show(Page::Player));
    match action {
        MenuAction::OpenFiles => {
            let files = pick_media_files(hwnd);
            if let Some(first) = files.first().cloned()
                && app.play(&first, files)
            {
                show_player();
            }
        }
        MenuAction::OpenFolder => {
            if let Some(dir) = pick_folder(hwnd) {
                let queue = app.library.media_in(&Place::Folder(Section::Videos, dir));
                if let Some(first) = queue.first().cloned()
                    && app.play(&first, queue)
                {
                    show_player();
                }
            }
        }
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
        MenuAction::Quit => unsafe { _ = DestroyWindow(hwnd) },
        MenuAction::TogglePause => app.shared.toggle_pause(),
        MenuAction::Stop => app.stop(),
        MenuAction::Previous => handle(app, hwnd, Hit::Previous),
        MenuAction::Next => handle(app, hwnd, Hit::Next),
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
            if let Err(e) = app.switch_profile(hwnd, &name, false) {
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
        MenuAction::OpenDisc(folder) => {
            let Some(folder) = folder.or_else(|| pick_folder(hwnd)) else {
                return;
            };
            open_disc(app, &folder);
        }
        MenuAction::OpenDiscImage => {
            let images = [("Blu-ray images", "*.iso".to_owned())];
            if let Some(image) = pick_files(hwnd, &images, false).pop() {
                open_disc(app, &image);
            }
        }
        MenuAction::OpenCapture(address, name) => {
            let path = PathBuf::from(address);
            app.stream_titles.insert(path.clone(), name);
            if app.play(&path, vec![path.clone()]) {
                show_player();
            }
        }
        MenuAction::Panel(a) => app.apply(hwnd, a),
        MenuAction::Queue(a) => {
            app.apply_queue(hwnd, a);
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
        MenuAction::ShowInExplorer(path) => {
            use std::os::windows::process::CommandExt;
            // explorer.exe wants the quotes inside its one argument, so it goes in raw.
            let select = format!("/select,\"{}\"", path.display());
            if let Err(e) = std::process::Command::new("explorer.exe")
                .raw_arg(select)
                .spawn()
            {
                eprintln!("explorer: {e}");
            }
        }
        MenuAction::GoToSource(path) => {
            if let Some(dir) = path.parent() {
                let place = Place::Folder(section_of(&path), dir.to_owned());
                UI.with_borrow_mut(|ui| ui.browse.go(place));
            }
        }
        MenuAction::AddSubtitleTo(path) => add_subtitle_to(app, hwnd, &path),
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
            add_audio_file(app, hwnd);
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
        MenuAction::AddFolder(section) => add_folder(app, hwnd, section),
        MenuAction::Resync => UI.with_borrow_mut(|ui| ui.browse.stale = true),
        MenuAction::PlayPlaylist(name) => play_playlist(app, &name, false),
        MenuAction::ShufflePlaylist(name) => play_playlist(app, &name, true),
        MenuAction::ExportPlaylist(name) => export_playlist(app, hwnd, &name),
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

/// Picks an audio file and plays its sound with the open file, as a track of its own.
fn add_audio_file(app: &mut App, hwnd: HWND) {
    let filters = [
        ("Audio files", file_spec(library::AUDIO_EXTS)),
        ("Video files", file_spec(library::VIDEO_EXTS)),
        ("All files", "*.*".to_owned()),
    ];
    let Some(path) = pick_files(hwnd, &filters, false).pop() else {
        return;
    };
    let Some(session) = app.session.as_mut() else {
        return;
    };
    match session.add_audio_file(&path) {
        Ok(stream) => {
            let audio = session.tracks(TrackKind::Audio);
            let i = audio.iter().position(|t| t.stream == stream);
            app.select_track(TrackTab::Audio, i);
        }
        Err(e) => eprintln!("audio: {e}"),
    }
}

/// Picks a subtitle file for `path` and shows it: on the open file, or on `path` once it plays.
fn add_subtitle_to(app: &mut App, hwnd: HWND, path: &Path) {
    let Some(subs) = pick_subtitle_file(hwnd) else {
        return;
    };
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

/// Writes a playlist to an M3U file the user picks, with absolute paths so it works anywhere.
fn export_playlist(app: &App, hwnd: HWND, name: &str) {
    let Some(file) = pick_save_file(hwnd, &format!("{name}.m3u")) else {
        return;
    };
    let mut text = String::from("#EXTM3U\n");
    for item in app.library.playlist_items(name) {
        text.push_str(&item.to_string_lossy());
        text.push('\n');
    }
    if let Err(e) = std::fs::write(&file, text) {
        eprintln!("playlist: cannot export to {}: {e}", file.display());
    }
}

fn add_folder(app: &mut App, hwnd: HWND, section: Section) {
    let section = match section {
        Section::Music => Section::Music,
        _ => Section::Videos,
    };
    if let Some(dir) = pick_folder(hwnd) {
        app.library.add_root(section, dir);
        app.library_changed = true;
        app.save();
        UI.with_borrow_mut(|ui| ui.browse.stale = true);
    }
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
fn apply_dialog(app: &mut App, hwnd: HWND, action: DialogAction) {
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
        DialogAction::CreateProfile(name) => match app.switch_profile(hwnd, &name, true) {
            Ok(()) => UI.with_borrow_mut(Ui::close_dialog),
            Err(e) => {
                let message = match e.kind() {
                    std::io::ErrorKind::AlreadyExists => e.to_string(),
                    _ => format!("Can't save the profile: {e}"),
                };
                UI.with_borrow_mut(|ui| ui.dialog_error(message));
            }
        },
        DialogAction::PickFiles => {
            let files = pick_media_files(hwnd);
            UI.with_borrow_mut(|ui| ui.dialog_add_files(files));
        }
        DialogAction::PickThumbnail => {
            let images = [(
                "Images",
                "*.png;*.jpg;*.jpeg;*.bmp;*.gif;*.webp;*.tif;*.tiff".to_owned(),
            )];
            if let Some(image) = pick_files(hwnd, &images, false).pop() {
                UI.with_borrow_mut(|ui| ui.dialog_set_thumbnail(Some(image)));
            }
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
        DialogAction::OpenLink(url) => {
            use windows::Win32::UI::Shell::ShellExecuteW;
            unsafe {
                ShellExecuteW(
                    Some(hwnd),
                    w!("open"),
                    &HSTRING::from(url),
                    None,
                    None,
                    SW_SHOWNORMAL,
                )
            };
        }
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

/// The header's "+" on a playlist page: picks files and appends them.
fn add_files_to_playlist(app: &mut App, hwnd: HWND, name: &str) {
    for file in pick_media_files(hwnd) {
        if let Err(e) = app.library.add_to_playlist(name, &file) {
            eprintln!("library: cannot add {file:?} to {name}: {e}");
        }
    }
    UI.with_borrow_mut(|ui| ui.browse.stale = true);
}

/// "*.a;*.b" for a file picker filter.
fn file_spec(exts: &[&str]) -> String {
    exts.iter()
        .map(|e| format!("*.{e}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// The system file picker for video and audio files, several at once.
fn pick_media_files(hwnd: HWND) -> Vec<PathBuf> {
    let (video, audio) = (
        file_spec(library::VIDEO_EXTS),
        file_spec(library::AUDIO_EXTS),
    );
    let filters = [
        ("Media files", format!("{video};{audio}")),
        ("Video files", video),
        ("Audio files", audio),
        ("All files", "*.*".to_owned()),
    ];
    pick_files(hwnd, &filters, true)
}

/// The system file picker with (name, "*.a;*.b") filters; empty when cancelled. Modal, like
/// the folder picker.
fn pick_files(hwnd: HWND, filters: &[(&str, String)], multiple: bool) -> Vec<PathBuf> {
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::FOS_ALLOWMULTISELECT;
    // The filter strings must outlive the dialog.
    let strings: Vec<(HSTRING, HSTRING)> = filters
        .iter()
        .map(|(name, spec)| (HSTRING::from(*name), HSTRING::from(spec.as_str())))
        .collect();
    let specs: Vec<COMDLG_FILTERSPEC> = strings
        .iter()
        .map(|(name, spec)| COMDLG_FILTERSPEC {
            pszName: windows::core::PCWSTR(name.as_ptr()),
            pszSpec: windows::core::PCWSTR(spec.as_ptr()),
        })
        .collect();
    let picked = || -> Option<Vec<PathBuf>> {
        unsafe {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            dialog.SetFileTypes(&specs).ok()?;
            let mut options = dialog.GetOptions().ok()? | FOS_FORCEFILESYSTEM;
            if multiple {
                options |= FOS_ALLOWMULTISELECT;
            }
            dialog.SetOptions(options).ok()?;
            dialog.Show(Some(hwnd)).ok()?; // fails when cancelled
            let items = dialog.GetResults().ok()?;
            let mut paths = Vec::new();
            for i in 0..items.GetCount().ok()? {
                let Ok(name) = items
                    .GetItemAt(i)
                    .and_then(|item| item.GetDisplayName(SIGDN_FILESYSPATH))
                else {
                    continue;
                };
                if let Ok(path) = name.to_string() {
                    paths.push(PathBuf::from(path));
                }
                CoTaskMemFree(Some(name.0 as _));
            }
            Some(paths)
        }
    };
    picked().unwrap_or_default()
}

/// "16:9" or "2.39:1" as a whole-number ratio; None for "Default".
fn ratio(label: Option<&str>) -> Option<(u32, u32)> {
    let (w, h) = label?.split_once(':')?;
    let (w, h): (f64, f64) = (w.parse().ok()?, h.parse().ok()?);
    Some(((w * 100.0).round() as u32, (h * 100.0).round() as u32))
}

/// The system file picker, for loading a subtitle file by hand.
fn pick_subtitle_file(hwnd: HWND) -> Option<PathBuf> {
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    let filters = [
        COMDLG_FILTERSPEC {
            pszName: w!("Subtitles"),
            pszSpec: w!("*.srt;*.ass;*.ssa;*.vtt;*.sub;*.idx;*.smi;*.sami;*.sup"),
        },
        COMDLG_FILTERSPEC {
            pszName: w!("All files"),
            pszSpec: w!("*.*"),
        },
    ];
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        dialog.SetFileTypes(&filters).ok()?;
        dialog
            .SetOptions(dialog.GetOptions().ok()? | FOS_FORCEFILESYSTEM)
            .ok()?;
        dialog.Show(Some(hwnd)).ok()?; // fails when cancelled
        let name = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as _));
        path
    }
}

/// The system Save As dialog for an M3U file, starting at `name`.
fn pick_save_file(hwnd: HWND, name: &str) -> Option<PathBuf> {
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    let filters = [COMDLG_FILTERSPEC {
        pszName: w!("M3U playlist"),
        pszSpec: w!("*.m3u"),
    }];
    unsafe {
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        dialog.SetFileTypes(&filters).ok()?;
        dialog.SetDefaultExtension(w!("m3u")).ok()?;
        dialog.SetFileName(&HSTRING::from(name)).ok()?;
        dialog
            .SetOptions(dialog.GetOptions().ok()? | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM)
            .ok()?;
        dialog.Show(Some(hwnd)).ok()?; // fails when cancelled
        let name = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as _));
        path
    }
}

/// The system folder picker. Modal: the window keeps repainting, playback keeps going.
fn pick_folder(hwnd: HWND) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        dialog
            .SetOptions(dialog.GetOptions().ok()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        dialog.Show(Some(hwnd)).ok()?; // fails when cancelled
        let name = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as _));
        path
    }
}

/// Creates the window hidden; `run` shows it once the renderer is ready. The window draws its own
/// title bar, so the system frame is removed in WM_NCCALCSIZE while DWM keeps the shadow,
/// rounded corners, snapping and minimize/maximize animations.
fn create_window(title: &str, (video_w, video_h): (u32, u32)) -> windows::core::Result<HWND> {
    unsafe {
        // Without this Windows bitmap-scales the window on high-DPI screens and blurs everything.
        _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: w!("myvlc"),
            ..Default::default()
        };
        RegisterClassExW(&class);

        // Open with the video at its own size, shrunk to fit 80% of the screen, plus the bars.
        let scale = GetDpiForSystem() as f64 / 96.0;
        let screen_w = f64::from(GetSystemMetrics(SM_CXSCREEN)) * 0.8;
        let screen_h = f64::from(GetSystemMetrics(SM_CYSCREEN)) * 0.8;
        let (chrome_w, chrome_h) = (20.0 * scale, 95.0 * scale);
        let fit = ((screen_w - chrome_w) / f64::from(video_w))
            .min((screen_h - chrome_h) / f64::from(video_h))
            .min(1.0);
        let width =
            ((f64::from(video_w) * fit + chrome_w) as i32).max((MIN_SIZE.0 * scale as f32) as i32);
        let height =
            ((f64::from(video_h) * fit + chrome_h) as i32).max((MIN_SIZE.1 * scale as f32) as i32);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("myvlc"),
            &HSTRING::from(format!("{title} - myvlc")),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let margins = MARGINS {
            cxLeftWidth: 0,
            cxRightWidth: 0,
            cyTopHeight: 1,
            cyBottomHeight: 0,
        };
        _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        let round = DWMWCP_ROUND;
        _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const round).cast(),
            4,
        );
        // Re-run WM_NCCALCSIZE now that the window procedure removes the frame.
        _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            // NOACTIVATE: activating here would steal focus from a minimized, unfocused launch.
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        Ok(hwnd)
    }
}

/// Puts the window where it was when myvlc last closed, if Settings remember it and that spot
/// is still on a screen. Returns whether it was maximized.
fn restore_placement(hwnd: HWND, p: &Prefs) -> bool {
    let Some(([x, y, w, h], maximized)) = p.window.filter(|_| p.remember_window) else {
        return false;
    };
    let rect = RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    if unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.is_invalid() {
        return false;
    }
    let placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        showCmd: SW_HIDE.0 as u32,
        rcNormalPosition: rect,
        ..Default::default()
    };
    unsafe { _ = SetWindowPlacement(hwnd, &placement) };
    maximized
}

/// Keeps the window's restored rect and maximized state for next time, as it closes.
fn remember_placement(hwnd: HWND) {
    // In fullscreen or the mini player, the placement to come back to is the one saved on the
    // way in.
    let saved = WINDOWED.lock().unwrap().as_ref().map(|(_, p)| *p);
    let saved = saved.or(*MINI.lock().unwrap());
    let placement = saved.or_else(|| {
        let mut p = WINDOWPLACEMENT {
            length: size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        unsafe { GetWindowPlacement(hwnd, &mut p) }.ok().map(|()| p)
    });
    if let Some(p) = placement {
        let r = p.rcNormalPosition;
        let maximized = p.showCmd == SW_SHOWMAXIMIZED.0 as u32
            || (p.showCmd == SW_SHOWMINIMIZED.0 as u32 && p.flags.contains(WPF_RESTORETOMAXIMIZED));
        let rect = [r.left, r.top, r.right - r.left, r.bottom - r.top];
        PREFS.with_borrow_mut(|prefs| prefs.window = Some((rect, maximized)));
    }
}

/// Whether the launcher asked for a minimized window (a "Run: Minimized" shortcut, or a script
/// that starts us without focus).
fn launched_minimized() -> bool {
    let mut info = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    unsafe { GetStartupInfoW(&mut info) };
    let minimized = [SW_MINIMIZE, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE];
    info.dwFlags.contains(STARTF_USESHOWWINDOW)
        && minimized.iter().any(|s| s.0 as u16 == info.wShowWindow)
}

/// Handles pending window messages. Returns false once the window is gone.
fn pump_messages() -> bool {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return false;
            }
            _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe { _ = GetClientRect(hwnd, &mut r) };
    ((r.right - r.left) as u32, (r.bottom - r.top) as u32)
}

/// Client-area pixels to the layout's DIPs.
fn to_dips(hwnd: HWND, x: i32, y: i32) -> (f32, f32) {
    let scale = 96.0 / unsafe { GetDpiForWindow(hwnd) }.max(96) as f32;
    (x as f32 * scale, y as f32 * scale)
}

fn mouse_position(lparam: LPARAM) -> (i32, i32) {
    (
        i32::from(lparam.0 as u16 as i16),
        i32::from((lparam.0 >> 16) as u16 as i16),
    )
}

fn is_fullscreen() -> bool {
    WINDOWED.lock().unwrap().is_some()
}

/// Thickness of the frame Windows hangs off-screen around a maximized window.
fn frame_thickness(hwnd: HWND) -> i32 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        GetSystemMetricsForDpi(SM_CXFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
    }
}

fn track_leave(hwnd: HWND, nonclient: bool) {
    let flags = if nonclient {
        TME_LEAVE | TME_NONCLIENT
    } else {
        TME_LEAVE
    };
    let mut track = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: flags,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    unsafe { _ = TrackMouseEvent(&mut track) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(shared) = SHARED.get() else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_NCCALCSIZE if wparam.0 != 0 => {
            // The client area covers the whole window. Maximized windows hang their frame off the
            // monitor, so pull the client back in by that much.
            if unsafe { IsZoomed(hwnd) }.as_bool() && !is_fullscreen() {
                let params = unsafe { &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS) };
                let inset = frame_thickness(hwnd);
                let r = &mut params.rgrc[0];
                (r.left, r.top, r.right, r.bottom) = (
                    r.left + inset,
                    r.top + inset,
                    r.right - inset,
                    r.bottom - inset,
                );
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => return LRESULT(nc_hit_test(hwnd, lparam) as isize),
        // Double-clicking the mini player's body would maximize it.
        WM_NCLBUTTONDBLCLK if is_mini() => return LRESULT(0),
        WM_GETMINMAXINFO => {
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
            // Narrower than this and the control bar's seek bar has no room left.
            let min = if is_mini() {
                (ui::mini::W, ui::mini::H)
            } else {
                MIN_SIZE
            };
            info.ptMinTrackSize.x = (min.0 * scale) as i32;
            info.ptMinTrackSize.y = (min.1 * scale) as i32;
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lparam.0 as *const RECT) };
            unsafe {
                _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
        }
        WM_SIZE => RESIZED.store(true, Relaxed),
        // The render loop can't run while Windows moves or sizes the window; a timer stands in.
        WM_ENTERSIZEMOVE => unsafe { _ = SetTimer(Some(hwnd), MODAL_TIMER, 1, None) },
        WM_EXITSIZEMOVE => unsafe { _ = KillTimer(Some(hwnd), MODAL_TIMER) },
        WM_TIMER if wparam.0 == MODAL_TIMER => modal_frame(),
        // Windows switched between light and dark apps.
        WM_SETTINGCHANGE if prefs().theme == 2 => {
            ui::theme::set(theme_of(&prefs()));
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        WM_MOUSEMOVE | WM_NCMOUSEMOVE => {
            let (mut x, mut y) = mouse_position(lparam);
            if msg == WM_NCMOUSEMOVE {
                // Non-client messages carry screen coordinates.
                let mut p = windows::Win32::Foundation::POINT { x, y };
                unsafe { _ = ScreenToClient(hwnd, &mut p) };
                (x, y) = (p.x, p.y);
            }
            track_leave(hwnd, msg == WM_NCMOUSEMOVE);
            let (x, y) = to_dips(hwnd, x, y);
            let action = UI.with_borrow_mut(|ui| ui.on_move(x, y));
            act(hwnd, shared, action);
        }
        WM_MOUSELEAVE | WM_NCMOUSELEAVE => UI.with_borrow_mut(Ui::on_leave),
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = mouse_position(lparam);
            let (x, y) = to_dips(hwnd, x, y);
            let on_video = UI.with_borrow(|ui| ui.layout.hit(x, y)) == Some(Hit::Video);
            let (click, double) = PREFS.with_borrow(|p| (p.click, p.double_click));
            if msg == WM_LBUTTONDBLCLK && on_video && double == 0 {
                // The first click of the two paused or played; put that back.
                if click == 1 {
                    shared.toggle_pause();
                }
                toggle_fullscreen(hwnd);
            } else {
                unsafe { SetCapture(hwnd) };
                let action = UI.with_borrow_mut(|ui| ui.on_down(x, y));
                act(hwnd, shared, action);
            }
        }
        WM_LBUTTONUP => {
            unsafe { _ = ReleaseCapture() };
            let (x, y) = mouse_position(lparam);
            let (x, y) = to_dips(hwnd, x, y);
            let action = UI.with_borrow_mut(|ui| ui.on_up(x, y));
            act(hwnd, shared, action);
        }
        WM_RBUTTONUP => {
            let (x, y) = mouse_position(lparam);
            let (x, y) = to_dips(hwnd, x, y);
            let action = UI.with_borrow_mut(|ui| ui.on_right(x, y));
            act(hwnd, shared, action);
        }
        WM_MOUSEWHEEL => {
            let notches = f32::from((wparam.0 >> 16) as u16 as i16) / WHEEL_DELTA as f32;
            // The wheel scrolls the popups, the library and the sidebar panel, and sets the volume.
            // Its position comes in screen coordinates.
            let (sx, sy) = mouse_position(lparam);
            let mut p = windows::Win32::Foundation::POINT { x: sx, y: sy };
            unsafe { _ = ScreenToClient(hwnd, &mut p) };
            let (x, y) = to_dips(hwnd, p.x, p.y);
            if UI.with_borrow(|ui| ui.dialog.is_some()) {
                UI.with_borrow_mut(|ui| ui.scroll_dialog(notches));
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
            } else {
                // Over the video: volume, seeking or nothing, per Settings.
                let (wheel, reverse, skip) =
                    PREFS.with_borrow(|p| (p.wheel, p.reverse_wheel, p.skip[1]));
                let notches = if reverse { -notches } else { notches };
                match wheel {
                    0 => act(
                        hwnd,
                        shared,
                        Some(Action::SetVolume(shared.volume() + 0.05 * notches)),
                    ),
                    1 => shared.seek_by(f64::from(notches) * f64::from(skip)),
                    _ => {}
                }
            }
        }
        WM_DROPFILES => {
            let drop = HDROP(wparam.0 as _);
            let mut name = [0u16; 32_768];
            // The New Playlist dialog takes every dropped file into its list.
            if UI.with_borrow(Ui::dialog_takes_files) {
                let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
                let files = (0..count)
                    .filter_map(|i| {
                        let len = unsafe { DragQueryFileW(drop, i, Some(&mut name)) } as usize;
                        (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&name[..len])))
                    })
                    .collect();
                unsafe { DragFinish(drop) };
                UI.with_borrow_mut(|ui| {
                    ui.dialog_add_files(files);
                    ui.dirty = true;
                });
                return LRESULT(0);
            }
            let len = unsafe { DragQueryFileW(drop, 0, Some(&mut name)) } as usize;
            unsafe { DragFinish(drop) };
            if len > 0 {
                *OPEN.lock().unwrap() = Some(PathBuf::from(String::from_utf16_lossy(&name[..len])));
            }
        }
        WM_SETCURSOR => {
            // Hide the pointer over the video while the bars are hidden, like VLC.
            let over_client = (lparam.0 & 0xFFFF) as u32 == HTCLIENT;
            let hidden = UI.with_borrow(|ui| {
                ui.page == Page::Player && !ui.layout.bars && ui.hover == Some(Hit::Video)
            });
            if over_client && hidden {
                unsafe { SetCursor(None) };
                return LRESULT(1);
            }
            if over_client && let Some(cursor) = UI.with_borrow(Ui::dialog_cursor) {
                unsafe { SetCursor(LoadCursorW(None, cursor).ok()) };
                return LRESULT(1);
            }
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        WM_CHAR => {
            // Typing goes to the library's search box while it has focus.
            let c = wparam.0 as u32;
            // An open dialog takes typing before the search box, then the Settings window.
            if UI.with_borrow_mut(|ui| ui.dialog_char(c) || ui.preferences_char(c)) {
                return LRESULT(0);
            }
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
        WM_KEYDOWN => {
            on_key(hwnd, shared, VIRTUAL_KEY(wparam.0 as u16));
            UI.with_borrow_mut(|ui| {
                ui.wake();
                ui.dirty = true;
            });
        }
        // Alt + Left/Right is the very short skip; other Alt keys (Alt+F4, the window menu)
        // stay Windows'.
        WM_SYSKEYDOWN
            if matches!(VIRTUAL_KEY(wparam.0 as u16), VK_LEFT | VK_RIGHT)
                && UI.with_borrow(|ui| {
                    ui.dialog.is_none() && ui.preferences.is_none() && !ui.menu.is_open()
                }) =>
        {
            let skip = f64::from(PREFS.with_borrow(|p| p.skip[0]));
            let back = VIRTUAL_KEY(wparam.0 as u16) == VK_LEFT;
            shared.seek_by(if back { -skip } else { skip });
            UI.with_borrow_mut(|ui| {
                ui.wake();
                ui.dirty = true;
            });
        }
        // The swap chain covers the whole client area; erasing first only causes flicker.
        WM_ERASEBKGND => return LRESULT(1),
        WM_DESTROY => {
            remember_placement(hwnd);
            shared.quit.store(true, Relaxed);
            unsafe { PostQuitMessage(0) };
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

/// Resize edges, the draggable title bar, and everything else as client area.
fn nc_hit_test(hwnd: HWND, lparam: LPARAM) -> u32 {
    let (sx, sy) = mouse_position(lparam);
    let mut p = windows::Win32::Foundation::POINT { x: sx, y: sy };
    unsafe { _ = ScreenToClient(hwnd, &mut p) };
    let (w, h) = client_size(hwnd);
    let (w, h) = (w as i32, h as i32);
    let resizable = !is_fullscreen() && !is_mini() && !unsafe { IsZoomed(hwnd) }.as_bool();
    if resizable {
        let edge = (6.0 * unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0) as i32;
        let (left, right, top, bottom) = (p.x < edge, p.x >= w - edge, p.y < edge, p.y >= h - edge);
        match (left, right, top, bottom) {
            (true, _, true, _) => return HTTOPLEFT,
            (_, true, true, _) => return HTTOPRIGHT,
            (true, _, _, true) => return HTBOTTOMLEFT,
            (_, true, _, true) => return HTBOTTOMRIGHT,
            (true, ..) => return HTLEFT,
            (_, true, ..) => return HTRIGHT,
            (_, _, true, _) => return HTTOP,
            (.., true) => return HTBOTTOM,
            _ => {}
        }
    }
    let (x, y) = to_dips(hwnd, p.x, p.y);
    let hit = UI.with_borrow(|ui| ui.layout.hit(x, y));
    if hit == Some(Hit::Caption) && !is_fullscreen() {
        HTCAPTION
    } else {
        HTCLIENT
    }
}

fn act(hwnd: HWND, shared: &Shared, action: Option<Action>) {
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
        Action::Click(Hit::Minimize) => unsafe { _ = ShowWindow(hwnd, SW_MINIMIZE) },
        Action::Click(Hit::Maximize) => {
            let zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
            unsafe { _ = ShowWindow(hwnd, if zoomed { SW_RESTORE } else { SW_MAXIMIZE }) };
        }
        Action::Click(Hit::Fullscreen) => toggle_fullscreen(hwnd),
        Action::Click(Hit::Close) => unsafe { _ = DestroyWindow(hwnd) },
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
fn use_tool(app: &mut App, hwnd: HWND, item: ui::ToolItem) {
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
        // The speed slider is on the settings popup's Playback tab.
        T::Speed => UI.with_borrow_mut(|ui| {
            let panel = ui::panels::Panel::Settings(ui::panels::SettingsTab::Playback);
            ui.panels.show(Some(panel), Some(Hit::Settings));
            ui.queue.open = false;
            ui.relayout = true;
        }),
        T::Quit => unsafe { _ = DestroyWindow(hwnd) },
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

/// Plays the DVD or Blu-ray in `path` (a drive, a folder, or a Blu-ray image).
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

/// Lists the capture devices into `devices` in the background; DirectShow takes a moment.
fn refresh_devices(devices: &Arc<Mutex<Vec<capture::Device>>>) {
    let devices = devices.clone();
    thread::spawn(move || {
        unsafe {
            _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
            )
        };
        *devices.lock().unwrap() = capture::devices();
    });
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
                let own = &windows::Win32::UI::Shell::FOLDERID_Pictures;
                (
                    snapshot::output_dir(key, own, &session.path),
                    p.snap_format == 1,
                )
            });
            match dir {
                Ok(dir) => {
                    snapshot::save(picture, dir, &stem, seconds, jpeg, app.notices.0.clone())
                }
                Err(e) => {
                    _ = app
                        .notices
                        .0
                        .send(format!("Can't save the snapshot ({})", e.message()))
                }
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

fn on_key(hwnd: HWND, shared: &Shared, key: VIRTUAL_KEY) {
    let shift = unsafe { GetKeyState(i32::from(VK_SHIFT.0)) } < 0;
    let ctrl = unsafe { GetKeyState(i32::from(VK_CONTROL.0)) } < 0;
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
        act(hwnd, shared, action);
        return;
    }
    if UI.with_borrow(|ui| ui.customize.is_some()) {
        let action = UI.with_borrow_mut(|ui| ui.customize_key(key));
        act(hwnd, shared, action);
        return;
    }
    if UI.with_borrow(|ui| ui.onboarding.is_some()) {
        let action = UI.with_borrow_mut(|ui| ui.onboarding_key(key));
        act(hwnd, shared, action);
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
            act(hwnd, shared, action);
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
    let menu = |action| act(hwnd, shared, Some(Action::Menu(action)));
    let shortcut = match key {
        VK_O if ctrl => Some(MenuAction::OpenFiles),
        VK_R if ctrl => Some(MenuAction::ConvertOpen),
        VK_D if ctrl => Some(MenuAction::OpenDisc(None)),
        VK_N if ctrl => Some(MenuAction::OpenNetwork),
        VK_F if ctrl => Some(MenuAction::OpenFolder),
        VK_P if ctrl => Some(MenuAction::Preferences(Category::Playback)),
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
        VK_F => toggle_fullscreen(hwnd),
        VK_ESCAPE if UI.with_borrow_mut(Ui::close_panel) => {}
        VK_ESCAPE if UI.with_borrow(|ui| ui.overlay && ui.page == Page::Player) => {
            UI.with_borrow_mut(|ui| ui.overlay = false)
        }
        VK_ESCAPE if is_fullscreen() => toggle_fullscreen(hwnd),
        VK_ESCAPE if page == Page::Mini => MINI_REQUEST.set(Some(Some(Page::Player))),
        // Esc leaves the player for the library, which keeps playing in the mini player.
        VK_ESCAPE => UI.with_borrow_mut(|ui| ui.show(Page::Library)),
        VK_Q => unsafe { _ = DestroyWindow(hwnd) },
        VK_F1 => UI.with_borrow_mut(|ui| ui.open_dialog(Dialog::Shortcuts)),
        // VLC's "Media Information": the file playing, else the file under the pointer.
        VK_I if unsafe { GetKeyState(i32::from(VK_CONTROL.0)) } < 0 => WANT_PROPERTIES.set(true),
        _ => {}
    }
}

fn is_mini() -> bool {
    MINI.lock().unwrap().is_some()
}

/// The mini player's size in pixels: the card, and its queue when that's open.
fn mini_size(hwnd: HWND, queue: bool) -> (i32, i32) {
    let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
    let h = ui::mini::H + if queue { ui::mini::QUEUE_H } else { 0.0 };
    (
        (ui::mini::W * scale).round() as i32,
        (h * scale).round() as i32,
    )
}

/// Shrinks the window to the mini player, above other windows: where it last was, or at the
/// bottom right of its monitor's work area.
fn enter_mini(hwnd: HWND) {
    if is_mini() {
        return;
    }
    if is_fullscreen() {
        toggle_fullscreen(hwnd);
    }
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetWindowPlacement(hwnd, &mut placement).is_err()
            || !GetMonitorInfoW(
                MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            )
            .as_bool()
        {
            return;
        }
        *MINI.lock().unwrap() = Some(placement);
        if IsZoomed(hwnd).as_bool() {
            _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }
    UI.with_borrow_mut(|ui| {
        ui.overlay = false;
        ui.show(Page::Mini);
    });
    let (w, h) = mini_size(hwnd, false);
    let work = monitor.rcWork;
    let margin = 16 * w / 391;
    let (l, b) = MINI_SPOT
        .get()
        .unwrap_or((work.right - margin - w, work.bottom - margin));
    let flags = SWP_NOOWNERZORDER | SWP_FRAMECHANGED;
    unsafe { _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), l, b - h, w, h, flags) };
}

/// Back to the full window, where it was, on `page`.
fn exit_mini(hwnd: HWND, page: Page) {
    let Some(placement) = MINI.lock().unwrap().take() else {
        return;
    };
    let mut r = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut r) }.is_ok() {
        MINI_SPOT.set(Some((r.left, r.bottom)));
    }
    UI.with_borrow_mut(|ui| ui.show(page));
    unsafe { _ = SetWindowPlacement(hwnd, &placement) };
    set_topmost(hwnd, prefs().on_top);
}

/// Grows the mini player up for its queue and back, keeping its bottom edge.
fn fit_mini(hwnd: HWND, queue: bool) {
    let (w, h) = mini_size(hwnd, queue);
    let mut r = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut r) }.is_err() || r.bottom - r.top == h {
        return;
    }
    let flags = SWP_NOZORDER | SWP_NOACTIVATE;
    unsafe { _ = SetWindowPos(hwnd, None, r.left, r.bottom - h, w, h, flags) };
}

/// Borderless fullscreen on the window's monitor, and back.
fn toggle_fullscreen(hwnd: HWND) {
    let mut windowed = WINDOWED.lock().unwrap();
    unsafe {
        match windowed.take() {
            None => {
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
                let mut placement = WINDOWPLACEMENT {
                    length: size_of::<WINDOWPLACEMENT>() as u32,
                    ..Default::default()
                };
                let mut monitor = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                let monitor_found = GetMonitorInfoW(
                    MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                    &mut monitor,
                );
                if GetWindowPlacement(hwnd, &mut placement).is_ok() && monitor_found.as_bool() {
                    // Set before resizing: WM_NCCALCSIZE checks it to skip the maximized inset.
                    *windowed = Some((style, placement));
                    drop(windowed);
                    SetWindowLongPtrW(hwnd, GWL_STYLE, style & !(WS_OVERLAPPEDWINDOW.0 as isize));
                    let r = monitor.rcMonitor;
                    let flags = SWP_NOOWNERZORDER | SWP_FRAMECHANGED;
                    _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOP),
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        flags,
                    );
                }
            }
            Some((style, placement)) => {
                drop(windowed);
                SetWindowLongPtrW(hwnd, GWL_STYLE, style);
                _ = SetWindowPlacement(hwnd, &placement);
                let flags =
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED;
                _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, flags);
            }
        }
    }
}

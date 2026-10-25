//! myvlc: a small video player built in the shape of VLC's pipeline, with the interface from
//! Juxtopposed's VLC redesign.
//!
//! demux thread -> video and audio decoder threads -> D3D11 renderer + Direct2D UI (this thread)
//!                                                 -> WASAPI callback (drives the clock)

mod audio;
mod bookmarks;
mod captions;
mod dsp;
mod library;
mod player;
mod queue;
mod probe;
mod render;
mod session;
mod subs;
mod thumbs;
mod ui;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use ffmpeg_next as ff;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, GetMonitorInfoW, GetStockObject, HBRUSH, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, ScreenToClient,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
};
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, FOS_FORCEFILESYSTEM,
    FOS_OVERWRITEPROMPT, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog, HDROP, IFileOpenDialog,
    IFileSaveDialog, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, w};

use bookmarks::Bookmarks;
use captions::CaptionStyle;
use dsp::{EQ_PRESETS, StereoMode};
use library::{Entry, Library, MediaKind, Place, Section, Track};
use player::{HwDevice, Shared, VideoFrame};
use queue::Queue;
use render::{DeviceLock, Flip, Renderer, VideoAdjust, Zoom};
use session::{Session, TrackKind};
use subs::Cues;
use ui::panels::{ASPECT, CROP, SettingsTab};
use ui::{
    Action, Dialog, DialogAction, Hit, Layout, MenuAction, Page, Panel, PanelAction, PanelData,
    QueueAction, QueueData, QueueItem, Scene, TrackTab, Ui,
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
    caption: CaptionStyle,
    devices: Vec<String>,
    /// Index into the panel's sleep list, and when a timed one runs out.
    sleep: usize,
    sleep_at: Option<Instant>,
    show_statistics: bool,
}

impl App {
    /// Plays `path` from where it was left, with `queue` for previous and next. Repeat and
    /// shuffle carry over from the last queue, as in VLC.
    fn play(&mut self, path: &Path, queue: Vec<PathBuf>) -> bool {
        let (repeat, shuffle) = (self.queue.repeat(), self.queue.shuffle());
        self.queue = Queue::new(queue, Some(path));
        self.queue.set_repeat(repeat);
        self.queue.set_shuffle(shuffle);
        self.queue_changed();
        self.open(path)
    }

    /// Opens `path` from where it was left, keeping the queue as it is.
    fn open(&mut self, path: &Path) -> bool {
        self.stop();
        let start = self.library.resume_position(path);
        match Session::open(path, start, &self.shared, self.hw.as_ref(), &self.audio) {
            Ok(s) => {
                if s.duration > 0.0 {
                    self.durations.insert(path.to_owned(), s.duration);
                }
                self.session = Some(s);
                self.start_subtitles();
                self.record();
                self.refresh_watched();
                true
            }
            Err(e) => {
                eprintln!("myvlc: {e}");
                false
            }
        }
    }

    /// Plays a file opened from outside the library, queued with the rest of its folder.
    fn play_file(&mut self, path: &Path) -> bool {
        let queue = path.parent().map_or_else(Vec::new, |dir| {
            self.library
                .media_in(&Place::Folder(section_of(path), dir.to_owned()))
        });
        self.play(path, queue)
    }

    fn stop(&mut self) {
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

    /// The file's default subtitles: an embedded default+forced track, else a matching subtitle
    /// file next to it, as VLC loads them automatically.
    fn start_subtitles(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let initial = session.take_initial_subtitles();
        self.external = subs::find_external(&session.path)
            .into_iter()
            .map(|p| (p, None))
            .collect();
        match initial {
            Some(stream) => self.decode_embedded(stream),
            None if !self.external.is_empty() => self.show_external(Some(0)),
            None => {}
        }
    }

    fn decode_embedded(&mut self, stream: session::SubtitleStream) {
        let decoder = subs::spawn_embedded(
            stream.parameters,
            stream.time_base,
            stream.packets,
            self.shared.clone(),
        );
        if let Some((old, _)) = self.embedded.replace(decoder) {
            _ = old.join();
        }
    }

    fn show_external(&mut self, i: Option<usize>) {
        self.external_selected = None;
        let Some((path, cues)) = i.and_then(|i| self.external.get_mut(i)) else {
            return;
        };
        if cues.is_none() {
            match subs::load_external(path) {
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
        let t = t - self.sub_delay;
        match self.external_selected {
            Some(i) => self.external[i]
                .1
                .as_ref()
                .map_or_else(Vec::new, |c| c.active(t)),
            None => self
                .embedded
                .as_ref()
                .map_or_else(Vec::new, |(_, c)| c.active(t)),
        }
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
            eq_bands: eq.bands,
            // The panel shows the preamp around 0; VLC's scale has unity at 12 dB.
            eq_preamp: eq.preamp - dsp::PREAMP_UNITY_DB,
            two_pass: eq.two_pass,
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
            PanelAction::SelectDevice(i) => {
                // The list starts with the system default, which also follows Windows' choice.
                let name = (i > 0).then(|| self.devices.get(i)).flatten();
                self.audio.switch_to(name.map(String::as_str));
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
        let Some(s) = &self.session else { return };
        let position = if s.finished() {
            s.duration
        } else {
            s.position().unwrap_or(0.0)
        };
        self.library.record_position(&s.path, position, s.duration);
        self.library_changed = true;
        self.recorded = Instant::now();
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
            .filter(|p| std::path::absolute(p).is_ok_and(|a| finished.contains(&a)))
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
}

fn section_of(path: &Path) -> Section {
    match library::media_kind(path) {
        Some(MediaKind::Audio) => Section::Music,
        _ => Section::Videos,
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    const USAGE: &str = "usage: myvlc [--sw] [--play-and-exit] [file]";
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
    ff::util::log::set_level(ff::util::log::Level::Error);
    // The folder picker is a COM object.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let shared = Arc::new(Shared::new());
    _ = SHARED.set(shared.clone());
    // WASAPI takes seconds to open on some machines, so start now, while the window comes up.
    let audio = audio::Device::open(shared.clone());

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
        caption: CaptionStyle::default(),
        devices: audio::output_devices(),
        sleep: 0,
        sleep_at: None,
        show_statistics: false,
    };
    if let Some(path) = &path {
        let opened = if play_and_exit {
            // Measurement runs play exactly this file from the start.
            app.session = Some(Session::open(
                path,
                None,
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
    let lock = app.hw.as_ref().and_then(|h| DeviceLock::from_hw(h.d3d11()));
    let mut renderer = Renderer::new(hwnd, client_size(hwnd), device, lock)?;
    // Show the window the way the launcher asked (a "Run: Minimized" shortcut, or a script that
    // starts us minimized without focus). The swap chain already exists, so a window that starts
    // minimized still gets full-size buffers.
    unsafe { _ = ShowWindow(hwnd, SW_SHOWDEFAULT) };
    unsafe { DragAcceptFiles(hwnd, true) };
    // libdav1d keeps every core busy in bursts, and at normal priority this render loop then woke
    // up 40 ms late and skipped frames. VLC runs its video output thread above normal for the
    // same reason. Only this thread is raised; Windows threads don't inherit priority.
    unsafe { _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) };

    let thumbnailer = thumbs::Thumbnailer::new(2, THUMB_SIZE.0, THUMB_SIZE.1);
    // Tags for Songs, Albums, Artists and Genres come from probing every music file once.
    let prober = thumbs::Prober::new();
    if !play_and_exit {
        prober.request_all(app.library.music_files());
    }
    let mut tracks: HashMap<PathBuf, Track> = HashMap::new();
    let mut tracks_changed = false;
    let mut tracks_set = Instant::now();
    let mut saved = Instant::now();

    let (mut shown, mut dropped) = (0u64, 0u64);
    let mut last_position = 0.0;
    while pump_messages() && !shared.quit.load(Relaxed) {
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
            // Video adjustments change the picture without a new frame.
            UI.with_borrow_mut(|ui| ui.dirty = true);
        }
        for action in DIALOG_ACTIONS.with_borrow_mut(std::mem::take) {
            apply_dialog(&mut app, hwnd, action);
        }
        if WANT_PROPERTIES.take() {
            show_properties(&app);
        }
        for action in QUEUE_ACTIONS.with_borrow_mut(std::mem::take) {
            app.apply_queue(hwnd, action);
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
            tracks.insert(
                path.clone(),
                Track {
                    path,
                    title: info.title,
                    artist: info.artist,
                    album: info.album,
                    genre: info.genre,
                    duration: info.duration,
                },
            );
            tracks_changed = true;
        }
        if tracks_changed && tracks_set.elapsed() > Duration::from_secs(1) {
            app.library.set_tracks(tracks.values().cloned().collect());
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
            saved = Instant::now();
        }

        let resized = RESIZED.swap(false, Relaxed);
        if resized {
            let (w, h) = client_size(hwnd);
            renderer.resize(w, h)?;
        }

        let serial = shared.serial.load(Relaxed);
        let fresh = app.current.as_ref().is_none_or(|c| c.serial != serial);
        let paused = shared.paused.load(Relaxed);
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
            // Nothing to show on the player page once playback ends or fails.
            if app.session.is_none() {
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
                    let bars = !has_video || ui.bars_visible(paused);
                    let changed = ui.relayout
                        || old.page != Page::Player
                        || bars != old.bars
                        || ui.overlay != old.has_overlay()
                        || ui.panel_stale(&panel_data)
                        || ui.queue_stale(queue_data.as_ref())
                        || (w, h) != (old.width, old.height);
                    changed.then(|| {
                        let mut layout = Layout::new(w, h, bars, fullscreen);
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
                        || card != (old.video.w() > 0.0);
                    changed.then(|| renderer.library_layout(w, h, &ui.browse, card))
                }
            };
            if let Some(mut layout) = layout {
                if let Some(d) = &ui.dialog {
                    renderer.add_dialog(&mut layout, d);
                }
                ui.layout = layout;
                ui.relayout = false;
                ui.dirty = true;
                ui.sync_menu();
            }
            if ui.settle_seek_preview() || ui.dialog_tick() {
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
        if next.is_some() || needs_paint || resized || tick {
            let new_frame = next.is_some();
            if let Some(f) = next {
                app.current = Some(f);
            }
            let session = app.session.as_ref();
            let cues = app.cues_at(session.and_then(Session::position).unwrap_or(last_position));
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
                    paused,
                    volume: shared.volume(),
                    muted: shared.muted.load(Relaxed),
                    title: session.map_or("", |s| &s.title),
                    folder: session.map_or("", |s| &s.folder),
                    file: &file,
                    art: session
                        .filter(|s| s.video_size.is_none())
                        .map(|s| s.path.as_path()),
                    fullscreen,
                    has_frame: app.current.is_some(),
                    browse: &ui.browse,
                    panel: Some(&panel_data),
                    queue: queue_data.as_ref(),
                    dialog: ui.dialog.as_ref(),
                    cues: &cues,
                    caption_style: &app.caption,
                    stats: stats.as_deref(),
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
            if !paused && app.session.as_ref().is_some_and(Session::finished) {
                if play_and_exit {
                    break;
                }
                // Like VLC's playlist: carry on with the next file, or stop at the end (or where
                // the sleep timer says).
                if app.sleep == SLEEP_END_OF_FILE || !app.advance() {
                    app.sleep = 0;
                    app.stop();
                }
                last_position = 0.0;
                UI.with_borrow_mut(|ui| {
                    ui.browse.stale = true;
                    ui.dirty = true;
                });
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    shared.quit.store(true, Relaxed);
    if !play_and_exit {
        app.stop();
        app.save();
    }
    drop(app);
    eprintln!("shown {shown} frames, dropped {dropped}");
    Ok(())
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
                        app.library.record_playlist_play(name);
                    }
                    let queue = app.library.media_in(&place);
                    if app.play(&path, queue) {
                        UI.with_borrow_mut(|ui| ui.show(Page::Player));
                    }
                }
                Entry::Action { action, .. } => match action {
                    library::Action::AddFolder(section) => add_folder(app, hwnd, section),
                    library::Action::CreatePlaylist => create_playlist(app),
                    // ponytail: network streams, podcasts and radio arrive with networking.
                    _ => {}
                },
            }
        }
        Hit::Card => UI.with_borrow_mut(|ui| ui.show(Page::Player)),
        Hit::Folder => {
            if let Some(dir) = app.session.as_ref().and_then(|s| s.path.parent()) {
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
            (Kind::Main, menu::main_menu(&recent, app.session.is_some()))
        }
        Hit::Sort => UI.with_borrow(|ui| {
            let b = &ui.browse;
            (Kind::Sort, menu::sort_menu(b.sort, b.has_artists()))
        }),
        Hit::Video => {
            let paused = app.shared.paused.load(Relaxed);
            (Kind::Player, menu::player_menu(&app.panel_data(), paused))
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
            let within = UI.with_borrow(|ui| match &ui.browse.place {
                Place::Playlist(name) => Some(name.clone()),
                _ => None,
            });
            match menu::entry_menu(&entry, &app.library.playlists(), &items, within.as_deref()) {
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
                app.library.record_playlist_play(name);
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
        MenuAction::Quit => unsafe { _ = DestroyWindow(hwnd) },
        MenuAction::TogglePause => app.shared.toggle_pause(),
        MenuAction::Stop => app.stop(),
        MenuAction::Previous => handle(app, hwnd, Hit::Previous),
        MenuAction::Next => handle(app, hwnd, Hit::Next),
        MenuAction::HideUi => UI.with_borrow_mut(Ui::hide_bars),
        MenuAction::Panel(a) => app.apply(hwnd, a),
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
            let watched = app.library.entries(&Place::Recent).iter().any(|e| {
                matches!(e, Entry::Media { path: p, finished: true, .. } if *p == path)
            });
            app.library.mark_watched(&path, !watched);
            app.library_changed = true;
            UI.with_borrow_mut(|ui| ui.browse.stale = true);
        }
        MenuAction::Favorite(path) => toggle_favorite(app, &path),
        // Deleting asks first; the dialog's Delete comes back as DialogAction::DeletePlaylist.
        MenuAction::DeletePlaylist(name) => open_dialog(app, Dialog::DeletePlaylist(name)),
        MenuAction::RemoveFromPlaylist(name, path) => {
            let index = app.library.playlist_items(&name).iter().position(|p| *p == path);
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
    app.library.record_playlist_play(name);
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

/// The system file picker for video and audio files, several at once.
fn pick_media_files(hwnd: HWND) -> Vec<PathBuf> {
    let spec = |exts: &[&str]| {
        exts.iter()
            .map(|e| format!("*.{e}"))
            .collect::<Vec<_>>()
            .join(";")
    };
    let (video, audio) = (spec(library::VIDEO_EXTS), spec(library::AUDIO_EXTS));
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
        WM_GETMINMAXINFO => {
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
            // Narrower than this and the control bar's seek bar has no room left.
            info.ptMinTrackSize.x = (MIN_SIZE.0 * scale) as i32;
            info.ptMinTrackSize.y = (MIN_SIZE.1 * scale) as i32;
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
            if msg == WM_LBUTTONDBLCLK && on_video {
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
                act(
                    hwnd,
                    shared,
                    Some(Action::SetVolume(shared.volume() + 0.05 * notches)),
                );
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
            // An open dialog takes typing before the search box.
            if UI.with_borrow_mut(|ui| ui.dialog_char(c)) {
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
        // The swap chain covers the whole client area; erasing first only causes flicker.
        WM_ERASEBKGND => return LRESULT(1),
        WM_DESTROY => {
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
    let resizable = !is_fullscreen() && !unsafe { IsZoomed(hwnd) }.as_bool();
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
        // A click on the video closes the panel.
        Action::Click(Hit::Video) => UI.with_borrow_mut(|ui| {
            if ui.overlay {
                ui.overlay = false;
                ui.browse.scroll = 0.0;
            }
        }),
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
    }
}

fn on_key(hwnd: HWND, shared: &Shared, key: VIRTUAL_KEY) {
    let shift = unsafe { GetKeyState(i32::from(VK_SHIFT.0)) } < 0;
    let ctrl = unsafe { GetKeyState(i32::from(VK_CONTROL.0)) } < 0;
    let step = if shift { 60.0 } else { 10.0 };
    let page = UI.with_borrow(|ui| ui.page);
    // An open dialog takes every key.
    if UI.with_borrow(|ui| ui.dialog.is_some()) {
        let ctrl = unsafe { GetKeyState(i32::from(VK_CONTROL.0)) } < 0;
        let action = UI.with_borrow_mut(|ui| ui.dialog_key(key, shift, ctrl));
        act(hwnd, shared, action);
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
        VK_F if ctrl => Some(MenuAction::OpenFolder),
        VK_P if ctrl => Some(MenuAction::Settings(SettingsTab::Playback)),
        VK_Q if ctrl => Some(MenuAction::Quit),
        VK_OEM_5 if ctrl && page == Page::Player => Some(MenuAction::HideUi),
        VK_F1 if shift => Some(MenuAction::ShowAbout),
        VK_S if page == Page::Player => Some(MenuAction::Stop),
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
        VK_UP => shared.set_volume(shared.volume() + 0.1),
        VK_DOWN => shared.set_volume(shared.volume() - 0.1),
        VK_M => shared.toggle_mute(),
        VK_F => toggle_fullscreen(hwnd),
        VK_ESCAPE if UI.with_borrow_mut(Ui::close_panel) => {}
        VK_ESCAPE if UI.with_borrow(|ui| ui.overlay && ui.page == Page::Player) => {
            UI.with_borrow_mut(|ui| ui.overlay = false)
        }
        VK_ESCAPE if is_fullscreen() => toggle_fullscreen(hwnd),
        // Esc leaves the player for the library, which keeps playing in the mini player.
        VK_ESCAPE => UI.with_borrow_mut(|ui| ui.show(Page::Library)),
        VK_Q => unsafe { _ = DestroyWindow(hwnd) },
        VK_F1 => UI.with_borrow_mut(|ui| ui.open_dialog(Dialog::Shortcuts)),
        // VLC's "Media Information": the file playing, else the file under the pointer.
        VK_I if unsafe { GetKeyState(i32::from(VK_CONTROL.0)) } < 0 => WANT_PROPERTIES.set(true),
        _ => {}
    }
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

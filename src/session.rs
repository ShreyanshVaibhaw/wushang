//! One open media file: its demux and decoder threads and what the interface shows about it.
//! The window, the GPU device and the audio device outlive sessions, so switching files only
//! stops these threads and starts new ones. Switching tracks does the same for one decoder.

use std::collections::HashMap;
use std::error::Error;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ffmpeg_next as ff;

use crate::audio;
use crate::net;
use crate::player::{self, FrameQueue, HwDevice, PacketQueue, Route, Routes, Shared};
use crate::record;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
}

/// One entry of a track menu.
#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub stream: usize,
    /// What the menu shows: the title tag, else the language, else "Track N".
    pub label: String,
    /// The language tag as the file has it ("eng", "ja"), None when missing or "und".
    pub language: Option<String>,
    pub codec: String,
    pub default: bool,
    /// A video track's picture size, when the file says.
    pub size: Option<(u32, u32)>,
}

/// A stream's picture size from its parameters, None for audio, subtitles and unknown sizes.
fn picture_size(parameters: &ff::codec::Parameters) -> Option<(u32, u32)> {
    // SAFETY: the parameters are valid while borrowed.
    let p = unsafe { &*parameters.as_ptr() };
    (p.width > 0 && p.height > 0).then_some((p.width as u32, p.height as u32))
}

/// A subtitle track's packets, for a subtitle decoder to read. The queue gets `Msg::Flush` on
/// seeks like the others and is closed when the track is switched away or the session ends.
// The subtitle decoder reads these fields; until it lands only the tests do.
pub struct SubtitleStream {
    pub parameters: ff::codec::Parameters,
    pub time_base: f64,
    pub packets: Arc<PacketQueue>,
}

/// What opening a decoder needs, copied out of the format context, which lives in the demux thread.
struct Codec {
    parameters: ff::codec::Parameters,
    time_base: ff::Rational,
    frame_rate: ff::Rational,
}

/// A running decoder thread and the queue that feeds it. Closing the queue stops only this thread.
struct Decoder {
    stream: usize,
    packets: Arc<PacketQueue>,
    thread: JoinHandle<()>,
}

/// An audio file played alongside the open one, like VLC's "Add audio track". Its streams
/// are numbered from `EXTERNAL` * (its place in `Session::externals` + 1), so they never meet
/// the open file's own stream numbers.
struct External {
    path: PathBuf,
    routes: Routes,
    demuxer: JoinHandle<()>,
}

const EXTERNAL: usize = 1 << 16;

/// How stream addresses open, from Settings' Network page.
#[derive(Clone, Copy, Debug)]
pub struct NetOptions {
    /// Seconds a connection may stall before it counts as dead.
    pub timeout: u32,
    /// Reconnect when the connection drops, as long as the server comes back within the timeout.
    pub reconnect: bool,
}

/// Opens the container at `path`: a file, or a stream address (`net::is_url_path`), which
/// connects and probes first and so can take seconds. Setting `cancel` makes FFmpeg give up at
/// once, then and on any read later that hangs on the network.
pub fn open_input(
    path: &Path,
    options: NetOptions,
    cancel: &Arc<AtomicBool>,
) -> Result<ff::format::context::Input, String> {
    if crate::capture::is_capture_path(path) {
        return crate::capture::open(path);
    }
    if crate::disc::is_disc_path(path) {
        return crate::disc::open(path, cancel);
    }
    if !net::is_url_path(path) {
        return open_file(path);
    }
    let url = path.to_string_lossy();
    let micros = (u64::from(options.timeout) * 1_000_000).to_string();
    let mut dict = ff::Dictionary::new();
    // Any protocol: a read that stalls this long fails. tcp, http, rtsp and udp also take it
    // as their connect and socket timeout.
    dict.set("rw_timeout", &micros);
    dict.set("timeout", &micros);
    dict.set("user_agent", concat!("wushang/", env!("CARGO_PKG_VERSION")));
    if options.reconnect {
        // http: reconnect on drops and errors, backing off up to the timeout; HLS retries a
        // segment instead of skipping it.
        for (key, value) in [
            ("reconnect", "1"),
            ("reconnect_streamed", "1"),
            ("reconnect_on_network_error", "1"),
            ("seg_max_retry", "5"),
        ] {
            dict.set(key, value);
        }
        dict.set("reconnect_delay_max", &options.timeout.to_string());
    }
    // A server can't hand back a concat script (the disc reader's demuxer) or name a device.
    dict.set("format_whitelist", stream_formats());
    let cancel = cancel.clone();
    ff::format::input_with_interrupt_and_dictionary(path, move || cancel.load(Relaxed), dict)
        .map_err(|e| format!("cannot open {}: {e}", net::redact(&url)))
}

/// Opens a local file. The `file:` prefix keeps FFmpeg from reading the path as another of its
/// protocols (`concat:`, `data:`), and the formats that open more connections or other files
/// (HLS and SDP playlists, RTSP, RTP, concat scripts) are left out, as are capture devices and
/// filter graphs, so a playlist posing as a video can't make wushang reach a server or read
/// anything but itself.
pub fn open_file(path: &Path) -> Result<ff::format::context::Input, String> {
    // ffmpeg-next panics on a path that isn't valid Unicode.
    let Some(p) = path.to_str() else {
        return Err(format!("{} has a name wushang can't open", path.display()));
    };
    let mut options = ff::Dictionary::new();
    options.set("format_whitelist", local_formats());
    ff::format::input_with_dictionary(&format!("file:{p}"), options)
        .map_err(|e| format!("cannot open {path:?}: {e}"))
}

/// Every demuxer this FFmpeg has except those `open_file` leaves out, comma-separated. The
/// Windows build's slim FFmpeg has none of the others; Homebrew's has them all.
fn local_formats() -> &'static str {
    static FORMATS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FORMATS.get_or_init(|| {
        demuxers_except(&[
            "hls",
            "sdp",
            "rtsp",
            "rtp",
            "concat",
            "avfoundation",
            "lavfi",
        ])
    })
}

/// The demuxers a stream address may use: playlists and RTSP, but not ones that read local
/// files or devices.
fn stream_formats() -> &'static str {
    static FORMATS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FORMATS.get_or_init(|| demuxers_except(&["concat", "avfoundation", "lavfi"]))
}

fn demuxers_except(excluded: &[&str]) -> String {
    let mut names = Vec::new();
    let mut opaque = std::ptr::null_mut();
    loop {
        let format = unsafe { ff::ffi::av_demuxer_iterate(&mut opaque) };
        if format.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*format).name) }.to_string_lossy();
        if !excluded.contains(&&*name) {
            names.push(name.into_owned());
        }
    }
    names.join(",")
}

/// Whether `path` is a live stream FFmpeg doesn't reconnect itself (it does http and HLS), so
/// the demuxer opens it again when it drops.
fn reopens(path: &Path) -> bool {
    let url = path.to_string_lossy().to_ascii_lowercase();
    [
        "rtsp://", "rtsps://", "rtp://", "udp://", "tcp://", "srt://", "rtmp://",
    ]
    .iter()
    .any(|scheme| url.starts_with(scheme))
}

/// Which tracks a file starts with, from Settings.
#[derive(Clone, Debug)]
pub struct TrackChoice {
    /// ISO 639 codes from `LANGUAGES`; empty keeps the file's own default track.
    pub audio_language: String,
    pub sub_language: String,
    /// Off, no subtitle track starts, not even a forced one.
    pub subtitles: bool,
}

impl Default for TrackChoice {
    fn default() -> Self {
        TrackChoice {
            audio_language: String::new(),
            sub_language: String::new(),
            subtitles: true,
        }
    }
}

pub struct Session {
    pub path: PathBuf,
    pub frames: Arc<FrameQueue>,
    /// The design titles the player after the media ("The_Wand_S01E04.mp4" shows as "The Wand
    /// S01E04") and puts the containing folder in the breadcrumb.
    pub title: String,
    pub folder: String,
    pub duration: f64,
    /// Size of the selected video track; None when video is off or the file has none.
    pub video_size: Option<(u32, u32)>,
    /// Start in seconds and title of each chapter, in file order.
    chapters: Vec<(f64, String)>,
    shared: Arc<Shared>,
    audio: Arc<audio::Device>,
    hw: Option<HwDevice>,
    /// Video, audio and subtitle tracks, in stream order.
    tracks: [Vec<TrackInfo>; 3],
    codecs: HashMap<usize, Codec>,
    routes: Routes,
    demuxer: Option<JoinHandle<()>>,
    externals: Vec<External>,
    video_track: Option<Decoder>,
    audio_track: Option<Decoder>,
    subtitle_track: Option<(usize, Arc<PacketQueue>)>,
    initial_subtitles: Option<SubtitleStream>,
    /// Fonts attached to the file (Matroska keeps a release's fonts this way), by file name.
    fonts: Vec<(String, Vec<u8>)>,
    /// Set on drop, so a network read that hangs gives up and the demuxer can be joined.
    cancel: Arc<AtomicBool>,
    /// Ask the demuxer to record or stream out, and hear back when that ends.
    record: record::Control,
    stream: record::Control,
}

impl Session {
    /// Opens the file at `path` and starts playing it, from `start` seconds if given.
    pub fn open(
        path: &Path,
        start: Option<f64>,
        choice: &TrackChoice,
        shared: &Arc<Shared>,
        hw: Option<&HwDevice>,
        audio: &Arc<audio::Device>,
    ) -> Result<Session, Box<dyn Error>> {
        let cancel = Arc::new(AtomicBool::new(false));
        let options = NetOptions {
            timeout: 15,
            reconnect: true,
        };
        let input = open_input(path, options, &cancel)?;
        Session::open_with(
            input, cancel, options, path, start, choice, shared, hw, audio,
        )
    }

    /// Starts playing `input`, opened from `path` by `open_input` with `cancel`.
    #[allow(clippy::too_many_arguments)]
    pub fn open_with(
        input: ff::format::context::Input,
        cancel: Arc<AtomicBool>,
        options: NetOptions,
        path: &Path,
        start: Option<f64>,
        choice: &TrackChoice,
        shared: &Arc<Shared>,
        hw: Option<&HwDevice>,
        audio: &Arc<audio::Device>,
    ) -> Result<Session, Box<dyn Error>> {
        shared.reset();
        // A DVD's VOBs carry no length or chapters; its IFO has both.
        let dvd = crate::disc::dvd_title(path);
        let duration = if let Some(title) = &dvd {
            title.length
        } else if input.duration() > 0 {
            input.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE)
        } else {
            0.0
        };

        let mut tracks: [Vec<TrackInfo>; 3] = Default::default();
        let mut titles: [Vec<Option<String>>; 3] = Default::default();
        let mut codecs = HashMap::new();
        let mut forced_subtitles = None;
        let mut fonts = Vec::new();
        for stream in input.streams() {
            let parameters = stream.parameters();
            if parameters.medium() == ff::media::Type::Attachment {
                fonts.extend(attached_font(&stream));
                continue;
            }
            let disposition = stream.disposition();
            let kind = match parameters.medium() {
                // Cover art is a one-picture video stream; it is not a track to watch.
                ff::media::Type::Video
                    if !disposition.contains(ff::format::stream::Disposition::ATTACHED_PIC) =>
                {
                    TrackKind::Video
                }
                ff::media::Type::Audio => TrackKind::Audio,
                ff::media::Type::Subtitle => TrackKind::Subtitle,
                _ => continue,
            };
            if parameters.id() == ff::codec::Id::None {
                continue;
            }
            let default = disposition.contains(ff::format::stream::Disposition::DEFAULT);
            let forced = disposition.contains(ff::format::stream::Disposition::FORCED);
            if kind == TrackKind::Subtitle && default && forced && forced_subtitles.is_none() {
                forced_subtitles = Some(stream.index());
            }
            let metadata = stream.metadata();
            let tag = |key: &CStr| meta(&metadata, key);
            let language = tag(c"language").filter(|l| !l.eq_ignore_ascii_case("und"));
            titles[kind as usize].push(tag(c"title"));
            tracks[kind as usize].push(TrackInfo {
                stream: stream.index(),
                label: String::new(),
                language,
                codec: parameters.id().name().to_owned(),
                default,
                size: picture_size(&parameters),
            });
            codecs.insert(
                stream.index(),
                Codec {
                    // A deep copy: the stream's own parameters belong to the format context.
                    parameters: parameters.clone(),
                    time_base: stream.time_base(),
                    frame_rate: stream.avg_frame_rate(),
                },
            );
        }
        for (list, titles) in tracks.iter_mut().zip(&titles) {
            label_tracks(list, titles);
        }
        let chapters = match &dvd {
            Some(title) => (title.chapters.iter().enumerate())
                .map(|(i, start)| (*start, format!("Chapter {}", i + 1)))
                .collect(),
            None => read_chapters(&input),
        };
        let best = |kind, medium| {
            let list: &Vec<TrackInfo> = &tracks[kind as usize];
            let best = input.streams().best(medium).map(|s| s.index());
            list.iter()
                .find(|t| t.default)
                .or_else(|| list.iter().find(|t| Some(t.stream) == best))
                .or(list.first())
                .map(|t| t.stream)
        };
        let video = best(TrackKind::Video, ff::media::Type::Video);
        let audio_stream = in_language(&tracks[TrackKind::Audio as usize], &choice.audio_language)
            .or_else(|| best(TrackKind::Audio, ff::media::Type::Audio));
        let subtitles = if !choice.subtitles {
            None
        } else if choice.sub_language.is_empty() {
            forced_subtitles
        } else {
            in_language(&tracks[TrackKind::Subtitle as usize], &choice.sub_language)
                .or(forced_subtitles)
        };

        let url = net::is_url_path(path).then(|| path.to_string_lossy().into_owned());
        let capture = crate::capture::is_capture_path(path);
        let disc = crate::disc::is_disc_path(path);
        let title = match &url {
            // The app names a device after the menu entry it came from.
            _ if capture => "Capture".to_owned(),
            _ if disc => crate::disc::title(path),
            // A stream's own title ("icy-name" for radio) beats its address.
            Some(url) => meta(&input.metadata(), c"icy-name")
                .or_else(|| meta(&input.metadata(), c"title"))
                .unwrap_or_else(|| net::url_name(url)),
            None => path
                .file_stem()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .replace('_', " "),
        };
        let folder = match &url {
            _ if capture || disc => String::new(),
            Some(url) => net::host_of(url).unwrap_or_default(),
            None => path
                .parent()
                .and_then(|p| p.file_name())
                .map_or_else(String::new, |f| f.to_string_lossy().into_owned()),
        };
        let mut session = Session {
            path: path.to_owned(),
            frames: Arc::new(FrameQueue::default()),
            title,
            folder,
            duration,
            video_size: None,
            chapters,
            shared: shared.clone(),
            audio: audio.clone(),
            hw: hw.cloned(),
            tracks,
            codecs,
            routes: Arc::new(Mutex::new(Vec::new())),
            demuxer: None,
            externals: Vec::new(),
            video_track: None,
            audio_track: None,
            subtitle_track: None,
            initial_subtitles: None,
            fonts,
            cancel,
            record: record::Control::default(),
            stream: record::Control::default(),
        };
        if let Some(stream) = video {
            let decoder = session.open_video(stream)?;
            session.start_video(stream, decoder);
        }
        if let Some(stream) = audio_stream {
            match session.open_audio(stream) {
                Ok(decoder) => session.start_audio(stream, decoder),
                Err(e) => eprintln!("audio: cannot decode ({e}), playing without sound"),
            }
        }
        if session.video_track.is_none() && session.audio_track.is_none() {
            return Err("no playable video or audio stream".into()); // drop stops the decoder
        }
        session.initial_subtitles = session.route_subtitles(subtitles);
        if let Some(t) = start.filter(|t| *t > 0.0) {
            shared.seek_to(t);
        }
        let reconnect = (options.reconnect && reopens(path)).then(|| {
            let (path, cancel) = (path.to_owned(), session.cancel.clone());
            player::Reconnect {
                open: Box::new(move || open_input(&path, options, &cancel)),
                give_up: std::time::Duration::from_secs(options.timeout.into()),
            }
        });
        // Plain files only: a stream or a disc would be read twice over.
        let reopen = (!capture && !disc && url.is_none()).then(|| {
            let path = path.to_owned();
            let open = move || ff::format::input(&path).map_err(|e| e.to_string());
            Arc::new(open) as player::Reopen
        });
        let copies = vec![session.record.clone(), session.stream.clone()];
        session.demuxer = Some(player::spawn_demuxer(
            input,
            session.routes.clone(),
            shared.clone(),
            player::Role::Main(copies, reopen),
            reconnect,
        ));
        Ok(session)
    }

    /// Video and audio both played to the end.
    pub fn finished(&self) -> bool {
        let shared = &self.shared;
        let video = self.video_size.is_none()
            || (shared.video_done.load(Relaxed) && self.frames.with(|q| q.is_empty()));
        let audio = !shared.audio_master.load(Relaxed) || shared.audio_done.load(Relaxed);
        video && audio
    }

    /// The selected video's picture size as shown: the stored width stretched by the pixels'
    /// shape, which anamorphic video (DVDs, broadcasts) stores apart.
    pub fn display_size(&self) -> Option<(u32, u32)> {
        let (w, h) = self.video_size?;
        let stream = self.video_track.as_ref()?.stream;
        // SAFETY: the parameters are valid while the session holds them.
        let sar = unsafe { (*self.codecs[&stream].parameters.as_ptr()).sample_aspect_ratio };
        if sar.num <= 0 || sar.den <= 0 {
            return Some((w, h));
        }
        // Real pixel shapes stay well inside 1:10 to 10:1. A crafted one would make the subtitle
        // canvas, which this sizes, billions of pixels wide.
        let sar = (f64::from(sar.num) / f64::from(sar.den)).clamp(0.1, 10.0);
        Some(((f64::from(w) * sar).round() as u32, h))
    }

    /// Where playback is in the media. The clock runs ahead by the audio delay (that is how the
    /// delay shifts the picture), so take it back off.
    pub fn position(&self) -> Option<f64> {
        let delay = self.audio.effects().delay();
        self.shared.clock.get().map(|t| t - delay)
    }

    fn open_video(&self, stream: usize) -> Result<ff::decoder::Video, String> {
        let codec = &self.codecs[&stream];
        player::open_video_decoder(codec.parameters.clone(), codec.time_base, self.hw.as_ref())
            .map_err(|e| format!("cannot decode video stream {stream}: {e}"))
    }

    fn open_audio(&self, stream: usize) -> Result<ff::decoder::Audio, String> {
        let codec = &self.codecs[&stream];
        player::open_audio_decoder(codec.parameters.clone(), codec.time_base)
            .map_err(|e| format!("cannot decode audio stream {stream}: {e}"))
    }

    fn start_video(&mut self, stream: usize, decoder: ff::decoder::Video) {
        let codec = &self.codecs[&stream];
        let fps = codec.frame_rate;
        let frame_duration = if fps.numerator() > 0 {
            1.0 / f64::from(fps)
        } else {
            1.0 / 30.0
        };
        // libavcodec may pick an external decoder (libdav1d for AV1) that ignores the GPU, so
        // name the implementation; the decoder thread reports where frames really come from.
        let implementation = decoder.codec().map_or("?".into(), |c| c.name().to_owned());
        eprintln!(
            "video: {} {}x{}, decoder {implementation}",
            codec.parameters.id().name(),
            decoder.width(),
            decoder.height()
        );
        self.video_size = Some((decoder.width(), decoder.height()));
        self.shared.video_done.store(false, Relaxed);
        let packets = Arc::new(PacketQueue::default());
        self.route(stream, &packets, false);
        let thread = player::spawn_video_decoder(
            decoder,
            f64::from(codec.time_base),
            frame_duration,
            packets.clone(),
            self.frames.clone(),
            self.shared.clone(),
        );
        self.video_track = Some(Decoder {
            stream,
            packets,
            thread,
        });
    }

    fn start_audio(&mut self, stream: usize, decoder: ff::decoder::Audio) {
        let codec = &self.codecs[&stream];
        eprintln!("audio: {}", codec.parameters.id().name());
        self.shared.audio_done.store(false, Relaxed);
        self.shared.audio_master.store(true, Relaxed);
        let packets = Arc::new(PacketQueue::default());
        self.route(stream, &packets, false);
        let thread = audio::spawn_decoder(
            decoder,
            f64::from(codec.time_base),
            packets.clone(),
            self.shared.clone(),
            self.audio.clone(),
        );
        self.audio_track = Some(Decoder {
            stream,
            packets,
            thread,
        });
    }

    fn stop_video(&mut self) {
        if let Some(d) = self.video_track.take() {
            self.unroute(d.stream);
            d.packets.close();
            _ = d.thread.join();
        }
        // Frames of the old track hold its decoder's surfaces; let them go.
        self.frames.with(|q| q.clear());
        self.video_size = None;
    }

    fn stop_audio(&mut self) {
        if let Some(d) = self.audio_track.take() {
            self.unroute(d.stream);
            d.packets.close();
            // While paused the device drains nothing, so the decoder may be waiting for room in a
            // full ring; emptying it lets the decoder reach its exit check.
            self.audio.clear();
            _ = d.thread.join();
            // Whatever it queued on its way out belongs to the old track.
            self.audio.clear();
        }
        // Without audio the clock runs on from its last value, and the video sets it after a seek.
        self.shared.audio_master.store(false, Relaxed);
    }

    /// Routes `stream`'s packets to a fresh bounded queue, closing the previous subtitle queue.
    fn route_subtitles(&mut self, stream: Option<usize>) -> Option<SubtitleStream> {
        if let Some((old, packets)) = self.subtitle_track.take() {
            self.unroute(old);
            packets.close();
        }
        let stream = stream?;
        let packets = Arc::new(PacketQueue::bounded(player::SUBTITLE_QUEUE_LEN));
        self.route(stream, &packets, true);
        self.subtitle_track = Some((stream, packets.clone()));
        let codec = &self.codecs[&stream];
        Some(SubtitleStream {
            parameters: codec.parameters.clone(),
            time_base: f64::from(codec.time_base),
            packets,
        })
    }

    fn route(&self, stream: usize, queue: &Arc<PacketQueue>, sparse: bool) {
        let (routes, stream) = self.routes_for(stream);
        routes.lock().unwrap().push(Route {
            stream,
            queue: queue.clone(),
            sparse,
        });
    }

    fn unroute(&self, stream: usize) {
        let (routes, stream) = self.routes_for(stream);
        routes.lock().unwrap().retain(|r| r.stream != stream);
    }

    /// The demuxer routes carrying `stream`, and its number in that demuxer's file.
    fn routes_for(&self, stream: usize) -> (&Routes, usize) {
        match (stream / EXTERNAL).checked_sub(1) {
            Some(n) => (&self.externals[n].routes, stream % EXTERNAL),
            None => (&self.routes, stream),
        }
    }

    /// Adds the audio tracks of the file at `path` to the audio track list, played in sync
    /// with the open file from its own demuxer. Returns the first new track's stream.
    pub fn add_audio_file(&mut self, path: &Path) -> Result<usize, String> {
        // Added before: its first track. Every added file has at least one.
        if let Some(n) = self.externals.iter().position(|e| e.path == path) {
            let audio = self.tracks(TrackKind::Audio).iter();
            let first = audio.map(|t| t.stream).find(|s| s / EXTERNAL == n + 1);
            return first.ok_or_else(|| format!("{path:?} has no audio track"));
        }
        let base = EXTERNAL * (self.externals.len() + 1);
        let input = open_file(path)?;
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut added = Vec::new();
        let mut titles = Vec::new();
        for stream in input.streams() {
            let parameters = stream.parameters();
            if parameters.medium() != ff::media::Type::Audio
                || parameters.id() == ff::codec::Id::None
            {
                continue;
            }
            let metadata = stream.metadata();
            let tag = |key: &CStr| meta(&metadata, key);
            titles.push(tag(c"title"));
            added.push(TrackInfo {
                stream: base + stream.index(),
                label: String::new(),
                language: tag(c"language").filter(|l| !l.eq_ignore_ascii_case("und")),
                codec: parameters.id().name().to_owned(),
                default: false,
                size: picture_size(&parameters),
            });
            self.codecs.insert(
                base + stream.index(),
                Codec {
                    parameters: parameters.clone(),
                    time_base: stream.time_base(),
                    frame_rate: stream.avg_frame_rate(),
                },
            );
        }
        let Some(first) = added.first().map(|t| t.stream) else {
            return Err(format!("{name} has no audio track"));
        };
        // One track goes by the file's name; several also say which one they are.
        label_tracks(&mut added, &titles);
        let several = added.len() > 1;
        for t in &mut added {
            t.label = if several {
                format!("{name} - {}", t.label)
            } else {
                name.clone()
            };
        }
        self.tracks[TrackKind::Audio as usize].extend(added);
        let routes: Routes = Arc::new(Mutex::new(Vec::new()));
        let demuxer = player::spawn_demuxer(
            input,
            routes.clone(),
            self.shared.clone(),
            player::Role::Follower,
            None,
        );
        self.externals.push(External {
            path: path.to_owned(),
            routes,
            demuxer,
        });
        Ok(first)
    }
}

impl Session {
    pub fn tracks(&self, kind: TrackKind) -> &[TrackInfo] {
        &self.tracks[kind as usize]
    }

    /// The selected track's stream index; None when that kind is off.
    pub fn selected(&self, kind: TrackKind) -> Option<usize> {
        match kind {
            TrackKind::Video => self.video_track.as_ref().map(|d| d.stream),
            TrackKind::Audio => self.audio_track.as_ref().map(|d| d.stream),
            TrackKind::Subtitle => self.subtitle_track.as_ref().map(|(s, _)| *s),
        }
    }

    /// Switches to the track playing `stream`, or turns that kind off with None, while the rest
    /// of playback carries on. A new audio or video track starts with a seek to the current
    /// position, as in VLC, so it starts in sync. Selecting subtitles returns their packets for
    /// a subtitle decoder; video and audio return None.
    pub fn select(
        &mut self,
        kind: TrackKind,
        stream: Option<usize>,
    ) -> Result<Option<SubtitleStream>, String> {
        if let Some(s) = stream
            && !self.tracks(kind).iter().any(|t| t.stream == s)
        {
            return Err(format!("stream {s} is not a {kind:?} track"));
        }
        if kind == TrackKind::Subtitle {
            return Ok(self.route_subtitles(stream));
        }
        if stream == self.selected(kind) {
            return Ok(None);
        }
        let other = match kind {
            TrackKind::Video => &self.audio_track,
            _ => &self.video_track,
        };
        if stream.is_none() && other.is_none() {
            return Err("cannot turn off both video and audio".into());
        }
        // Open the new decoder before stopping the old one, so a failure keeps the current track.
        match (kind, stream) {
            (TrackKind::Video, None) => self.stop_video(),
            (TrackKind::Video, Some(s)) => {
                let decoder = self.open_video(s)?;
                self.stop_video();
                self.start_video(s, decoder);
            }
            (_, None) => self.stop_audio(),
            (_, Some(s)) => {
                let decoder = self.open_audio(s)?;
                self.stop_audio();
                self.start_audio(s, decoder);
            }
        }
        // A new track's packets start where the demuxer is, ahead of the clock and usually
        // mid-GOP, so seek to the current position (or the pending target, right after a seek).
        // The seek flushes every queue and bumps the serial, which drops the old track's frames.
        // Turning a track off needs no seek: the other one plays on undisturbed.
        if stream.is_some() {
            self.shared.seek_by(0.0);
        }
        Ok(None)
    }

    /// The fonts attached to the file, handed out once.
    pub fn take_fonts(&mut self) -> Vec<(String, Vec<u8>)> {
        std::mem::take(&mut self.fonts)
    }

    /// The subtitle track opened with the file (a default and forced one), handed out once.
    pub fn take_initial_subtitles(&mut self) -> Option<SubtitleStream> {
        self.initial_subtitles.take()
    }

    /// Records from here on into `path`, or stops recording with None.
    pub fn set_recording(&self, path: Option<PathBuf>) {
        self.record.lock().unwrap().want = path.map(record::Sink::File);
    }

    pub fn recording(&self) -> bool {
        self.record.lock().unwrap().want.is_some()
    }

    /// Streams out to `broadcast`'s players from here on, or stops with None.
    pub fn set_streaming(&self, broadcast: Option<Arc<crate::stream::Broadcast>>) {
        self.stream.lock().unwrap().want = broadcast.map(record::Sink::Stream);
    }

    pub fn streaming(&self) -> bool {
        self.stream.lock().unwrap().want.is_some()
    }

    /// The controls the demuxer reports through (recording, streaming), which outlive the
    /// session.
    pub fn record_control(&self) -> record::Control {
        self.record.clone()
    }

    pub fn stream_control(&self) -> record::Control {
        self.stream.clone()
    }

    /// Start in seconds and title of each chapter, read when the file opened.
    pub fn chapters(&self) -> &[(f64, String)] {
        &self.chapters
    }
}

/// An attachment that is a font: its file name and bytes.
fn attached_font(stream: &ff::Stream) -> Option<(String, Vec<u8>)> {
    let metadata = stream.metadata();
    let name = meta(&metadata, c"filename").unwrap_or_default();
    let mime = meta(&metadata, c"mimetype")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let lower = name.to_ascii_lowercase();
    let id = stream.parameters().id();
    let font = matches!(id, ff::codec::Id::TTF | ff::codec::Id::OTF)
        || mime.contains("font")
        || [".ttf", ".otf", ".ttc"].iter().any(|e| lower.ends_with(e));
    let p = unsafe { &*stream.parameters().as_ptr() };
    if !font || p.extradata.is_null() || p.extradata_size <= 0 {
        return None;
    }
    let data = unsafe { std::slice::from_raw_parts(p.extradata, p.extradata_size as usize) };
    Some((name, data.to_vec()))
}

/// The container's chapters with their "title" tag, else "Chapter N", earliest first.
fn read_chapters(input: &ff::format::context::Input) -> Vec<(f64, String)> {
    let mut chapters: Vec<(f64, String)> = input
        .chapters()
        .enumerate()
        .map(|(i, c)| {
            let start = c.start() as f64 * f64::from(c.time_base());
            let title =
                meta(&c.metadata(), c"title").unwrap_or_else(|| format!("Chapter {}", i + 1));
            (start.max(0.0), title)
        })
        .collect();
    chapters.sort_by(|a, b| a.0.total_cmp(&b.0));
    chapters
}

/// A tag, trimmed, None when empty. Never ffmpeg-next's `get`: it hands back file bytes as a
/// `&str` without checking they are UTF-8, which a crafted file turns into undefined behaviour.
fn meta(dict: &ff::DictionaryRef, key: &CStr) -> Option<String> {
    crate::thumbs::tag(unsafe { dict.as_ptr() }, key)
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Relaxed);
        self.cancel.store(true, Relaxed);
        let decoders = [self.video_track.take(), self.audio_track.take()];
        let threads = decoders.into_iter().flatten().map(|d| d.thread);
        let externals = self.externals.drain(..).map(|e| e.demuxer);
        for t in self
            .demuxer
            .take()
            .into_iter()
            .chain(externals)
            .chain(threads)
        {
            _ = t.join();
        }
        if let Some((_, packets)) = self.subtitle_track.take() {
            packets.close();
        }
        self.audio.clear();
    }
}

/// Labels a track menu like VLC: the title tag, else the language name, else "Track N". Tracks
/// that would share a label get their codec appended.
fn label_tracks(tracks: &mut [TrackInfo], titles: &[Option<String>]) {
    for (i, (track, title)) in tracks.iter_mut().zip(titles).enumerate() {
        track.label = title
            .clone()
            .or_else(|| {
                let code = track.language.as_deref()?;
                Some(language_name(code).map_or_else(|| code.to_owned(), str::to_owned))
            })
            .unwrap_or_else(|| format!("Track {}", i + 1));
    }
    let labels: Vec<String> = tracks.iter().map(|t| t.label.clone()).collect();
    for track in tracks.iter_mut() {
        if labels.iter().filter(|l| **l == track.label).count() > 1 {
            track.label = format!("{} ({})", track.label, track.codec);
        }
    }
}

/// The stream of the track in `language` (an ISO 639 code), its default one if several are.
fn in_language(tracks: &[TrackInfo], language: &str) -> Option<usize> {
    if language.is_empty() {
        return None;
    }
    let want = language_name(language);
    let matches = |t: &&TrackInfo| {
        t.language
            .as_deref()
            .is_some_and(|code| match (language_name(code), want) {
                (Some(a), Some(b)) => a == b,
                _ => code.eq_ignore_ascii_case(language),
            })
    };
    let mut found = tracks.iter().filter(matches);
    let first = found.clone().next()?;
    Some(found.find(|t| t.default).unwrap_or(first).stream)
}

/// The English name of an ISO 639-2 or 639-1 language code, also accepting tags like "en-US".
fn language_name(code: &str) -> Option<&'static str> {
    let code = code.split(['-', '_']).next()?.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|(codes, _)| codes.contains(&code.as_str()))
        .map(|(_, name)| *name)
}

/// The most common languages by ISO 639-2 bibliographic and terminology codes, then 639-1.
/// ponytail: unknown codes show as the code itself; take the full ISO 639 table if that bothers anyone.
pub const LANGUAGES: &[(&[&str], &str)] = &[
    (&["eng", "en"], "English"),
    (&["jpn", "ja"], "Japanese"),
    (&["fre", "fra", "fr"], "French"),
    (&["ger", "deu", "de"], "German"),
    (&["spa", "es"], "Spanish"),
    (&["ita", "it"], "Italian"),
    (&["por", "pt"], "Portuguese"),
    (&["rus", "ru"], "Russian"),
    (&["chi", "zho", "zh"], "Chinese"),
    (&["kor", "ko"], "Korean"),
    (&["ara", "ar"], "Arabic"),
    (&["hin", "hi"], "Hindi"),
    (&["ben", "bn"], "Bengali"),
    (&["urd", "ur"], "Urdu"),
    (&["tam", "ta"], "Tamil"),
    (&["tel", "te"], "Telugu"),
    (&["mar", "mr"], "Marathi"),
    (&["pan", "pa"], "Punjabi"),
    (&["dut", "nld", "nl"], "Dutch"),
    (&["swe", "sv"], "Swedish"),
    (&["nor", "no", "nob", "nb", "nno", "nn"], "Norwegian"),
    (&["dan", "da"], "Danish"),
    (&["fin", "fi"], "Finnish"),
    (&["ice", "isl", "is"], "Icelandic"),
    (&["pol", "pl"], "Polish"),
    (&["cze", "ces", "cs"], "Czech"),
    (&["slo", "slk", "sk"], "Slovak"),
    (&["hun", "hu"], "Hungarian"),
    (&["rum", "ron", "ro"], "Romanian"),
    (&["bul", "bg"], "Bulgarian"),
    (&["hrv", "hr"], "Croatian"),
    (&["srp", "sr"], "Serbian"),
    (&["slv", "sl"], "Slovenian"),
    (&["ukr", "uk"], "Ukrainian"),
    (&["gre", "ell", "el"], "Greek"),
    (&["tur", "tr"], "Turkish"),
    (&["heb", "he"], "Hebrew"),
    (&["per", "fas", "fa"], "Persian"),
    (&["tha", "th"], "Thai"),
    (&["vie", "vi"], "Vietnamese"),
    (&["ind", "id"], "Indonesian"),
    (&["may", "msa", "ms"], "Malay"),
    (&["fil", "tgl", "tl"], "Filipino"),
    (&["cat", "ca"], "Catalan"),
    (&["baq", "eus", "eu"], "Basque"),
    (&["glg", "gl"], "Galician"),
    (&["est", "et"], "Estonian"),
    (&["lav", "lv"], "Latvian"),
    (&["lit", "lt"], "Lithuanian"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::{Msg, VideoFrame};
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::{Duration, Instant};

    /// A server answering with a concat script: plain FFmpeg follows it to more files, a stream
    /// address won't, and an ordinary stream from the same server still plays.
    #[test]
    fn a_stream_cannot_be_a_concat_script() {
        use std::io::{BufRead, BufReader, Write};
        ff::init().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            let video =
                std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_h264.mp4"))
                    .unwrap();
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                _ = reader.read_line(&mut line);
                let path = line.split(' ').nth(1).unwrap_or("/").to_owned();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {}
                let body = match path.as_str() {
                    "/live" => b"ffconcat version 1.0\nfile video.mp4\n".to_vec(),
                    _ => video.clone(),
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                _ = reader
                    .get_mut()
                    .write_all(&[head.as_bytes(), &body].concat());
            }
        });
        let script = format!("{base}/live");
        let plain = ff::format::input(&script).expect("plain FFmpeg opens it as concat");
        assert_eq!(plain.format().name(), "concat");
        drop(plain);
        let options = NetOptions {
            timeout: 5,
            reconnect: false,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(open_input(Path::new(&script), options, &cancel).is_err());
        let video = open_input(Path::new(&format!("{base}/video.mp4")), options, &cancel);
        assert!(video.is_ok(), "real streams still open");
    }

    /// A downloaded HLS playlist pulling its segment from elsewhere: plain FFmpeg follows it
    /// (here to a local file; in an attack to a server, which learns the file was opened),
    /// `open_file` won't. FFmpeg itself only takes HLS from .m3u8, .m3u and .hls names.
    #[test]
    fn a_downloaded_playlist_opens_nothing_else() {
        let dir = std::env::temp_dir().join(format!("wushang-hls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let segment = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_h264.mp4");
        let fake = dir.join("holiday.m3u8");
        std::fs::write(
            &fake,
            format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:5,\n{}\n#EXT-X-ENDLIST\n",
                segment.display()
            ),
        )
        .unwrap();
        let plain = ff::format::input(&fake).expect("plain FFmpeg opens it as HLS");
        assert_eq!(plain.format().name(), "hls");
        drop(plain);
        assert!(open_file(&fake).is_err());
        assert!(open_file(&segment).is_ok(), "real files still open");
        assert!(
            open_file(Path::new("concat:a|b")).is_err(),
            "no protocol names"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Made with the GPL ffmpeg from test_h264.mp4: 5 s of 640x360 H.264 with a keyframe every
    /// 0.5 s, a 440 Hz English and an 880 Hz Japanese AAC track (English default), and
    /// tracks.srt as an English subrip track with cues at 0.5, 2 and 4 s.
    fn clip() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_tracks.mkv")
    }

    const VIDEO: usize = 0;
    const ENGLISH: usize = 1;
    const JAPANESE: usize = 2;
    const SUBRIP: usize = 3;

    fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if done() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        done()
    }

    /// Aborts the test run if `f` hangs, since a deadlock would otherwise stall `cargo test`.
    fn within<R>(limit: Duration, what: &'static str, f: impl FnOnce() -> R) -> R {
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        thread::spawn(move || {
            if !wait_until(limit, || flag.load(Relaxed)) {
                eprintln!("{what} did not finish within {limit:?}");
                std::process::abort();
            }
        });
        let r = f();
        done.store(true, Relaxed);
        r
    }

    /// A playing session plus a stand-in for the render loop in main.rs.
    struct Player {
        shared: Arc<Shared>,
        session: Option<Session>,
        rendering: Arc<AtomicBool>,
        renderer: Option<JoinHandle<()>>,
    }

    impl Player {
        fn start() -> Player {
            let shared = Arc::new(Shared::new());
            // Tests play on the real device; keep them silent.
            shared.toggle_mute();
            let audio = audio::Device::open(shared.clone(), None);
            let session = Session::open(
                &clip(),
                None,
                &TrackChoice::default(),
                &shared,
                None,
                &audio,
            )
            .unwrap();
            let rendering = Arc::new(AtomicBool::new(true));
            let renderer =
                spawn_renderer(shared.clone(), session.frames.clone(), rendering.clone());
            Player {
                shared,
                session: Some(session),
                rendering,
                renderer: Some(renderer),
            }
        }

        fn session(&mut self) -> &mut Session {
            self.session.as_mut().unwrap()
        }

        fn position(&self) -> f64 {
            self.session.as_ref().unwrap().position().unwrap_or(-1.0)
        }

        /// Waits until playback runs for `by` seconds without jumping. A switch seeks back to the
        /// keyframe before the position, so measure from wherever the clock settles.
        fn advances(&self, by: f64) -> bool {
            wait_until(Duration::from_secs(10), || self.position() >= 0.0) && {
                let mut from = self.position();
                let mut last = from;
                wait_until(Duration::from_secs(10), || {
                    let p = self.position();
                    if p < last || p < 0.0 {
                        from = p.max(0.0);
                    }
                    last = p;
                    p > from + by
                })
            }
        }

        /// False when the machine has no audio output, so the audio decoder gave up.
        fn has_audio_device(&self) -> bool {
            wait_until(Duration::from_secs(10), || {
                self.position() > 0.0 || !self.shared.audio_master.load(Relaxed)
            });
            let ok = self.shared.audio_master.load(Relaxed);
            if !ok {
                eprintln!("no audio output device here; skipping the audio checks");
            }
            ok
        }
    }

    impl Drop for Player {
        fn drop(&mut self) {
            within(Duration::from_secs(10), "dropping the session", || {
                self.session = None;
            });
            self.rendering.store(false, Relaxed);
            _ = self.renderer.take().unwrap().join();
        }
    }

    /// Takes frames the way the render loop in main.rs does: the first one after a seek at
    /// once, then each one when the clock reaches it, and starts the clock from video when
    /// there is no audio.
    fn spawn_renderer(
        shared: Arc<Shared>,
        frames: Arc<FrameQueue>,
        running: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let mut current: Option<(f64, u64)> = None;
            while running.load(Relaxed) {
                let serial = shared.serial.load(Relaxed);
                if !shared.audio_master.load(Relaxed)
                    && shared.clock.get().is_none()
                    && let Some((pts, _)) = current.filter(|c| c.1 == serial)
                {
                    shared.clock.set(pts);
                }
                let fresh = current.is_none_or(|c| c.1 != serial);
                let now = shared.clock.get();
                let shown: Option<VideoFrame> = frames.with(|q| {
                    q.retain(|f| f.serial == serial);
                    let front = q.front()?;
                    if fresh {
                        return q.pop_front();
                    }
                    let now = now?;
                    if front.pts > now {
                        return None;
                    }
                    let mut frame = q.pop_front();
                    while q.front().is_some_and(|f| f.pts <= now) {
                        frame = q.pop_front();
                    }
                    frame
                });
                if let Some(f) = shown {
                    current = Some((f.pts, f.serial));
                }
                thread::sleep(Duration::from_millis(4));
            }
        })
    }

    /// Two tabs: two sessions at once, each on its own clock and sound output. Both play on;
    /// the one out of sight is silenced, the other heard, here at -120 dB so the test stays
    /// silent while the samples it sends aren't zero. Then they swap.
    #[test]
    fn two_sessions_play_at_once_and_only_one_is_heard() {
        let heard = Player::start();
        let unseen = Player::start();
        // `start` mutes; setting a volume unmutes.
        heard.shared.set_volume(1e-6);
        unseen.shared.set_volume(1e-6);
        unseen.shared.silenced.store(true, Relaxed);
        if !heard.has_audio_device() || !unseen.has_audio_device() {
            return;
        }
        // The clip is 5 s long, so measure in short spans.
        assert!(heard.advances(0.5), "the tab on screen plays");
        assert!(unseen.advances(0.5), "the tab out of sight plays on");
        // What the callback last sent to the device, after the gain.
        let sounding = |p: &Player| (p.shared.scope.lock().unwrap().1.iter()).any(|s| *s != 0.0);
        assert!(sounding(&heard));
        assert!(!sounding(&unseen));
        assert_eq!(unseen.shared.gain(), 0.0);
        assert!(
            !unseen.shared.muted.load(Relaxed),
            "the user's mute is untouched"
        );

        heard.shared.silenced.store(true, Relaxed);
        unseen.shared.silenced.store(false, Relaxed);
        // The scope holds about 20 ms; give the callbacks time to replace it.
        thread::sleep(Duration::from_millis(300));
        assert!(heard.advances(0.3) && unseen.advances(0.3));
        assert!(!sounding(&heard));
        assert!(sounding(&unseen));
    }

    #[test]
    fn lists_tracks_like_vlc() {
        let mut p = Player::start();
        let s = p.session();
        let streams =
            |s: &Session, kind| s.tracks(kind).iter().map(|t| t.stream).collect::<Vec<_>>();
        assert_eq!(streams(s, TrackKind::Video), [VIDEO]);
        assert_eq!(streams(s, TrackKind::Audio), [ENGLISH, JAPANESE]);
        assert_eq!(streams(s, TrackKind::Subtitle), [SUBRIP]);
        let labels = |s: &Session, kind| {
            s.tracks(kind)
                .iter()
                .map(|t| t.label.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(s, TrackKind::Audio), ["English", "Japanese"]);
        assert_eq!(labels(s, TrackKind::Subtitle), ["English"]);
        assert_eq!(labels(s, TrackKind::Video), ["Track 1"]);
        let japanese = &s.tracks(TrackKind::Audio)[1];
        assert_eq!(japanese.language.as_deref(), Some("jpn"));
        assert_eq!(japanese.codec, "aac");
        assert!(s.tracks(TrackKind::Audio)[0].default && !japanese.default);

        // The default tracks play; the subtitle track is neither default nor forced, so it stays off.
        assert_eq!(s.selected(TrackKind::Video), Some(VIDEO));
        assert_eq!(s.selected(TrackKind::Audio), Some(ENGLISH));
        assert_eq!(s.selected(TrackKind::Subtitle), None);
        assert!(s.take_initial_subtitles().is_none());
        assert_eq!(s.video_size, Some((640, 360)));

        assert!(s.select(TrackKind::Audio, Some(VIDEO)).is_err());
        assert!(s.select(TrackKind::Subtitle, Some(9)).is_err());
        s.select(TrackKind::Video, None).unwrap();
        assert!(s.select(TrackKind::Audio, None).is_err(), "both off");
        assert_eq!(s.selected(TrackKind::Audio), Some(ENGLISH));
    }

    #[test]
    fn lists_chapters() {
        let shared = Arc::new(Shared::new());
        shared.toggle_mute();
        let audio = audio::Device::open(shared.clone(), None);
        // test_tracks.mkv remuxed with three chapters, the middle one without a title.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_chapters.mkv");
        let s = Session::open(&path, None, &TrackChoice::default(), &shared, None, &audio).unwrap();
        let expected = [
            (0.0, "Opening".to_owned()),
            (1.5, "Chapter 2".to_owned()),
            (3.0, "Finale".to_owned()),
        ];
        assert_eq!(s.chapters(), expected);
        within(Duration::from_secs(10), "dropping the session", || drop(s));
        let s = Session::open(
            &clip(),
            None,
            &TrackChoice::default(),
            &shared,
            None,
            &audio,
        )
        .unwrap();
        assert!(s.chapters().is_empty());
        within(Duration::from_secs(10), "dropping the session", || drop(s));
    }

    #[test]
    fn labels_fall_back_and_disambiguate() {
        let track = |language: Option<&str>, codec: &str| TrackInfo {
            stream: 0,
            label: String::new(),
            language: language.map(str::to_owned),
            codec: codec.into(),
            default: false,
            size: None,
        };
        let mut tracks = [
            track(Some("eng"), "aac"),
            track(Some("en"), "ac3"),
            track(Some("fr-CA"), "aac"),
            track(None, "opus"),
            track(Some("tlh"), "aac"),
            track(Some("ger"), "aac"),
        ];
        let titles = [None, None, None, None, None, Some("Commentary".to_owned())];
        label_tracks(&mut tracks, &titles);
        let labels: Vec<_> = tracks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "English (aac)",
                "English (ac3)",
                "French",
                "Track 4",
                "tlh",
                "Commentary"
            ]
        );
        assert_eq!(language_name("JPN"), Some("Japanese"));
        assert_eq!(language_name("zh_TW"), Some("Chinese"));
        assert_eq!(language_name("xx"), None);
    }

    #[test]
    fn preferred_language_picks_its_default_track() {
        let track = |stream, language: Option<&str>, default| TrackInfo {
            stream,
            label: String::new(),
            language: language.map(str::to_owned),
            codec: "aac".into(),
            default,
            size: None,
        };
        let tracks = [
            track(1, Some("eng"), true),
            track(2, Some("ja"), false),
            track(3, Some("jpn"), true),
            track(4, Some("tlh"), false),
        ];
        assert_eq!(
            in_language(&tracks, "jpn"),
            Some(3),
            "the default of the two"
        );
        assert_eq!(in_language(&tracks, "en"), Some(1), "639-1 matches 639-2");
        assert_eq!(
            in_language(&tracks, "tlh"),
            Some(4),
            "unknown codes match as written"
        );
        assert_eq!(in_language(&tracks, "fre"), None);
        assert_eq!(in_language(&tracks, ""), None);
    }

    #[test]
    fn switches_audio_while_playing() {
        let mut p = Player::start();
        assert!(p.advances(0.3), "playback never started");
        let audio = p.has_audio_device();

        within(Duration::from_secs(10), "switching to Japanese", || {
            p.session()
                .select(TrackKind::Audio, Some(JAPANESE))
                .unwrap()
        });
        assert_eq!(p.session().selected(TrackKind::Audio), Some(JAPANESE));
        assert_eq!(p.shared.audio_master.load(Relaxed), audio);
        assert!(p.advances(0.3), "stalled after switching to Japanese");

        // No audio: the clock runs on and the video keeps playing.
        p.session().select(TrackKind::Audio, None).unwrap();
        assert_eq!(p.session().selected(TrackKind::Audio), None);
        assert!(!p.shared.audio_master.load(Relaxed));
        assert!(p.advances(0.3), "stalled without audio");

        p.session().select(TrackKind::Audio, Some(ENGLISH)).unwrap();
        assert!(p.advances(0.3), "stalled after switching back");

        let s = p.session.as_ref().unwrap();
        assert!(
            wait_until(Duration::from_secs(15), || s.finished()),
            "never finished at {:.2}",
            p.position()
        );
        if audio {
            assert!(p.shared.audio_done.load(Relaxed));
        }
        assert!(p.shared.video_done.load(Relaxed));
    }

    #[test]
    fn plays_an_external_audio_file_in_sync() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
        let mut p = Player::start();
        assert!(p.advances(0.3), "playback never started");

        let external = data.join("test_60s.mp4");
        let stream = p.session().add_audio_file(&external).unwrap();
        assert!(stream >= EXTERNAL);
        let track = p.session().tracks(TrackKind::Audio).last().unwrap().clone();
        assert_eq!(
            (track.stream, track.label.as_str()),
            (stream, "test_60s.mp4")
        );
        // Adding it again finds the same track; a file without sound adds nothing.
        assert_eq!(p.session().add_audio_file(&external), Ok(stream));
        assert!(
            p.session()
                .add_audio_file(&data.join("test_60s_noaudio.mp4"))
                .is_err()
        );
        assert_eq!(p.session().tracks(TrackKind::Audio).len(), 3);

        within(
            Duration::from_secs(10),
            "switching to the external file",
            || p.session().select(TrackKind::Audio, Some(stream)).unwrap(),
        );
        assert_eq!(p.session().selected(TrackKind::Audio), Some(stream));
        assert!(p.advances(0.3), "stalled on the external audio");
        // A seek reaches the external file's demuxer too, and both play on from there.
        p.shared.seek_to(1.5);
        assert!(p.advances(0.3), "stalled after seeking");
        // Audio restarts with the whole frame holding the target, a few ms early. The clock is
        // what is heard, so it trails that by the output's latency: a few ms on the built-in
        // speakers, up to about 300 ms on Bluetooth headphones.
        assert!(p.position() >= 1.15, "seek went to {:.2}", p.position());

        p.session().select(TrackKind::Audio, Some(ENGLISH)).unwrap();
        assert!(p.advances(0.3), "stalled after switching back");
    }

    #[test]
    fn switches_video_off_and_on() {
        let mut p = Player::start();
        assert!(p.advances(0.3));
        p.has_audio_device();
        p.session().select(TrackKind::Video, None).unwrap();
        assert_eq!(p.session().video_size, None);
        assert_eq!(p.session().selected(TrackKind::Video), None);
        assert!(p.advances(0.2), "audio stalled without video");
        p.session().select(TrackKind::Video, Some(VIDEO)).unwrap();
        assert_eq!(p.session().video_size, Some((640, 360)));
        assert!(p.advances(0.3), "stalled after turning video back on");
        let s = p.session.as_ref().unwrap();
        assert!(wait_until(Duration::from_secs(15), || s.finished()));
        assert!(p.shared.video_done.load(Relaxed));
    }

    #[test]
    fn routes_subtitle_packets() {
        let mut p = Player::start();
        let subtitles = p
            .session()
            .select(TrackKind::Subtitle, Some(SUBRIP))
            .unwrap()
            .expect("a subtitle stream");
        assert_eq!(p.session().selected(TrackKind::Subtitle), Some(SUBRIP));
        assert_eq!(subtitles.parameters.id(), ff::codec::Id::SUBRIP);
        assert_eq!(subtitles.time_base, 0.001);
        // The demuxer may have read past the first cue already; a seek to the start replays it.
        p.shared.seek_to(0.0);
        let mut flushed = false;
        let mut times = Vec::new();
        let start = Instant::now();
        while times.len() < 3 && start.elapsed() < Duration::from_secs(15) {
            match subtitles.packets.pop() {
                Some(Msg::Flush(_)) => {
                    flushed = true;
                    times.clear();
                }
                Some(Msg::Packet(packet)) => {
                    let pts = packet.pts().unwrap() as f64 * subtitles.time_base;
                    let duration = packet.duration() as f64 * subtitles.time_base;
                    times.push((pts, duration));
                }
                _ => {}
            }
        }
        assert!(flushed, "no flush after the seek");
        assert_eq!(times, [(0.5, 1.0), (2.0, 1.0), (4.0, 1.0)]);

        // Turning subtitles off closes the queue for its decoder and routes nothing.
        assert!(
            p.session()
                .select(TrackKind::Subtitle, None)
                .unwrap()
                .is_none()
        );
        assert!(subtitles.packets.closed());
        assert_eq!(p.session().selected(TrackKind::Subtitle), None);
    }

    #[test]
    fn rapid_switching_neither_deadlocks_nor_leaks() {
        let mut p = Player::start();
        assert!(p.advances(0.2));
        within(Duration::from_secs(60), "20 rapid switches", || {
            for i in 0..20 {
                // Some switches happen while paused, when the audio ring is full and nothing drains it.
                if i == 8 {
                    p.shared.toggle_pause();
                    thread::sleep(Duration::from_millis(300));
                }
                if i == 14 {
                    p.shared.toggle_pause();
                }
                let s = p.session();
                let audio = [Some(JAPANESE), Some(ENGLISH), None][i % 3];
                s.select(TrackKind::Audio, audio).unwrap();
                if i % 5 == 0 && audio.is_none() {
                    assert!(s.select(TrackKind::Video, None).is_err(), "both off");
                } else if i % 5 == 0 {
                    s.select(TrackKind::Video, None).unwrap();
                    s.select(TrackKind::Video, Some(VIDEO)).unwrap();
                }
                s.select(TrackKind::Subtitle, (i % 2 == 0).then_some(SUBRIP))
                    .unwrap();
            }
        });
        let s = p.session();
        // select() joins every decoder it replaces, so only the live ones remain, routed once each.
        // The last round (i = 19) left English audio and no subtitles.
        let mut routed: Vec<_> = s.routes.lock().unwrap().iter().map(|r| r.stream).collect();
        routed.sort();
        assert_eq!(routed, [VIDEO, ENGLISH]);
        assert!(s.video_track.is_some() && s.audio_track.is_some() && s.subtitle_track.is_none());
        s.select(TrackKind::Audio, Some(JAPANESE)).unwrap();
        assert!(p.advances(0.3), "stalled after rapid switching");
        // Dropping the player joins every thread under a watchdog.
    }
}

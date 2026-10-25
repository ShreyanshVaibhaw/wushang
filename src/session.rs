//! One open media file: its demux and decoder threads and what the interface shows about it.
//! The window, the GPU device and the audio device outlive sessions, so switching files only
//! stops these threads and starts new ones. Switching tracks does the same for one decoder.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ffmpeg_next as ff;

use crate::audio;
use crate::player::{self, FrameQueue, HwDevice, PacketQueue, Route, Routes, Shared};

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
    video_track: Option<Decoder>,
    audio_track: Option<Decoder>,
    subtitle_track: Option<(usize, Arc<PacketQueue>)>,
    initial_subtitles: Option<SubtitleStream>,
}

impl Session {
    /// Opens `path` and starts playing it, from `start` seconds if given.
    pub fn open(
        path: &Path,
        start: Option<f64>,
        shared: &Arc<Shared>,
        hw: Option<&HwDevice>,
        audio: &Arc<audio::Device>,
    ) -> Result<Session, Box<dyn Error>> {
        let input = ff::format::input(path).map_err(|e| format!("cannot open {path:?}: {e}"))?;
        shared.reset();
        let duration = if input.duration() > 0 {
            input.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE)
        } else {
            0.0
        };

        let mut tracks: [Vec<TrackInfo>; 3] = Default::default();
        let mut titles: [Vec<Option<String>>; 3] = Default::default();
        let mut codecs = HashMap::new();
        let mut forced_subtitles = None;
        for stream in input.streams() {
            let parameters = stream.parameters();
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
            let tag = |key: &str| {
                metadata
                    .get(key)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
            };
            let language = tag("language").filter(|l| !l.eq_ignore_ascii_case("und"));
            titles[kind as usize].push(tag("title"));
            tracks[kind as usize].push(TrackInfo {
                stream: stream.index(),
                label: String::new(),
                language,
                codec: parameters.id().name().to_owned(),
                default,
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
        let chapters = read_chapters(&input);
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
        let audio_stream = best(TrackKind::Audio, ff::media::Type::Audio);

        let mut session = Session {
            path: path.to_owned(),
            frames: Arc::new(FrameQueue::default()),
            title: path
                .file_stem()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .replace('_', " "),
            folder: path
                .parent()
                .and_then(|p| p.file_name())
                .map_or_else(String::new, |f| f.to_string_lossy().into_owned()),
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
            video_track: None,
            audio_track: None,
            subtitle_track: None,
            initial_subtitles: None,
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
        session.initial_subtitles = session.route_subtitles(forced_subtitles);
        if let Some(t) = start.filter(|t| *t > 0.0) {
            shared.seek_to(t);
        }
        session.demuxer = Some(player::spawn_demuxer(
            input,
            session.routes.clone(),
            shared.clone(),
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
        self.routes.lock().unwrap().push(Route {
            stream,
            queue: queue.clone(),
            sparse,
        });
    }

    fn unroute(&self, stream: usize) {
        self.routes.lock().unwrap().retain(|r| r.stream != stream);
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

    /// The subtitle track opened with the file (a default and forced one), handed out once.
    pub fn take_initial_subtitles(&mut self) -> Option<SubtitleStream> {
        self.initial_subtitles.take()
    }

    /// Start in seconds and title of each chapter, read when the file opened.
    pub fn chapters(&self) -> &[(f64, String)] {
        &self.chapters
    }
}

/// The container's chapters with their "title" tag, else "Chapter N", earliest first.
fn read_chapters(input: &ff::format::context::Input) -> Vec<(f64, String)> {
    let mut chapters: Vec<(f64, String)> = input
        .chapters()
        .enumerate()
        .map(|(i, c)| {
            let start = c.start() as f64 * f64::from(c.time_base());
            let title = c
                .metadata()
                .get("title")
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map_or_else(|| format!("Chapter {}", i + 1), str::to_owned);
            (start.max(0.0), title)
        })
        .collect();
    chapters.sort_by(|a, b| a.0.total_cmp(&b.0));
    chapters
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Relaxed);
        let decoders = [self.video_track.take(), self.audio_track.take()];
        let threads = decoders.into_iter().flatten().map(|d| d.thread);
        for t in self.demuxer.take().into_iter().chain(threads) {
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
const LANGUAGES: &[(&[&str], &str)] = &[
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
            let audio = audio::Device::open(shared.clone());
            let session = Session::open(&clip(), None, &shared, None, &audio).unwrap();
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
        let audio = audio::Device::open(shared.clone());
        // test_tracks.mkv remuxed with three chapters, the middle one without a title.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_chapters.mkv");
        let s = Session::open(&path, None, &shared, None, &audio).unwrap();
        let expected = [
            (0.0, "Opening".to_owned()),
            (1.5, "Chapter 2".to_owned()),
            (3.0, "Finale".to_owned()),
        ];
        assert_eq!(s.chapters(), expected);
        within(Duration::from_secs(10), "dropping the session", || drop(s));
        let s = Session::open(&clip(), None, &shared, None, &audio).unwrap();
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

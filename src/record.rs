//! Recording, VLC's "Record": the open file or stream copied packet for packet, without
//! re-encoding, into a Matroska file in Videos\myvlc, from the moment Record is pressed until it is
//! pressed again or the media ends. Matroska holds every codec myvlc plays.
//!
//! The demux thread does the writing, since it holds the packets anyway; the interface asks for
//! a recording through a [`Control`] and hears back when a file is done. The demuxer reads seconds
//! ahead of the picture, so packets wait in the recorder until playback reaches them: the file
//! holds what was seen, from the press of Record to the next, and nothing read ahead past a seek.
//!
//! Streaming out (`stream.rs`) is the same copy, into MPEG-TS for a [`stream::Broadcast`] instead
//! of a file, which is what players and TVs read from a network.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ff::codec::Id;
use ff::format::stream::Disposition;
use ff::media::Type;
use ff::packet::Mut as _;
use ffmpeg_next as ff;

use crate::stream;

/// Pause between runs of a recording that a seek split, so a run's B-frames, whose decode time
/// comes before their keyframe's, never step back behind the last run's packets.
const RUN_GAP: f64 = 0.2;

/// Where the copy goes.
pub enum Sink {
    /// A Matroska file.
    File(PathBuf),
    /// MPEG-TS for the players connected to a stream.
    Stream(Arc<stream::Broadcast>),
}

/// What the interface wants and what the demux thread finished, shared between them.
#[derive(Default)]
pub struct Slot {
    /// Where to copy to; None stops.
    pub want: Option<Sink>,
    /// Recordings finished since the interface last looked, or why one couldn't start.
    pub done: Vec<Result<PathBuf, String>>,
}

pub type Control = Arc<Mutex<Slot>>;

/// Where an input stream goes: its output stream and both time bases.
struct Route {
    out: usize,
    from: f64,
    to: f64,
    video: bool,
    /// The last decode time written, in output units; each must come after the one before.
    last_dts: Option<i64>,
    /// For a stream's H.264 and HEVC, see [`Filter`].
    filter: Option<Filter>,
}

/// Bitstream filters that repeat H.264's or HEVC's parameter sets before every keyframe. MP4
/// and Matroska keep them once, in the header; a player joining a network stream midway would
/// otherwise wait for an IDR frame that may never come ("non-existing PPS referenced").
struct Filter(*mut ff::ffi::AVBSFContext);

// The context is only ever used by the demux thread that owns the recorder.
unsafe impl Send for Filter {}

impl Filter {
    fn new(id: Id, parameters: &ff::codec::Parameters) -> Option<Filter> {
        let chain = match id {
            Id::H264 => c"h264_mp4toannexb,dump_extra=freq=keyframe",
            Id::HEVC => c"hevc_mp4toannexb,dump_extra=freq=keyframe",
            _ => return None,
        };
        unsafe {
            let mut ctx = std::ptr::null_mut();
            if ff::ffi::av_bsf_list_parse_str(chain.as_ptr(), &mut ctx) < 0 {
                return None;
            }
            let filter = Filter(ctx);
            let ok = ff::ffi::avcodec_parameters_copy((*ctx).par_in, parameters.as_ptr()) >= 0
                && ff::ffi::av_bsf_init(ctx) >= 0;
            ok.then_some(filter)
        }
    }

    /// The parameters packets come out with (Annex B, with the extradata to repeat).
    fn parameters(&self) -> *const ff::ffi::AVCodecParameters {
        unsafe { (*self.0).par_out }
    }

    /// `packet` filtered; usually one packet back.
    fn run(&mut self, packet: &mut ff::Packet) -> Vec<ff::Packet> {
        let mut out = Vec::new();
        unsafe {
            if ff::ffi::av_bsf_send_packet(self.0, packet.as_mut_ptr()) < 0 {
                return out;
            }
            let mut filtered = ff::Packet::empty();
            while ff::ffi::av_bsf_receive_packet(self.0, filtered.as_mut_ptr()) >= 0 {
                out.push(std::mem::replace(&mut filtered, ff::Packet::empty()));
            }
        }
        out
    }
}

impl Drop for Filter {
    fn drop(&mut self) {
        unsafe { ff::ffi::av_bsf_free(&mut self.0) };
    }
}

pub struct Recorder {
    out: ff::format::context::Output,
    /// The file, for a recording.
    path: Option<PathBuf>,
    /// By input stream index; None for streams left out (data, attachments, cover art).
    routes: Vec<Option<Route>>,
    has_video: bool,
    /// Input seconds that land on output 0: set by the keyframe that starts each run, so a seek
    /// continues the file instead of jumping in it.
    offset: Option<f64>,
    /// Output seconds the current run starts at, and the furthest written so far.
    run_start: f64,
    reached: f64,
    /// Packets read but not yet played, with their input seconds.
    waiting: VecDeque<(f64, ff::Packet)>,
}

impl Recorder {
    pub fn start(input: &ff::format::context::Input, sink: &Sink) -> Result<Recorder, String> {
        let (out, path) = match sink {
            Sink::File(path) => {
                let out = ff::format::output_as(path, "matroska")
                    .map_err(|e| format!("can't record to {}: {e}", path.display()))?;
                (out, Some(path.clone()))
            }
            Sink::Stream(broadcast) => {
                let fail = |e: ff::Error| format!("can't stream: {e}");
                let io =
                    ff::format::context::StreamIo::from_write(stream::Writer(broadcast.clone()))
                        .map_err(fail)?;
                (
                    ff::format::output_to_stream(io, None, Some("mpegts")).map_err(fail)?,
                    None,
                )
            }
        };
        let streaming = path.is_none();
        let mut out = out;
        let fail = |e: ff::Error| format!("can't copy this media: {e}");
        let mut routes = Vec::new();
        let mut kept = Vec::new();
        for stream in input.streams() {
            let parameters = stream.parameters();
            let cover = stream.disposition().contains(Disposition::ATTACHED_PIC);
            let medium = parameters.medium();
            let wanted = matches!(medium, Type::Video | Type::Audio | Type::Subtitle)
                && !cover
                && parameters.id() != ff::codec::Id::None
                && (!streaming || in_mpegts(parameters.id()));
            if !wanted {
                routes.push(None);
                continue;
            }
            let mut ost = out
                .add_stream(ff::encoder::find(ff::codec::Id::None))
                .map_err(fail)?;
            let filter = if streaming {
                Filter::new(parameters.id(), &parameters)
            } else {
                None
            };
            match &filter {
                Some(f) => unsafe {
                    ff::ffi::avcodec_parameters_copy(ost.parameters().as_mut_ptr(), f.parameters());
                },
                None => ost.set_parameters(parameters),
            }
            // The input container's codec tag means nothing to the output's.
            unsafe { (*ost.parameters().as_mut_ptr()).codec_tag = 0 };
            kept.push(stream.index());
            routes.push(Some(Route {
                out: ost.index(),
                from: f64::from(stream.time_base()),
                to: 0.0,
                video: medium == Type::Video,
                last_dts: None,
                filter,
            }));
        }
        if kept.is_empty() {
            return Err(if streaming {
                "this media's video and sound can't go in a network stream; convert it to MP4 first"
                    .into()
            } else {
                "nothing in this media can be recorded".into()
            });
        }
        out.write_header().map_err(fail)?;
        // The muxer picks the output time bases while writing the header.
        for route in routes.iter_mut().flatten() {
            route.to = f64::from(out.stream(route.out).unwrap().time_base());
        }
        let has_video = routes.iter().flatten().any(|r| r.video);
        Ok(Recorder {
            out,
            path,
            routes,
            has_video,
            offset: None,
            run_start: 0.0,
            reached: 0.0,
            waiting: VecDeque::new(),
        })
    }

    /// Holds `packet` until playback reaches it.
    pub fn push(&mut self, packet: &ff::Packet) {
        let Some(Some(route)) = self.routes.get(packet.stream()) else {
            return;
        };
        // A packet without a time goes with the one before it.
        let seconds = match packet.pts().or(packet.dts()) {
            Some(ts) => ts as f64 * route.from,
            None => self.waiting.back().map_or(0.0, |(s, _)| *s),
        };
        self.waiting.push_back((seconds, packet.clone()));
    }

    /// Writes the packets playback has reached at `playhead` seconds of the input.
    pub fn play_to(&mut self, playhead: f64) {
        while self.waiting.front().is_some_and(|(s, _)| *s <= playhead) {
            let (_, packet) = self.waiting.pop_front().unwrap();
            self.write(&packet);
        }
    }

    /// Copies `packet` in, if its stream is recorded and the run has started.
    fn write(&mut self, packet: &ff::Packet) {
        let Some(Some(route)) = self.routes.get(packet.stream()) else {
            return;
        };
        let Some(ts) = packet.pts().or(packet.dts()) else {
            return;
        };
        let seconds = ts as f64 * route.from;
        let offset = match self.offset {
            Some(offset) => offset,
            // A run starts on a video keyframe (any packet without video), so the file opens.
            None if self.has_video && !(route.video && packet.is_key()) => return,
            None => {
                self.run_start = if self.reached > 0.0 {
                    self.reached + RUN_GAP
                } else {
                    0.0
                };
                let offset = seconds - self.run_start;
                self.offset = Some(offset);
                offset
            }
        };
        let at = seconds - offset;
        // Audio and subtitles from before the run's keyframe.
        if at < self.run_start {
            return;
        }
        let (from, to, out) = (route.from, route.to, route.out);
        let shift = |ts: i64| ((ts as f64 * from - offset) / to).round() as i64;
        let duration = (packet.duration() as f64 * from / to).round() as i64;
        let pts = packet.pts().map(shift);
        // Demuxers leave the decode time out of the first packets after a seek, and the muxer's
        // guess then clashes with the next real one. Video decodes just after the packet before
        // (a keyframe starting a run two frames before it shows, room for B-frames); other
        // streams decode as they show. Decode times must always rise.
        let last = self.routes[packet.stream()].as_ref().unwrap().last_dts;
        let mut dts = packet.dts().map(shift).or(match (last, route.video) {
            (Some(l), true) => Some(l + 1),
            (None, true) => pts.map(|p| p - 2 * duration.max(1)),
            (_, false) => pts,
        });
        if let (Some(d), Some(l)) = (dts, last)
            && d <= l
        {
            dts = Some(l + 1);
        }
        let mut copy = packet.clone();
        copy.set_pts(pts);
        copy.set_dts(dts);
        copy.set_duration(duration);
        copy.set_stream(out);
        copy.set_position(-1);
        let end = at + packet.duration() as f64 * from;
        let route = self.routes[packet.stream()].as_mut().unwrap();
        let packets = match &mut route.filter {
            Some(f) => f.run(&mut copy),
            None => vec![copy],
        };
        // A packet the muxer turns down (a timestamp out of order) is skipped, not fatal.
        let mut written = false;
        for mut p in packets {
            p.set_stream(out);
            written |= p.write_interleaved(&mut self.out).is_ok();
        }
        if written {
            self.reached = self.reached.max(end);
            self.routes[packet.stream()].as_mut().unwrap().last_dts = dts;
        }
    }

    /// The input jumped: what was read ahead won't be seen, and the next run starts at the next
    /// keyframe, after what is written.
    pub fn seeked(&mut self) {
        self.waiting.clear();
        self.offset = None;
    }

    /// Writes what was played up to `playhead` and closes the file (it plays from here on), or
    /// ends the stream. The file, for a recording.
    pub fn finish(mut self, playhead: f64) -> Result<Option<PathBuf>, String> {
        self.play_to(playhead);
        self.out
            .write_trailer()
            .map(|()| self.path)
            .map_err(|e| format!("can't finish the recording: {e}"))
    }
}

/// Starts or stops the recorder as `control` asks, with playback at `playhead`; the demux
/// thread calls this every turn. True when a recording just started: the demuxer is then seconds
/// ahead, and should seek back to the picture so the file starts there.
pub fn sync(
    recorder: &mut Option<Recorder>,
    control: &Control,
    input: &ff::format::context::Input,
    playhead: f64,
) -> bool {
    let mut slot = control.lock().unwrap();
    match (&slot.want, recorder.is_some()) {
        (Some(sink), false) => match Recorder::start(input, sink) {
            Ok(r) => {
                *recorder = Some(r);
                return true;
            }
            Err(e) => {
                slot.want = None;
                slot.done.push(Err(e));
            }
        },
        (None, true) => {
            if let Some(finished) = recorder.take().unwrap().finish(playhead).transpose() {
                slot.done.push(finished);
            }
        }
        _ => {}
    }
    false
}

/// Ends a recording because the media closed, with playback at `playhead`.
pub fn stop(recorder: &mut Option<Recorder>, control: &Control, playhead: f64) {
    if let Some(r) = recorder.take() {
        let mut slot = control.lock().unwrap();
        slot.want = None;
        if let Some(finished) = r.finish(playhead).transpose() {
            slot.done.push(finished);
        }
    }
}

/// Codecs MPEG-TS carries and players expect in it.
fn in_mpegts(id: Id) -> bool {
    matches!(
        id,
        Id::H264
            | Id::HEVC
            | Id::MPEG2VIDEO
            | Id::MPEG1VIDEO
            | Id::AAC
            | Id::MP3
            | Id::MP2
            | Id::AC3
            | Id::EAC3
            | Id::DTS
            | Id::OPUS
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn streams_mpeg_ts_a_player_joining_midway_can_play() {
        use std::io::{Read, Write};
        let server = stream::Server::start(std::net::Ipv4Addr::LOCALHOST.into()).unwrap();
        // A keyframe every 2 s, the first of them IDR: a late player needs the parameter sets
        // repeated at the others.
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_60s.mp4");
        let mut input = ff::format::input(&clip).unwrap();
        let sink = Sink::Stream(server.broadcast.clone());
        let mut recorder = Recorder::start(&input, &sink).unwrap();
        let mut packets = input.packets().map(|(_, p)| p);
        // About 2.5 s go out before the player connects.
        for packet in packets.by_ref().take(180) {
            recorder.push(&packet);
        }
        recorder.play_to(f64::INFINITY);
        let mut player = std::net::TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        player.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        while server.broadcast.players() == 0 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for packet in packets.take(400) {
            recorder.push(&packet);
        }
        assert_eq!(recorder.finish(f64::INFINITY), Ok(None));
        drop(server);
        let mut received = Vec::new();
        player.read_to_end(&mut received).unwrap();
        let body = received.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let ts = std::env::temp_dir().join(format!("myvlc-stream-{}.ts", std::process::id()));
        std::fs::write(&ts, &received[body..]).unwrap();
        let check = ff::format::input(&ts).unwrap();
        assert_eq!(check.format().name(), "mpegts");
        let names: Vec<&str> = check
            .streams()
            .map(|s| s.parameters().id().name())
            .collect();
        assert_eq!(names, ["h264", "aac"]);
        drop(check);
        // It plays from the next keyframe on.
        let mut check = ff::format::input(&ts).unwrap();
        let stream = check.streams().best(Type::Video).unwrap();
        let index = stream.index();
        let mut decoder = ff::codec::Context::from_parameters(stream.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        let mut frames = 0;
        let mut frame = ff::frame::Video::empty();
        for (s, packet) in check.packets() {
            if s.index() == index && decoder.send_packet(&packet).is_ok() {
                while decoder.receive_frame(&mut frame).is_ok() {
                    frames += 1;
                }
            }
        }
        assert!(frames >= 30, "only {frames} frames decoded");
        drop(check);
        std::fs::remove_file(&ts).unwrap();
    }

    #[test]
    fn copies_from_a_keyframe_and_continues_after_a_seek() {
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_tracks.mkv");
        let out = std::env::temp_dir().join(format!("myvlc-record-{}.mkv", std::process::id()));
        let mut input = ff::format::input(&clip).unwrap();
        let mut recorder = Recorder::start(&input, &Sink::File(out.clone())).unwrap();
        // Start mid-file, as pressing Record would, then seek back once.
        input.seek(1_000_000, ..1_000_001).unwrap();
        let read = |input: &mut ff::format::context::Input, recorder: &mut Recorder, n: usize| {
            for (_, packet) in input.packets().take(n) {
                recorder.push(&packet);
            }
            recorder.play_to(f64::INFINITY);
        };
        read(&mut input, &mut recorder, 60);
        let first_run = recorder.reached;
        assert!(first_run > 0.3, "only {first_run:.2}s written");
        input.seek(0, ..1).unwrap();
        recorder.seeked();
        read(&mut input, &mut recorder, 60);
        let second_run = recorder.reached;
        assert!(second_run > first_run + RUN_GAP);
        // Packets read ahead of the picture stay out of the file.
        for (_, packet) in input.packets().take(60) {
            recorder.push(&packet);
        }
        recorder.finish(0.0).unwrap();

        // The file opens, starts at 0 and holds the clip's video, two audio tracks and subtitles.
        let mut check = ff::format::input(&out).unwrap();
        let kinds: Vec<Type> = check.streams().map(|s| s.parameters().medium()).collect();
        assert_eq!(
            kinds,
            [Type::Video, Type::Audio, Type::Audio, Type::Subtitle]
        );
        let length = check.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE);
        assert!(
            (length - second_run).abs() < 0.1,
            "{length:.2}s long, {second_run:.2}s written"
        );
        let (stream, packet) = check.packets().find(|(s, _)| s.index() == 0).unwrap();
        let start = packet.pts().unwrap() as f64 * f64::from(stream.time_base());
        assert!(start.abs() < 0.05, "video starts at {start:.3}s");
        assert!(packet.is_key());
        drop(check);
        std::fs::remove_file(&out).unwrap();
    }
}

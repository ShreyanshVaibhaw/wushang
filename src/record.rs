//! Recording, VLC's "Record": the open file or stream copied packet for packet, without
//! re-encoding, into a Matroska file in Videos\myvlc, from the moment Record is pressed until it is
//! pressed again or the media ends. Matroska holds every codec myvlc plays.
//!
//! The demux thread does the writing, since it holds the packets anyway; the interface asks for
//! a recording through a [`Control`] and hears back when a file is done. The demuxer reads seconds
//! ahead of the picture, so packets wait in the recorder until playback reaches them: the file
//! holds what was seen, from the press of Record to the next, and nothing read ahead past a seek.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ff::format::stream::Disposition;
use ff::media::Type;
use ffmpeg_next as ff;

/// Pause between runs of a recording that a seek split, so a run's B-frames, whose decode time
/// comes before their keyframe's, never step back behind the last run's packets.
const RUN_GAP: f64 = 0.2;

/// What the interface wants and what the demux thread finished, shared between them.
#[derive(Default)]
pub struct Slot {
    /// The file to record into; None stops a recording.
    pub want: Option<PathBuf>,
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
}

pub struct Recorder {
    out: ff::format::context::Output,
    path: PathBuf,
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
    pub fn start(input: &ff::format::context::Input, path: &Path) -> Result<Recorder, String> {
        let fail = |e: ff::Error| format!("can't record to {}: {e}", path.display());
        let mut out = ff::format::output_as(path, "matroska").map_err(fail)?;
        let mut routes = Vec::new();
        let mut kept = Vec::new();
        for stream in input.streams() {
            let parameters = stream.parameters();
            let cover = stream.disposition().contains(Disposition::ATTACHED_PIC);
            let medium = parameters.medium();
            let wanted = matches!(medium, Type::Video | Type::Audio | Type::Subtitle)
                && !cover
                && parameters.id() != ff::codec::Id::None;
            if !wanted {
                routes.push(None);
                continue;
            }
            let mut ost = out
                .add_stream(ff::encoder::find(ff::codec::Id::None))
                .map_err(fail)?;
            ost.set_parameters(parameters);
            // The input container's codec tag means nothing to Matroska.
            unsafe { (*ost.parameters().as_mut_ptr()).codec_tag = 0 };
            kept.push(stream.index());
            routes.push(Some(Route {
                out: ost.index(),
                from: f64::from(stream.time_base()),
                to: 0.0,
                video: medium == Type::Video,
                last_dts: None,
            }));
        }
        if kept.is_empty() {
            return Err("nothing in this media can be recorded".into());
        }
        out.write_header().map_err(fail)?;
        // The muxer picks the output time bases while writing the header.
        for route in routes.iter_mut().flatten() {
            route.to = f64::from(out.stream(route.out).unwrap().time_base());
        }
        let has_video = routes.iter().flatten().any(|r| r.video);
        Ok(Recorder {
            out,
            path: path.to_owned(),
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
        // A packet the muxer turns down (a timestamp out of order) is skipped, not fatal.
        if copy.write_interleaved(&mut self.out).is_ok() {
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

    /// Writes what was played up to `playhead` and closes the file; it plays from here on.
    pub fn finish(mut self, playhead: f64) -> Result<PathBuf, String> {
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
        (Some(path), false) => match Recorder::start(input, path) {
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
            let finished = recorder.take().unwrap().finish(playhead);
            slot.done.push(finished);
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
        slot.done.push(r.finish(playhead));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_from_a_keyframe_and_continues_after_a_seek() {
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_tracks.mkv");
        let out = std::env::temp_dir().join(format!("myvlc-record-{}.mkv", std::process::id()));
        let mut input = ff::format::input(&clip).unwrap();
        let mut recorder = Recorder::start(&input, &out).unwrap();
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

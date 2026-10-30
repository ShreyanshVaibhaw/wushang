//! Demux and video decode threads, the queues between them, and the master clock.
//!
//! Same stages as VLC's input/decoder core, minus the plugin system:
//! demux thread -> packet queues -> decoder threads -> frame queue / audio ring -> outputs.

use std::collections::VecDeque;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ff::ffi;
use ffmpeg_next as ff;

use crate::record;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0, ID3D11Device, ID3D11VideoDevice,
};
use windows::core::Interface;

/// Decoded frames waiting for the renderer.
pub const FRAME_QUEUE_LEN: usize = 3;
/// Decoder surfaces on top of the codec's reference frames: the frame queue, plus the frame on
/// screen and the one replacing it, which the renderer holds together for a moment.
const EXTRA_HW_FRAMES: i32 = FRAME_QUEUE_LEN as i32 + 2;
/// The demuxer rests once every stream has this many packets queued...
const ENOUGH_PACKETS: usize = 50;
/// ...or once all queues together hold this many bytes. Throttling on both, instead of blocking on
/// one full queue, avoids the deadlock where video waits for an audio clock that starves for packets.
const MAX_QUEUED_BYTES: usize = 32 << 20;
/// Subtitle queues keep at most this many packets and drop the oldest beyond it, so a subtitle
/// stream nobody reads never holds up the demuxer or grows without bound.
pub const SUBTITLE_QUEUE_LEN: usize = 500;
/// How far before a seek the demuxer looks for subtitles still on screen there.
const SUBTITLE_PREROLL: f64 = 10.0;
pub const POLL: Duration = Duration::from_millis(20);
pub const MAX_VOLUME: f32 = 2.0;
/// Video packets that may fail before the first frame until the stream counts as undecodable,
/// about two seconds at 30 fps.
const GIVE_UP_AFTER: u32 = 60;
/// Failed packets before the first frame after which a stream opened for the GPU is tried in
/// software instead; a few, so one corrupt packet at the start doesn't cost the GPU.
const FALLBACK_AFTER: u32 = 8;

/// Playback position in seconds. The audio callback drives it; without audio, the first video
/// frame starts it and it runs on the system clock.
pub struct Clock(Mutex<ClockState>);

struct ClockState {
    pts: f64,
    at: Instant,
    paused: bool,
    valid: bool,
    /// Media seconds per real second between updates: the playback speed.
    speed: f64,
}

impl Clock {
    fn new() -> Self {
        Clock(Mutex::new(ClockState {
            pts: 0.0,
            at: Instant::now(),
            paused: false,
            valid: false,
            speed: 1.0,
        }))
    }

    pub fn set(&self, pts: f64) {
        let mut s = self.0.lock().unwrap();
        s.pts = pts;
        s.at = Instant::now();
        s.valid = true;
    }

    pub fn get(&self) -> Option<f64> {
        let s = self.0.lock().unwrap();
        s.valid.then(|| {
            if s.paused {
                s.pts
            } else {
                s.pts + s.at.elapsed().as_secs_f64() * s.speed
            }
        })
    }

    pub fn set_paused(&self, paused: bool) {
        let mut s = self.0.lock().unwrap();
        if !s.paused {
            s.pts += s.at.elapsed().as_secs_f64() * s.speed;
        }
        s.at = Instant::now();
        s.paused = paused;
    }

    /// Video without audio runs on this clock, so it has to follow the speed setting too.
    pub fn set_speed(&self, speed: f32) {
        let mut s = self.0.lock().unwrap();
        if !s.paused {
            s.pts += s.at.elapsed().as_secs_f64() * s.speed;
        }
        s.at = Instant::now();
        s.speed = f64::from(speed);
    }

    pub fn invalidate(&self) {
        self.0.lock().unwrap().valid = false;
    }
}

/// State shared by every thread and the window procedure.
pub struct Shared {
    pub clock: Clock,
    pub paused: AtomicBool,
    pub quit: AtomicBool,
    /// Set while a session's threads wind down so the next file can start.
    pub stopping: AtomicBool,
    /// Bumped on every seek. Frames and samples tagged with an older serial are stale.
    pub serial: AtomicU64,
    /// Where the last seek asked to land, as f64 bits. The demuxer can only seek to a keyframe,
    /// so decoders drop what comes before this, like VLC's exact seek.
    seek_floor: AtomicU64,
    /// False after a quick seek: video shows the keyframe it lands on instead of decoding on to
    /// the target, so dragging the seek bar keeps up.
    exact: AtomicBool,
    volume: AtomicU32,
    pub muted: AtomicBool,
    seek: Mutex<Seek>,
    pub video_done: AtomicBool,
    /// True when audio is playing or about to, so the audio callback owns the clock.
    pub audio_master: AtomicBool,
    /// Audio decoded and played to the end.
    pub audio_done: AtomicBool,
    /// The last `SCOPE` mono samples sent to the speakers, and their rate, for the visualizer.
    pub scope: Mutex<(u32, Vec<f32>)>,
    /// A dropped live stream is being opened again.
    pub reconnecting: AtomicBool,
}

/// How a live stream FFmpeg can't reconnect itself (RTSP, UDP) opens again when it drops.
/// Opens the playing file again, for a second reader.
pub type Reopen = Arc<dyn Fn() -> Result<ff::format::context::Input, String> + Send + Sync>;

pub struct Reconnect {
    pub open: Box<dyn Fn() -> Result<ff::format::context::Input, String> + Send>,
    /// Keep trying this long before the stream counts as ended.
    pub give_up: Duration,
}

impl Reconnect {
    /// Opens the stream again, trying each second until it answers, `give_up` passes or the
    /// player stops.
    fn reopen(&self, shared: &Shared) -> Option<ff::format::context::Input> {
        shared.reconnecting.store(true, Relaxed);
        let since = std::time::Instant::now();
        let input = loop {
            if shared.quitting() {
                break None;
            }
            match (self.open)() {
                Ok(input) => break Some(input),
                Err(e) if since.elapsed() < self.give_up => {
                    eprintln!("stream: still down ({e})");
                    thread::sleep(Duration::from_secs(1));
                }
                Err(e) => {
                    eprintln!("stream: giving up ({e})");
                    break None;
                }
            }
        };
        shared.reconnecting.store(false, Relaxed);
        input
    }
}

/// Samples the visualizer sees at once, about 20 ms.
pub const SCOPE: usize = 1024;

#[derive(Default)]
struct Seek {
    /// Target the demuxer has not picked up yet.
    pending: Option<f64>,
    /// Last target requested. The clock is unknown right after a seek, so a second key press
    /// builds on this instead of jumping back to the start.
    last: f64,
    /// The pending seek is `Shared::seek_quick`'s.
    quick: bool,
}

impl Shared {
    pub fn new() -> Self {
        Shared {
            clock: Clock::new(),
            paused: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            serial: AtomicU64::new(0),
            seek_floor: AtomicU64::new(0),
            exact: AtomicBool::new(true),
            volume: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            seek: Mutex::default(),
            video_done: AtomicBool::new(false),
            audio_master: AtomicBool::new(false),
            audio_done: AtomicBool::new(false),
            scope: Mutex::new((0, Vec::with_capacity(SCOPE * 16))),
            reconnecting: AtomicBool::new(false),
        }
    }

    pub fn quitting(&self) -> bool {
        self.quit.load(Relaxed) || self.stopping.load(Relaxed)
    }

    /// Fresh playback state for the next file. Volume and mute carry over, as in VLC.
    pub fn reset(&self) {
        self.stopping.store(false, Relaxed);
        self.paused.store(false, Relaxed);
        self.clock.set_paused(false);
        self.clock.invalidate();
        self.serial.fetch_add(1, Relaxed);
        *self.seek.lock().unwrap() = Seek::default();
        self.seek_floor.store(0, Relaxed);
        self.exact.store(true, Relaxed);
        for flag in [&self.video_done, &self.audio_master, &self.audio_done] {
            flag.store(false, Relaxed);
        }
    }

    pub fn seek_floor(&self) -> f64 {
        f64::from_bits(self.seek_floor.load(Relaxed))
    }

    /// Whether video decodes on to the last seek's target (see `exact`).
    fn exact(&self) -> bool {
        self.exact.load(Relaxed)
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Relaxed))
    }

    /// Like VLC, volume goes up to 200%; above 100% the samples are amplified and may clip.
    pub fn set_volume(&self, v: f32) {
        self.volume
            .store(v.clamp(0.0, MAX_VOLUME).to_bits(), Relaxed);
        self.muted.store(false, Relaxed);
    }

    /// The gain the audio callback applies: zero while muted.
    pub fn gain(&self) -> f32 {
        if self.muted.load(Relaxed) {
            0.0
        } else {
            self.volume()
        }
    }

    pub fn toggle_mute(&self) {
        self.muted.fetch_xor(true, Relaxed);
    }

    pub fn seek_to(&self, target: f64) {
        let mut seek = self.seek.lock().unwrap();
        seek.last = target.max(0.0);
        seek.pending = Some(seek.last);
        seek.quick = false;
    }

    /// A seek that shows the keyframe at or before `target` rather than the frame at it: fast
    /// enough to repeat while the seek bar is dragged.
    pub fn seek_quick(&self, target: f64) {
        let mut seek = self.seek.lock().unwrap();
        seek.last = target.max(0.0);
        seek.pending = Some(seek.last);
        seek.quick = true;
    }

    pub fn toggle_pause(&self) {
        let paused = !self.paused.load(Relaxed);
        self.paused.store(paused, Relaxed);
        self.clock.set_paused(paused);
    }

    pub fn seek_by(&self, delta: f64) {
        let mut seek = self.seek.lock().unwrap();
        let from = self.clock.get().unwrap_or(seek.last);
        seek.last = (from + delta).max(0.0);
        seek.pending = Some(seek.last);
        seek.quick = false;
    }

    /// The seek the demuxer should do, and whether it is quick.
    fn take_seek(&self) -> Option<(f64, bool)> {
        let mut seek = self.seek.lock().unwrap();
        seek.pending.take().map(|t| (t, seek.quick))
    }
}

pub enum Msg {
    Packet(ff::Packet),
    /// Sent after a seek: flush the decoder and tag new output with this serial.
    Flush(u64),
    Eof,
}

#[derive(Default)]
pub struct PacketQueue {
    inner: Mutex<(VecDeque<Msg>, usize)>,
    ready: Condvar,
    /// Packets kept at most; None for no limit.
    limit: Option<usize>,
    /// Set when the track is switched away: the decoder reading this queue exits.
    closed: AtomicBool,
}

impl PacketQueue {
    /// A queue that drops its oldest packet once it holds `limit`.
    pub fn bounded(limit: usize) -> Self {
        PacketQueue {
            limit: Some(limit),
            ..Default::default()
        }
    }

    pub(crate) fn push(&self, msg: Msg) {
        let mut q = self.inner.lock().unwrap();
        if let Msg::Packet(p) = &msg {
            q.1 += p.size();
        }
        q.0.push_back(msg);
        if self.limit.is_some_and(|limit| q.0.len() > limit) {
            // Drop the oldest packet but keep Flush and Eof, which the reader still needs.
            let oldest = q.0.iter().position(|m| matches!(m, Msg::Packet(_)));
            if let Some(Msg::Packet(p)) = oldest.and_then(|i| q.0.remove(i)) {
                q.1 -= p.size();
            }
        }
        self.ready.notify_one();
    }

    /// Tells the decoder reading this queue to exit, without stopping the rest of playback.
    pub fn close(&self) {
        self.closed.store(true, Relaxed);
        self.ready.notify_all();
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Relaxed)
    }

    /// Waits up to `POLL` so callers can notice `quit`.
    pub fn pop(&self) -> Option<Msg> {
        let q = self.inner.lock().unwrap();
        let (mut q, _) = self
            .ready
            .wait_timeout_while(q, POLL, |q| q.0.is_empty())
            .unwrap();
        let msg = q.0.pop_front()?;
        if let Msg::Packet(p) = &msg {
            q.1 -= p.size();
        }
        Some(msg)
    }

    fn clear(&self) {
        *self.inner.lock().unwrap() = (VecDeque::new(), 0);
    }

    fn len_and_bytes(&self) -> (usize, usize) {
        let q = self.inner.lock().unwrap();
        (q.0.len(), q.1)
    }
}

pub struct VideoFrame {
    pub frame: ff::frame::Video,
    pub pts: f64,
    pub serial: u64,
}

#[derive(Default)]
pub struct FrameQueue {
    frames: Mutex<VecDeque<VideoFrame>>,
    space: Condvar,
}

impl FrameQueue {
    /// Blocks while the queue is full. Drops the frame instead if a seek made it stale or the
    /// decoder's packet queue `source` was closed.
    fn push(&self, frame: VideoFrame, shared: &Shared, source: &PacketQueue) {
        let mut q = self.frames.lock().unwrap();
        loop {
            if shared.quitting() || source.closed() || frame.serial != shared.serial.load(Relaxed) {
                return;
            }
            if q.len() < FRAME_QUEUE_LEN {
                q.push_back(frame);
                return;
            }
            q = self.space.wait_timeout(q, POLL).unwrap().0;
        }
    }

    /// Runs `f` with the queue locked, then wakes the decoder in case `f` made room.
    pub fn with<R>(&self, f: impl FnOnce(&mut VecDeque<VideoFrame>) -> R) -> R {
        let r = f(&mut self.frames.lock().unwrap());
        self.space.notify_one();
        r
    }
}

/// A D3D11VA device created by FFmpeg. The decoder writes into its textures and the renderer
/// presents them with the same ID3D11Device, so decoded frames never leave the GPU.
pub struct HwDevice(*mut ffi::AVBufferRef);

impl HwDevice {
    /// Wraps our own device rather than letting FFmpeg create one: Direct2D draws the interface
    /// on the same device, and that needs BGRA support, which FFmpeg's device lacks.
    pub fn from_d3d11(device: &ID3D11Device) -> Option<Self> {
        unsafe {
            let buf = ffi::av_hwdevice_ctx_alloc(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
            if buf.is_null() {
                return None;
            }
            let ctx = (*buf).data as *mut ffi::AVHWDeviceContext;
            let d3d11 = (*ctx).hwctx as *mut ffi::AVD3D11VADeviceContext;
            // FFmpeg releases this reference when the context is freed.
            (*d3d11).device = device.clone().into_raw().cast();
            let mut hw = HwDevice(buf);
            if ffi::av_hwdevice_ctx_init(hw.0) < 0 {
                ffi::av_buffer_unref(&mut hw.0);
                return None;
            }
            Some(hw)
        }
    }

    pub fn d3d11(&self) -> &ffi::AVD3D11VADeviceContext {
        unsafe {
            let device = (*self.0).data as *const ffi::AVHWDeviceContext;
            &*((*device).hwctx as *const ffi::AVD3D11VADeviceContext)
        }
    }

    /// Whether the GPU's video decoder lists an AV1 profile.
    fn decodes_av1(&self) -> bool {
        unsafe {
            let raw = self.d3d11().video_device as *mut std::ffi::c_void;
            let Some(video) = ID3D11VideoDevice::from_raw_borrowed(&raw) else {
                return false;
            };
            (0..video.GetVideoDecoderProfileCount()).any(|i| {
                video
                    .GetVideoDecoderProfile(i)
                    .is_ok_and(|p| p == D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0)
            })
        }
    }
}

/// Another reference to the same device, so a session can open decoders after `Session::open`.
impl Clone for HwDevice {
    fn clone(&self) -> Self {
        HwDevice(unsafe { ffi::av_buffer_ref(self.0) })
    }
}

impl Drop for HwDevice {
    fn drop(&mut self) {
        unsafe { ffi::av_buffer_unref(&mut self.0) }
    }
}

/// libavcodec offers the formats it can decode into; take D3D11 textures when the GPU supports
/// the stream, otherwise let FFmpeg pick its software format.
unsafe extern "C" fn prefer_d3d11(
    ctx: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    unsafe {
        let mut p = formats;
        while *p != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if *p == ffi::AVPixelFormat::AV_PIX_FMT_D3D11 {
                return *p;
            }
            p = p.add(1);
        }
        // libavcodec asks again on every failed frame, so warn once.
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Relaxed) {
            eprintln!("video: the GPU cannot decode this stream, trying software");
        }
        ffi::avcodec_default_get_format(ctx, formats)
    }
}

pub fn open_video_decoder(
    parameters: ff::codec::Parameters,
    time_base: ff::Rational,
    hw: Option<&HwDevice>,
) -> Result<ff::decoder::Video, ff::Error> {
    let id = parameters.id();
    // Only these have D3D11VA hwaccels in the build. Anything else (MJPEG webcams, BMP screen
    // grabs, MPEG-4 Part 2) decodes in software from the start, instead of being opened for the
    // GPU, turned down and reopened, which also seeks, and live sources can't.
    use ff::codec::Id;
    let gpu = matches!(
        id,
        Id::H264 | Id::HEVC | Id::AV1 | Id::VP9 | Id::MPEG2VIDEO | Id::VC1 | Id::WMV3
    );
    let hw = hw.filter(|_| gpu);
    // libavcodec prefers libdav1d for AV1, and libdav1d never uses the GPU. Take FFmpeg's own AV1
    // decoder when the GPU has an AV1 profile; otherwise keep libdav1d and give it every core.
    // A stream the GPU then turns down is reopened in software by the decoder thread.
    let (codec, hw) = match hw {
        Some(device) if id == ff::codec::Id::AV1 && device.decodes_av1() => {
            (ff::decoder::find_by_name("av1"), hw)
        }
        _ if id == ff::codec::Id::AV1 => (ff::decoder::find(id), None),
        _ => (ff::decoder::find(id), hw),
    };
    let codec = codec.ok_or(ff::Error::DecoderNotFound)?;
    let mut ctx = ff::codec::Context::from_parameters(parameters)?;
    unsafe {
        let raw = ctx.as_mut_ptr();
        (*raw).pkt_timebase = time_base.into();
        match hw {
            Some(hw) => {
                (*raw).hw_device_ctx = ffi::av_buffer_ref(hw.0);
                (*raw).get_format = Some(prefer_d3d11);
                (*raw).extra_hw_frames = EXTRA_HW_FRAMES;
            }
            // The hardware path decodes on one thread to keep the surface pool small; software
            // takes every core.
            None => (*raw).thread_count = 0,
        }
    }
    ctx.decoder().open_as(codec)?.video()
}

pub fn open_audio_decoder(
    parameters: ff::codec::Parameters,
    time_base: ff::Rational,
) -> Result<ff::decoder::Audio, ff::Error> {
    let mut ctx = ff::codec::Context::from_parameters(parameters)?;
    unsafe { (*ctx.as_mut_ptr()).pkt_timebase = time_base.into() };
    ctx.decoder().audio()
}

/// Where the demuxer sends one stream's packets.
pub struct Route {
    pub stream: usize,
    pub queue: Arc<PacketQueue>,
    /// Subtitle streams have long gaps between packets, so throttling ignores them.
    pub sparse: bool,
}

/// The routing table. Track selection edits it while the demuxer runs, which reads it per packet.
pub type Routes = Arc<Mutex<Vec<Route>>>;

/// Reads packets and routes them by stream index. Also performs seeks, since only this thread
/// touches the format context.
///
/// A `follower` reads a second file played alongside, like an external audio track. It leaves
/// seek requests to the main demuxer and seeks its own file after each one, to the same target
/// and under the same serial, so its decoder flushes and drops stale samples like the others.
pub fn spawn_demuxer(
    mut input: ff::format::context::Input,
    routes: Routes,
    shared: Arc<Shared>,
    role: Role,
    reconnect: Option<Reconnect>,
) -> JoinHandle<()> {
    let (follower, copies, reopen) = match role {
        Role::Main(copies, reopen) => (false, copies, reopen),
        Role::Follower => (true, Vec::new(), None),
    };
    // Read before the thread starts, which may be after the seek that selects this file's track.
    let mut followed = shared.serial.load(Relaxed);
    thread::spawn(move || {
        let mut eof = false;
        // A recording and a stream, each running or not.
        let mut recorders: Vec<Option<record::Recorder>> = copies.iter().map(|_| None).collect();
        // Where playback is in the input, for the recorders; the clock is unknown just after a
        // seek, and the last value stands in.
        let mut playhead = 0.0;
        // Subtitle streams already sent what shows at the reading position from before it.
        let mut prerolled: Vec<usize> = Vec::new();
        while !shared.quitting() {
            playhead = shared.clock.get().unwrap_or(playhead);
            for (control, recorder) in copies.iter().zip(&mut recorders) {
                if record::sync(recorder, control, &input, playhead) {
                    shared.seek_by(0.0);
                }
                if let Some(r) = recorder {
                    r.play_to(playhead);
                }
            }
            let (target, quick) = if follower {
                let now = shared.serial.load(Relaxed);
                let target = (now != followed).then(|| {
                    followed = now;
                    shared.seek_floor()
                });
                (target, None)
            } else {
                shared.take_seek().unzip()
            };
            let quick = quick.unwrap_or(false);
            if let Some(target) = target {
                // Lands on the keyframe at or before the target; the decoders skip up to the target.
                let ts = (target.max(0.0) * f64::from(ffi::AV_TIME_BASE)) as i64;
                let played = routes.lock().unwrap().first().map(|r| r.stream);
                let sought = match played.filter(|_| follower) {
                    Some(stream) => seek_stream(&mut input, stream, ts),
                    None => input.seek(ts, ..ts + 1),
                };
                if let Err(e) = sought {
                    eprintln!("seek to {target:.1}s failed: {e}");
                    continue;
                }
                let serial = if follower {
                    followed
                } else {
                    shared.seek_floor.store(target.max(0.0).to_bits(), Relaxed);
                    shared.exact.store(!quick, Relaxed);
                    shared.serial.fetch_add(1, Relaxed) + 1
                };
                for r in routes.lock().unwrap().iter() {
                    r.queue.clear();
                    r.queue.push(Msg::Flush(serial));
                }
                if !follower {
                    shared.clock.invalidate();
                }
                for r in recorders.iter_mut().flatten() {
                    r.seeked();
                }
                eof = false;
                // Quick seeks come many a second from a dragged seek bar; the exact one that
                // ends the drag reads the subtitles before it.
                if !quick {
                    prerolled.clear();
                }
            }
            if let Some(reopen) = &reopen {
                let fresh: Vec<_> = {
                    let routes = routes.lock().unwrap();
                    prerolled.retain(|s| routes.iter().any(|r| r.sparse && r.stream == *s));
                    (routes.iter())
                        .filter(|r| r.sparse && !prerolled.contains(&r.stream))
                        .map(|r| (r.stream, r.queue.clone()))
                        .collect()
                };
                if !fresh.is_empty() {
                    prerolled.extend(fresh.iter().map(|(s, _)| *s));
                    let at = shared.clock.get().unwrap_or(shared.seek_floor());
                    if at > 0.0 {
                        spawn_preroll(reopen.clone(), fresh, at, shared.clone());
                    }
                }
            }

            let (enough, bytes) = routes.lock().unwrap().iter().filter(|r| !r.sparse).fold(
                (true, 0),
                |(enough, bytes), r| {
                    let (n, b) = r.queue.len_and_bytes();
                    (enough && n >= ENOUGH_PACKETS, bytes + b)
                },
            );
            if eof || enough || bytes > MAX_QUEUED_BYTES {
                thread::sleep(POLL / 2);
                continue;
            }

            let mut packet = ff::Packet::empty();
            match packet.read(&mut input) {
                Ok(()) => {
                    for r in recorders.iter_mut().flatten() {
                        r.push(&packet);
                    }
                    let routes = routes.lock().unwrap();
                    if let Some(r) = routes.iter().find(|r| r.stream == packet.stream()) {
                        r.queue.push(Msg::Packet(packet));
                    }
                }
                // A live stream with nothing to read yet. FFmpeg reconnects http and HLS itself
                // (see `session::open_input`); RTSP and UDP open again here.
                Err(ff::Error::Other { errno }) if errno == ffi::EAGAIN => thread::sleep(POLL / 2),
                Err(e) if let Some(reconnect) = &reconnect => {
                    eprintln!("stream: dropped ({e}), reconnecting");
                    // A copy can't go on across the break; it ends there.
                    for (control, recorder) in copies.iter().zip(&mut recorders) {
                        record::stop(recorder, control, playhead);
                    }
                    // The old connection closes first: a UDP port takes one listener at a time.
                    drop(input);
                    let Some(again) = reconnect.reopen(&shared) else {
                        for r in routes.lock().unwrap().iter() {
                            r.queue.push(Msg::Eof);
                        }
                        return;
                    };
                    input = again;
                    // Like a seek: the decoders start over, and the clock from the new packets,
                    // whose times may begin anywhere.
                    shared
                        .seek_floor
                        .store(f64::NEG_INFINITY.to_bits(), Relaxed);
                    let serial = shared.serial.fetch_add(1, Relaxed) + 1;
                    for r in routes.lock().unwrap().iter() {
                        r.queue.clear();
                        r.queue.push(Msg::Flush(serial));
                    }
                    shared.clock.invalidate();
                }
                Err(_) => {
                    for r in routes.lock().unwrap().iter() {
                        r.queue.push(Msg::Eof);
                    }
                    eof = true;
                }
            }
        }
        // The media closed, at its end (the recording ends with it, as in VLC) or before.
        for (control, recorder) in copies.iter().zip(&mut recorders) {
            record::stop(recorder, control, playhead);
        }
    })
}

/// Which demuxer a thread is: the open file's, which owns seeks and can record, or a second
/// file's (an external audio track) that follows its seeks.
pub enum Role {
    /// With the controls of its copies (recording and streaming out), and for a local file a
    /// way to open it again for `spawn_preroll`.
    Main(Vec<record::Control>, Option<Reopen>),
    Follower,
}

/// Reads the `SUBTITLE_PREROLL` seconds before `at` from a second opening of the file and sends
/// the packets of `streams` found there, so a line that began before a seek, a resume or a track
/// switch shows instead of waiting for the next one. The demuxer reads on from a keyframe at or
/// before `at`, so both may send a line; libass and `Cues` keep one. Gives up at the next seek.
// ponytail: reads every stream's packets in those seconds; discard the others and stop by byte
// position if seeking big remuxes on a slow disk stutters.
fn spawn_preroll(
    reopen: Reopen,
    streams: Vec<(usize, Arc<PacketQueue>)>,
    at: f64,
    shared: Arc<Shared>,
) {
    let serial = shared.serial.load(Relaxed);
    thread::spawn(move || {
        let Ok(mut input) = reopen() else {
            return;
        };
        let bases: Vec<f64> = input.streams().map(|s| f64::from(s.time_base())).collect();
        let from = ((at - SUBTITLE_PREROLL).max(0.0) * f64::from(ffi::AV_TIME_BASE)) as i64;
        if input.seek(from, ..from + 1).is_err() {
            return;
        }
        loop {
            let mut packet = ff::Packet::empty();
            if packet.read(&mut input).is_err()
                || shared.quitting()
                || shared.serial.load(Relaxed) != serial
            {
                return;
            }
            let Some(pts) = packet.pts() else {
                continue;
            };
            let t = pts as f64 * bases[packet.stream()];
            // Video comes in decoding order, a few frames ahead of its own times.
            if t > at + 1.0 {
                return;
            }
            if t <= at
                && let Some((_, queue)) = streams.iter().find(|(s, _)| *s == packet.stream())
            {
                queue.push(Msg::Packet(packet));
            }
        }
    });
}

/// Seeks `input` to the keyframe of `stream` at or before `ts` (in `AV_TIME_BASE` units). A file
/// with video otherwise seeks on that video's keyframes, which can be seconds before `ts`.
fn seek_stream(
    input: &mut ff::format::context::Input,
    stream: usize,
    ts: i64,
) -> Result<(), ff::Error> {
    unsafe {
        let ctx = input.as_mut_ptr();
        let base = ffi::AVRational {
            num: 1,
            den: ffi::AV_TIME_BASE,
        };
        let ts = ffi::av_rescale_q(ts, base, (**(*ctx).streams.add(stream)).time_base);
        match ffi::avformat_seek_file(ctx, stream as i32, i64::MIN, ts, ts, 0) {
            e if e < 0 => Err(ff::Error::from(e)),
            _ => Ok(()),
        }
    }
}

/// `decoder`'s stream reopened for software decoding on every core (libdav1d for AV1).
fn software_decoder(decoder: &ff::decoder::Video) -> Option<ff::decoder::Video> {
    let time_base = unsafe { (*decoder.as_ptr()).pkt_timebase }.into();
    open_video_decoder(ff::codec::Parameters::from(decoder), time_base, None).ok()
}

pub fn spawn_video_decoder(
    mut decoder: ff::decoder::Video,
    time_base: f64,
    frame_duration: f64,
    packets: Arc<PacketQueue>,
    frames: Arc<FrameQueue>,
    shared: Arc<Shared>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut serial = shared.serial.load(Relaxed);
        let mut next_pts = 0.0;
        let mut format = None;
        let mut failures = 0;
        let mut nv12 = crate::nv12::Nv12::default();
        // Opened for the GPU. If the GPU turns the stream down, libavcodec falls back to
        // software on one thread, or (FFmpeg's own AV1 decoder) can't decode it at all; either
        // way the stream is reopened in software and decoded again from the last seek.
        let mut gpu = unsafe { !(*decoder.as_ptr()).hw_device_ctx.is_null() };
        let to_software = |decoder: &mut ff::decoder::Video, gpu: &mut bool| {
            *gpu = false;
            let Some(software) = software_decoder(decoder) else {
                return false;
            };
            *decoder = software;
            eprintln!("video: reopened in software on every core");
            shared.seek_to(shared.seek_floor());
            true
        };
        while !shared.quitting() && !packets.closed() {
            let msg = packets.pop();
            // An undecodable stream is still drained, so the demuxer and the audio carry on.
            if failures >= GIVE_UP_AFTER {
                continue;
            }
            let mut failed = match msg {
                None => continue,
                Some(Msg::Packet(p)) => decoder.send_packet(&p).is_err(),
                Some(Msg::Flush(s)) => {
                    decoder.flush();
                    serial = s;
                    shared.video_done.store(false, Relaxed);
                    continue;
                }
                Some(Msg::Eof) => {
                    _ = decoder.send_eof();
                    false
                }
            };
            loop {
                let mut frame = ff::frame::Video::empty();
                match decoder.receive_frame(&mut frame) {
                    Ok(()) => {
                        if gpu
                            && frame.format() != ff::format::Pixel::D3D11
                            && to_software(&mut decoder, &mut gpu)
                        {
                            break;
                        }
                        if format != Some(frame.format()) {
                            format = Some(frame.format());
                            match frame.format() {
                                ff::format::Pixel::D3D11 => {
                                    eprintln!("video: decoding on the GPU (D3D11VA)")
                                }
                                f => eprintln!("video: decoding in software ({f:?} frames)"),
                            }
                        }
                        let pts = frame.timestamp().map_or(next_pts, |t| t as f64 * time_base);
                        next_pts = pts + frame_duration;
                        // Decoded only to reach the seek target from the keyframe before it.
                        if shared.exact() && pts + frame_duration / 2.0 < shared.seek_floor() {
                            continue;
                        }
                        // Software frames are repacked for the GPU here, off the render thread.
                        let frame = match frame.format() {
                            ff::format::Pixel::D3D11 => frame,
                            _ => match nv12.convert(&frame) {
                                Ok(f) => f,
                                Err(e) => {
                                    eprintln!("video: cannot repack a frame ({e})");
                                    continue;
                                }
                            },
                        };
                        frames.push(VideoFrame { frame, pts, serial }, &shared, &packets);
                    }
                    Err(ff::Error::Eof) => {
                        shared.video_done.store(true, Relaxed);
                        break;
                    }
                    Err(ff::Error::Other {
                        errno: ff::util::error::EAGAIN,
                    }) => break,
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            // Once frames flow, a corrupt packet only costs its own frame. Failing from the start
            // means neither the GPU nor this FFmpeg build can decode the stream (AV1 without a
            // GPU decoder, for example), so stop feeding it and let the audio play on.
            if failed && format.is_none() {
                failures += 1;
                if gpu && failures == FALLBACK_AFTER && to_software(&mut decoder, &mut gpu) {
                    failures = 0;
                    continue;
                }
                if failures == GIVE_UP_AFTER {
                    eprintln!(
                        "video: cannot decode this stream on this machine, playing audio only"
                    );
                    shared.video_done.store(true, Relaxed);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_queue_drops_oldest_packets_but_keeps_markers() {
        let q = PacketQueue::bounded(3);
        q.push(Msg::Flush(7));
        for i in 0..5 {
            let mut p = ff::Packet::new(10);
            p.set_pts(Some(i));
            q.push(Msg::Packet(p));
        }
        assert_eq!(q.len_and_bytes(), (3, 20));
        let Some(Msg::Flush(7)) = q.pop() else {
            panic!("the flush marker was dropped")
        };
        let pts: Vec<_> = std::iter::from_fn(|| match q.pop()? {
            Msg::Packet(p) => p.pts(),
            _ => None,
        })
        .collect();
        assert_eq!(pts, [3, 4]);
        assert!(!q.closed());
        q.close();
        assert!(q.closed());
    }
}

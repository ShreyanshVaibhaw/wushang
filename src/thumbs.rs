//! Thumbnails and tag probing for the library, on background worker threads.
//!
//! Both run FFmpeg in software on their own contexts, so they never touch the player's GPU device.

use std::collections::HashSet;
use std::ffi::CStr;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use ff::ffi;
use ff::format::stream::Disposition;
use ff::media::Type;
use ffmpeg_next as ff;

/// Frames decoded after the seek before settling for the last one, about 2 s at 30 fps.
const MAX_FRAMES: u32 = 60;
/// Video packets read without reaching the target before giving up on the stream.
const MAX_PACKETS: u32 = 600;
/// The still comes from 10% into the video, but never later than this.
const LATEST_SECS: f64 = 30.0;

/// A decoded still, BGRA8 premultiplied (opaque), `width * 4` bytes per row.
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaInfo {
    pub duration: f64,
    pub has_video: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
}

/// Opens a file. ffmpeg-next unwraps the path as UTF-8, so other paths are refused here instead.
/// Files only: a stream address would download from the network just for a thumbnail or tags.
fn open(path: &Path) -> Option<ff::format::context::Input> {
    if crate::net::is_address(path) {
        return None;
    }
    ff::format::input(path.to_str()?).ok()
}

/// A real video stream: cover art also comes as a video stream, flagged as an attached picture.
fn is_video(s: &ff::Stream) -> bool {
    s.parameters().medium() == Type::Video && !s.disposition().contains(Disposition::ATTACHED_PIC)
}

/// Reads a tag from a raw dictionary. FFmpeg matches keys case-insensitively. The value is read
/// lossily because ffmpeg-next's own `get` assumes UTF-8, which a broken file can violate.
fn tag(dict: *const ffi::AVDictionary, key: &CStr) -> Option<String> {
    unsafe {
        let entry = ffi::av_dict_get(dict, key.as_ptr(), std::ptr::null(), 0);
        if entry.is_null() {
            return None;
        }
        let value = CStr::from_ptr((*entry).value).to_string_lossy();
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    }
}

/// Reads duration, stream kinds and tags without decoding (tags: case-insensitive keys title/artist/album/genre from the container and, failing that, the first audio stream; treat empty strings as None).
pub fn probe(path: &Path) -> Option<MediaInfo> {
    let input = open(path)?;
    let audio = input
        .streams()
        .find(|s| s.parameters().medium() == Type::Audio);
    let get = |key: &CStr| unsafe {
        tag((*input.as_ptr()).metadata, key).or_else(|| {
            audio
                .as_ref()
                .and_then(|s| tag((*s.as_ptr()).metadata, key))
        })
    };
    Some(MediaInfo {
        // AV_NOPTS_VALUE is i64::MIN, so unknown durations land here as 0.
        duration: input.duration().max(0) as f64 / f64::from(ffi::AV_TIME_BASE),
        has_video: input.streams().any(|s| is_video(&s)),
        title: get(c"title"),
        artist: get(c"artist"),
        album: get(c"album"),
        genre: get(c"genre"),
    })
}

/// One still `width` x `height`, scaled to cover the box and centre-cropped (no letterbox).
/// Video: a frame at 10% of the duration, but no later than 30 s (seek to the keyframe before, then decode until the first frame at/after the target or give up after ~60 frames and use the last one); fall back to the first frame if seeking fails.
/// Audio: the embedded cover picture (attached_pic stream, decoded with the mjpeg decoder) if any, else None.
/// Software decoding only (never touch the GPU), thread_count 2. Must not panic on corrupt input; return None.
pub fn render(path: &Path, width: u32, height: u32) -> Option<Thumbnail> {
    if width == 0 || height == 0 {
        return None;
    }
    let mut input = open(path)?;
    // A video whose stream cannot be decoded still gets its cover art, if it carries one.
    let frame = video_frame(&mut input, path).or_else(|| cover_frame(&input))?;
    scale_cover(frame, width, height)
}

/// `skim` trades quality nobody sees at thumbnail size for speed: no deblocking, and no decoding
/// of the frames it names (non-reference ones, B-frames mostly, cut render() by about a third).
fn open_decoder(stream: &ff::Stream, skim: Option<ffi::AVDiscard>) -> Option<ff::decoder::Video> {
    let mut ctx = ff::codec::Context::from_parameters(stream.parameters()).ok()?;
    // No hw_device_ctx, so this is software. For AV1 libavcodec picks libdav1d, which needs no GPU.
    unsafe {
        let raw = ctx.as_mut_ptr();
        (*raw).pkt_timebase = stream.time_base().into();
        (*raw).thread_count = 2;
        if let Some(skip) = skim {
            (*raw).skip_loop_filter = ffi::AVDiscard::AVDISCARD_ALL;
            (*raw).skip_frame = skip;
        }
    }
    ctx.decoder().video().ok()
}

fn video_frame(input: &mut ff::format::context::Input, path: &Path) -> Option<ff::frame::Video> {
    if !input.streams().any(|s| is_video(&s)) {
        return None;
    }
    // AV_NOPTS_VALUE is i64::MIN, so an unknown start or duration clamps to 0.
    let start = unsafe { (*input.as_ptr()).start_time }.max(0);
    let latest = LATEST_SECS * f64::from(ffi::AV_TIME_BASE);
    let offset = (input.duration().max(0) as f64 * 0.1).min(latest) as i64;
    if offset > 0 {
        // Saturating, since a corrupt header can put anything in start_time.
        let target = start.saturating_add(offset);
        let secs = target as f64 / f64::from(ffi::AV_TIME_BASE);
        if input.seek(target, ..target.saturating_add(1)).is_ok()
            && let Some(frame) = decode_until(input, secs)
        {
            return Some(frame);
        }
        // The seek failed or found nothing: start over and take the first frame.
        return decode_until(&mut open(path)?, f64::NEG_INFINITY);
    }
    decode_until(input, f64::NEG_INFINITY)
}

/// Decodes the first video stream until a frame at or after `target` seconds, settling for the
/// last frame after `MAX_FRAMES` or at the end of the file.
fn decode_until(input: &mut ff::format::context::Input, target: f64) -> Option<ff::frame::Video> {
    let stream = input.streams().find(is_video)?;
    let index = stream.index();
    let time_base = f64::from(stream.time_base());
    let mut decoder = open_decoder(&stream, Some(ffi::AVDiscard::AVDISCARD_NONREF))?;
    let mut last = None;
    let (mut frames, mut packets) = (0, 0);
    let mut eof = false;
    while !eof && packets < MAX_PACKETS {
        let mut packet = ff::Packet::empty();
        match packet.read(input) {
            Ok(()) if packet.stream() == index => {
                packets += 1;
                // A corrupt packet only costs its own frame.
                _ = decoder.send_packet(&packet);
            }
            Ok(()) => continue,
            Err(_) => {
                _ = decoder.send_eof();
                eof = true;
            }
        }
        let mut frame = ff::frame::Video::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            frames += 1;
            let pts = frame
                .timestamp()
                .map_or(f64::NEG_INFINITY, |t| t as f64 * time_base);
            if pts >= target || frames >= MAX_FRAMES {
                return Some(frame);
            }
            last = Some(std::mem::replace(&mut frame, ff::frame::Video::empty()));
        }
    }
    last
}

/// Decodes the attached cover picture, if the file has one and the build has its decoder (mjpeg;
/// this FFmpeg has no PNG decoder, so PNG covers give None).
fn cover_frame(input: &ff::format::context::Input) -> Option<ff::frame::Video> {
    let stream = input
        .streams()
        .find(|s| s.disposition().contains(Disposition::ATTACHED_PIC))?;
    // The demuxer keeps the picture in the stream itself, so no packets need reading.
    let data = unsafe {
        let pic = &(*stream.as_ptr()).attached_pic;
        if pic.data.is_null() || pic.size <= 0 {
            return None;
        }
        std::slice::from_raw_parts(pic.data, pic.size as usize)
    };
    let mut decoder = open_decoder(&stream, None)?;
    decoder.send_packet(&ff::Packet::copy(data)).ok()?;
    _ = decoder.send_eof();
    let mut frame = ff::frame::Video::empty();
    decoder.receive_frame(&mut frame).ok()?;
    Some(frame)
}

/// Scales `frame` so it covers `width` x `height`, then copies the centre into a tight buffer.
fn scale_cover(mut frame: ff::frame::Video, width: u32, height: u32) -> Option<Thumbnail> {
    use ff::format::Pixel;
    let (sw, sh) = (frame.width(), frame.height());
    if sw == 0 || sh == 0 {
        return None;
    }
    let scale = f64::max(
        f64::from(width) / f64::from(sw),
        f64::from(height) / f64::from(sh),
    );
    let cw = ((f64::from(sw) * scale).round() as u32).max(width);
    let ch = ((f64::from(sh) * scale).round() as u32).max(height);
    // The mjpeg decoder hands out the deprecated full-range YUVJ formats, which make swscale warn.
    // The planes are laid out the same, so relabel them and pass the range on its own instead.
    let (format, full) = match frame.format() {
        Pixel::YUVJ420P => (Pixel::YUV420P, true),
        Pixel::YUVJ422P => (Pixel::YUV422P, true),
        Pixel::YUVJ444P => (Pixel::YUV444P, true),
        Pixel::YUVJ440P => (Pixel::YUV440P, true),
        Pixel::YUVJ411P => (Pixel::YUV411P, true),
        f => (f, frame.color_range() == ff::color::Range::JPEG),
    };
    frame.set_format(format);
    let mut scaler = ff::software::scaling::Context::get(
        format,
        sw,
        sh,
        Pixel::BGRA,
        cw,
        ch,
        ff::software::scaling::Flags::BILINEAR,
    )
    .ok()?;
    // swscale's YUV matrix is indexed by AVColorSpace. Untagged HD video is almost always BT.709.
    let space = match frame.color_space() {
        ff::color::Space::Unspecified if sh >= 720 => ffi::AVColorSpace::AVCOL_SPC_BT709,
        s => s.into(),
    };
    unsafe {
        let coefs = ffi::sws_getCoefficients(space as i32);
        ffi::sws_setColorspaceDetails(
            scaler.as_mut_ptr(),
            coefs,
            full.into(),
            coefs,
            1,
            0,
            1 << 16,
            1 << 16,
        );
    }
    let mut out = ff::frame::Video::empty();
    scaler.run(&frame, &mut out).ok()?;

    let (x0, y0) = ((cw - width) / 2, (ch - height) / 2);
    let (stride, row) = (out.stride(0), width as usize * 4);
    let src = out.data(0);
    let mut bgra = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        let at = (y0 as usize + y) * stride + x0 as usize * 4;
        bgra.extend_from_slice(src.get(at..at + row)?);
    }
    // swscale leaves alpha at 255 for formats without it; force it for the few that carry one,
    // since the type promises an opaque picture.
    for px in bgra.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    Some(Thumbnail {
        width,
        height,
        bgra,
    })
}

/// A newest-first job stack served by worker threads.
struct Queue<T> {
    state: Mutex<State<T>>,
    wake: Condvar,
}

struct State<T> {
    stack: Vec<PathBuf>,
    /// Every path ever requested, so duplicates are ignored whether queued, running or done.
    seen: HashSet<PathBuf>,
    ready: Vec<(PathBuf, Option<T>)>,
    quit: bool,
}

struct Workers<T> {
    queue: Arc<Queue<T>>,
    threads: Vec<JoinHandle<()>>,
}

impl<T: Send + 'static> Workers<T> {
    fn new(count: usize, job: impl Fn(&Path) -> Option<T> + Send + Sync + 'static) -> Self {
        let queue = Arc::new(Queue {
            state: Mutex::new(State {
                stack: Vec::new(),
                seen: HashSet::new(),
                ready: Vec::new(),
                quit: false,
            }),
            wake: Condvar::new(),
        });
        let job = Arc::new(job);
        let threads = (0..count.max(1))
            .map(|_| {
                let (queue, job) = (queue.clone(), job.clone());
                thread::spawn(move || {
                    loop {
                        let path = {
                            let s = queue.state.lock().unwrap();
                            let mut s = queue
                                .wake
                                .wait_while(s, |s| s.stack.is_empty() && !s.quit)
                                .unwrap();
                            if s.quit {
                                return;
                            }
                            s.stack.pop().unwrap()
                        };
                        // A panic inside ffmpeg-next on some odd file must not kill the worker.
                        let result =
                            panic::catch_unwind(AssertUnwindSafe(|| job(&path))).unwrap_or(None);
                        queue.state.lock().unwrap().ready.push((path, result));
                    }
                })
            })
            .collect();
        Workers { queue, threads }
    }

    /// Queues the paths not seen before; the last one is served first.
    fn push(&self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut s = self.queue.state.lock().unwrap();
        for path in paths {
            if s.seen.insert(path.clone()) {
                s.stack.push(path);
            }
        }
        self.queue.wake.notify_all();
    }

    fn take_ready(&self) -> Vec<(PathBuf, Option<T>)> {
        std::mem::take(&mut self.queue.state.lock().unwrap().ready)
    }
}

impl<T> Drop for Workers<T> {
    /// Waits only for the items in flight; queued ones are dropped.
    fn drop(&mut self) {
        self.queue.state.lock().unwrap().quit = true;
        self.queue.wake.notify_all();
        for t in self.threads.drain(..) {
            _ = t.join();
        }
    }
}

/// Background workers. Requests are served newest first (a stack), so what the user is looking at now wins over items scrolled past. Duplicate requests for a path already queued, running or done are ignored.
/// A still from `secs` into a video, for the seek bar's preview: the keyframe at or before it,
/// `width` pixels wide in the shape the video shows at. Only keyframes are decoded, in software,
/// so it takes a fraction of a second even for 4K.
pub fn still_at(path: &Path, secs: f64, width: u32) -> Option<Thumbnail> {
    let mut input = open(path)?;
    let stream = input.streams().find(is_video)?;
    let index = stream.index();
    let mut decoder = open_decoder(&stream, Some(ffi::AVDiscard::AVDISCARD_NONKEY))?;
    let ts = (secs.max(0.0) * f64::from(ffi::AV_TIME_BASE)) as i64;
    input.seek(ts, ..ts.saturating_add(1)).ok()?;
    let mut frame = ff::frame::Video::empty();
    let mut got = false;
    for _ in 0..MAX_PACKETS {
        let mut packet = ff::Packet::empty();
        match packet.read(&mut input) {
            Ok(()) if packet.stream() == index => _ = decoder.send_packet(&packet),
            Ok(()) => continue,
            Err(_) => _ = decoder.send_eof(),
        }
        if decoder.receive_frame(&mut frame).is_ok() {
            got = true;
            break;
        }
    }
    if !got {
        return None;
    }
    let (w, h) = (f64::from(frame.width()), f64::from(frame.height()));
    // Anamorphic video shows wider than it is stored.
    let aspect = frame.aspect_ratio();
    let sar = if aspect.numerator() > 0 && aspect.denominator() > 0 {
        f64::from(aspect)
    } else {
        1.0
    };
    let height = (f64::from(width) * h / (w * sar)).round().max(1.0) as u32;
    scale_cover(frame, width, height)
}

/// Stills for the seek bar's preview, made on a thread of their own. Only the newest request
/// counts: one that is overtaken before its turn is dropped.
pub struct SeekPreviewer(Arc<(Mutex<PreviewState>, Condvar)>);

#[derive(Default)]
struct PreviewState {
    want: Option<(PathBuf, f64)>,
    ready: Option<(PathBuf, Option<Thumbnail>)>,
    quit: bool,
}

impl SeekPreviewer {
    /// Stills `width` pixels wide.
    pub fn new(width: u32) -> Self {
        let state = Arc::new((Mutex::new(PreviewState::default()), Condvar::new()));
        let shared = state.clone();
        thread::spawn(move || {
            let (lock, wake) = &*shared;
            loop {
                let (path, secs) = {
                    let s = lock.lock().unwrap();
                    let mut s = wake.wait_while(s, |s| s.want.is_none() && !s.quit).unwrap();
                    if s.quit {
                        return;
                    }
                    s.want.take().unwrap()
                };
                let still = still_at(&path, secs, width);
                lock.lock().unwrap().ready = Some((path, still));
            }
        });
        SeekPreviewer(state)
    }

    /// Asks for the still at `secs` into `path`, in place of any not started yet.
    pub fn request(&self, path: &Path, secs: f64) {
        let (lock, wake) = &*self.0;
        lock.lock().unwrap().want = Some((path.to_owned(), secs));
        wake.notify_one();
    }

    /// The newest still made, with the file it is from.
    pub fn take_ready(&self) -> Option<(PathBuf, Option<Thumbnail>)> {
        self.0.0.lock().unwrap().ready.take()
    }
}

impl Drop for SeekPreviewer {
    fn drop(&mut self) {
        let (lock, wake) = &*self.0;
        lock.lock().unwrap().quit = true;
        wake.notify_one();
    }
}

pub struct Thumbnailer(Workers<Thumbnail>);

impl Thumbnailer {
    pub fn new(workers: usize, width: u32, height: u32) -> Self {
        Thumbnailer(Workers::new(workers, move |p| render(p, width, height)))
    }

    pub fn request(&self, path: &Path) {
        self.0.push([path.to_path_buf()]);
    }

    /// Finished results since the last call (None = no picture for that path, so the UI stops asking).
    pub fn take_ready(&self) -> Vec<(PathBuf, Option<Thumbnail>)> {
        self.0.take_ready()
    }
}

/// Same pattern for tags: the library hands over every music file once; results trickle back.
pub struct Prober(Workers<MediaInfo>);

impl Prober {
    /// One worker thread.
    pub fn new() -> Self {
        Prober(Workers::new(1, probe))
    }

    /// Queues the paths so they are probed in the order given.
    pub fn request_all(&self, paths: Vec<PathBuf>) {
        self.0.push(paths.into_iter().rev());
    }

    pub fn take_ready(&self) -> Vec<(PathBuf, Option<MediaInfo>)> {
        self.0.take_ready()
    }
}

impl Default for Prober {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const CLIPS: [&str; 4] = [
        "test_h264.mp4",
        "test_hevc.mkv",
        "test_vp9.webm",
        "test_av1.mkv",
    ];

    fn testdata(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name)
    }

    fn temp(name: &str, bytes: &[u8]) -> PathBuf {
        // Fixed names, so reruns overwrite instead of piling up files.
        let path = std::env::temp_dir().join(format!("wushang-thumbs-{name}"));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// A 16x16 blue JPEG (0x3066ca), stripped of its JFIF and comment segments.
    const JPEG: &str = "ffd8ffdb004300083e3e493e49555555555555645d6468686864646464686868707070838383\
        707070686870707c7c83838f938f8787838793939b9b9bbabab2b2d9d9e0ffffffffc4004b000101000000000000\
        000000000000000000050101000000000000000000000000000000061001000000000000000000000000000000001101\
        00000000000000000000000000000000ffc00011080010001003012200021100031100ffda000c03010002110311003f\
        008201d8e3ffd9";

    fn chunk(id: &[u8], body: &[u8]) -> Vec<u8> {
        let pad: &[u8] = if body.len() % 2 == 1 { &[0] } else { &[] };
        [id, &(body.len() as u32).to_le_bytes(), body, pad].concat()
    }

    /// A 1 s silent WAV with RIFF INFO tags and, optionally, an ID3 chunk holding a cover JPEG.
    fn wav(name: &str, tags: &[(&[u8], &str)], cover: Option<&[u8]>) -> PathBuf {
        // PCM, mono, 8 kHz, 16000 bytes/s, 2-byte frames, 16 bits.
        let fmt = b"\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0";
        let mut info = b"INFO".to_vec();
        for (id, value) in tags {
            info.extend(chunk(id, format!("{value}\0").as_bytes()));
        }
        let mut body = [&b"WAVE"[..], &chunk(b"fmt ", fmt), &chunk(b"LIST", &info)].concat();
        if let Some(jpeg) = cover {
            // ID3v2.3 with one APIC frame: Latin-1, image/jpeg, front cover, no description.
            let apic = [&b"\0image/jpeg\0\x03\0"[..], jpeg].concat();
            let len = (apic.len() as u32).to_be_bytes();
            let frame = [&b"APIC"[..], &len, &[0, 0], &apic].concat();
            let n = frame.len() as u32;
            let size = [21, 14, 7, 0].map(|shift| (n >> shift) as u8 & 0x7f);
            let id3 = [&b"ID3\x03\0\0"[..], &size, &frame].concat();
            body.extend(chunk(b"id3 ", &id3));
        }
        body.extend(chunk(b"data", &[0; 16000]));
        temp(name, &chunk(b"RIFF", &body))
    }

    fn assert_picture(t: &Thumbnail, width: u32, height: u32) {
        assert_eq!((t.width, t.height), (width, height));
        assert_eq!(t.bgra.len(), (width * height * 4) as usize);
        assert!(t.bgra.chunks(4).all(|p| p[3] == 255));
        assert!(t.bgra.chunks(4).any(|p| p != &t.bgra[..4]), "uniform");
    }

    #[test]
    fn renders_video() {
        let started = Instant::now();
        let t = render(&testdata("test_h264.mp4"), 320, 180).unwrap();
        println!("render(test_h264.mp4) took {:?}", started.elapsed());
        assert_picture(&t, 320, 180);
        // libdav1d, cropped to a square.
        assert_picture(
            &render(&testdata("test_av1.mkv"), 200, 200).unwrap(),
            200,
            200,
        );
    }

    #[test]
    fn seek_bar_stills_keep_the_videos_shape() {
        let started = Instant::now();
        let still = still_at(&testdata("test_hevc.mkv"), 3.0, 320).unwrap();
        println!("still_at(test_hevc.mkv) took {:?}", started.elapsed());
        // 1920x1080 at 320 wide.
        assert_picture(&still, 320, 180);
        // Past the end it still finds the last keyframe; a file that isn't there gives none.
        assert!(still_at(&testdata("test_h264.mp4"), 600.0, 160).is_some());
        assert!(still_at(Path::new("C:/nonexistent/clip.mp4"), 1.0, 160).is_none());
    }

    #[test]
    fn bad_input_gives_none() {
        assert!(render(Path::new("C:/nonexistent/clip.mp4"), 320, 180).is_none());
        assert!(probe(Path::new("C:/nonexistent/clip.mp4")).is_none());
        let noise: Vec<u8> = (0..65536u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        assert!(render(&temp("noise.mkv", &noise), 320, 180).is_none());
        // A clip cut short, with its index missing: whatever comes back, no panic.
        let clip = std::fs::read(testdata("test_h264.mp4")).unwrap();
        _ = render(&temp("cut.mp4", &clip[..200_000]), 320, 180);
    }

    #[test]
    fn probes_tags_and_duration() {
        let tags: [(&[u8], &str); 4] = [
            (b"INAM", "Night Drive"),
            (b"IART", "Testers"),
            (b"IPRD", "Fixtures"),
            (b"IGNR", "Ambient"),
        ];
        let info = probe(&wav("tagged.wav", &tags, None)).unwrap();
        let some = |s: &str| Some(s.to_owned());
        let want = MediaInfo {
            duration: 1.0,
            has_video: false,
            title: some("Night Drive"),
            artist: some("Testers"),
            album: some("Fixtures"),
            genre: some("Ambient"),
        };
        assert_eq!(info, want);

        let info = probe(&testdata("test_60s.mp4")).unwrap();
        assert!(
            info.has_video && (info.duration - 60.0).abs() < 0.5,
            "{info:?}"
        );
    }

    #[test]
    fn renders_cover_art() {
        let hex = |i: usize| u8::from_str_radix(&JPEG[i..i + 2], 16).unwrap();
        let jpeg: Vec<u8> = (0..JPEG.len()).step_by(2).map(hex).collect();
        let path = wav("cover.wav", &[(b"INAM", " ")], Some(&jpeg));
        let info = probe(&path).unwrap();
        assert!(!info.has_video && info.title.is_none(), "{info:?}");
        let t = render(&path, 64, 64).unwrap();
        assert_eq!((t.width, t.height, t.bgra.len()), (64, 64, 64 * 64 * 4));
        assert!(
            t.bgra[0] > 180 && t.bgra[2] < 80 && t.bgra[3] == 255,
            "{:?}",
            &t.bgra[..4]
        );
        assert!(render(&wav("plain.wav", &[], None), 64, 64).is_none());
    }

    fn wait_for<T>(take: impl Fn() -> Vec<T>, n: usize) -> Vec<T> {
        let (mut got, started) = (Vec::new(), Instant::now());
        while got.len() < n && started.elapsed() < Duration::from_secs(30) {
            got.extend(take());
            std::thread::sleep(Duration::from_millis(10));
        }
        got
    }

    #[test]
    fn thumbnailer_serves_each_path_once() {
        let thumbs = Thumbnailer::new(2, 160, 90);
        for name in &CLIPS[..3] {
            thumbs.request(&testdata(name));
        }
        thumbs.request(&testdata(CLIPS[0]));
        let got = wait_for(|| thumbs.take_ready(), 3);
        assert_eq!(got.len(), 3);
        assert!(
            got.iter()
                .all(|(_, t)| t.as_ref().is_some_and(|t| t.width == 160))
        );
        // Paths already done are ignored too.
        thumbs.request(&testdata(CLIPS[1]));
        std::thread::sleep(Duration::from_millis(300));
        assert!(thumbs.take_ready().is_empty());
    }

    #[test]
    fn dropping_thumbnailer_is_prompt() {
        let thumbs = Thumbnailer::new(1, 320, 180);
        for name in CLIPS {
            thumbs.request(&testdata(name));
        }
        std::thread::sleep(Duration::from_millis(20));
        let started = Instant::now();
        drop(thumbs);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn prober_keeps_list_order() {
        let prober = Prober::new();
        let paths: Vec<PathBuf> = ["test_60s.mp4", "test_hevc.mkv", "missing.mp3"]
            .map(testdata)
            .into();
        prober.request_all(paths.clone());
        prober.request_all(paths.clone());
        let got = wait_for(|| prober.take_ready(), 3);
        assert_eq!(
            got.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            paths.iter().collect::<Vec<_>>()
        );
        assert!(
            got[1]
                .1
                .as_ref()
                .is_some_and(|i| i.has_video && i.duration > 5.0)
        );
        assert!(got[2].1.is_none());
    }
}

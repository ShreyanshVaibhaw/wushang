//! Converting, VLC's "Convert": a file re-encoded into an MP4 (H.264 and AAC) or an MP3. H.264
//! and AAC come from macOS's own encoders (VideoToolbox, AudioToolbox), so no encoder library is
//! needed for them; FFmpeg's software ones stand in where those are missing. MP3 needs LAME,
//! which Homebrew's FFmpeg has; without it the sound goes into an M4A instead. A job runs on a
//! thread of its own, reports how far it got, and can be cancelled; a job that doesn't finish
//! leaves no file behind.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32};
use std::sync::mpsc::{Receiver, channel};
use std::thread::{self, JoinHandle};

use ff::format::Pixel;
use ff::format::stream::Disposition;
use ff::media::Type;
use ff::{codec, decoder, encoder, frame, software};
use ffmpeg_next as ff;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// H.264 video and AAC audio in MP4.
    Mp4,
    /// The sound alone, in MP3 (AAC in M4A with an FFmpeg that has no LAME).
    Mp3,
}

/// The encoders tried for each stream, best first: macOS's own, then FFmpeg's.
const H264: [&str; 2] = ["h264_videotoolbox", "libx264"];
const AAC: [&str; 2] = ["aac_at", "aac"];
const MP3: &str = "libmp3lame";

impl Target {
    /// The output's file extension, which also names its muxer.
    pub fn extension(self) -> &'static str {
        match self {
            Target::Mp4 => "mp4",
            Target::Mp3 if encoder::find_by_name(MP3).is_some() => "mp3",
            Target::Mp3 => "m4a",
        }
    }

    /// The FFmpeg muxer for the output.
    fn muxer(self) -> &'static str {
        match self.extension() {
            // FFmpeg's name for the M4A flavour of MP4.
            "m4a" => "ipod",
            ext => ext,
        }
    }

    /// The sound encoders to try.
    fn audio_encoders(self) -> &'static [&'static str] {
        match self.extension() {
            "mp3" => &[MP3],
            _ => &AAC,
        }
    }
}

/// A conversion running in the background. Dropping it (quitting mid-conversion) cancels it
/// and waits a moment for the thread, which removes the unfinished file. A thread stuck on an
/// input that doesn't answer (a share gone quiet) isn't waited for: the file goes from here,
/// and the thread is let go, so quitting isn't held up. A conversion already writing out its
/// finished file is waited for, however long that takes, rather than have the file deleted.
pub struct Job {
    pub output: PathBuf,
    /// Thousandths done, by the input's timestamps.
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    /// `READING`, `FINISHING` or `ABANDONED`: which of the thread and `drop` decides the file.
    phase: Arc<AtomicU8>,
    done: Receiver<Result<PathBuf, String>>,
    thread: Option<JoinHandle<()>>,
}

impl Job {
    pub fn start(input: &Path, target: Target, output: PathBuf) -> Job {
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (send, done) = channel();
        let (input, out) = (input.to_owned(), output.clone());
        let phase = Arc::new(AtomicU8::new(READING));
        let (p, c, ph) = (progress.clone(), cancel.clone(), phase.clone());
        let thread = thread::spawn(move || {
            let result = run(&input, target, &out, &p, &c, &ph);
            if result.is_err() {
                _ = std::fs::remove_file(&out);
            }
            _ = send.send(result.map(|()| out));
        });
        Job {
            output,
            progress,
            cancel,
            phase,
            done,
            thread: Some(thread),
        }
    }

    /// 0..1.
    pub fn progress(&self) -> f32 {
        self.progress.load(Relaxed) as f32 / 1000.0
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }

    /// The finished file or what went wrong, once the job is over.
    pub fn finished(&self) -> Option<Result<PathBuf, String>> {
        self.done.try_recv().ok()
    }
}

/// How long dropping a job waits for its thread to stop.
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// A job's phases: reading the input, writing out the finished file, or let go by `drop`
/// while still reading. The thread and `drop` each claim the step from `READING`, so a file
/// can't be both finished and deleted.
const READING: u8 = 0;
const FINISHING: u8 = 1;
const ABANDONED: u8 = 2;

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
        let Some(t) = self.thread.take() else {
            return;
        };
        let deadline = std::time::Instant::now() + STOP_WAIT;
        while !t.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(std::time::Duration::from_millis(10));
        }
        let claim = |to| (self.phase).compare_exchange(READING, to, Relaxed, Relaxed);
        if !t.is_finished() && claim(ABANDONED).is_ok() {
            _ = std::fs::remove_file(&self.output);
        } else {
            _ = t.join();
        }
    }
}

/// Bits per pixel per frame for H.264: about 5 Mb/s for 1080p30, plenty for a copy to keep.
const VIDEO_BITS_PER_PIXEL: f64 = 0.08;
const AUDIO_BIT_RATE: usize = 192_000;

fn run(
    input: &Path,
    target: Target,
    output: &Path,
    progress: &AtomicU32,
    cancel: &AtomicBool,
    phase: &AtomicU8,
) -> Result<(), String> {
    // ffmpeg-next panics on a path that isn't valid Unicode, so those stop here.
    let Some(output) = output.to_str() else {
        return Err("wushang can't write a file whose name isn't valid Unicode".into());
    };
    let mut ictx = crate::session::open_file(input)?;
    let mut octx = ff::format::output_as(output, target.muxer())
        .map_err(|e| format!("can't write {output}: {e}"))?;
    let global_header = octx
        .format()
        .flags()
        .contains(ff::format::Flags::GLOBAL_HEADER);
    let length = (ictx.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE)).max(0.0);
    // Input seconds that land on output 0, the same for the picture and the sound so they keep
    // in step: the file's start, 1.4 s in FFmpeg's MPEG-TS files and anything in a broadcast.
    let origin = match unsafe { (*ictx.as_ptr()).start_time } {
        ff::ffi::AV_NOPTS_VALUE => 0.0,
        t => t as f64 / f64::from(ff::ffi::AV_TIME_BASE),
    };

    let video_stream = (target == Target::Mp4)
        .then(|| {
            ictx.streams()
                .filter(|s| s.parameters().medium() == Type::Video)
                .find(|s| !s.disposition().contains(Disposition::ATTACHED_PIC))
                .map(|s| s.index())
        })
        .flatten();
    let audio_stream = ictx.streams().best(Type::Audio).map(|s| s.index());
    if video_stream.is_none() && audio_stream.is_none() {
        return Err(match target {
            Target::Mp3 => "the file has no sound".into(),
            Target::Mp4 => "the file has no video or sound".into(),
        });
    }

    let mut video = match video_stream {
        Some(i) => Some(VideoPath::new(
            &ictx.stream(i).unwrap(),
            &mut octx,
            origin,
            global_header,
        )?),
        None => None,
    };
    let mut audio = match audio_stream {
        Some(i) => Some(AudioPath::new(
            &ictx.stream(i).unwrap(),
            &mut octx,
            target.audio_encoders(),
            origin,
            global_header,
        )?),
        None => None,
    };

    // FFmpeg copies the tags itself: ffmpeg-next's iterator would read them as UTF-8 unchecked.
    unsafe {
        ff::ffi::av_dict_copy(
            &mut (*octx.as_mut_ptr()).metadata,
            (*ictx.as_ptr()).metadata,
            0,
        );
    }
    let mut options = ff::Dictionary::new();
    if target.extension() != "mp3" {
        // The index up front, so the file starts playing before it has all arrived.
        options.set("movflags", "+faststart");
    }
    octx.write_header_with(options)
        .map_err(|e| format!("can't write the file: {e}"))?;
    // The muxer settles the streams' time bases while writing the header.
    for (out, base) in [
        video.as_mut().map(|v| (v.out, &mut v.out_base)),
        audio.as_mut().map(|a| (a.out, &mut a.out_base)),
    ]
    .into_iter()
    .flatten()
    {
        *base = octx.stream(out).unwrap().time_base();
    }

    for (stream, packet) in ictx.packets() {
        if cancel.load(Relaxed) {
            return Err("cancelled".into());
        }
        let index = stream.index();
        if let Some(t) = packet.pts().or(packet.dts())
            && length > 0.0
        {
            let at = t as f64 * f64::from(stream.time_base());
            progress.store((at / length * 1000.0).clamp(0.0, 999.0) as u32, Relaxed);
        }
        if let Some(v) = video.as_mut().filter(|v| v.index == index) {
            v.decode(&packet, &mut octx)?;
        } else if let Some(a) = audio.as_mut().filter(|a| a.index == index) {
            a.decode(&packet, &mut octx)?;
        }
    }
    // From here the file is written out whole, which a quit waits for, unless it let go first.
    if phase
        .compare_exchange(READING, FINISHING, Relaxed, Relaxed)
        .is_err()
    {
        return Err("cancelled".into());
    }
    if let Some(v) = video.as_mut() {
        v.finish(&mut octx)?;
    }
    if let Some(a) = audio.as_mut() {
        a.finish(&mut octx)?;
    }
    octx.write_trailer()
        .map_err(|e| format!("can't finish the file: {e}"))?;
    progress.store(1000, Relaxed);
    Ok(())
}

/// A software decoder on every core for `stream`.
fn open_decoder(stream: &ff::Stream) -> Result<codec::Context, String> {
    let mut ctx = codec::Context::from_parameters(stream.parameters())
        .map_err(|e| format!("can't decode the file: {e}"))?;
    unsafe {
        (*ctx.as_mut_ptr()).thread_count = 0;
        (*ctx.as_mut_ptr()).pkt_timebase = stream.time_base().into();
    }
    Ok(ctx)
}

/// The encoders of `names` this FFmpeg has, in order, or why there are none.
fn encoders(names: &[&str]) -> Result<Vec<ff::Codec>, String> {
    let found: Vec<ff::Codec> = names
        .iter()
        .filter_map(|n| encoder::find_by_name(n))
        .collect();
    if found.is_empty() {
        return Err(format!("this FFmpeg has no {} encoder", names.join(" or ")));
    }
    Ok(found)
}

/// Sends what `encoder` has ready to the muxer as `out`'s packets.
fn drain(
    encoder: &mut encoder::Encoder,
    from: ff::Rational,
    out: usize,
    to: ff::Rational,
    octx: &mut ff::format::context::Output,
) -> Result<(), String> {
    let mut packet = ff::Packet::empty();
    while encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(out);
        packet.rescale_ts(from, to);
        packet
            .write_interleaved(octx)
            .map_err(|e| format!("can't write the file: {e}"))?;
    }
    Ok(())
}

struct VideoPath {
    index: usize,
    in_base: f64,
    /// Input seconds that land on output 0.
    origin: f64,
    decoder: decoder::Video,
    encoder: encoder::Video,
    /// Made for the frames' size and format, which the decoder only knows once it has them.
    scaler: Option<software::scaling::Context>,
    size: (u32, u32),
    /// The encoder counts in frames.
    base: ff::Rational,
    last_pts: Option<i64>,
    out: usize,
    out_base: ff::Rational,
}

impl VideoPath {
    fn new(
        stream: &ff::Stream,
        octx: &mut ff::format::context::Output,
        origin: f64,
        global_header: bool,
    ) -> Result<VideoPath, String> {
        let decoder = open_decoder(stream)?
            .decoder()
            .video()
            .map_err(|e| format!("can't decode the video: {e}"))?;
        // H.264 in 4:2:0 wants even sides.
        let size = (decoder.width() & !1, decoder.height() & !1);
        let rate = stream.avg_frame_rate();
        let rate = if rate.numerator() > 0 && rate.denominator() > 0 {
            rate
        } else {
            ff::Rational(30, 1)
        };
        let bits = f64::from(size.0 * size.1) * f64::from(rate) * VIDEO_BITS_PER_PIXEL;
        // A hardware encoder can be there and still not start (no such hardware in a virtual
        // machine), so each is tried in turn.
        let mut last = String::new();
        let mut opened = None;
        for codec in encoders(&H264)? {
            let mut enc = codec::Context::new_with_codec(codec)
                .encoder()
                .video()
                .map_err(|e| e.to_string())?;
            enc.set_width(size.0);
            enc.set_height(size.1);
            enc.set_format(Pixel::NV12);
            enc.set_aspect_ratio(decoder.aspect_ratio());
            enc.set_frame_rate(Some(rate));
            enc.set_time_base(rate.invert());
            enc.set_bit_rate(bits.clamp(500e3, 40e6) as usize);
            if global_header {
                enc.set_flags(codec::Flags::GLOBAL_HEADER);
            }
            let mut options = ff::Dictionary::new();
            // VideoToolbox: the software encoder when the GPU has none; x264: quick enough to
            // run beside playback.
            options.set("allow_sw", "1");
            options.set("preset", "fast");
            match enc.open_as_with(codec, options) {
                Ok(encoder) => {
                    opened = Some((codec, encoder));
                    break;
                }
                Err(e) => last = e.to_string(),
            }
        }
        let (codec, encoder) =
            opened.ok_or_else(|| format!("can't start the H.264 encoder: {last}"))?;
        let mut ost = octx.add_stream(codec).map_err(|e| e.to_string())?;
        let out = ost.index();
        ost.set_parameters(&encoder);
        Ok(VideoPath {
            index: stream.index(),
            in_base: f64::from(stream.time_base()),
            origin,
            decoder,
            encoder,
            scaler: None,
            size,
            base: rate.invert(),
            last_pts: None,
            out,
            out_base: ff::Rational(1, 1),
        })
    }

    fn decode(
        &mut self,
        packet: &ff::Packet,
        octx: &mut ff::format::context::Output,
    ) -> Result<(), String> {
        // A broken packet costs a frame, not the conversion.
        _ = self.decoder.send_packet(packet);
        self.encode_ready(octx)
    }

    fn encode_ready(&mut self, octx: &mut ff::format::context::Output) -> Result<(), String> {
        let mut decoded = frame::Video::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            // Made for the first frame, and again when the picture changes size or pixel format
            // midway (a broadcast moving to another programme); the encoder keeps its size.
            let fits = |s: &software::scaling::Context| {
                let d = s.input();
                (d.format, d.width, d.height)
                    == (decoded.format(), decoded.width(), decoded.height())
            };
            if !self.scaler.as_ref().is_some_and(fits) {
                let scaler = software::scaling::Context::get(
                    decoded.format(),
                    decoded.width(),
                    decoded.height(),
                    Pixel::NV12,
                    self.size.0,
                    self.size.1,
                    software::scaling::Flags::BILINEAR,
                )
                .map_err(|e| format!("can't convert the picture: {e}"))?;
                self.scaler = Some(scaler);
            }
            let mut nv12 = frame::Video::empty();
            self.scaler
                .as_mut()
                .unwrap()
                .run(&decoded, &mut nv12)
                .map_err(|e| format!("can't convert the picture: {e}"))?;
            // Frame numbers from the timestamps, kept rising for files with uneven frame times.
            let seconds = decoded
                .timestamp()
                .map_or(0.0, |t| t as f64 * self.in_base - self.origin);
            let mut pts = (seconds / f64::from(self.base)).round() as i64;
            if let Some(last) = self.last_pts
                && pts <= last
            {
                pts = last + 1;
            }
            self.last_pts = Some(pts);
            nv12.set_pts(Some(pts));
            self.encoder
                .send_frame(&nv12)
                .map_err(|e| format!("the H.264 encoder failed: {e}"))?;
            drain(&mut self.encoder, self.base, self.out, self.out_base, octx)?;
        }
        Ok(())
    }

    fn finish(&mut self, octx: &mut ff::format::context::Output) -> Result<(), String> {
        _ = self.decoder.send_eof();
        self.encode_ready(octx)?;
        _ = self.encoder.send_eof();
        drain(&mut self.encoder, self.base, self.out, self.out_base, octx)
    }
}

struct AudioPath {
    index: usize,
    in_base: f64,
    /// Input seconds that land on output 0.
    origin: f64,
    decoder: decoder::Audio,
    encoder: encoder::Audio,
    /// Made from the first frame, whose layout the decoder only knows then.
    resampler: Option<software::resampling::Context>,
    rate: u32,
    layout: ff::ChannelLayout,
    /// The encoder's sample format: packed 16-bit for AudioToolbox, planar for LAME and FFmpeg.
    format: ff::format::Sample,
    /// Samples per encoder frame.
    frame_size: usize,
    /// Converted samples waiting to fill a frame.
    pending: Fifo,
    /// The next frame's first sample, counted from output 0; set by the first decoded sound,
    /// which can come after the picture's start.
    next: Option<i64>,
    out: usize,
    out_base: ff::Rational,
}

/// An AVAudioFifo: samples in the encoder's format, packed or planar, cut into its frames.
struct Fifo(*mut ff::ffi::AVAudioFifo);

impl Drop for Fifo {
    fn drop(&mut self) {
        unsafe { ff::ffi::av_audio_fifo_free(self.0) };
    }
}

// The FIFO is only used by the conversion thread that owns its AudioPath.
unsafe impl Send for Fifo {}

impl AudioPath {
    fn new(
        stream: &ff::Stream,
        octx: &mut ff::format::context::Output,
        names: &[&str],
        origin: f64,
        global_header: bool,
    ) -> Result<AudioPath, String> {
        let decoder = open_decoder(stream)?
            .decoder()
            .audio()
            .map_err(|e| format!("can't decode the sound: {e}"))?;
        // 44.1 or 48 kHz, in mono or stereo, which every AAC and MP3 encoder takes.
        let rate = if decoder.rate() == 44_100 {
            44_100
        } else {
            48_000
        };
        let channels = decoder.channels().clamp(1, 2) as usize;
        let layout = ff::ChannelLayout::default(channels as i32);
        let mut last = String::new();
        let mut opened = None;
        for codec in encoders(names)? {
            // The encoder's own first choice of sample format.
            let format = (codec.audio().ok())
                .and_then(|a| a.formats()?.next())
                .unwrap_or(ff::format::Sample::I16(ff::format::sample::Type::Packed));
            let mut enc = codec::Context::new_with_codec(codec)
                .encoder()
                .audio()
                .map_err(|e| e.to_string())?;
            enc.set_rate(rate as i32);
            enc.set_channel_layout(layout);
            enc.set_format(format);
            enc.set_bit_rate(AUDIO_BIT_RATE);
            enc.set_time_base(ff::Rational(1, rate as i32));
            if global_header {
                enc.set_flags(codec::Flags::GLOBAL_HEADER);
            }
            match enc.open_as(codec) {
                Ok(encoder) => {
                    opened = Some((codec, format, encoder));
                    break;
                }
                Err(e) => last = format!("can't start the {} encoder: {e}", codec.name()),
            }
        }
        let (codec, format, encoder) = opened.ok_or(last)?;
        let mut ost = octx.add_stream(codec).map_err(|e| e.to_string())?;
        let out = ost.index();
        ost.set_parameters(&encoder);
        let frame_size = match encoder.frame_size() {
            0 => 1152,
            n => n as usize,
        };
        let fifo = unsafe {
            ff::ffi::av_audio_fifo_alloc(format.into(), channels as i32, frame_size as i32)
        };
        if fifo.is_null() {
            return Err("out of memory".into());
        }
        Ok(AudioPath {
            index: stream.index(),
            in_base: f64::from(stream.time_base()),
            origin,
            decoder,
            encoder,
            resampler: None,
            rate,
            layout,
            format,
            frame_size,
            pending: Fifo(fifo),
            next: None,
            out,
            out_base: ff::Rational(1, 1),
        })
    }

    fn decode(
        &mut self,
        packet: &ff::Packet,
        octx: &mut ff::format::context::Output,
    ) -> Result<(), String> {
        _ = self.decoder.send_packet(packet);
        self.encode_ready(octx)
    }

    fn encode_ready(&mut self, octx: &mut ff::format::context::Output) -> Result<(), String> {
        let mut decoded = frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            // Plain PCM (a WAV) decodes with no channel layout, which swresample refuses.
            let layout = crate::audio::label_layout(&mut decoded);
            let (format, rate) = (decoded.format(), decoded.rate());
            if self.next.is_none() {
                let seconds = decoded
                    .timestamp()
                    .map_or(0.0, |t| t as f64 * self.in_base - self.origin);
                self.next = Some((seconds * f64::from(self.rate)).round() as i64);
            }
            // Made for the first frame, and again when the sound's shape changes midway (a
            // broadcast moving to another programme), after the old one's last samples.
            let fits = |r: &software::resampling::Context| {
                let d = r.input();
                (d.format, d.channel_layout, d.rate) == (format, layout, rate)
            };
            if !self.resampler.as_ref().is_some_and(fits) {
                self.flush_resampler()?;
                let resampler = software::resampling::Context::get(
                    format,
                    layout,
                    rate,
                    self.format,
                    self.layout,
                    self.rate,
                )
                .map_err(|e| format!("can't convert the sound: {e}"))?;
                self.resampler = Some(resampler);
            }
            // Given an empty frame, ffmpeg-next's run() makes room for the input's sample count
            // only, and upsampling's extra samples would pile up inside swresample: size it for
            // the converted count, plus slack for swresample's delay.
            let room = decoded.samples() * self.rate as usize / rate.max(1) as usize + 64;
            let mut converted = frame::Audio::new(self.format, room, self.layout);
            self.resampler
                .as_mut()
                .unwrap()
                .run(&decoded, &mut converted)
                .map_err(|e| format!("can't convert the sound: {e}"))?;
            self.queue(&converted)?;
            self.send_full_frames(octx, false)?;
        }
        Ok(())
    }

    /// Adds converted samples to those waiting for a whole encoder frame.
    fn queue(&mut self, converted: &frame::Audio) -> Result<(), String> {
        let samples = converted.samples() as i32;
        let written = unsafe {
            let data = (*converted.as_ptr()).extended_data as *const *mut c_void;
            ff::ffi::av_audio_fifo_write(self.pending.0, data, samples)
        };
        if written < samples {
            return Err("out of memory".into());
        }
        Ok(())
    }

    /// Takes the samples the resampler still holds (its filter's delay) and lets it go.
    fn flush_resampler(&mut self) -> Result<(), String> {
        let Some(mut resampler) = self.resampler.take() else {
            return Ok(());
        };
        loop {
            let mut out = frame::Audio::new(self.format, 4096, self.layout);
            resampler
                .flush(&mut out)
                .map_err(|e| format!("can't convert the sound: {e}"))?;
            if out.samples() == 0 {
                return Ok(());
            }
            self.queue(&out)?;
        }
    }

    /// Hands the encoder every whole frame pending; at the end, the rest padded with silence.
    fn send_full_frames(
        &mut self,
        octx: &mut ff::format::context::Output,
        last: bool,
    ) -> Result<(), String> {
        loop {
            let pending = unsafe { ff::ffi::av_audio_fifo_size(self.pending.0) } as usize;
            if pending == 0 || (pending < self.frame_size && !last) {
                return Ok(());
            }
            let mut f = frame::Audio::new(self.format, self.frame_size, self.layout);
            f.set_rate(self.rate);
            let next = self.next.unwrap_or(0);
            f.set_pts(Some(next));
            unsafe {
                let data = (*f.as_mut_ptr()).extended_data;
                let channels = self.layout.channels();
                ff::ffi::av_samples_set_silence(
                    data,
                    0,
                    self.frame_size as i32,
                    channels,
                    self.format.into(),
                );
                let size = self.frame_size as i32;
                ff::ffi::av_audio_fifo_read(self.pending.0, data as *const *mut c_void, size);
            }
            self.next = Some(next + self.frame_size as i64);
            self.encoder
                .send_frame(&f)
                .map_err(|e| format!("the audio encoder failed: {e}"))?;
            let base = ff::Rational(1, self.rate as i32);
            drain(&mut self.encoder, base, self.out, self.out_base, octx)?;
        }
    }

    fn finish(&mut self, octx: &mut ff::format::context::Output) -> Result<(), String> {
        _ = self.decoder.send_eof();
        self.encode_ready(octx)?;
        self.flush_resampler()?;
        self.send_full_frames(octx, true)?;
        _ = self.encoder.send_eof();
        let base = ff::Rational(1, self.rate as i32);
        drain(&mut self.encoder, base, self.out, self.out_base, octx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(target: Target) -> PathBuf {
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_h264.mp4");
        convert_file(&clip, target)
    }

    fn convert_file(clip: &Path, target: Target) -> PathBuf {
        let stem = clip.file_stem().unwrap().to_string_lossy();
        let out = std::env::temp_dir().join(format!(
            "wushang-convert-{stem}-{}.{}",
            std::process::id(),
            target.extension()
        ));
        let job = Job::start(clip, target, out.clone());
        let result = loop {
            if let Some(r) = job.finished() {
                break r;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_eq!(result, Ok(out.clone()));
        assert_eq!(job.progress(), 1.0);
        out
    }

    /// Seconds of sound in `file`, counted from its decoded samples rather than the header.
    fn sound_seconds(file: &Path) -> f64 {
        let mut input = ff::format::input(file).unwrap();
        let stream = input.streams().best(ff::media::Type::Audio).unwrap();
        let index = stream.index();
        let context = codec::Context::from_parameters(stream.parameters()).unwrap();
        let mut decoder = context.decoder().audio().unwrap();
        let (mut samples, mut rate) = (0usize, 0u32);
        let mut frame = frame::Audio::empty();
        let mut take = |d: &mut ff::decoder::Audio| {
            while d.receive_frame(&mut frame).is_ok() {
                samples += frame.samples();
                rate = frame.rate();
            }
        };
        for (s, packet) in input.packets() {
            if s.index() == index {
                _ = decoder.send_packet(&packet);
                take(&mut decoder);
            }
        }
        _ = decoder.send_eof();
        take(&mut decoder);
        samples as f64 / f64::from(rate)
    }

    /// A plain 16-bit WAV, which decodes with no channel layout, at a rate the encoder doesn't
    /// take: the sound has to be resampled up, and all of it has to come through.
    #[test]
    fn converts_a_plain_wav_at_a_low_rate() {
        let (rate, seconds) = (22_050u32, 3u32);
        let n = rate * seconds;
        let mut wav = Vec::new();
        let data = n * 2;
        for part in [
            &b"RIFF"[..],
            &(36 + data).to_le_bytes(),
            b"WAVEfmt ",
            &16u32.to_le_bytes(),
            &1u16.to_le_bytes(),
            &1u16.to_le_bytes(),
            &rate.to_le_bytes(),
            &(rate * 2).to_le_bytes(),
            &2u16.to_le_bytes(),
            &16u16.to_le_bytes(),
            b"data",
            &data.to_le_bytes(),
        ] {
            wav.extend_from_slice(part);
        }
        for i in 0..n {
            let t = f64::from(i) / f64::from(rate);
            let v = ((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16;
            wav.extend_from_slice(&v.to_le_bytes());
        }
        let clip = std::env::temp_dir().join(format!("wushang-tone-{}.wav", std::process::id()));
        std::fs::write(&clip, wav).unwrap();
        let mp3 = convert_file(&clip, Target::Mp3);
        let got = sound_seconds(&mp3);
        // An encoder's padding adds a frame or two at most.
        assert!((3.0..3.2).contains(&got), "{got} s of sound");
        std::fs::remove_file(&mp3).unwrap();
        std::fs::remove_file(&clip).unwrap();
    }

    /// The first time in each of `file`'s streams, in seconds.
    fn first_times(file: &Path) -> Vec<f64> {
        let mut input = ff::format::input(file).unwrap();
        let mut first = vec![None; input.streams().count()];
        for (stream, packet) in input.packets() {
            let slot = &mut first[stream.index()];
            if slot.is_none() {
                *slot = packet
                    .pts()
                    .map(|t| t as f64 * f64::from(stream.time_base()));
            }
        }
        first.into_iter().map(Option::unwrap).collect()
    }

    /// The test clip copied into MPEG-TS starting at 1.4 s, as broadcasts and FFmpeg's own TS
    /// files do, with the sound a second later than the picture, as some MKVs have it.
    fn late_sound_ts() -> PathBuf {
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_h264.mp4");
        let ts = std::env::temp_dir().join(format!("wushang-late-{}.ts", std::process::id()));
        let mut input = ff::format::input(&clip).unwrap();
        let mut output = ff::format::output_as(&ts, "mpegts").unwrap();
        let mut bases = Vec::new();
        for stream in input.streams() {
            let mut out = output.add_stream(encoder::find(codec::Id::None)).unwrap();
            out.set_parameters(stream.parameters());
            unsafe { (*out.parameters().as_mut_ptr()).codec_tag = 0 };
            bases.push(stream.time_base());
        }
        output.write_header().unwrap();
        for (stream, mut packet) in input.packets() {
            let i = stream.index();
            let late = if stream.parameters().medium() == Type::Audio {
                2.4
            } else {
                1.4
            };
            let late = (late / f64::from(bases[i])).round() as i64;
            packet.set_pts(packet.pts().map(|t| t + late));
            packet.set_dts(packet.dts().map(|t| t + late));
            packet.rescale_ts(bases[i], output.stream(i).unwrap().time_base());
            packet.set_position(-1);
            packet.write_interleaved(&mut output).unwrap();
        }
        output.write_trailer().unwrap();
        ts
    }

    /// An MPEG-2 TS whose picture changes size and pixel format a second in, as a broadcast's
    /// does when it moves to another programme.
    fn changing_picture_ts() -> PathBuf {
        let ts = std::env::temp_dir().join(format!("wushang-reshaped-{}.ts", std::process::id()));
        let mut output = ff::format::output_as(&ts, "mpegts").unwrap();
        let codec = encoder::find(codec::Id::MPEG2VIDEO).unwrap();
        let base = ff::Rational(1, 25);
        let open = |(w, h): (u32, u32), format: Pixel| {
            let mut enc = codec::Context::new_with_codec(codec)
                .encoder()
                .video()
                .unwrap();
            enc.set_width(w);
            enc.set_height(h);
            enc.set_format(format);
            enc.set_time_base(base);
            enc.set_frame_rate(Some(base.invert()));
            enc.open_as(codec).unwrap()
        };
        let shapes = [((320, 240), Pixel::YUV420P), ((176, 144), Pixel::YUV422P)];
        let mut ost = output.add_stream(codec).unwrap();
        ost.set_parameters(open(shapes[0].0, shapes[0].1));
        output.write_header().unwrap();
        let out_base = output.stream(0).unwrap().time_base();
        let mut pts = 0;
        for ((w, h), format) in shapes {
            let mut enc = open((w, h), format);
            let mut write = |enc: &mut encoder::Video| {
                let mut packet = ff::Packet::empty();
                while enc.receive_packet(&mut packet).is_ok() {
                    packet.set_stream(0);
                    packet.rescale_ts(base, out_base);
                    packet.write_interleaved(&mut output).unwrap();
                }
            };
            for _ in 0..25 {
                let mut picture = frame::Video::new(format, w, h);
                for plane in 0..picture.planes() {
                    picture.data_mut(plane).fill(128);
                }
                picture.set_pts(Some(pts));
                pts += 1;
                enc.send_frame(&picture).unwrap();
                write(&mut enc);
            }
            enc.send_eof().unwrap();
            write(&mut enc);
        }
        output.write_trailer().unwrap();
        ts
    }

    #[test]
    fn converts_a_picture_that_changes_shape_midway() {
        let ts = changing_picture_ts();
        let mp4 = convert_file(&ts, Target::Mp4);
        let input = ff::format::input(&mp4).unwrap();
        let seconds = input.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE);
        assert!((1.8..2.2).contains(&seconds), "{seconds} s of picture");
        drop(input);
        std::fs::remove_file(&mp4).unwrap();
        std::fs::remove_file(&ts).unwrap();
    }

    /// Quitting mid-conversion drops the job, which mustn't leave a half-written file behind.
    #[test]
    fn dropping_a_job_leaves_no_file() {
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_60s.mp4");
        let out = std::env::temp_dir().join(format!("wushang-dropped-{}.mp4", std::process::id()));
        let job = Job::start(&clip, Target::Mp4, out.clone());
        while job.progress() == 0.0 {
            thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(job);
        assert!(!out.exists(), "{} was left behind", out.display());
    }

    /// Dropping a job whose input stops answering (a share gone quiet) mid-copy doesn't wait for
    /// it, so quitting isn't held up, and the unfinished file goes. The input is a stream fed
    /// through a pipe that stops partway and stays open.
    #[test]
    fn dropping_a_stuck_job_returns_and_removes_its_file() {
        let dir = std::env::temp_dir().join(format!("wushang-stuck-{}", std::process::id()));
        _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A minute of TS, of which the pipe gets the first 6 MB before it goes quiet.
        let ts = dir.join("film.ts");
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_60s_noaudio.mp4");
        let mut input = ff::format::input(&clip).unwrap();
        let mut output = ff::format::output_as(&ts, "mpegts").unwrap();
        let mut bases = Vec::new();
        for stream in input.streams() {
            let mut out = output.add_stream(encoder::find(codec::Id::None)).unwrap();
            out.set_parameters(stream.parameters());
            unsafe { (*out.parameters().as_mut_ptr()).codec_tag = 0 };
            bases.push(stream.time_base());
        }
        output.write_header().unwrap();
        for (stream, mut packet) in input.packets() {
            let i = stream.index();
            packet.rescale_ts(bases[i], output.stream(i).unwrap().time_base());
            packet.set_position(-1);
            packet.write_interleaved(&mut output).unwrap();
        }
        output.write_trailer().unwrap();
        let bytes = std::fs::read(&ts).unwrap();
        let pipe = dir.join("never.ts");
        let made = std::process::Command::new("/usr/bin/mkfifo")
            .arg(&pipe)
            .status()
            .unwrap();
        assert!(made.success());
        let (quiet, stop) = channel::<()>();
        let feed = pipe.clone();
        let feeder = thread::spawn(move || {
            use std::io::Write;
            let mut w = std::fs::OpenOptions::new().write(true).open(feed).unwrap();
            _ = w.write_all(&bytes[..bytes.len().min(6 << 20)]);
            // Holds the pipe open, sending nothing more, until told.
            _ = stop.recv();
        });

        let out = dir.join("out.mp4");
        let job = Job::start(&pipe, Target::Mp4, out.clone());
        let start = std::time::Instant::now();
        // Through a pipe the length is unknown, so there is no progress to watch: the file
        // shows the copy has begun.
        while !out.exists() {
            if let Some(r) = job.finished() {
                panic!("the job ended: {r:?}");
            }
            assert!(start.elapsed().as_secs() < 20, "the copy never began");
            thread::sleep(std::time::Duration::from_millis(10));
        }
        // Until it has copied what the pipe had and waits on it: the file stops growing.
        let size = || std::fs::metadata(&out).map_or(0, |m| m.len());
        let mut last = (size(), std::time::Instant::now());
        while last.1.elapsed().as_millis() < 1500 {
            assert!(start.elapsed().as_secs() < 60, "the copy never settled");
            thread::sleep(std::time::Duration::from_millis(50));
            if size() != last.0 {
                last = (size(), std::time::Instant::now());
            }
        }
        if let Some(r) = job.finished() {
            panic!("the job ended: {r:?}");
        }
        let (tx, rx) = channel();
        thread::spawn(move || {
            drop(job);
            _ = tx.send(());
        });
        let dropping = std::time::Instant::now();
        let returned = rx.recv_timeout(std::time::Duration::from_secs(10)).is_ok();
        let took = dropping.elapsed();
        let left = out.exists();
        _ = quiet.send(());
        feeder.join().unwrap();
        assert!(returned, "dropping the job waited for its input");
        // It waited its while for the thread, which never stopped, rather than return at once.
        assert!(
            took >= STOP_WAIT,
            "dropped in {took:?}: the job wasn't stuck"
        );
        assert!(!left, "the unfinished file stayed");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A job already writing out its finished file (a faststart MP4 rewrites it whole, which
    /// can outlast the drop's wait) is waited for, and its file kept.
    #[test]
    fn dropping_a_finishing_job_keeps_its_file() {
        let out =
            std::env::temp_dir().join(format!("wushang-finishing-{}.mp4", std::process::id()));
        std::fs::write(&out, b"finished").unwrap();
        let (send, done) = channel();
        let phase = Arc::new(AtomicU8::new(FINISHING));
        let thread = thread::spawn(move || {
            thread::sleep(STOP_WAIT + std::time::Duration::from_secs(1));
            _ = send.send(Err(String::new()));
        });
        drop(Job {
            output: out.clone(),
            progress: Arc::new(AtomicU32::new(999)),
            cancel: Arc::new(AtomicBool::new(false)),
            phase,
            done,
            thread: Some(thread),
        });
        assert!(out.exists(), "a finished file was deleted");
        std::fs::remove_file(out).unwrap();
    }

    /// The picture and the sound keep their places against each other, and the copy starts at 0.
    #[test]
    fn keeps_the_sound_in_step_with_the_picture() {
        let ts = late_sound_ts();
        let before = first_times(&ts);
        assert!(before[0] > 1.0, "the TS starts at {:.2} s", before[0]);
        let mp4 = convert_file(&ts, Target::Mp4);
        let after = first_times(&mp4);
        assert!(
            after[0].abs() < 0.1,
            "the picture starts at {:.3} s",
            after[0]
        );
        let gap = after[1] - after[0];
        assert!(
            (gap - 1.0).abs() < 0.1,
            "the sound starts {gap:.3} s after the picture"
        );
        std::fs::remove_file(&mp4).unwrap();
        std::fs::remove_file(&ts).unwrap();
    }

    #[test]
    fn converts_to_mp4_and_mp3() {
        let mp4 = convert(Target::Mp4);
        let input = ff::format::input(&mp4).unwrap();
        let names: Vec<&str> = input
            .streams()
            .map(|s| s.parameters().id().name())
            .collect();
        assert_eq!(names, ["h264", "aac"]);
        assert!(input.duration() > 0);
        drop(input);
        std::fs::remove_file(&mp4).unwrap();

        let mp3 = convert(Target::Mp3);
        let input = ff::format::input(&mp3).unwrap();
        let names: Vec<&str> = input
            .streams()
            .map(|s| s.parameters().id().name())
            .collect();
        let sound = if Target::Mp3.extension() == "mp3" {
            "mp3"
        } else {
            "aac"
        };
        assert_eq!(names, [sound]);
        // Five seconds and more of the six-second clip's sound came through.
        let seconds = input.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE);
        assert!((5.5..6.5).contains(&seconds), "{seconds} s");
        drop(input);
        std::fs::remove_file(&mp3).unwrap();
    }
}

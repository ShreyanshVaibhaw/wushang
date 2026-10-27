//! Converting, VLC's "Convert": a file re-encoded into an MP4 (H.264 and AAC) or an MP3 with
//! Windows' own Media Foundation encoders, so the FFmpeg build carries no encoder library and
//! stays LGPL. A job runs on a thread of its own, reports how far it got, and can be cancelled;
//! a job that doesn't finish leaves no file behind.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::mpsc::{Receiver, channel};
use std::thread;

use ff::format::Pixel;
use ff::format::stream::Disposition;
use ff::media::Type;
use ff::{codec, decoder, encoder, frame, software};
use ffmpeg_next as ff;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// H.264 video and AAC audio in MP4.
    Mp4,
    /// The sound alone, in MP3.
    Mp3,
}

impl Target {
    pub fn extension(self) -> &'static str {
        match self {
            Target::Mp4 => "mp4",
            Target::Mp3 => "mp3",
        }
    }
}

/// A conversion running in the background.
pub struct Job {
    pub output: PathBuf,
    /// Thousandths done, by the input's timestamps.
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    done: Receiver<Result<PathBuf, String>>,
}

impl Job {
    pub fn start(input: &Path, target: Target, output: PathBuf) -> Job {
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (send, done) = channel();
        let (input, out) = (input.to_owned(), output.clone());
        let (p, c) = (progress.clone(), cancel.clone());
        thread::spawn(move || {
            // Media Foundation's encoders are COM objects.
            unsafe {
                _ = windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_MULTITHREADED,
                )
            };
            let result = run(&input, target, &out, &p, &c);
            if result.is_err() {
                _ = std::fs::remove_file(&out);
            }
            _ = send.send(result.map(|()| out));
        });
        Job {
            output,
            progress,
            cancel,
            done,
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

/// Bits per pixel per frame for H.264: about 5 Mb/s for 1080p30, plenty for a copy to keep.
const VIDEO_BITS_PER_PIXEL: f64 = 0.08;
const AUDIO_BIT_RATE: usize = 192_000;

fn run(
    input: &Path,
    target: Target,
    output: &Path,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut ictx = ff::format::input(input).map_err(|e| format!("can't open the file: {e}"))?;
    let mut octx = ff::format::output_as(output, target.extension())
        .map_err(|e| format!("can't write {}: {e}", output.display()))?;
    let global_header = octx
        .format()
        .flags()
        .contains(ff::format::Flags::GLOBAL_HEADER);
    let length = (ictx.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE)).max(0.0);

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
            global_header,
        )?),
        None => None,
    };
    let codec_name = match target {
        Target::Mp4 => "aac_mf",
        Target::Mp3 => "mp3_mf",
    };
    let mut audio = match audio_stream {
        Some(i) => Some(AudioPath::new(
            &ictx.stream(i).unwrap(),
            &mut octx,
            codec_name,
            global_header,
        )?),
        None => None,
    };

    octx.set_metadata(ictx.metadata().to_owned());
    let mut options = ff::Dictionary::new();
    if target == Target::Mp4 {
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

fn find_encoder(name: &str) -> Result<ff::Codec, String> {
    encoder::find_by_name(name).ok_or_else(|| format!("this FFmpeg has no {name} encoder"))
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
    decoder: decoder::Video,
    encoder: encoder::Video,
    /// Made from the first frame, whose format the decoder only knows then.
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
        global_header: bool,
    ) -> Result<VideoPath, String> {
        let decoder = open_decoder(stream)?
            .decoder()
            .video()
            .map_err(|e| format!("can't decode the video: {e}"))?;
        let codec = find_encoder("h264_mf")?;
        let mut ost = octx.add_stream(codec).map_err(|e| e.to_string())?;
        let out = ost.index();
        let mut enc = codec::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| e.to_string())?;
        // H.264 in 4:2:0 wants even sides.
        let size = (decoder.width() & !1, decoder.height() & !1);
        let rate = stream.avg_frame_rate();
        let rate = if rate.numerator() > 0 && rate.denominator() > 0 {
            rate
        } else {
            ff::Rational(30, 1)
        };
        enc.set_width(size.0);
        enc.set_height(size.1);
        enc.set_format(Pixel::NV12);
        enc.set_aspect_ratio(decoder.aspect_ratio());
        enc.set_frame_rate(Some(rate));
        enc.set_time_base(rate.invert());
        let bits = f64::from(size.0 * size.1) * f64::from(rate) * VIDEO_BITS_PER_PIXEL;
        enc.set_bit_rate(bits.clamp(500e3, 40e6) as usize);
        if global_header {
            enc.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let encoder = enc
            .open_as(codec)
            .map_err(|e| format!("can't start the H.264 encoder: {e}"))?;
        ost.set_parameters(&encoder);
        Ok(VideoPath {
            index: stream.index(),
            in_base: f64::from(stream.time_base()),
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
            if self.scaler.is_none() {
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
            let seconds = decoded.timestamp().unwrap_or(0) as f64 * self.in_base;
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
    decoder: decoder::Audio,
    encoder: encoder::Audio,
    /// Made from the first frame, whose layout the decoder only knows then.
    resampler: Option<software::resampling::Context>,
    rate: u32,
    layout: ff::ChannelLayout,
    channels: usize,
    /// Samples per encoder frame.
    frame_size: usize,
    /// Interleaved 16-bit samples waiting to fill a frame.
    pending: Vec<i16>,
    /// The next frame's first sample, counted from the start.
    next: i64,
    out: usize,
    out_base: ff::Rational,
}

impl AudioPath {
    fn new(
        stream: &ff::Stream,
        octx: &mut ff::format::context::Output,
        codec_name: &str,
        global_header: bool,
    ) -> Result<AudioPath, String> {
        let decoder = open_decoder(stream)?
            .decoder()
            .audio()
            .map_err(|e| format!("can't decode the sound: {e}"))?;
        let codec = find_encoder(codec_name)?;
        let mut ost = octx.add_stream(codec).map_err(|e| e.to_string())?;
        let out = ost.index();
        let mut enc = codec::Context::new_with_codec(codec)
            .encoder()
            .audio()
            .map_err(|e| e.to_string())?;
        // Media Foundation's AAC and MP3 encoders take 44.1 or 48 kHz, in mono or stereo.
        let rate = if decoder.rate() == 44_100 {
            44_100
        } else {
            48_000
        };
        let channels = decoder.channels().clamp(1, 2) as usize;
        let layout = ff::ChannelLayout::default(channels as i32);
        enc.set_rate(rate as i32);
        enc.set_channel_layout(layout);
        enc.set_format(ff::format::Sample::I16(ff::format::sample::Type::Packed));
        enc.set_bit_rate(AUDIO_BIT_RATE);
        enc.set_time_base(ff::Rational(1, rate as i32));
        if global_header {
            enc.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let encoder = enc
            .open_as(codec)
            .map_err(|e| format!("can't start the {codec_name} encoder: {e}"))?;
        ost.set_parameters(&encoder);
        let frame_size = match encoder.frame_size() {
            0 => 1152,
            n => n as usize,
        };
        Ok(AudioPath {
            index: stream.index(),
            decoder,
            encoder,
            resampler: None,
            rate,
            layout,
            channels,
            frame_size,
            pending: Vec::new(),
            next: 0,
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
            if self.resampler.is_none() {
                let resampler = software::resampling::Context::get(
                    decoded.format(),
                    decoded.channel_layout(),
                    decoded.rate(),
                    ff::format::Sample::I16(ff::format::sample::Type::Packed),
                    self.layout,
                    self.rate,
                )
                .map_err(|e| format!("can't convert the sound: {e}"))?;
                self.resampler = Some(resampler);
            }
            let mut converted = frame::Audio::empty();
            self.resampler
                .as_mut()
                .unwrap()
                .run(&decoded, &mut converted)
                .map_err(|e| format!("can't convert the sound: {e}"))?;
            let n = converted.samples() * self.channels;
            let bytes = &converted.data(0)[..n * 2];
            self.pending.extend(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_ne_bytes(*b)),
            );
            self.send_full_frames(octx, false)?;
        }
        Ok(())
    }

    /// Hands the encoder every whole frame pending; at the end, the rest padded with silence.
    fn send_full_frames(
        &mut self,
        octx: &mut ff::format::context::Output,
        last: bool,
    ) -> Result<(), String> {
        let whole = self.frame_size * self.channels;
        if last && !self.pending.is_empty() {
            let padded = self.pending.len().div_ceil(whole) * whole;
            self.pending.resize(padded, 0);
        }
        while self.pending.len() >= whole {
            let mut f = frame::Audio::new(
                ff::format::Sample::I16(ff::format::sample::Type::Packed),
                self.frame_size,
                self.layout,
            );
            f.set_rate(self.rate);
            f.set_pts(Some(self.next));
            for (dst, src) in f.data_mut(0)[..whole * 2]
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(self.pending.drain(..whole))
            {
                *dst = src.to_ne_bytes();
            }
            self.next += self.frame_size as i64;
            self.encoder
                .send_frame(&f)
                .map_err(|e| format!("the audio encoder failed: {e}"))?;
            let base = ff::Rational(1, self.rate as i32);
            drain(&mut self.encoder, base, self.out, self.out_base, octx)?;
        }
        Ok(())
    }

    fn finish(&mut self, octx: &mut ff::format::context::Output) -> Result<(), String> {
        _ = self.decoder.send_eof();
        self.encode_ready(octx)?;
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
        let out = std::env::temp_dir().join(format!(
            "myvlc-convert-{}.{}",
            std::process::id(),
            target.extension()
        ));
        let job = Job::start(&clip, target, out.clone());
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
        assert_eq!(names, ["mp3"]);
        drop(input);
        std::fs::remove_file(&mp3).unwrap();
    }
}

//! Audio decode thread, resampling to the device format, effects, and the WASAPI callback that
//! drives the clock.

use std::collections::VecDeque;
use std::error::Error;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg_next as ff;

use crate::dsp::{Chain, Effects};
use crate::player::{Msg, POLL, PacketQueue, Shared};

/// Seconds of output kept ahead of the device. Effect changes are heard after at most this long.
const BUFFER_SECONDS: f64 = 0.3;
/// WASAPI shared mode's mix format: interleaved f32.
const DEVICE_FORMAT: ff::format::Sample = ff::format::Sample::F32(ff::format::sample::Type::Packed);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Format {
    rate: u32,
    channels: usize,
}

/// Samples waiting for the device: interleaved f32 in the device's format, after effects.
struct AudioRing {
    state: Mutex<Ring>,
    space: Condvar,
}

struct Ring {
    samples: VecDeque<f32>,
    /// The media time each run of queued frames covers, in queue order. At speed s a span of
    /// n frames covers n * s / rate seconds of media.
    spans: VecDeque<Span>,
    /// None until a device opens.
    format: Option<Format>,
    /// Bumped when the format changes, so a decode thread drops samples made for the old one.
    generation: u64,
}

struct Span {
    left: usize,
    frames: usize,
    start: f64,
    end: f64,
    /// The seek these samples belong to; see the callback.
    serial: u64,
}

impl Ring {
    /// Media time of the next frame to play, and media seconds per second of output there.
    fn head(&self, rate: f64) -> Option<(f64, f64)> {
        let s = self.spans.front()?;
        let speed = (s.end - s.start) * rate / s.frames as f64;
        let played = (s.frames - s.left) as f64 / s.frames as f64;
        Some((s.start + (s.end - s.start) * played, speed))
    }

    /// Takes `frames` frames off the spans after the callback played them.
    fn consume(&mut self, mut frames: usize) {
        while frames > 0 {
            let Some(s) = self.spans.front_mut() else {
                return;
            };
            let n = frames.min(s.left);
            s.left -= n;
            frames -= n;
            if s.left == 0 {
                self.spans.pop_front();
            }
        }
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.spans.clear();
    }
}

impl AudioRing {
    fn is_empty(&self) -> bool {
        self.state.lock().unwrap().samples.is_empty()
    }

    fn clear(&self) {
        self.state.lock().unwrap().clear();
        self.space.notify_all();
    }

    fn format(&self) -> Option<(Format, u64)> {
        let ring = self.state.lock().unwrap();
        ring.format.map(|f| (f, ring.generation))
    }

    /// Queues `samples`, covering media `start..end`. Blocks while the ring is full. Drops the
    /// samples if a seek made them stale or the device changed format since they were made.
    fn push(
        &self,
        samples: &[f32],
        (start, end): (f64, f64),
        generation: u64,
        serial: u64,
        shared: &Shared,
    ) {
        let mut ring = self.state.lock().unwrap();
        loop {
            let Some(f) = ring.format.filter(|_| ring.generation == generation) else {
                return;
            };
            if shared.quitting() || serial != shared.serial.load(Relaxed) {
                return;
            }
            if ring.samples.len() < (f64::from(f.rate) * BUFFER_SECONDS) as usize * f.channels {
                let frames = samples.len() / f.channels;
                if frames > 0 {
                    ring.samples.extend(samples);
                    ring.spans.push_back(Span {
                        left: frames,
                        frames,
                        start,
                        end,
                        serial,
                    });
                }
                return;
            }
            ring = self.space.wait_timeout(ring, POLL).unwrap().0;
        }
    }
}

enum Output {
    Opening,
    Failed,
    /// Dropping the stream stops it.
    Open {
        _stream: cpal::Stream,
        /// The device asked for; None follows the system default.
        name: Option<String>,
    },
}

/// The audio device, opened once for the whole run and shared by every file played. Opening
/// WASAPI can take seconds (1.7 s in `IAudioClient::Initialize` on the test laptop), so it opens on
/// a background thread at startup, and switching files only clears the ring.
pub struct Device {
    shared: Arc<Shared>,
    effects: Arc<Effects>,
    ring: Arc<AudioRing>,
    output: Mutex<Output>,
    opened: Condvar,
    /// Serialises stream opens, so the last `switch_to` wins.
    opening: Mutex<()>,
    me: Weak<Device>,
}

impl Device {
    pub fn open(shared: Arc<Shared>) -> Arc<Device> {
        let device = Arc::new_cyclic(|me| Device {
            shared,
            effects: Arc::new(Effects::new()),
            ring: Arc::new(AudioRing {
                state: Mutex::new(Ring {
                    samples: VecDeque::new(),
                    spans: VecDeque::new(),
                    format: None,
                    generation: 0,
                }),
                space: Condvar::new(),
            }),
            output: Mutex::new(Output::Opening),
            opened: Condvar::new(),
            opening: Mutex::new(()),
            me: me.clone(),
        });
        device.reopen(None);
        device
    }

    /// Speed, equalizer, stereo mode and delay. Changes are heard within `BUFFER_SECONDS`.
    pub fn effects(&self) -> &Effects {
        &self.effects
    }

    /// Moves playback to the output device named as in `output_devices`, or to the system
    /// default for None, without stopping it. The old device keeps playing while the new one
    /// opens in the background. If the new device mixes at another rate or channel count, the
    /// queued 0.3 s is dropped and decoding carries on in the new format.
    pub fn switch_to(&self, name: Option<&str>) {
        self.reopen(name.map(str::to_owned));
    }

    /// The device asked for last; None means the system default.
    pub fn current_output(&self) -> Option<String> {
        match &*self.output.lock().unwrap() {
            Output::Open { name, .. } => name.clone(),
            _ => None,
        }
    }

    fn reopen(&self, name: Option<String>) {
        let Some(device) = self.me.upgrade() else {
            return;
        };
        thread::spawn(move || {
            let _one_at_a_time = device.opening.lock().unwrap();
            match device.build_stream(name.as_deref()) {
                Ok((stream, format)) => device.install(stream, format, name),
                Err(e) => {
                    let which = name.as_deref().unwrap_or("the default device");
                    eprintln!("audio: cannot open {which} ({e})");
                    let mut output = device.output.lock().unwrap();
                    if matches!(*output, Output::Opening) {
                        eprintln!("audio: playing without sound");
                        *output = Output::Failed;
                    }
                }
            }
            device.opened.notify_all();
        });
    }

    /// Swaps the new stream in. The old one stops first, so only one callback ever reads the ring.
    fn install(&self, stream: cpal::Stream, format: Format, name: Option<String>) {
        let mut output = self.output.lock().unwrap();
        *output = Output::Failed; // drops the old stream
        {
            let mut ring = self.ring.state.lock().unwrap();
            if ring.format != Some(format) {
                ring.clear();
                ring.format = Some(format);
                ring.generation += 1;
            }
        }
        self.ring.space.notify_all();
        if let Err(e) = stream.play() {
            eprintln!("audio: cannot start the stream ({e})");
            return;
        }
        *output = Output::Open {
            _stream: stream,
            name,
        };
    }

    fn build_stream(&self, name: Option<&str>) -> Result<(cpal::Stream, Format), Box<dyn Error>> {
        let host = cpal::default_host();
        let device = match name {
            None => host.default_output_device(),
            Some(name) => host
                .output_devices()?
                .find(|d| device_name(d).as_deref() == Some(name)),
        }
        .ok_or("no such audio output device")?;
        let supported = device.default_output_config()?;
        // ponytail: WASAPI shared mode mixes in f32 on every device seen so far; convert if one differs.
        if supported.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!(
                "unsupported device sample format {:?}",
                supported.sample_format()
            )
            .into());
        }
        let config = supported.config();
        let format = Format {
            rate: config.sample_rate,
            channels: usize::from(config.channels),
        };
        let rate = f64::from(format.rate);
        let shared = self.shared.clone();
        let effects = self.effects.clone();
        let feed = self.ring.clone();
        let stream = device.build_output_stream::<f32, _, _>(
            config,
            move |out, info| {
                // ponytail: a mutex in the audio callback; move to a lock-free ring if it ever glitches.
                let mut ring = feed.state.lock().unwrap();
                // A seek made what is queued stale, and the decoder's flush hasn't reached it yet
                // (sooner or later for a second file's demuxer). Playing it would also restart
                // the clock the seek just stopped, at the old position.
                if ring
                    .spans
                    .front()
                    .is_some_and(|s| s.serial != shared.serial.load(Relaxed))
                {
                    ring.clear();
                }
                let n = if shared.paused.load(Relaxed) {
                    0
                } else {
                    out.len().min(ring.samples.len())
                };
                if n > 0
                    && let Some((pts, speed)) = ring.head(rate)
                {
                    // The first sample written now reaches the speaker at `playback`, so at
                    // `callback` the listener hears the media `latency * speed` before it. A
                    // positive delay moves the clock, and so the picture, ahead of the sound.
                    let ts = info.timestamp();
                    let latency = ts.playback.duration_since(ts.callback).as_secs_f64();
                    // A new stream's first callback reports no latency; trust it only to start
                    // the clock, or a device switch would nudge the picture ahead for a moment.
                    if latency > 0.0 || shared.clock.get().is_none() {
                        shared.clock.set(pts - latency * speed + effects.delay());
                    }
                }
                let volume = shared.gain();
                for (o, s) in out.iter_mut().zip(ring.samples.drain(..n)) {
                    *o = s * volume;
                }
                out[n..].fill(0.0);
                ring.consume(n / format.channels);
                drop(ring);
                // The visualizer's copy; skipped rather than waited for while the UI reads it.
                if n > 0
                    && let Ok(mut scope) = shared.scope.try_lock()
                {
                    let ch = format.channels;
                    let (rate, samples) = &mut *scope;
                    *rate = format.rate;
                    let mono = out[..n]
                        .chunks_exact(ch)
                        .map(|f| f.iter().sum::<f32>() / ch as f32);
                    samples.extend(mono);
                    let extra = samples.len().saturating_sub(crate::player::SCOPE);
                    samples.drain(..extra);
                }
                feed.space.notify_one();
            },
            |e| eprintln!("audio: {e}"),
            None,
        )?;
        eprintln!(
            "audio: WASAPI shared mode on {}, {} Hz, {} channels",
            device_name(&device).unwrap_or_default(),
            format.rate,
            format.channels
        );
        Ok((stream, format))
    }

    /// Waits for the device to open. None if it failed, the player is shutting down, or this
    /// decoder's track was switched off meanwhile.
    fn ring(&self, shared: &Shared, packets: &PacketQueue) -> Option<Arc<AudioRing>> {
        let mut output = self.output.lock().unwrap();
        while matches!(*output, Output::Opening) {
            if shared.quitting() || packets.closed() {
                return None;
            }
            output = self.opened.wait_timeout(output, POLL).unwrap().0;
        }
        matches!(*output, Output::Open { .. }).then(|| self.ring.clone())
    }

    /// Drops whatever the previous file or audio track left queued.
    pub fn clear(&self) {
        self.ring.clear();
    }
}

/// Output device names, the system default first.
pub fn output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let default = host.default_output_device().and_then(|d| device_name(&d));
    let mut names: Vec<String> = host
        .output_devices()
        .map(|ds| ds.filter_map(|d| device_name(&d)).collect())
        .unwrap_or_default();
    if let Some(i) = default.and_then(|d| names.iter().position(|n| *n == d)) {
        let name = names.remove(i);
        names.insert(0, name);
    }
    names
}

fn device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_owned())
}

/// Decodes one file's audio into the device. The clock starts once audio plays, so a device that
/// is still opening simply delays motion while the first video frame is already on screen. It may
/// be stopped and started again on the same device, as switching audio tracks does; call
/// `Device::clear` in between.
pub fn spawn_decoder(
    mut decoder: ff::decoder::Audio,
    time_base: f64,
    packets: Arc<PacketQueue>,
    shared: Arc<Shared>,
    device: Arc<Device>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let Some(ring) = device.ring(&shared, &packets) else {
            if packets.closed() {
                return;
            }
            shared.audio_master.store(false, Relaxed);
            // Keep draining so the demuxer never stalls on a full audio queue.
            while !shared.quitting() && !packets.closed() {
                packets.pop();
            }
            return;
        };
        let ring = &ring;
        let effects = &device.effects;
        let mut resampler: Option<ff::software::resampling::Context> = None;
        // The effect chain and the ring generation (device format) it was built for.
        let mut chain: Option<(Chain, u64)> = None;
        let mut samples = Vec::new();
        let mut serial = shared.serial.load(Relaxed);
        let mut next_pts = 0.0;
        let mut drained = false;
        while !shared.quitting() && !packets.closed() {
            if drained && ring.is_empty() {
                shared.audio_done.store(true, Relaxed);
            }
            match packets.pop() {
                None => continue,
                Some(Msg::Packet(p)) => _ = decoder.send_packet(&p),
                Some(Msg::Flush(s)) => {
                    decoder.flush();
                    ring.clear();
                    if let Some((c, _)) = &mut chain {
                        c.reset();
                    }
                    serial = s;
                    drained = false;
                    shared.audio_done.store(false, Relaxed);
                    continue;
                }
                Some(Msg::Eof) => _ = decoder.send_eof(),
            }
            let mut frame = ff::frame::Audio::empty();
            loop {
                match decoder.receive_frame(&mut frame) {
                    Ok(()) => {}
                    Err(ff::Error::Eof) => {
                        if !drained && let Some((c, generation)) = &mut chain {
                            let (out, start, end) = c.finish();
                            ring.push(out, (start, end), *generation, serial, &shared);
                        }
                        drained = true;
                        break;
                    }
                    Err(_) => break, // needs more input
                }
                let Some((format, generation)) = ring.format() else {
                    break;
                };
                if chain.as_ref().is_none_or(|(_, g)| *g != generation) {
                    chain = Some((Chain::new(format.rate, format.channels), generation));
                    resampler = None;
                }
                // Real audio never runs this slow. A file claiming 1 Hz would size the resampler's
                // output for hundreds of millions of samples per frame and run out of memory.
                if frame.rate() < 1000 {
                    static TOLD: std::sync::Once = std::sync::Once::new();
                    TOLD.call_once(|| eprintln!("audio: {} Hz is no sample rate", frame.rate()));
                    continue;
                }
                let layout = label_layout(&mut frame);
                let stale = resampler.as_ref().is_none_or(|r| {
                    let i = r.input();
                    i.format != frame.format()
                        || i.channel_layout != layout
                        || i.rate != frame.rate()
                });
                if stale {
                    let r = ff::software::resampling::Context::get(
                        frame.format(),
                        layout,
                        frame.rate(),
                        DEVICE_FORMAT,
                        ff::ChannelLayout::default(format.channels as i32),
                        format.rate,
                    );
                    match r {
                        Ok(r) => resampler = Some(r),
                        Err(e) => {
                            eprintln!("audio: cannot resample this stream: {e}");
                            continue;
                        }
                    }
                }
                let resampler = resampler.as_mut().unwrap();
                let Some(out) = resample(resampler, &frame, format.channels, format.rate) else {
                    continue;
                };
                let pts = frame.timestamp().map_or(next_pts, |t| t as f64 * time_base);
                let n = out.samples();
                next_pts = pts + n as f64 / f64::from(format.rate);
                // Before an exact seek's target: decoded only because the demuxer lands earlier.
                if next_pts <= shared.seek_floor() {
                    continue;
                }
                let bytes = &out.data(0)[..n * format.channels * 4];
                samples.clear();
                samples.extend(
                    bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_ne_bytes(*b)),
                );
                let (c, generation) = chain.as_mut().unwrap();
                let (out, start, end) = c.process(effects, &mut samples, pts);
                ring.push(out, (start, end), *generation, serial, &shared);
            }
        }
    })
}

/// The frame's channel layout, filling in the default one when the decoder left it unset. PCM in
/// WAV or Matroska often comes that way, and the frame itself needs the label too: swresample
/// rejects a frame whose layout differs from the one it was set up with, which silenced every
/// such file.
fn label_layout(frame: &mut ff::frame::Audio) -> ff::ChannelLayout {
    let layout = frame.channel_layout();
    if !layout.is_empty() {
        return layout;
    }
    let layout = ff::ChannelLayout::default(i32::from(frame.channels()));
    frame.set_channel_layout(layout);
    layout
}

/// Converts one decoded frame to interleaved f32 at the device's rate and channel count.
fn resample(
    resampler: &mut ff::software::resampling::Context,
    frame: &ff::frame::Audio,
    channels: usize,
    rate: u32,
) -> Option<ff::frame::Audio> {
    // Given an empty frame, ffmpeg-next's run() allocates room for only the input's sample count.
    // Upsampling 44.1 to 48 kHz then leaves the extra 9% stuck inside swresample and the audio
    // plays fast, so size the output for the converted count, plus slack for swresample's delay.
    let capacity = frame.samples() * rate as usize / frame.rate() as usize + 64;
    let layout = ff::ChannelLayout::default(channels as i32);
    let mut out = ff::frame::Audio::new(DEVICE_FORMAT, capacity, layout);
    resampler.run(frame, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player;
    use ff::format::sample::Type::Planar;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn plays_pcm_without_a_channel_layout() {
        // A mono s16 frame labelled only with its channel count, as the WAV and Matroska PCM
        // decoders hand them out.
        let format = ff::format::Sample::I16(ff::format::sample::Type::Packed);
        let mut frame = ff::frame::Audio::new(format, 1000, ff::ChannelLayout::default(1));
        frame.set_rate(8000);
        unsafe {
            let layout = &mut (*frame.as_mut_ptr()).ch_layout;
            layout.order = ff::ffi::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC;
            layout.nb_channels = 1;
        }
        assert!(frame.channel_layout().is_empty());
        let layout = label_layout(&mut frame);
        let mut resampler = ff::software::resampling::Context::get(
            format,
            layout,
            8000,
            DEVICE_FORMAT,
            ff::ChannelLayout::default(2),
            48_000,
        )
        .unwrap();
        assert!(resample(&mut resampler, &frame, 2, 48_000).is_some());
    }

    #[test]
    fn upsampling_keeps_every_sample() {
        // 44 frames of 44.1 kHz mono (45,056 samples) must become ~49,040 samples at 48 kHz.
        // Before the fix the resampler returned exactly 45,056 and audio played 8.8% fast.
        let input_format = ff::format::Sample::F32(Planar);
        let mono = || ff::ChannelLayout::default(1);
        let mut resampler = ff::software::resampling::Context::get(
            input_format,
            mono(),
            44_100,
            DEVICE_FORMAT,
            ff::ChannelLayout::default(2),
            48_000,
        )
        .unwrap();
        let mut total = 0;
        for _ in 0..44 {
            let mut frame = ff::frame::Audio::new(input_format, 1024, mono());
            frame.set_rate(44_100);
            frame.plane_mut::<f32>(0).fill(0.0);
            total += resample(&mut resampler, &frame, 2, 48_000)
                .unwrap()
                .samples();
        }
        let expected = 45_056 * 48_000 / 44_100;
        assert!(
            total.abs_diff(expected) < 64,
            "got {total} samples, expected about {expected}"
        );
    }

    #[test]
    fn ring_clock_scales_with_speed() {
        // 4800 frames at 48 kHz that cover 0.15 s of media: 1.5x.
        let mut ring = Ring {
            samples: VecDeque::new(),
            spans: VecDeque::from([Span {
                left: 4800,
                frames: 4800,
                start: 10.0,
                end: 10.15,
                serial: 0,
            }]),
            format: None,
            generation: 0,
        };
        let (pts, speed) = ring.head(48_000.0).unwrap();
        assert_eq!(pts, 10.0);
        assert!((speed - 1.5).abs() < 1e-9);
        ring.consume(1600);
        let (pts, _) = ring.head(48_000.0).unwrap();
        assert!((pts - 10.05).abs() < 1e-9);
        ring.consume(3200);
        assert!(ring.head(48_000.0).is_none());
    }

    #[test]
    fn push_drops_samples_made_for_an_old_format() {
        let shared = Shared::new();
        let ring = AudioRing {
            state: Mutex::new(Ring {
                samples: VecDeque::new(),
                spans: VecDeque::new(),
                format: Some(Format {
                    rate: 48_000,
                    channels: 2,
                }),
                generation: 3,
            }),
            space: Condvar::new(),
        };
        let serial = shared.serial.load(Relaxed);
        ring.push(&[0.5; 64], (0.0, 0.1), 2, serial, &shared);
        assert!(ring.is_empty());
        ring.push(&[0.5; 64], (0.0, 0.1), 3, serial, &shared);
        assert_eq!(ring.state.lock().unwrap().samples.len(), 64);
        ring.push(&[0.5; 64], (0.1, 0.2), 3, serial + 1, &shared);
        assert_eq!(
            ring.state.lock().unwrap().spans.len(),
            1,
            "stale serial queued"
        );
    }

    /// Starts the audio of `path` on `device`, like a session with no video.
    fn play(path: &PathBuf, shared: &Arc<Shared>, device: &Arc<Device>) -> Vec<JoinHandle<()>> {
        let input = ff::format::input(path).unwrap();
        let stream = input.streams().best(ff::media::Type::Audio).unwrap();
        let decoder = player::open_audio_decoder(stream.parameters(), stream.time_base()).unwrap();
        let (index, tb) = (stream.index(), f64::from(stream.time_base()));
        shared.reset();
        shared.audio_master.store(true, Relaxed);
        let packets = Arc::new(PacketQueue::default());
        vec![
            spawn_decoder(decoder, tb, packets.clone(), shared.clone(), device.clone()),
            player::spawn_demuxer(
                input,
                Arc::new(std::sync::Mutex::new(vec![player::Route {
                    stream: index,
                    queue: packets,
                    sparse: false,
                }])),
                shared.clone(),
                player::Role::Main(Vec::new(), None),
                None,
            ),
        ]
    }

    /// Clock rate in media seconds per second, measured over `seconds`.
    fn clock_rate(shared: &Shared, seconds: f64) -> f64 {
        let start = Instant::now();
        while shared.clock.get().is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "no sound after 10 s"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let (t0, p0) = (Instant::now(), shared.clock.get().unwrap());
        thread::sleep(Duration::from_secs_f64(seconds));
        (shared.clock.get().unwrap() - p0) / t0.elapsed().as_secs_f64()
    }

    /// Plays test_60s.mp4's audio through the real default device: 3 s at 1.5x, a delay change,
    /// a device reopen, back to 1x, then a decoder restart at 0.75x. Run with
    /// `cargo test --release -- --ignored --nocapture plays_through_the_device`.
    #[test]
    #[ignore = "plays sound on the default output device"]
    fn plays_through_the_device() {
        ff::init().unwrap();
        ff::util::log::set_level(ff::util::log::Level::Error);
        eprintln!("output devices: {:?}", output_devices());
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/test_60s.mp4");
        let shared = Arc::new(Shared::new());
        let device = Device::open(shared.clone());
        device.effects().set_speed(1.5);
        let threads = play(&path, &shared, &device);

        // Clock::get still extrapolates at 1x between callbacks, but the callback sets it every
        // few ms, so over seconds the clock follows the media time heard: about 1.5x.
        let rate = clock_rate(&shared, 3.0);
        eprintln!("1.5x: clock ran at {rate:.3}");
        assert!((rate - 1.5).abs() < 0.1, "clock ran at {rate}");

        let before = shared.clock.get().unwrap();
        let t = Instant::now();
        device.effects().set_delay(0.5);
        thread::sleep(Duration::from_millis(100));
        let jump = shared.clock.get().unwrap() - before - 1.5 * t.elapsed().as_secs_f64();
        eprintln!("delay 0.5 s: clock jumped {jump:.3}");
        assert!((jump - 0.5).abs() < 0.1, "clock jumped {jump}");
        device.effects().set_delay(0.0);
        thread::sleep(Duration::from_millis(100)); // let a callback apply it

        device.switch_to(None);
        let rate = clock_rate(&shared, 3.0);
        eprintln!("across a device reopen: clock ran at {rate:.3}");
        assert!((rate - 1.5).abs() < 0.1, "clock ran at {rate}");

        // Another device by name, then back to the default. A device that fails to open
        // leaves the current one playing, so the rate holds either way.
        let devices = output_devices();
        if let Some(other) = devices.get(1) {
            device.switch_to(Some(other));
            let rate = clock_rate(&shared, 3.0);
            eprintln!(
                "on {other:?}: clock ran at {rate:.3}, current {:?}",
                device.current_output()
            );
            assert!((rate - 1.5).abs() < 0.1, "clock ran at {rate}");
            device.switch_to(None);
            thread::sleep(Duration::from_secs(3));
        }

        device.effects().set_speed(1.0);
        thread::sleep(Duration::from_millis(400));
        let rate = clock_rate(&shared, 1.0);
        eprintln!("back to 1x: clock ran at {rate:.3}");
        assert!((rate - 1.0).abs() < 0.1, "clock ran at {rate}");

        // Stop and restart the decoder on the same device, as an audio track switch does.
        shared.stopping.store(true, Relaxed);
        threads.into_iter().for_each(|t| t.join().unwrap());
        device.clear();
        device.effects().set_speed(0.75);
        let threads = play(&path, &shared, &device);
        let rate = clock_rate(&shared, 1.5);
        eprintln!("restarted at 0.75x: clock ran at {rate:.3}");
        assert!((rate - 0.75).abs() < 0.1, "clock ran at {rate}");
        let position = shared.clock.get().unwrap();
        assert!(position < 3.0, "restart began at {position}, not the start");

        shared.quit.store(true, Relaxed);
        threads.into_iter().for_each(|t| t.join().unwrap());
    }
}

//! Audio effects, applied in the decode thread on interleaved f32 at the device rate: stereo
//! mode, VLC's 10-band equalizer, and pitch-keeping playback speed (WSOLA, like VLC's
//! scaletempo). `Effects` holds the settings the interface changes; the decode thread reads them
//! once per decoded frame, and the realtime callback only reads the delay, an atomic.

use std::collections::VecDeque;
use std::f64::consts::PI;
use std::sync::Mutex;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64};

pub const MIN_SPEED: f32 = 0.25;
pub const MAX_SPEED: f32 = 4.0;
/// Audio delay range in seconds, as in VLC's synchronisation panel.
pub const MAX_DELAY: f64 = 10.0;

/// Centre frequencies of VLC's 10 equalizer bands (its default set; it also has an ISO set
/// starting at 31.25 Hz).
pub const EQ_BANDS: [f32; 10] = [
    60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12000.0, 14000.0, 16000.0,
];
/// VLC scales the dry signal by 1/4 inside its equalizer and its presets add 12 dB of preamp
/// back, so a preamp of 12 dB is unity gain. Presets and `set_eq_bands` use that same scale.
pub const PREAMP_UNITY_DB: f32 = 12.0;
const EQ_Q: f64 = 1.4;

pub struct EqPreset {
    pub name: &'static str,
    /// dB, on VLC's scale where `PREAMP_UNITY_DB` is unity.
    pub preamp: f32,
    /// dB per band of `EQ_BANDS`.
    pub bands: [f32; 10],
}

const fn preset(name: &'static str, preamp: f32, bands: [f32; 10]) -> EqPreset {
    EqPreset {
        name,
        preamp,
        bands,
    }
}

/// VLC's built-in presets, from vlc/modules/audio_filter/equalizer_presets.h (VLC writes its
/// zeros as -1.11022e-15, which is 0 here).
#[rustfmt::skip]
pub const EQ_PRESETS: [EqPreset; 18] = [
    preset("Flat", 12.0, [0.0; 10]),
    preset("Classical", 12.0, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -7.2, -7.2, -7.2, -9.6]),
    preset("Club", 6.0, [0.0, 0.0, 8.0, 5.6, 5.6, 5.6, 3.2, 0.0, 0.0, 0.0]),
    preset("Dance", 5.0, [9.6, 7.2, 2.4, 0.0, 0.0, -5.6, -7.2, -7.2, 0.0, 0.0]),
    preset("Full bass", 5.0, [-8.0, 9.6, 9.6, 5.6, 1.6, -4.0, -8.0, -10.4, -11.2, -11.2]),
    preset("Full bass and treble", 4.0, [7.2, 5.6, 0.0, -7.2, -4.8, 1.6, 8.0, 11.2, 12.0, 12.0]),
    preset("Full treble", 3.0, [-9.6, -9.6, -9.6, -4.0, 2.4, 11.2, 16.0, 16.0, 16.0, 16.8]),
    preset("Headphones", 4.0, [4.8, 11.2, 5.6, -3.2, -2.4, 1.6, 4.8, 9.6, 12.8, 14.4]),
    preset("Large Hall", 5.0, [10.4, 10.4, 5.6, 5.6, 0.0, -4.8, -4.8, -4.8, 0.0, 0.0]),
    preset("Live", 7.0, [-4.8, 0.0, 4.0, 5.6, 5.6, 5.6, 4.0, 2.4, 2.4, 2.4]),
    preset("Party", 6.0, [7.2, 7.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 7.2, 7.2]),
    preset("Pop", 6.0, [-1.6, 4.8, 7.2, 8.0, 5.6, 0.0, -2.4, -2.4, -1.6, -1.6]),
    preset("Reggae", 8.0, [0.0, 0.0, 0.0, -5.6, 0.0, 6.4, 6.4, 0.0, 0.0, 0.0]),
    preset("Rock", 5.0, [8.0, 4.8, -5.6, -8.0, -3.2, 4.0, 8.8, 11.2, 11.2, 11.2]),
    preset("Ska", 6.0, [-2.4, -4.8, -4.0, 0.0, 4.0, 5.6, 8.8, 9.6, 11.2, 9.6]),
    preset("Soft", 5.0, [4.8, 1.6, 0.0, -2.4, 0.0, 4.0, 8.0, 9.6, 11.2, 12.0]),
    preset("Soft rock", 7.0, [4.0, 4.0, 2.4, 0.0, -4.0, -5.6, -3.2, 0.0, 2.4, 8.8]),
    preset("Techno", 5.0, [8.0, 5.6, 0.0, -5.6, -4.8, 0.0, 8.0, 9.6, 9.6, 8.8]),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StereoMode {
    #[default]
    Stereo,
    /// Both channels play (L + R) / 2.
    Mono,
    /// Both channels play L.
    Left,
    /// Both channels play R.
    Right,
    /// L and R swap.
    Reverse,
}

impl StereoMode {
    pub const ALL: [StereoMode; 5] = [
        StereoMode::Stereo,
        StereoMode::Mono,
        StereoMode::Left,
        StereoMode::Right,
        StereoMode::Reverse,
    ];
}

/// The equalizer as the interface set it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqSettings {
    /// False means the equalizer is off ("None" in the design).
    pub enabled: bool,
    /// The preset the bands came from; None for custom bands.
    pub preset: Option<usize>,
    pub preamp: f32,
    pub bands: [f32; 10],
    /// VLC's "2 Pass": the whole chain, preamp included, runs twice.
    pub two_pass: bool,
}

impl EqSettings {
    const OFF: EqSettings = EqSettings {
        enabled: false,
        preset: None,
        preamp: PREAMP_UNITY_DB,
        bands: [0.0; 10],
        two_pass: false,
    };

    /// Off, or set to unity everywhere: the samples pass untouched.
    fn bypassed(&self) -> bool {
        !self.enabled || (self.preamp == PREAMP_UNITY_DB && self.bands == [0.0; 10])
    }
}

/// Effect settings shared by the interface, the decode thread and the audio callback.
/// Setters are atomics or a short lock the realtime callback never takes.
pub struct Effects {
    speed: AtomicU32,
    stereo: AtomicU8,
    delay: AtomicU64,
    eq: Mutex<EqSettings>,
    /// Bumped on every equalizer change, so the decode thread copies the settings only then.
    eq_version: AtomicU64,
}

impl Effects {
    pub fn new() -> Effects {
        Effects {
            speed: AtomicU32::new(1.0f32.to_bits()),
            stereo: AtomicU8::new(0),
            delay: AtomicU64::new(0.0f64.to_bits()),
            eq: Mutex::new(EqSettings::OFF),
            eq_version: AtomicU64::new(0),
        }
    }

    /// Playback speed, clamped to 0.25..4. Speeds within 0.5% of 1 snap to exactly 1, which
    /// bypasses the stretcher.
    pub fn set_speed(&self, speed: f32) {
        let s = if (speed - 1.0).abs() < 0.005 {
            1.0
        } else {
            speed.clamp(MIN_SPEED, MAX_SPEED)
        };
        self.speed.store(s.to_bits(), Relaxed);
    }

    pub fn speed(&self) -> f32 {
        f32::from_bits(self.speed.load(Relaxed))
    }

    pub fn set_stereo(&self, mode: StereoMode) {
        self.stereo.store(mode as u8, Relaxed);
    }

    pub fn stereo(&self) -> StereoMode {
        StereoMode::ALL[usize::from(self.stereo.load(Relaxed)) % StereoMode::ALL.len()]
    }

    /// Audio delay in seconds, clamped to -10..10. Positive means the sound comes later than
    /// the picture: the clock the video follows runs `delay` ahead of what is heard.
    pub fn set_delay(&self, seconds: f64) {
        let d = seconds.clamp(-MAX_DELAY, MAX_DELAY);
        self.delay.store(d.to_bits(), Relaxed);
    }

    pub fn delay(&self) -> f64 {
        f64::from_bits(self.delay.load(Relaxed))
    }

    /// Selects one of `EQ_PRESETS`, or None to turn the equalizer off. "2 Pass" carries over.
    pub fn set_eq(&self, preset: Option<usize>) {
        self.update_eq(
            |eq| match preset.and_then(|i| EQ_PRESETS.get(i).map(|p| (i, p))) {
                Some((i, p)) => {
                    eq.enabled = true;
                    eq.preset = Some(i);
                    eq.preamp = p.preamp;
                    eq.bands = p.bands;
                }
                None => eq.enabled = false,
            },
        );
    }

    /// Custom bands in dB (clamped to -20..20, VLC's range) and preamp on VLC's scale.
    pub fn set_eq_bands(&self, bands: [f32; 10], preamp: f32) {
        self.update_eq(|eq| {
            eq.enabled = true;
            eq.preset = None;
            eq.preamp = preamp.clamp(-20.0, 20.0);
            eq.bands = bands.map(|b| b.clamp(-20.0, 20.0));
        });
    }

    pub fn set_two_pass(&self, on: bool) {
        self.update_eq(|eq| eq.two_pass = on);
    }

    pub fn eq(&self) -> EqSettings {
        *self.eq.lock().unwrap()
    }

    fn update_eq(&self, change: impl FnOnce(&mut EqSettings)) {
        change(&mut self.eq.lock().unwrap());
        self.eq_version.fetch_add(1, Relaxed);
    }
}

/// The decode thread's effect chain for one device format. Rebuilt when the device changes.
pub struct Chain {
    channels: usize,
    eq: Equalizer,
    eq_version: Option<u64>,
    stretch: Stretcher,
    out: Vec<f32>,
}

impl Chain {
    pub fn new(rate: u32, channels: usize) -> Chain {
        Chain {
            channels,
            eq: Equalizer::new(rate, channels),
            eq_version: None,
            stretch: Stretcher::new(rate, channels),
            out: Vec::new(),
        }
    }

    /// Runs stereo mode, equalizer and speed over `samples` (modified in place), which start at
    /// media time `pts`. Returns the output and the media time range it covers.
    pub fn process(
        &mut self,
        effects: &Effects,
        samples: &mut [f32],
        pts: f64,
    ) -> (&[f32], f64, f64) {
        let version = effects.eq_version.load(Relaxed);
        if self.eq_version != Some(version) {
            self.eq_version = Some(version);
            self.eq.configure(&effects.eq());
        }
        apply_stereo(effects.stereo(), samples, self.channels);
        self.eq.process(samples);
        self.out.clear();
        let (start, end) = self
            .stretch
            .process(samples, pts, effects.speed(), &mut self.out);
        (&self.out, start, end)
    }

    /// At end of stream: what the stretcher still holds.
    pub fn finish(&mut self) -> (&[f32], f64, f64) {
        self.out.clear();
        let (start, end) = self.stretch.drain(&mut self.out);
        (&self.out, start, end)
    }

    /// After a seek: forget the previous position's audio.
    pub fn reset(&mut self) {
        self.stretch.reset();
        self.eq.reset();
    }
}

pub fn apply_stereo(mode: StereoMode, samples: &mut [f32], channels: usize) {
    if channels < 2 || mode == StereoMode::Stereo {
        return;
    }
    for frame in samples.chunks_exact_mut(channels) {
        let (l, r) = (frame[0], frame[1]);
        (frame[0], frame[1]) = match mode {
            StereoMode::Stereo => (l, r),
            StereoMode::Mono => ((l + r) * 0.5, (l + r) * 0.5),
            StereoMode::Left => (l, l),
            StereoMode::Right => (r, r),
            StereoMode::Reverse => (r, l),
        };
    }
}

/// The stereo "Test" sound as a 16-bit WAV file: a low tone on the left, then a high one on the
/// right, each 0.4 s, passed through `mode` as playback would.
pub fn test_tone(mode: StereoMode) -> Vec<u8> {
    const RATE: u32 = 44_100;
    let half = (RATE as f32 * 0.4) as usize;
    let mut samples = Vec::with_capacity(half * 4);
    for i in 0..half * 2 {
        let (right, f) = if i < half {
            (false, 440.0)
        } else {
            (true, 660.0)
        };
        let t = (i % half) as f32 / RATE as f32;
        // A short fade at both ends keeps it from clicking.
        let fade = (t / 0.02).min((0.4 - t) / 0.02).clamp(0.0, 1.0);
        let v = (t * f * std::f32::consts::TAU).sin() * 0.3 * fade;
        samples.extend(if right { [0.0, v] } else { [v, 0.0] });
    }
    apply_stereo(mode, &mut samples, 2);
    let data = samples.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + data as usize);
    let mut put = |b: &[u8]| wav.extend_from_slice(b);
    put(b"RIFF");
    put(&(36 + data).to_le_bytes());
    put(b"WAVEfmt ");
    put(&16u32.to_le_bytes());
    put(&1u16.to_le_bytes()); // PCM
    put(&2u16.to_le_bytes());
    put(&RATE.to_le_bytes());
    put(&(RATE * 4).to_le_bytes());
    put(&4u16.to_le_bytes());
    put(&16u16.to_le_bytes());
    put(b"data");
    put(&data.to_le_bytes());
    for s in samples {
        put(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    wav
}

/// The level of `bands` log-spaced frequency bands from 50 Hz to 16 kHz in mono `samples` at
/// `rate`, each 0..1 on a 60 dB scale, for the spectrum visualizer.
// ponytail: a windowed DFT per band, a few thousand multiply-adds; an FFT if bands grow past ~64.
pub fn spectrum(samples: &[f32], rate: u32, bands: usize) -> Vec<f32> {
    let n = samples.len();
    if n < 2 || rate == 0 {
        return vec![0.0; bands];
    }
    let (lo, hi) = (50.0f32, 16_000.0f32.min(rate as f32 / 2.0));
    (0..bands)
        .map(|b| {
            let f = lo * (hi / lo).powf((b as f32 + 0.5) / bands as f32);
            let w = std::f32::consts::TAU * f / rate as f32;
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, s) in samples.iter().enumerate() {
                // Hann window.
                let hann = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (n - 1) as f32).cos();
                let (sin, cos) = (w * i as f32).sin_cos();
                re += s * hann * cos;
                im += s * hann * sin;
            }
            // A full-scale sine reads about n / 4 through the window.
            let level = (re * re + im * im).sqrt() / (n as f32 / 4.0);
            let db = 20.0 * level.max(1e-6).log10();
            ((db + 60.0) / 60.0).clamp(0.0, 1.0)
        })
        .collect()
}

/// One band of the equalizer: a band-pass biquad (RBJ audio EQ cookbook, 0 dB peak gain) in
/// transposed direct form II, with b1 = 0 and b2 = -b0, plus the gain its output is mixed in with.
#[derive(Clone, Copy)]
struct Band {
    b0: f64,
    a1: f64,
    a2: f64,
    /// 10^(dB/20) - 1. At its centre the band-pass passes the input unchanged, phase included,
    /// so input + mix * band-pass gives exactly the band's gain there.
    mix: f64,
}

impl Band {
    fn new(freq: f64, gain_db: f64, rate: f64) -> Band {
        let w0 = 2.0 * PI * freq / rate;
        let alpha = w0.sin() / (2.0 * EQ_Q);
        let a0 = 1.0 + alpha;
        Band {
            b0: alpha / a0,
            a1: -2.0 * w0.cos() / a0,
            a2: (1.0 - alpha) / a0,
            mix: 10f64.powf(gain_db / 20.0) - 1.0,
        }
    }
}

/// The 10-band equalizer plus preamp. As in VLC's equalizer the bands run in parallel:
/// output = preamp * (input + sum of mix * band-pass(input)). Peaking filters in series would add
/// their dB where bands overlap, and with 12, 14 and 16 kHz this close, Full treble would lift
/// 14 kHz by about 40 dB instead of the low 20s it gets this way.
pub struct Equalizer {
    rate: f64,
    channels: usize,
    bypass: bool,
    passes: usize,
    preamp: f32,
    /// Only the bands with a nonzero gain.
    bands: Vec<Band>,
    /// [pass][band][channel] -> (z1, z2).
    state: Vec<[f64; 2]>,
    /// Each pass's input, which every band filters.
    dry: Vec<f32>,
}

impl Equalizer {
    pub fn new(rate: u32, channels: usize) -> Equalizer {
        Equalizer {
            rate: f64::from(rate),
            channels,
            bypass: true,
            passes: 1,
            preamp: 1.0,
            bands: Vec::new(),
            state: Vec::new(),
            dry: Vec::new(),
        }
    }

    pub fn configure(&mut self, eq: &EqSettings) {
        self.bypass = eq.bypassed();
        self.passes = if eq.two_pass { 2 } else { 1 };
        self.preamp = 10f32.powf((eq.preamp - PREAMP_UNITY_DB) / 20.0);
        let nyquist_guard = self.rate * 0.48;
        let bands: Vec<Band> = EQ_BANDS
            .iter()
            .zip(eq.bands)
            .filter(|&(&f, g)| g != 0.0 && f64::from(f) < nyquist_guard)
            .map(|(&f, g)| Band::new(f64::from(f), f64::from(g), self.rate))
            .collect();
        // Keep the filter memory when only gains moved, so dragging a slider doesn't click.
        let len = self.passes * bands.len() * self.channels;
        if self.state.len() != len {
            self.state = vec![[0.0; 2]; len];
        }
        self.bands = bands;
    }

    pub fn reset(&mut self) {
        self.state.fill([0.0; 2]);
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        if self.bypass {
            return;
        }
        let ch = self.channels;
        let count = self.bands.len();
        for pass in 0..self.passes {
            self.dry.clear();
            self.dry.extend_from_slice(samples);
            for (i, b) in self.bands.iter().enumerate() {
                let state = &mut self.state[(pass * count + i) * ch..][..ch];
                for (frame, dry) in samples.chunks_exact_mut(ch).zip(self.dry.chunks_exact(ch)) {
                    for ((s, &x), z) in frame.iter_mut().zip(dry).zip(state.iter_mut()) {
                        let x = f64::from(x);
                        let y = b.b0 * x + z[0];
                        z[0] = z[1] - b.a1 * y;
                        z[1] = -b.b0 * x - b.a2 * y;
                        *s += (b.mix * y) as f32;
                    }
                }
                // Flush denormals after silence, which would otherwise slow the thread down.
                for z in state.iter_mut().flatten() {
                    if z.abs() < 1e-30 {
                        *z = 0.0;
                    }
                }
            }
            if self.preamp != 1.0 {
                samples.iter_mut().for_each(|s| *s *= self.preamp);
            }
        }
    }
}

/// Pitch-keeping time stretch by WSOLA (waveform-similarity overlap-add), the method of VLC's
/// scaletempo. Output is cut into Hann windows of `window` frames at 50% overlap, so each step
/// emits `hop` frames. Step k reads its window from the input near `k * hop * speed`; within
/// `search` frames of that spot it takes the start whose waveform best matches the natural
/// continuation of the previous window, so the overlap-add joins in phase and pitch is kept.
///
/// At speed 1 with nothing buffered the input passes through bit-exact.
pub struct Stretcher {
    rate: f64,
    channels: usize,
    window: usize,
    hop: usize,
    search: usize,
    hann: Vec<f32>,
    /// Buffered input, interleaved, starting at absolute input frame `first`.
    input: Vec<f32>,
    first: u64,
    /// Absolute input frame just past the buffered input.
    end: u64,
    /// Absolute input position the next window is centred on before the search, fractional
    /// so that any speed advances it exactly.
    pos: f64,
    /// Where the previous window's input would naturally continue; None before the first.
    natural: Option<u64>,
    /// The previous window's faded-out second half, added under the next window's first half.
    overlap: Vec<f32>,
    /// (absolute input frame, media time) of each input block, to map positions to media time.
    anchors: VecDeque<(u64, f64)>,
    stretching: bool,
    /// Scratch for the mono mixes the search correlates.
    reference: Vec<f32>,
    region: Vec<f32>,
}

/// Window length: 25 ms, in the middle of what scaletempo and SoundTouch use for speech and
/// music alike.
const WINDOW_SECONDS: f64 = 0.025;
/// How far the search may move a window from its nominal spot.
const SEARCH_SECONDS: f64 = 0.012;

impl Stretcher {
    pub fn new(rate: u32, channels: usize) -> Stretcher {
        let rate = f64::from(rate);
        let hop = (rate * WINDOW_SECONDS / 2.0).round() as usize;
        let window = hop * 2;
        // Periodic Hann: w[i] + w[i + hop] == 1, so overlap-added windows sum to unity gain.
        let hann = (0..window)
            .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / window as f64).cos()) as f32)
            .collect();
        Stretcher {
            rate,
            channels,
            window,
            hop,
            search: (rate * SEARCH_SECONDS).round() as usize,
            hann,
            input: Vec::new(),
            first: 0,
            end: 0,
            pos: 0.0,
            natural: None,
            overlap: vec![0.0; hop * channels],
            anchors: VecDeque::new(),
            stretching: false,
            reference: Vec::new(),
            region: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.input.clear();
        self.anchors.clear();
        self.first = self.end;
        self.natural = None;
        self.stretching = false;
    }

    /// Appends the stretched `input` (starting at media time `pts`) to `out` and returns the
    /// media time range of what it appended. The stretcher holds back about `window + search`
    /// frames, which come out with the next call or `drain`.
    pub fn process(
        &mut self,
        input: &[f32],
        pts: f64,
        speed: f32,
        out: &mut Vec<f32>,
    ) -> (f64, f64) {
        let frames = (input.len() / self.channels) as u64;
        if speed == 1.0 && !self.stretching {
            out.extend_from_slice(input);
            self.end += frames;
            self.first = self.end;
            return (pts, pts + frames as f64 / self.rate);
        }
        self.anchors.push_back((self.end, pts));
        self.input.extend_from_slice(input);
        self.end += frames;
        if !self.stretching {
            self.stretching = true;
            self.pos = self.first as f64;
            self.natural = None;
        }
        if speed == 1.0 {
            return self.drain(out);
        }

        let start = self.media_at(self.pos);
        let ch = self.channels;
        let step = self.hop as f64 * f64::from(speed);
        loop {
            let nominal = self.pos.round() as u64;
            if nominal + (self.search + self.window) as u64 > self.end {
                break;
            }
            let seg = match self.natural {
                None => nominal,
                Some(natural) => self.best_start(nominal, natural),
            };
            let x = &self.input[(seg - self.first) as usize * ch..][..self.window * ch];
            let (head, tail) = x.split_at(self.hop * ch);
            if self.natural.is_none() {
                // Nothing to overlap yet: act as if the previous window continued naturally,
                // which adds up to the input itself.
                out.extend_from_slice(head);
            } else {
                out.extend(
                    head.iter()
                        .zip(&self.overlap)
                        .enumerate()
                        .map(|(i, (x, o))| o + self.hann[i / ch] * x),
                );
            }
            for (i, (o, x)) in self.overlap.iter_mut().zip(tail).enumerate() {
                *o = self.hann[self.hop + i / ch] * x;
            }
            self.natural = Some(seg + self.hop as u64);
            self.pos += step;
        }
        let end = self.media_at(self.pos);
        self.trim();
        (start, end)
    }

    /// Emits everything still buffered, joined to the last window, and returns to passthrough.
    /// Used when the speed goes back to 1 and at end of stream.
    pub fn drain(&mut self, out: &mut Vec<f32>) -> (f64, f64) {
        if !self.stretching {
            return (0.0, 0.0);
        }
        let ch = self.channels;
        let from = self
            .natural
            .unwrap_or(self.pos.round() as u64)
            .clamp(self.first, self.end);
        let start = self.media_at(from as f64);
        let end = self.media_at(self.end as f64);
        let x = &self.input[(from - self.first) as usize * ch..];
        if self.natural.is_some() {
            // The overlap is the faded-out start of exactly this input, so fading the input in
            // under it rebuilds the input and the rest follows seamlessly.
            let n = x.len().min(self.hop * ch);
            out.extend(
                x[..n]
                    .iter()
                    .zip(&self.overlap)
                    .enumerate()
                    .map(|(i, (x, o))| o + self.hann[i / ch] * x),
            );
            out.extend_from_slice(&x[n..]);
        } else {
            out.extend_from_slice(x);
        }
        self.reset();
        (start, end)
    }

    /// Start of the window within `search` frames of `nominal` whose first half best matches
    /// the input at `natural`, by normalised cross-correlation of the mono mix.
    fn best_start(&mut self, nominal: u64, natural: u64) -> u64 {
        let lo = nominal.saturating_sub(self.search as u64).max(self.first);
        let hi = nominal + self.search as u64;
        let len = self.hop;
        mix_mono(
            &self.input,
            self.channels,
            (natural - self.first) as usize,
            len,
            &mut self.reference,
        );
        let span = (hi - lo) as usize + len;
        mix_mono(
            &self.input,
            self.channels,
            (lo - self.first) as usize,
            span,
            &mut self.region,
        );

        let mut energy: f64 = self.region[..len].iter().map(|&s| f64::from(s * s)).sum();
        let (mut best, mut best_score) = (nominal - lo, f64::NEG_INFINITY);
        for offset in 0..=(hi - lo) as usize {
            let corr = f64::from(dot(&self.reference, &self.region[offset..offset + len]));
            let score = corr / (energy + 1e-9).sqrt();
            if score > best_score {
                (best, best_score) = (offset as u64, score);
            }
            if offset + len < span {
                let (add, sub) = (self.region[offset + len], self.region[offset]);
                energy = (energy + f64::from(add * add) - f64::from(sub * sub)).max(0.0);
            }
        }
        lo + best
    }

    /// Media time of an absolute input position.
    fn media_at(&self, frame: f64) -> f64 {
        let anchor = self.anchors.iter().rev().find(|a| a.0 as f64 <= frame);
        anchor
            .or(self.anchors.front())
            .map_or(0.0, |&(f, pts)| pts + (frame - f as f64) / self.rate)
    }

    /// Drops input the next window can no longer reach.
    fn trim(&mut self) {
        let reach = (self.pos.round() as u64).saturating_sub(self.search as u64);
        let keep = self
            .natural
            .map_or(reach, |n| n.min(reach))
            .clamp(self.first, self.end);
        self.input
            .drain(..(keep - self.first) as usize * self.channels);
        self.first = keep;
        while self.anchors.len() > 1 && self.anchors[1].0 <= keep {
            self.anchors.pop_front();
        }
    }
}

/// Writes the mono mix of `frames` frames from frame `from` of `input` into `out`.
fn mix_mono(input: &[f32], channels: usize, from: usize, frames: usize, out: &mut Vec<f32>) {
    out.clear();
    let src = &input[from * channels..(from + frames) * channels];
    out.extend(src.chunks_exact(channels).map(|f| f.iter().sum::<f32>()));
}

/// Dot product with eight independent accumulators, so the compiler vectorises it.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let (ac, ar) = a.as_chunks::<8>();
    let (bc, br) = b.as_chunks::<8>();
    for (x, y) in ac.iter().zip(bc) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    acc.iter().sum::<f32>() + ar.iter().zip(br).map(|(x, y)| x * y).sum::<f32>()
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_tone_follows_the_stereo_mode() {
        let level = |wav: &[u8], from: usize, channel: usize| {
            (wav[44..].as_chunks::<4>().0.iter().skip(from).take(8000))
                .map(|f| i16::from_le_bytes([f[channel * 2], f[channel * 2 + 1]]).unsigned_abs())
                .max()
                .unwrap()
        };
        let stereo = test_tone(StereoMode::Stereo);
        assert_eq!(&stereo[..4], b"RIFF");
        // The first tone is on the left only, the second on the right only.
        assert!(level(&stereo, 2000, 0) > 5000 && level(&stereo, 2000, 1) == 0);
        assert!(level(&stereo, 20000, 0) == 0 && level(&stereo, 20000, 1) > 5000);
        let reverse = test_tone(StereoMode::Reverse);
        assert!(level(&reverse, 2000, 0) == 0 && level(&reverse, 2000, 1) > 5000);
        let right = test_tone(StereoMode::Right);
        assert!(level(&right, 2000, 0) == 0 && level(&right, 20000, 0) > 5000);
    }

    #[test]
    fn spectrum_peaks_at_the_tone() {
        let rate = 48_000;
        let samples: Vec<f32> = (0..1024)
            .map(|i| (i as f32 * 1000.0 * std::f32::consts::TAU / rate as f32).sin())
            .collect();
        let bands = spectrum(&samples, rate, 24);
        let peak = (0..24)
            .max_by(|&a, &b| bands[a].total_cmp(&bands[b]))
            .unwrap();
        // 1 kHz sits in band 12 of 24 between 50 Hz and 16 kHz.
        assert_eq!(peak, 12, "{bands:?}");
        assert!(bands[peak] > 0.9 && bands[0] < 0.5);
        assert!(spectrum(&[0.0; 1024], rate, 24).iter().all(|b| *b == 0.0));
    }

    use super::*;
    use std::time::Instant;

    const RATE: u32 = 48_000;

    fn sine(freq: f64, seconds: f64, channels: usize) -> Vec<f32> {
        let n = (f64::from(RATE) * seconds) as usize;
        (0..n)
            .flat_map(|i| {
                let s = (0.5 * (2.0 * PI * freq * i as f64 / f64::from(RATE)).sin()) as f32;
                std::iter::repeat_n(s, channels)
            })
            .collect()
    }

    /// Runs `input` through a stretcher in 1024-frame blocks, like decoded AAC frames.
    fn stretch(input: &[f32], channels: usize, speed: f32) -> (Vec<f32>, Vec<(f64, f64)>) {
        let mut s = Stretcher::new(RATE, channels);
        let mut out = Vec::new();
        let mut spans = Vec::new();
        for (i, block) in input.chunks(1024 * channels).enumerate() {
            let pts = (i * 1024) as f64 / f64::from(RATE);
            let before = out.len();
            let span = s.process(block, pts, speed, &mut out);
            if out.len() > before {
                spans.push(span);
            }
        }
        spans.push(s.drain(&mut out));
        (out, spans)
    }

    /// Frequency of a mono signal from its rising zero crossings.
    fn frequency(mono: &[f32]) -> f64 {
        let rising = mono
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        rising as f64 * f64::from(RATE) / mono.len() as f64
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&s| f64::from(s * s)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn wsola_keeps_pitch_and_scales_length() {
        let input = sine(440.0, 10.0, 1);
        for speed in [0.5f32, 1.5, 2.0, 0.25, 4.0] {
            let (out, spans) = stretch(&input, 1, speed);
            let expected = input.len() as f64 / f64::from(speed);
            let ratio = out.len() as f64 / expected;
            assert!(
                (ratio - 1.0).abs() < 0.02,
                "speed {speed}: length ratio {ratio}"
            );
            // Skip the edges, where the first window and the drain sit.
            let middle = &out[out.len() / 10..out.len() * 9 / 10];
            let f = frequency(middle);
            assert!((f - 440.0).abs() < 440.0 * 0.02, "speed {speed}: {f} Hz");
            // Spans tile the media timeline in order and end near the input's end. The drain
            // restarts at the last window's natural continuation, up to a hop and a search back.
            let slack = (0.0125 * f64::from(speed)).max(0.0125) + 0.012;
            for w in spans.windows(2) {
                assert!(
                    (w[1].0 - w[0].1).abs() < slack,
                    "speed {speed}: gap between {w:?}"
                );
            }
            let last = spans.last().unwrap().1;
            assert!(
                (last - 10.0).abs() < 0.03,
                "speed {speed}: media ends at {last}"
            );
            // A clean splice keeps the level steady: no window-rate dips or cancellations.
            let level = rms(middle) / rms(&input);
            assert!((level - 1.0).abs() < 0.05, "speed {speed}: level {level}");
        }
    }

    #[test]
    fn speed_one_is_bit_exact() {
        let input = sine(440.0, 1.0, 2);
        let (out, spans) = stretch(&input, 2, 1.0);
        assert_eq!(
            out.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
            input.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
        );
        assert_eq!(spans[0], (0.0, 1024.0 / f64::from(RATE)));
    }

    #[test]
    fn speed_changes_keep_media_time_continuous() {
        let input = sine(440.0, 6.0, 2);
        let mut s = Stretcher::new(RATE, 2);
        let mut out = Vec::new();
        let mut last_end = 0.0;
        for (i, block) in input.chunks(1024 * 2).enumerate() {
            let speed = [1.0, 1.5, 1.0, 0.5, 2.0, 1.0][i * 6 / (input.len() / 2048 + 1)];
            let pts = (i * 1024) as f64 / f64::from(RATE);
            let before = out.len();
            let (start, end) = s.process(block, pts, speed, &mut out);
            if out.len() > before {
                assert!(
                    (start - last_end).abs() < 0.03,
                    "block {i}: {last_end} -> {start}"
                );
                last_end = end;
            }
        }
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
    }

    /// Prints what WSOLA costs at 2x on 48 kHz stereo; run with --nocapture to see it.
    #[test]
    fn wsola_cost_at_2x() {
        let seconds = 20.0;
        let input = sine(440.0, seconds, 2);
        let started = Instant::now();
        let (out, _) = stretch(&input, 2, 2.0);
        let elapsed = started.elapsed().as_secs_f64();
        assert!(!out.is_empty());
        eprintln!(
            "WSOLA 2x, 48 kHz stereo: {:.1} ms per second of media ({:.2}% of one core)",
            elapsed * 1000.0 / seconds,
            elapsed / seconds * 100.0
        );
    }

    fn eq_with(bands: [f32; 10], preamp: f32) -> Equalizer {
        let mut eq = Equalizer::new(RATE, 1);
        eq.configure(&EqSettings {
            enabled: true,
            preset: None,
            preamp,
            bands,
            two_pass: false,
        });
        eq
    }

    #[test]
    fn flat_eq_is_identity() {
        let input = sine(1000.0, 0.5, 2);
        let effects = Effects::new();
        for preset in [None, Some(0)] {
            effects.set_eq(preset);
            let mut chain = Chain::new(RATE, 2);
            let mut samples = input.clone();
            let (out, ..) = chain.process(&effects, &mut samples, 0.0);
            assert_eq!(out, &input[..], "preset {preset:?}");
        }
    }

    #[test]
    fn band_boost_hits_its_band_only() {
        let gain_db = |freq: f64| {
            let mut bands = [0.0; 10];
            bands[4] = 12.0; // 1 kHz
            let mut eq = eq_with(bands, PREAMP_UNITY_DB);
            let input = sine(freq, 1.0, 1);
            let mut out = input.clone();
            eq.process(&mut out);
            let settled = input.len() / 2;
            20.0 * (rms(&out[settled..]) / rms(&input[settled..])).log10()
        };
        let at_band = gain_db(1000.0);
        assert!((at_band - 12.0).abs() < 0.5, "1 kHz gained {at_band} dB");
        let far = gain_db(60.0);
        assert!(far.abs() < 1.0, "60 Hz gained {far} dB");
    }

    #[test]
    fn preamp_follows_vlc_scale() {
        let mut eq = eq_with([0.0; 10], PREAMP_UNITY_DB - 6.0);
        let mut out = vec![1.0f32; 16];
        eq.process(&mut out);
        assert!((out[0] - 10f32.powf(-6.0 / 20.0)).abs() < 1e-6);
    }

    #[test]
    fn biquads_stay_stable_on_noise() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        };
        let effects = Effects::new();
        for (preset, two_pass) in [(6, true), (13, false), (4, true)] {
            effects.set_eq(Some(preset));
            effects.set_two_pass(two_pass);
            let mut chain = Chain::new(RATE, 2);
            for _ in 0..(10 * RATE / 1024) {
                let mut block: Vec<f32> = (0..2048).map(|_| noise()).collect();
                let (out, ..) = chain.process(&effects, &mut block, 0.0);
                // Presets lift some bands by over 20 dB, so only a runaway filter gets this far.
                assert!(out.iter().all(|s| s.is_finite() && s.abs() < 1000.0));
            }
        }
        effects.set_eq_bands(
            [
                20.0, -20.0, 20.0, -20.0, 20.0, -20.0, 20.0, -20.0, 20.0, -20.0,
            ],
            20.0,
        );
        let mut chain = Chain::new(RATE, 2);
        for _ in 0..(10 * RATE / 1024) {
            let mut block: Vec<f32> = (0..2048).map(|_| noise()).collect();
            let (out, ..) = chain.process(&effects, &mut block, 0.0);
            assert!(out.iter().all(|s| s.is_finite()));
        }
    }

    #[test]
    fn stereo_modes_map_channels() {
        let frame = [0.2f32, 0.6, 0.9];
        let expect = [
            (StereoMode::Stereo, [0.2, 0.6, 0.9]),
            (StereoMode::Mono, [0.4, 0.4, 0.9]),
            (StereoMode::Left, [0.2, 0.2, 0.9]),
            (StereoMode::Right, [0.6, 0.6, 0.9]),
            (StereoMode::Reverse, [0.6, 0.2, 0.9]),
        ];
        for (mode, want) in expect {
            let mut x = frame.repeat(2);
            apply_stereo(mode, &mut x, 3);
            assert_eq!(x, want.repeat(2), "{mode:?}");
        }
        let effects = Effects::new();
        for mode in StereoMode::ALL {
            effects.set_stereo(mode);
            assert_eq!(effects.stereo(), mode);
        }
    }

    #[test]
    fn settings_clamp() {
        let effects = Effects::new();
        effects.set_speed(9.0);
        assert_eq!(effects.speed(), MAX_SPEED);
        effects.set_speed(1.001);
        assert_eq!(effects.speed(), 1.0);
        effects.set_delay(-30.0);
        assert_eq!(effects.delay(), -MAX_DELAY);
        effects.set_eq(Some(EQ_PRESETS.len()));
        assert!(!effects.eq().enabled);
    }
}

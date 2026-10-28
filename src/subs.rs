//! Subtitle cues, decoded from an embedded stream as the demuxer routes it or from an external
//! file all at once. No drawing here; `captions` paints what `Cues::active` returns.
//!
//! libavcodec turns every text format (SRT, ASS, WebVTT, mov_text, SAMI...) into ASS event lines,
//! so one small ASS parser covers them all. Picture formats (DVD, DVB, PGS) arrive as palettized
//! rects, converted here to premultiplied BGRA.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use ff::ffi;
use ff::packet::Ref;
use ffmpeg_next as ff;

use crate::player::{Msg, PacketQueue, Shared};

/// A run of text with one style.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub italic: bool,
    pub bold: bool,
}

/// One picture of a bitmap subtitle, `w * h` premultiplied BGRA pixels in rows of `w * 4` bytes.
/// The pixels are shared, so cloning a cue every frame copies no image data.
#[derive(Clone, Debug, PartialEq)]
pub struct BitmapRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub bgra: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CueContent {
    /// Lines of text; ASS override tags stripped, \N and \n become line breaks. Spans keep italic/bold.
    Text(Vec<Vec<Span>>),
    /// Picture subtitles (DVD, DVB, PGS): BGRA premultiplied rects positioned on a canvas of
    /// `width` x `height` (the video size the subtitle was authored for).
    Bitmap {
        width: u32,
        height: u32,
        rects: Vec<BitmapRect>,
    },
}

/// Something to show from `start` until just before `end`, in seconds of media time.
#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub content: CueContent,
}

/// How long a cue stays when neither the stream nor a following cue says when it ends.
const DEFAULT_DURATION: f64 = 5.0;
/// PGS pictures never carry a duration; each stays until an empty "clear" picture or the next
/// one, which the demuxer delivers ahead of time, so they get a much longer guess.
const PGS_HOLD: f64 = 60.0;

/// A subtitle track's cues, filled by a decoder thread or loaded at once from a file. An ASS or
/// SSA track goes to libass instead, which draws each moment whole.
#[derive(Default)]
pub struct Cues {
    list: Mutex<Vec<Entry>>,
    ass: Option<crate::ass::Track>,
    /// Every cue's span seen so far, by start time's bits (starts are never negative, so the
    /// bits sort like the times). A seek clears `list` but not this, for the timeline.
    seen: Mutex<std::collections::BTreeMap<u64, f64>>,
}

struct Entry {
    cue: Cue,
    /// The end is a guess (`DEFAULT_DURATION` or `PGS_HOLD`); the next cue or a clear event cuts
    /// it short. PGS and some DVD tracks work this way: each picture stays until the next.
    open: bool,
}

impl Cues {
    /// Cues whose `[start, end)` contains `t`, earliest first. For libass, one picture of the
    /// moment, its start the time it was first drawn for.
    #[cfg(test)]
    pub fn active(&self, t: f64) -> Vec<Cue> {
        self.active_stretched(t, 0.0)
    }

    /// `active`, with each text or picture cue lasting `stretch` times as long; 0 leaves them
    /// as they are. libass draws its own timing.
    pub fn active_stretched(&self, t: f64, stretch: f64) -> Vec<Cue> {
        if let Some(track) = &self.ass {
            let (width, height) = track.size();
            return track
                .render(t)
                .map(|(at, picture)| Cue {
                    start: at,
                    end: f64::INFINITY,
                    content: CueContent::Bitmap {
                        width,
                        height,
                        rects: vec![picture],
                    },
                })
                .into_iter()
                .collect();
        }
        let cues = self.list.lock().unwrap();
        let started = cues.partition_point(|e| e.cue.start <= t);
        cues[..started]
            .iter()
            .filter(|e| {
                let end = if stretch > 0.0 {
                    e.cue.start + (e.cue.end - e.cue.start) * stretch
                } else {
                    e.cue.end
                };
                t < end
            })
            .map(|e| e.cue.clone())
            .collect()
    }

    /// When each cue starts and ends, in start order, for the seek bar's subtitle timeline.
    // ponytail: empty for libass tracks, and an embedded track only knows the cues the demuxer
    // has read so far; read libass' events and scan the file ahead if a full timeline matters.
    pub fn spans(&self) -> Vec<(f64, f64)> {
        let mut seen = self.seen.lock().unwrap().clone();
        // The cues held now have their final ends; earlier guesses may have been cut short.
        for e in self.list.lock().unwrap().iter() {
            seen.insert(e.cue.start.max(0.0).to_bits(), e.cue.end);
        }
        seen.into_iter()
            .map(|(s, e)| (f64::from_bits(s), e))
            .collect()
    }

    pub fn clear(&self) {
        self.list.lock().unwrap().clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.list.lock().unwrap().len()
    }

    /// Adds a cue in start order. With `open`, its end is only a guess.
    fn insert(&self, mut cue: Cue, open: bool) {
        let mut cues = self.list.lock().unwrap();
        let at = cues.partition_point(|e| e.cue.start <= cue.start);
        if open && let Some(next) = cues.get(at) {
            cue.end = cue.end.min(next.cue.start).max(cue.start);
        }
        close_open(&mut cues[..at], cue.start);
        // What `close_open` just cut short is final, so it goes in `seen` with the new cue.
        let mut seen = self.seen.lock().unwrap();
        for e in &cues[at.saturating_sub(8)..at] {
            seen.insert(e.cue.start.max(0.0).to_bits(), e.cue.end);
        }
        seen.insert(cue.start.max(0.0).to_bits(), cue.end);
        cues.insert(at, Entry { cue, open });
    }

    /// An empty subtitle at `t`: whatever is showing with a guessed end stops there.
    fn close(&self, t: f64) {
        let mut cues = self.list.lock().unwrap();
        let at = cues.partition_point(|e| e.cue.start < t);
        close_open(&mut cues[..at], t);
    }
}

/// Ends the open cues among `earlier` (sorted by start, all starting before `t`) at `t`.
fn close_open(earlier: &mut [Entry], t: f64) {
    // An open cue lasts at most PGS_HOLD, so older ones can't reach `t`.
    for e in earlier
        .iter_mut()
        .rev()
        .take_while(|e| e.cue.start + PGS_HOLD > t)
    {
        if e.open && e.cue.end > t {
            e.cue.end = t;
            e.open = false;
        }
    }
}

/// Whether libass draws a track of codec `id`.
fn styled(id: ff::codec::Id) -> bool {
    matches!(id, ff::codec::Id::ASS | ff::codec::Id::SSA) && crate::ass::styling()
}

/// Decodes an embedded subtitle stream's packets as the demuxer routes them. Stops when
/// `shared.quitting()`. On `Msg::Flush` (seek) it clears the cues, since the demuxer sends the
/// packets after the new position again. ASS and SSA go to libass, drawn on a `canvas` the
/// video's size.
pub fn spawn_embedded(
    parameters: ff::codec::Parameters,
    time_base: f64,
    packets: Arc<PacketQueue>,
    shared: Arc<Shared>,
    canvas: (u32, u32),
) -> (JoinHandle<()>, Arc<Cues>) {
    if styled(parameters.id()) {
        let header = unsafe {
            let p = &*parameters.as_ptr();
            match p.extradata.is_null() {
                true => &[][..],
                false => std::slice::from_raw_parts(p.extradata, p.extradata_size as usize),
            }
        };
        if let Some(track) = crate::ass::Track::new(header, canvas) {
            return spawn_styled(track, time_base, packets, shared);
        }
    }
    let cues = Arc::new(Cues::default());
    // Open here rather than on the thread: the parameters point into the demuxer's live stream.
    let mut decoder = Decoder::open(parameters, time_base)
        .inspect_err(|e| eprintln!("subtitles: {e}"))
        .ok();
    let out = cues.clone();
    let handle = thread::spawn(move || {
        // Without a decoder the queue is still drained, so it never holds the demuxer back.
        while !shared.quitting() && !packets.closed() {
            match packets.pop() {
                Some(Msg::Packet(p)) => {
                    if let Some(d) = &mut decoder {
                        d.decode(&p, &out);
                    }
                }
                Some(Msg::Flush(_)) => {
                    out.clear();
                    if let Some(d) = &mut decoder {
                        d.inner.flush();
                    }
                }
                Some(Msg::Eof) | None => {}
            }
        }
    });
    (handle, cues)
}

/// Hands an ASS track's packets to libass as the demuxer routes them. Each is one event in
/// Matroska's layout, and libass skips one it has, so seeks need nothing.
fn spawn_styled(
    track: crate::ass::Track,
    time_base: f64,
    packets: Arc<PacketQueue>,
    shared: Arc<Shared>,
) -> (JoinHandle<()>, Arc<Cues>) {
    let cues = Arc::new(Cues {
        ass: Some(track),
        ..Default::default()
    });
    let out = cues.clone();
    let handle = thread::spawn(move || {
        let track = out.ass.as_ref().unwrap();
        while !shared.quitting() && !packets.closed() {
            if let Some(Msg::Packet(p)) = packets.pop()
                && let (Some(data), Some(pts)) = (p.data(), p.pts())
            {
                let duration = p.duration() as f64 * time_base;
                track.add_event(data, pts as f64 * time_base, duration);
            }
        }
    });
    (handle, cues)
}

/// Reads a whole external subtitle file (.srt, .ass, .ssa, .vtt, .sub, .idx, .smi, .sup) with
/// libavformat and decodes every cue up front. An .ass or .ssa file goes to libass whole, drawn on
/// a `canvas` the video's size.
pub fn load_external(path: &Path, canvas: (u32, u32)) -> Result<Arc<Cues>, String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
    if matches!(ext.as_deref(), Some("ass" | "ssa"))
        && crate::ass::styling()
        && let Ok(bytes) = std::fs::read(path)
    {
        // libass reads UTF-8; old .ssa files are often in a Windows code page.
        let text = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes,
            Err(_) => {
                let language = crate::codepage::language_of(path);
                let cp = crate::codepage::detect(&bytes, language.as_deref());
                match crate::codepage::decode(&bytes, cp, false) {
                    Some(text) => text.into_bytes(),
                    None => bytes,
                }
            }
        };
        if let Some(track) = crate::ass::Track::from_file(&text, canvas) {
            return Ok(Arc::new(Cues {
                ass: Some(track),
                ..Default::default()
            }));
        }
    }
    let mut input = ff::format::input(path).map_err(|e| format!("cannot open {path:?}: {e}"))?;
    let stream = input
        .streams()
        .best(ff::media::Type::Subtitle)
        .ok_or_else(|| format!("no subtitles in {path:?}"))?;
    let index = stream.index();
    let mut decoder = Decoder::open(stream.parameters(), f64::from(stream.time_base()))?;
    // The whole file says far more about its code page than one cue does.
    if decoder.plain_text
        && let Ok(text) = std::fs::read(path)
    {
        let language = crate::codepage::language_of(path);
        decoder.code_page = Some(crate::codepage::detect(&text, language.as_deref()));
    }
    let cues = Cues::default();
    loop {
        let mut packet = ff::Packet::empty();
        match packet.read(&mut input) {
            Ok(()) if packet.stream() == index => decoder.decode(&packet, &cues),
            Ok(()) | Err(ff::Error::InvalidData) => {}
            Err(_) => break,
        }
    }
    Ok(Arc::new(cues))
}

/// File types `find_external` picks up. A `.sub` next to an `.idx` of the same name is VobSub
/// picture data, which libavformat reads through the `.idx`.
const EXTENSIONS: [&str; 9] = [
    "srt", "ass", "ssa", "vtt", "sub", "idx", "smi", "sami", "sup",
];

/// Subtitle files that belong to `video`: same stem (also "stem.en.srt" style suffixes) in its
/// folder or in a "Subs"/"Subtitles" subfolder, plus any subtitle in "Subs/<stem>/" as release
/// packs lay them out. The video's folder comes first, then by name.
pub fn find_external(video: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(stem)) = (video.parent(), video.file_stem()) else {
        return Vec::new();
    };
    let stem = stem.to_string_lossy().to_lowercase();
    let lower = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase());
    let subdirs = |dir: &Path, names: &[&str]| -> Vec<PathBuf> {
        let Ok(entries) = dir.read_dir() else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && lower(p).is_some_and(|n| names.contains(&n.as_str())))
            .collect();
        dirs.sort();
        dirs
    };

    // (folder rank, path); rank 0 is the video's own folder.
    let mut found: Vec<(usize, PathBuf)> = Vec::new();
    let mut scan = |rank: usize, dir: &Path, any_name: bool| {
        let Ok(entries) = dir.read_dir() else { return };
        let files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        let ext = |p: &Path| p.extension().map(|e| e.to_string_lossy().to_lowercase());
        for path in &files {
            let Some(e) = ext(path).filter(|e| EXTENSIONS.contains(&e.as_str())) else {
                continue;
            };
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            let ours = any_name
                || name == stem
                || name
                    .strip_prefix(&stem)
                    .is_some_and(|rest| rest.starts_with('.'));
            let vobsub_data = e == "sub"
                && files.iter().any(|o| {
                    o.with_extension("") == path.with_extension("")
                        && ext(o).as_deref() == Some("idx")
                });
            if ours && path.is_file() && !vobsub_data {
                found.push((rank, path.clone()));
            }
        }
    };
    scan(0, dir, false);
    for sub in subdirs(dir, &["subs", "subtitles"]) {
        scan(1, &sub, false);
        for own in subdirs(&sub, &[stem.as_str()]) {
            scan(2, &own, true);
        }
    }
    found.sort_by_key(|(rank, p)| (*rank, lower(p)));
    found.into_iter().map(|(_, p)| p).collect()
}

/// A subtitle decoder and what turning its output into cues needs.
struct Decoder {
    inner: ff::decoder::Subtitle,
    time_base: f64,
    /// How long a cue without an end time stays at most.
    hold: f64,
    /// Text codecs whose packets may arrive in a legacy code page instead of UTF-8.
    plain_text: bool,
    /// The code page of a whole file's text, guessed up front; None guesses per packet.
    code_page: Option<u32>,
}

impl Decoder {
    fn open(parameters: ff::codec::Parameters, time_base: f64) -> Result<Decoder, String> {
        let id = parameters.id();
        let mut ctx = ff::codec::Context::from_parameters(parameters).map_err(|e| e.to_string())?;
        unsafe { (*ctx.as_mut_ptr()).pkt_timebase = ff::Rational::from(time_base).into() };
        let inner = ctx
            .decoder()
            .subtitle()
            .map_err(|e| format!("cannot decode {} subtitles: {e}", id.name()))?;
        let text = unsafe {
            let desc = ffi::avcodec_descriptor_get(id.into());
            !desc.is_null() && (*desc).props & ffi::AV_CODEC_PROP_TEXT_SUB != 0
        };
        Ok(Decoder {
            inner,
            time_base,
            hold: match id {
                ff::codec::Id::HDMV_PGS_SUBTITLE => PGS_HOLD,
                _ => DEFAULT_DURATION,
            },
            // mov_text packets carry binary style records after the text.
            plain_text: text && id != ff::codec::Id::MOV_TEXT,
            code_page: None,
        })
    }

    fn decode(&mut self, packet: &ff::Packet, cues: &Cues) {
        // libavcodec rejects text that isn't UTF-8, and many old .srt files are in a Windows
        // code page instead.
        let recoded;
        let packet = match packet.data() {
            Some(data) if self.plain_text && std::str::from_utf8(data).is_err() => {
                let cp = (self.code_page).unwrap_or_else(|| crate::codepage::detect(data, None));
                recoded = recode(packet, data, cp);
                &recoded
            }
            _ => packet,
        };
        let mut sub = Subtitle::default();
        let mut got = 0;
        let ret = unsafe {
            ffi::avcodec_decode_subtitle2(
                self.inner.as_mut_ptr(),
                &mut sub.0,
                &mut got,
                packet.as_ptr(),
            )
        };
        if ret < 0 || got == 0 {
            return;
        }
        let s = &sub.0;
        // libavcodec fills pts from the packet (and PGS from its own segments), in AV_TIME_BASE.
        let base = if s.pts != ffi::AV_NOPTS_VALUE {
            s.pts as f64 / f64::from(ffi::AV_TIME_BASE)
        } else if let Some(pts) = packet.pts() {
            pts as f64 * self.time_base
        } else {
            return;
        };
        let start = base + f64::from(s.start_display_time) / 1000.0;
        // libavcodec already turns the packet duration into end_display_time when it has one.
        let (end, open) = match s.end_display_time {
            0 | u32::MAX => (start + self.hold, true),
            ms => (base + f64::from(ms) / 1000.0, false),
        };
        let rects: &[*mut ffi::AVSubtitleRect] = if s.num_rects == 0 || s.rects.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(s.rects, s.num_rects as usize) }
        };
        let mut lines = Vec::new();
        let mut bitmaps = Vec::new();
        for &rect in rects {
            let r = unsafe { &*rect };
            match r.type_ {
                ffi::AVSubtitleType::SUBTITLE_ASS if !r.ass.is_null() => {
                    let event = unsafe { std::ffi::CStr::from_ptr(r.ass) }.to_string_lossy();
                    lines.extend(parse_ass_event(&event));
                }
                ffi::AVSubtitleType::SUBTITLE_TEXT if !r.text.is_null() => {
                    let text = unsafe { std::ffi::CStr::from_ptr(r.text) }.to_string_lossy();
                    lines.extend(text.lines().filter(|l| !l.trim().is_empty()).map(|l| {
                        vec![Span {
                            text: l.trim_end().to_owned(),
                            italic: false,
                            bold: false,
                        }]
                    }));
                }
                ffi::AVSubtitleType::SUBTITLE_BITMAP => bitmaps.extend(bitmap_rect(r)),
                _ => {}
            }
        }
        let content = if !bitmaps.is_empty() {
            let (width, height) = self.canvas(&bitmaps);
            CueContent::Bitmap {
                width,
                height,
                rects: bitmaps,
            }
        } else if !lines.is_empty() {
            CueContent::Text(lines)
        } else {
            // An empty subtitle clears the screen (PGS and DVB end pictures this way).
            cues.close(start);
            return;
        };
        cues.insert(
            Cue {
                start,
                end,
                content,
            },
            open,
        );
    }

    /// The video size bitmap rects are placed on. DVD takes it from the .idx/extradata, PGS and
    /// DVB from their own segments; without one, guess the DVD/DVB frame the rects fit in.
    fn canvas(&self, rects: &[BitmapRect]) -> (u32, u32) {
        let ctx = unsafe { &*self.inner.as_ptr() };
        if ctx.width > 0 && ctx.height > 0 {
            return (ctx.width as u32, ctx.height as u32);
        }
        let bottom = rects.iter().map(|r| r.y + r.h as i32).max().unwrap_or(0);
        (720, if bottom > 480 { 576 } else { 480 })
    }
}

/// Owns an AVSubtitle and frees what the decoder allocated in it.
struct Subtitle(ffi::AVSubtitle);

impl Default for Subtitle {
    fn default() -> Self {
        Subtitle(unsafe { std::mem::zeroed() })
    }
}

impl Drop for Subtitle {
    fn drop(&mut self) {
        unsafe { ffi::avsubtitle_free(&mut self.0) }
    }
}

/// A copy of `packet` with its text converted from code page `cp` to UTF-8.
fn recode(packet: &ff::Packet, data: &[u8], cp: u32) -> ff::Packet {
    let text = crate::codepage::decode(data, cp, false)
        .unwrap_or_else(|| String::from_utf8_lossy(data).into_owned());
    let mut out = ff::Packet::copy(text.as_bytes());
    out.set_pts(packet.pts());
    out.set_dts(packet.dts());
    out.set_duration(packet.duration());
    out
}

/// Converts a PAL8 rect (indices in data[0], ARGB palette in data[1]) to premultiplied BGRA.
fn bitmap_rect(r: &ffi::AVSubtitleRect) -> Option<BitmapRect> {
    if r.w <= 0 || r.h <= 0 || r.data[0].is_null() || r.data[1].is_null() {
        return None;
    }
    let (w, h) = (r.w as usize, r.h as usize);
    let colors = r.nb_colors.clamp(0, 256) as usize;
    let palette: Vec<[u8; 4]> =
        unsafe { std::slice::from_raw_parts(r.data[1] as *const u32, colors) }
            .iter()
            .map(|&argb| {
                let a = argb >> 24;
                let premul = |shift: u32| (((argb >> shift) & 0xFF) * a / 255) as u8;
                [premul(0), premul(8), premul(16), a as u8]
            })
            .collect();
    let stride = r.linesize[0] as usize;
    if stride < w {
        return None;
    }
    let indices = unsafe { std::slice::from_raw_parts(r.data[0], stride * (h - 1) + w) };
    let mut bgra = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for &i in &indices[y * stride..y * stride + w] {
            bgra.extend(palette.get(usize::from(i)).unwrap_or(&[0; 4]));
        }
    }
    Some(BitmapRect {
        x: r.x,
        y: r.y,
        w: w as u32,
        h: h as u32,
        bgra: bgra.into(),
    })
}

/// Lines of styled text from one ASS event. Takes a full "Dialogue:" line (text after the 9th
/// comma) or the form libavcodec decoders emit, "ReadOrder,Layer,Style,Name,MarginL,MarginR,
/// MarginV,Effect,Text" (text after the 8th comma).
fn parse_ass_event(event: &str) -> Vec<Vec<Span>> {
    let (fields, commas) = match event.strip_prefix("Dialogue:") {
        Some(rest) => (rest, 9),
        None => (event, 8),
    };
    let text = fields.splitn(commas + 1, ',').nth(commas).unwrap_or("");
    parse_ass_text(text.trim_end_matches(['\r', '\n']))
}

/// Strips `{...}` override blocks, keeping `\i` and `\b` (and `\r`, which resets them). `\N` and
/// `\n` break lines, `\h` is a space, and vector drawings (`\p1` ... `\p0`) are dropped.
/// Real ASS and SSA tracks go to libass (`ass.rs`); this reads the ASS events libavcodec makes
/// of every other text format, and ASS itself when libass isn't there.
fn parse_ass_text(text: &str) -> Vec<Vec<Span>> {
    let mut lines = vec![Vec::new()];
    let (mut italic, mut bold, mut drawing) = (false, false, false);
    let mut run = String::new();
    // Ends the current run as a span, merging it into the previous one when the style matches.
    let flush = |run: &mut String, line: &mut Vec<Span>, italic: bool, bold: bool| {
        if run.is_empty() {
            return;
        }
        match line.last_mut() {
            Some(last) if last.italic == italic && last.bold == bold => last.text.push_str(run),
            _ => line.push(Span {
                text: run.clone(),
                italic,
                bold,
            }),
        }
        run.clear();
    };
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let line = lines.last_mut().unwrap();
        if c == '{'
            && let Some(close) = rest.find('}')
        {
            flush(&mut run, line, italic, bold);
            for tag in rest[1..close].split('\\').map(str::trim) {
                // A flag is the tag letter plus digits only, so \blur, \bord and \iclip don't count.
                let value = |name: char| {
                    tag.strip_prefix(name)
                        .filter(|v| v.chars().all(|c| c.is_ascii_digit()))
                        .map(|v| v.parse::<u32>().unwrap_or(0))
                };
                if let Some(v) = value('i') {
                    italic = v == 1;
                } else if let Some(v) = value('b') {
                    // \b1, or a weight like \b700.
                    bold = v == 1 || v >= 600;
                } else if let Some(v) = value('p') {
                    drawing = v > 0;
                } else if tag.starts_with('r') {
                    (italic, bold) = (false, false);
                }
            }
            rest = &rest[close + 1..];
            continue;
        }
        if c == '\\' {
            match rest[1..].chars().next() {
                Some('N' | 'n') => {
                    flush(&mut run, line, italic, bold);
                    lines.push(Vec::new());
                    rest = &rest[2..];
                    continue;
                }
                Some('h') => {
                    if !drawing {
                        run.push(' ');
                    }
                    rest = &rest[2..];
                    continue;
                }
                _ => {}
            }
        }
        if !drawing {
            run.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut run, lines.last_mut().unwrap(), italic, bold);
    // Spaces at the line ends would shift centred text; empty lines at either end are noise.
    for line in &mut lines {
        if let Some(first) = line.first_mut() {
            first.text = first.text.trim_start().to_owned();
        }
        if let Some(last) = line.last_mut() {
            last.text = last.text.trim_end().to_owned();
        }
        line.retain(|s| !s.text.is_empty());
    }
    while lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    let blank = lines.iter().take_while(|l| l.is_empty()).count();
    lines.drain(..blank);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;
    use std::time::{Duration, Instant};

    fn span(text: &str, italic: bool, bold: bool) -> Span {
        Span {
            text: text.into(),
            italic,
            bold,
        }
    }

    fn text(cue: &Cue) -> &Vec<Vec<Span>> {
        match &cue.content {
            CueContent::Text(lines) => lines,
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// A fresh folder under the temp dir, emptied on every run.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("myvlc-subs-{name}"));
        _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.002
    }

    #[test]
    fn parses_ass_event_tags() {
        let line = r"Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\an8\i1}Hello,{\i0} wor{\b1}ld{\b0}!\NSecond\hline{\blur3\bord2} done";
        assert_eq!(
            parse_ass_event(line),
            vec![
                vec![
                    span("Hello,", true, false),
                    span(" wor", false, false),
                    span("ld", false, true),
                    span("!", false, false)
                ],
                vec![span("Second line done", false, false)],
            ]
        );
        // The decoder form has one comma less before the text; drawings and \r are handled.
        let decoded = r"3,0,Default,,0,0,0,,{\b700}Bold{\r} plain{\p1}m 0 0 l 10 10{\p0}\n";
        assert_eq!(
            parse_ass_event(decoded),
            vec![vec![
                span("Bold", false, true),
                span(" plain", false, false)
            ]]
        );
        assert!(parse_ass_event(r"0,0,Default,,0,0,0,,{\p1}m 0 0 l 1 1").is_empty());
    }

    #[test]
    fn open_cues_end_at_the_next_one() {
        let cues = Cues::default();
        let pic = || CueContent::Text(vec![vec![span("x", false, false)]]);
        cues.insert(
            Cue {
                start: 1.0,
                end: 6.0,
                content: pic(),
            },
            true,
        );
        cues.insert(
            Cue {
                start: 3.0,
                end: 8.0,
                content: pic(),
            },
            true,
        );
        cues.close(4.0);
        assert_eq!(cues.active(2.9).len(), 1);
        assert_eq!(cues.active(3.0)[0].start, 3.0);
        assert!(cues.active(4.0).is_empty());
        // Out of order: an open cue inserted before a later one ends where that one starts.
        cues.insert(
            Cue {
                start: 0.0,
                end: 5.0,
                content: pic(),
            },
            true,
        );
        assert!(cues.active(0.5).len() == 1 && cues.active(1.0).len() == 1);
    }

    #[test]
    fn stretched_cues_last_longer() {
        let cues = Cues::default();
        let cue = Cue {
            start: 1.0,
            end: 2.0,
            content: CueContent::Text(Vec::new()),
        };
        cues.insert(cue, false);
        assert!(cues.active(2.5).is_empty());
        assert_eq!(cues.active_stretched(2.5, 2.0).len(), 1);
        assert!(cues.active_stretched(3.0, 2.0).is_empty());
        assert!(cues.active_stretched(1.6, 0.5).is_empty());
        assert_eq!(cues.spans(), [(1.0, 2.0)]);
        // A seek empties the cues; the timeline keeps them.
        cues.clear();
        assert!(cues.active(1.5).is_empty());
        assert_eq!(cues.spans(), [(1.0, 2.0)]);
    }

    #[test]
    fn loads_cyrillic_srt() {
        let dir = temp_dir("srt-1251");
        let path = dir.join("movie.srt");
        // "Привет, мир!" and "До свидания." in Windows-1251.
        let srt = b"1\r\n00:00:01,000 --> 00:00:02,000\r\n\xcf\xf0\xe8\xe2\xe5\xf2, \xec\xe8\xf0!\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\n\xc4\xee \xf1\xe2\xe8\xe4\xe0\xed\xe8\xff.\r\n\r\n";
        std::fs::write(&path, srt).unwrap();
        let cues = load_external(&path, (1920, 1080)).unwrap();
        assert_eq!(
            text(&cues.active(1.5)[0]),
            &vec![vec![span("Привет, мир!", false, false)]]
        );
        assert_eq!(
            text(&cues.active(3.5)[0]),
            &vec![vec![span("До свидания.", false, false)]]
        );
    }

    #[test]
    fn loads_srt() {
        let dir = temp_dir("srt");
        let path = dir.join("movie.srt");
        // Windows-1252 bytes (0x92 is a curly apostrophe), as many old .srt files are.
        let srt = b"1\r\n00:00:01,000 --> 00:00:02,500\r\nIt\x92s <i>fine</i>\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nTwo\r\n<b>lines</b>\r\n\r\n";
        std::fs::write(&path, srt).unwrap();
        let cues = load_external(&path, (1920, 1080)).unwrap();
        assert_eq!(cues.len(), 2);
        let first = &cues.active(1.0)[0];
        assert!(
            close(first.start, 1.0) && close(first.end, 2.5),
            "{first:?}"
        );
        assert_eq!(
            text(first),
            &vec![vec![
                span("It\u{2019}s ", false, false),
                span("fine", true, false)
            ]]
        );
        let second = &cues.active(3.5)[0];
        assert!(
            close(second.start, 3.0) && close(second.end, 4.0),
            "{second:?}"
        );
        assert_eq!(
            text(second),
            &vec![
                vec![span("Two", false, false)],
                vec![span("lines", false, true)]
            ]
        );
        assert!(cues.active(2.5).is_empty() && cues.active(0.5).is_empty());
    }

    #[test]
    fn loads_ass() {
        let dir = temp_dir("ass");
        let path = dir.join("movie.ass");
        let ass = "\u{feff}[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\n\n\
            [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
            Style: Default,Arial,60,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,40,1\n\n\
            [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
            Dialogue: 0,0:00:05.50,0:00:07.00,Default,,0,0,0,,Later, with commas\n\
            Dialogue: 0,0:00:01.25,0:00:03.00,Default,,0,0,0,,{\\i1}Leaning{\\i0} in\\Nnext line\n";
        std::fs::write(&path, ass).unwrap();
        // libass draws the file, one picture per moment on the video-sized canvas.
        let cues = load_external(&path, (1920, 1080)).unwrap();
        let shown = |t: f64| match cues.active(t).as_slice() {
            [
                Cue {
                    content:
                        CueContent::Bitmap {
                            width: 1920,
                            height: 1080,
                            rects,
                        },
                    ..
                },
            ] => rects.len() == 1,
            [] => false,
            other => panic!("{other:?}"),
        };
        assert!(!shown(1.0) && shown(2.0) && !shown(4.0) && shown(6.0) && !shown(7.5));

        // Without libass, and for every other text format, the events are read as text.
        assert_eq!(
            parse_ass_event("0,0,Default,,0,0,0,,{\\i1}Leaning{\\i0} in\\Nnext line"),
            vec![
                vec![span("Leaning", true, false), span(" in", false, false)],
                vec![span("next line", false, false)]
            ]
        );
        assert_eq!(
            parse_ass_event("Dialogue: 0,0:00:05.50,0:00:07.00,Default,,0,0,0,,Later, with commas"),
            vec![vec![span("Later, with commas", false, false)]]
        );
    }

    #[test]
    fn loads_webvtt() {
        let path = temp_dir("vtt").join("movie.en.vtt");
        let vtt = "WEBVTT\n\n00:00.500 --> 00:02.000\n<i>Over</i> here\n\n00:03.000 --> 00:04.000 align:start\nThere\n";
        std::fs::write(&path, vtt).unwrap();
        let cues = load_external(&path, (1920, 1080)).unwrap();
        assert_eq!(cues.len(), 2);
        let first = &cues.active(1.0)[0];
        assert!(
            close(first.start, 0.5) && close(first.end, 2.0),
            "{first:?}"
        );
        assert_eq!(
            text(first),
            &vec![vec![span("Over", true, false), span(" here", false, false)]]
        );
        assert_eq!(
            text(&cues.active(3.0)[0]),
            &vec![vec![span("There", false, false)]]
        );
    }

    #[test]
    fn finds_external_files() {
        let dir = temp_dir("find");
        let touch = |rel: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        };
        for f in [
            "Show S01E01.mkv",
            "Show S01E01.srt",
            "show s01e01.en.forced.SRT",
            "Show S01E01.idx",
            "Show S01E01.sub",
            "Show S01E01.txt",
            "Show S01E02.srt",
            "Show S01E011.srt",
            "Subs/Show S01E01.ass",
            "Subs/Other.srt",
            "Subtitles/Show S01E01.de.vtt",
            "Subs/Show S01E01/2_English.srt",
            "Subs/Show S01E01/notes.nfo",
        ] {
            touch(f);
        }
        let names: Vec<String> = find_external(&dir.join("Show S01E01.mkv"))
            .iter()
            .map(|p| {
                p.strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            names,
            [
                "show s01e01.en.forced.SRT",
                "Show S01E01.idx",
                "Show S01E01.srt",
                "Subs/Show S01E01.ass",
                "Subtitles/Show S01E01.de.vtt",
                "Subs/Show S01E01/2_English.srt",
            ]
        );
        assert!(find_external(Path::new("C:/nonexistent/clip.mp4")).is_empty());
    }

    /// Waits until `cues` holds `n` cues, as a decoder thread fills them.
    fn wait_for(cues: &Cues, n: usize) {
        let started = Instant::now();
        while cues.len() < n {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "only {} cues",
                cues.len()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Matroska tracks stored zlib-compressed (mkvmerge's default for subtitles) need FFmpeg
    /// built with zlib; without it the demuxer reports "Unsupported encoding type".
    #[test]
    fn reads_zlib_compressed_matroska_tracks() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_zlib_subs.mkv");
        let mut input = ff::format::input(&path).unwrap();
        let stream = input.streams().best(ff::media::Type::Subtitle).unwrap();
        let index = stream.index();
        let shared = Arc::new(Shared::new());
        let queue = Arc::new(PacketQueue::default());
        let (thread, cues) = spawn_embedded(
            stream.parameters(),
            f64::from(stream.time_base()),
            queue.clone(),
            shared.clone(),
            (1920, 1080),
        );
        for (s, packet) in input.packets() {
            if s.index() == index {
                queue.push(Msg::Packet(packet));
            }
        }
        queue.push(Msg::Eof);
        wait_for(&cues, 2);
        let first = &cues.active(1.5)[0];
        assert!(
            close(first.start, 1.0) && close(first.end, 3.0),
            "{first:?}"
        );
        assert_eq!(
            text(first),
            &vec![vec![span("First zlib cue", false, false)]]
        );
        assert_eq!(
            text(&cues.active(4.0)[0]),
            &vec![vec![span("Second zlib cue", false, false)]]
        );
        shared.quit.store(true, Relaxed);
        thread.join().unwrap();
    }

    #[test]
    fn decodes_embedded_subrip() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/test_subs.mkv");
        let mut input = ff::format::input(&path).unwrap();
        let stream = input.streams().best(ff::media::Type::Subtitle).unwrap();
        let index = stream.index();
        let shared = Arc::new(Shared::new());
        let queue = Arc::new(PacketQueue::default());
        let (thread, cues) = spawn_embedded(
            stream.parameters(),
            f64::from(stream.time_base()),
            queue.clone(),
            shared.clone(),
            (1920, 1080),
        );
        for (s, packet) in input.packets() {
            if s.index() == index {
                queue.push(Msg::Packet(packet));
            }
        }
        queue.push(Msg::Eof);
        wait_for(&cues, 2);
        let first = &cues.active(1.0)[0];
        assert!(
            close(first.start, 0.5) && close(first.end, 1.5),
            "{first:?}"
        );
        assert_eq!(
            text(first),
            &vec![vec![
                span("Hello ", false, false),
                span("world", true, false)
            ]]
        );
        let second = &cues.active(2.5)[0];
        assert!(
            close(second.start, 2.0) && close(second.end, 3.0),
            "{second:?}"
        );
        assert_eq!(
            text(second),
            &vec![
                vec![span("Second cue", false, false)],
                vec![span("on two lines", false, false)]
            ]
        );

        queue.push(Msg::Flush(1));
        let started = Instant::now();
        while cues.len() > 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "flush kept the cues"
            );
            thread::sleep(Duration::from_millis(5));
        }
        shared.quit.store(true, Relaxed);
        thread.join().unwrap();
    }

    /// One DVD subpicture packet: a `w` x `h` box of colour 1 at (`x`, `y`), shown for `ms`.
    fn dvd_packet(x: u16, y: u16, w: u16, h: u16, ms: u32) -> Vec<u8> {
        // Each line is one "colour 1 to the end of the line" RLE code; both fields share them.
        let rle: Vec<u8> = (0..h.div_ceil(2)).flat_map(|_| [0x00, 0x01]).collect();
        let first = 4 + rle.len() as u16;
        let (x2, y2) = (x + w - 1, y + h - 1);
        let mut start_seq = vec![0, 0];
        let stop_at = first + 2 + 2 + 3 + 3 + 7 + 5 + 1 + 1;
        start_seq.extend(stop_at.to_be_bytes());
        start_seq.extend([0x03, 0x00, 0x10]); // colours 0-3 -> palette 0, 1, 0, 0
        start_seq.extend([0x04, 0x00, 0xF0]); // colour 1 opaque, colour 0 transparent
        start_seq.extend([
            0x05,
            (x >> 4) as u8,
            ((x & 15) << 4 | x2 >> 8) as u8,
            x2 as u8,
        ]);
        start_seq.extend([(y >> 4) as u8, ((y & 15) << 4 | y2 >> 8) as u8, y2 as u8]);
        start_seq.extend([0x06, 0, 4, 0, 4]);
        start_seq.extend([0x01, 0xFF]);
        assert_eq!(first as usize + start_seq.len(), stop_at as usize);
        // Delays count 1024/90000 s ticks.
        let ticks = (ms * 90 / 1024) as u16;
        let mut stop_seq = ticks.to_be_bytes().to_vec();
        stop_seq.extend(stop_at.to_be_bytes());
        stop_seq.extend([0x02, 0xFF]);
        let body = [rle, start_seq, stop_seq].concat();
        let size = (4 + body.len()) as u16;
        [&size.to_be_bytes()[..], &first.to_be_bytes(), &body].concat()
    }

    #[test]
    fn decodes_dvd_bitmaps() {
        let mut parameters = ff::codec::Parameters::new();
        parameters.set_medium(ff::media::Type::Subtitle);
        parameters.set_id(ff::codec::Id::DVD_SUBTITLE);
        let extradata = b"size: 720x480\npalette: 000000, 3366cc, 000000, 000000, 000000, 000000, 000000, 000000, 000000, 000000, 000000, 000000, 000000, 000000, 000000, 000000\n";
        unsafe {
            let p = parameters.as_mut_ptr();
            let pad = ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
            (*p).extradata = ffi::av_mallocz(extradata.len() + pad).cast();
            std::ptr::copy_nonoverlapping(extradata.as_ptr(), (*p).extradata, extradata.len());
            (*p).extradata_size = extradata.len() as i32;
        }
        let shared = Arc::new(Shared::new());
        let queue = Arc::new(PacketQueue::default());
        let (thread, cues) = spawn_embedded(
            parameters,
            1.0 / 90000.0,
            queue.clone(),
            shared.clone(),
            (720, 480),
        );
        for (pts, x) in [(90000, 100), (3 * 90000, 300)] {
            let mut packet = ff::Packet::copy(&dvd_packet(x, 400, 64, 20, 1000));
            packet.set_pts(Some(pts));
            queue.push(Msg::Packet(packet));
        }
        wait_for(&cues, 2);
        let cue = &cues.active(1.5)[0];
        assert!(
            close(cue.start, 1.0) && (cue.end - 2.0).abs() < 0.02,
            "{cue:?}"
        );
        let CueContent::Bitmap {
            width,
            height,
            rects,
        } = &cue.content
        else {
            panic!("{cue:?}")
        };
        assert_eq!((*width, *height, rects.len()), (720, 480, 1));
        let r = &rects[0];
        assert_eq!((r.x, r.y, r.w, r.h), (100, 400, 64, 20));
        // Opaque 0x3366cc, premultiplied BGRA.
        assert!(
            r.bgra.chunks(4).all(|p| p == [0xcc, 0x66, 0x33, 0xff]),
            "{:?}",
            &r.bgra[..8]
        );
        assert!(
            matches!(&cues.active(3.5)[0].content, CueContent::Bitmap { rects, .. } if rects[0].x == 300)
        );
        shared.quit.store(true, Relaxed);
        thread.join().unwrap();
    }
}

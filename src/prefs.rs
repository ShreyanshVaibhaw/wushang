//! Settings the Settings window edits, kept in `settings.txt` next to the library state as
//! `key = value` lines. A missing file, unknown key or bad value falls back to the default, so a
//! hand edit or an older file can't lose the rest.
//!
//! Choices are stored by a short key ("fullscreen", not 1), so reordering a list later doesn't
//! change what a saved file means.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use crate::captions::CaptionStyle;

const FILE: &str = "settings.txt";
const HEADER: &str = "# myvlc settings v1";

/// A list of choices: (stored key, label).
pub type Choices = &'static [(&'static str, &'static str)];

pub const THEME: Choices = &[("dark", "Dark"), ("light", "Light"), ("system", "System")];
pub const AUTO_HIDE: Choices = &[
    ("always", "Always"),
    ("fullscreen", "In fullscreen only"),
    ("never", "Never"),
];
pub const CLICK: Choices = &[("nothing", "Nothing"), ("pause", "Play / Pause")];
pub const DOUBLE_CLICK: Choices = &[("fullscreen", "Toggle fullscreen"), ("nothing", "Nothing")];
pub const WHEEL: Choices = &[
    ("volume", "Volume"),
    ("seek", "Seek"),
    ("nothing", "Nothing"),
];
pub const RESUME: Choices = &[("always", "Always"), ("never", "Never")];
pub const REPEAT: Choices = &[("off", "Off"), ("all", "All"), ("one", "One")];
pub const AT_END: Choices = &[("library", "Go to the library"), ("quit", "Quit myvlc")];
pub const STARTUP_VOLUME: Choices = &[
    ("remember", "Remember last volume"),
    ("25", "25%"),
    ("50", "50%"),
    ("75", "75%"),
    ("100", "100%"),
];
pub const OUTLINE: Choices = &[
    ("none", "None"),
    ("thin", "Thin"),
    ("normal", "Normal"),
    ("thick", "Thick"),
];
/// Outline widths in DIPs, by `OUTLINE`.
pub const OUTLINE_WIDTH: [f32; 4] = [0.0, 1.0, 2.0, 3.5];
/// The skip choices in seconds, for the four skip lengths.
pub const SKIPS: [u32; 12] = [1, 3, 5, 10, 15, 20, 30, 60, 90, 120, 300, 600];
pub const VOLUME_STEPS: [u32; 5] = [1, 2, 5, 10, 20];
/// Subtitle and outline colours to pick from; the caption's own grey and near-black first.
pub const COLORS: [(u32, &str); 8] = [
    (0xB8B8B8, "Light grey"),
    (0xFFFFFF, "White"),
    (0xFFE14D, "Yellow"),
    (0x6BE3FF, "Cyan"),
    (0x8CF27A, "Green"),
    (0xFF9EC7, "Pink"),
    (0x808080, "Grey"),
    (0x0C0C0C, "Black"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    // General.
    /// Index into `THEME`.
    pub theme: usize,
    /// One of `ui::theme::ACCENTS`, or any colour a hand edit put there.
    pub accent: u32,
    // Interface.
    pub on_top: bool,
    pub remember_window: bool,
    /// The window's last restored rect in screen pixels (left, top, width, height), and whether
    /// it was maximized.
    pub window: Option<([i32; 4], bool)>,
    /// Index into `AUTO_HIDE`.
    pub auto_hide: usize,
    /// Seconds without mouse movement before the bars hide.
    pub hide_after: f32,
    // Privacy.
    pub save_recent: bool,
    // Controls.
    /// Seconds for Alt, plain, Shift and Ctrl + Left/Right.
    pub skip: [u32; 4],
    pub click: usize,
    pub double_click: usize,
    pub wheel: usize,
    pub reverse_wheel: bool,
    // Playback.
    pub resume: usize,
    pub autoplay: bool,
    pub speed: f32,
    /// An ISO 639 code from `session::LANGUAGES`; empty plays the file's own default track.
    pub audio_language: String,
    pub repeat: usize,
    pub shuffle: bool,
    pub at_end: usize,
    // Video.
    pub start_fullscreen: bool,
    pub keep_awake: bool,
    /// A label of `ui::panels::ASPECT`.
    pub aspect: String,
    // Audio.
    /// An output device name; empty follows the system default.
    pub device: String,
    pub startup_volume: usize,
    /// The volume when myvlc last closed, 0..2.
    pub volume: f32,
    /// Percent per Up/Down press.
    pub volume_step: u32,
    /// A name of `dsp::EQ_PRESETS`; empty turns the equalizer off.
    pub eq_preset: String,
    pub two_pass: bool,
    /// Index into `ui::panels::STEREO`.
    pub stereo: usize,
    // Subtitles.
    pub subtitles: bool,
    pub sub_language: String,
    pub auto_subs: bool,
    /// Relative to the design's size, 0.5..2.
    pub sub_size: f32,
    pub sub_bold: bool,
    pub sub_color: u32,
    pub sub_opacity: f32,
    pub outline: usize,
    pub outline_color: u32,
    pub sub_shadow: bool,
    // Library.
    pub show_hidden: bool,
    // Performance.
    pub hw_decoding: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        let caption = CaptionStyle::default();
        Prefs {
            theme: 0,
            accent: crate::ui::theme::DEFAULT_ACCENT,
            on_top: false,
            remember_window: true,
            window: None,
            auto_hide: 0,
            hide_after: 3.0,
            save_recent: true,
            skip: [3, 10, 60, 300],
            click: 0,
            double_click: 0,
            wheel: 0,
            reverse_wheel: false,
            resume: 0,
            autoplay: true,
            speed: 1.0,
            audio_language: String::new(),
            repeat: 0,
            shuffle: false,
            at_end: 0,
            start_fullscreen: false,
            keep_awake: true,
            aspect: "Default".into(),
            device: String::new(),
            startup_volume: 0,
            volume: 1.0,
            volume_step: 10,
            eq_preset: String::new(),
            two_pass: false,
            stereo: 0,
            subtitles: true,
            sub_language: String::new(),
            auto_subs: true,
            sub_size: 1.0,
            sub_bold: false,
            sub_color: caption.text,
            sub_opacity: 1.0,
            outline: 2,
            outline_color: caption.outline,
            sub_shadow: false,
            show_hidden: false,
            hw_decoding: true,
        }
    }
}

impl Prefs {
    /// Reads `dir`\settings.txt; anything missing or unreadable keeps its default.
    pub fn load(dir: &Path) -> Prefs {
        fs::read_to_string(dir.join(FILE)).map_or_else(|_| Prefs::default(), |t| parse(&t))
    }

    /// Writes settings.txt through a temporary file, so a crash mid-write keeps the old one.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.tmp"));
        fs::write(&tmp, self.to_text())?;
        fs::rename(tmp, dir.join(FILE))
    }

    fn to_text(&self) -> String {
        let key = |list: Choices, i: usize| list.get(i).map_or("", |c| c.0);
        let mut out = format!("{HEADER}\n");
        let mut line = |k: &str, v: String| _ = writeln!(out, "{k} = {v}");
        line("theme", key(THEME, self.theme).into());
        line("accent", format!("#{:06X}", self.accent));
        line("on_top", self.on_top.to_string());
        line("remember_window", self.remember_window.to_string());
        if let Some(([x, y, w, h], max)) = self.window {
            line("window", format!("{x} {y} {w} {h} {max}"));
        }
        line("auto_hide", key(AUTO_HIDE, self.auto_hide).into());
        line("hide_after", self.hide_after.to_string());
        line("save_recent", self.save_recent.to_string());
        let [a, b, c, d] = self.skip;
        line("skip", format!("{a} {b} {c} {d}"));
        line("click", key(CLICK, self.click).into());
        line("double_click", key(DOUBLE_CLICK, self.double_click).into());
        line("wheel", key(WHEEL, self.wheel).into());
        line("reverse_wheel", self.reverse_wheel.to_string());
        line("resume", key(RESUME, self.resume).into());
        line("autoplay", self.autoplay.to_string());
        line("speed", self.speed.to_string());
        line("audio_language", self.audio_language.clone());
        line("repeat", key(REPEAT, self.repeat).into());
        line("shuffle", self.shuffle.to_string());
        line("at_end", key(AT_END, self.at_end).into());
        line("start_fullscreen", self.start_fullscreen.to_string());
        line("keep_awake", self.keep_awake.to_string());
        line("aspect", self.aspect.clone());
        line("device", self.device.clone());
        line(
            "startup_volume",
            key(STARTUP_VOLUME, self.startup_volume).into(),
        );
        line("volume", self.volume.to_string());
        line("volume_step", self.volume_step.to_string());
        line("eq_preset", self.eq_preset.clone());
        line("two_pass", self.two_pass.to_string());
        line("stereo", self.stereo.to_string());
        line("subtitles", self.subtitles.to_string());
        line("sub_language", self.sub_language.clone());
        line("auto_subs", self.auto_subs.to_string());
        line("sub_size", self.sub_size.to_string());
        line("sub_bold", self.sub_bold.to_string());
        line("sub_color", format!("#{:06X}", self.sub_color));
        line("sub_opacity", self.sub_opacity.to_string());
        line("outline", key(OUTLINE, self.outline).into());
        line("outline_color", format!("#{:06X}", self.outline_color));
        line("sub_shadow", self.sub_shadow.to_string());
        line("show_hidden", self.show_hidden.to_string());
        line("hw_decoding", self.hw_decoding.to_string());
        out
    }

    /// The subtitle look these settings describe.
    pub fn caption(&self) -> CaptionStyle {
        let base = CaptionStyle::default();
        CaptionStyle {
            size_ratio: base.size_ratio * self.sub_size,
            text: self.sub_color,
            outline: self.outline_color,
            outline_width: OUTLINE_WIDTH[self.outline.min(3)],
            bold: self.sub_bold,
            opacity: self.sub_opacity,
            shadow: self.sub_shadow,
            ..base
        }
    }

    /// The volume to start at.
    pub fn startup_volume(&self) -> f32 {
        match STARTUP_VOLUME.get(self.startup_volume) {
            Some((percent, _)) if self.startup_volume > 0 => {
                percent.parse::<f32>().map_or(1.0, |p| p / 100.0)
            }
            _ => self.volume,
        }
    }
}

fn parse(text: &str) -> Prefs {
    let mut p = Prefs::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        let flag = |b: &mut bool| {
            if let Ok(v) = v.parse() {
                *b = v;
            }
        };
        let choice = |i: &mut usize, list: Choices| {
            if let Some(n) = list.iter().position(|c| c.0 == v) {
                *i = n;
            }
        };
        let number = |f: &mut f32, min: f32, max: f32| {
            if let Ok(n) = v.parse::<f32>()
                && n.is_finite()
            {
                *f = n.clamp(min, max);
            }
        };
        let colour = |c: &mut u32| {
            if let Some(n) = v
                .strip_prefix('#')
                .and_then(|h| u32::from_str_radix(h, 16).ok())
            {
                *c = n & 0xFF_FFFF;
            }
        };
        match k {
            "theme" => choice(&mut p.theme, THEME),
            "accent" => colour(&mut p.accent),
            "on_top" => flag(&mut p.on_top),
            "remember_window" => flag(&mut p.remember_window),
            "window" => {
                let parts: Vec<&str> = v.split_whitespace().collect();
                if let [x, y, w, h, max] = parts[..]
                    && let (Ok(x), Ok(y), Ok(w), Ok(h), Ok(max)) =
                        (x.parse(), y.parse(), w.parse(), h.parse(), max.parse())
                    && w > 0
                    && h > 0
                {
                    p.window = Some(([x, y, w, h], max));
                }
            }
            "auto_hide" => choice(&mut p.auto_hide, AUTO_HIDE),
            "hide_after" => number(&mut p.hide_after, 1.0, 10.0),
            "save_recent" => flag(&mut p.save_recent),
            "skip" => {
                let n: Vec<u32> = v
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if let [a, b, c, d] = n[..]
                    && n.iter().all(|s| (1..=3600).contains(s))
                {
                    p.skip = [a, b, c, d];
                }
            }
            "click" => choice(&mut p.click, CLICK),
            "double_click" => choice(&mut p.double_click, DOUBLE_CLICK),
            "wheel" => choice(&mut p.wheel, WHEEL),
            "reverse_wheel" => flag(&mut p.reverse_wheel),
            "resume" => choice(&mut p.resume, RESUME),
            "autoplay" => flag(&mut p.autoplay),
            "speed" => number(&mut p.speed, 0.25, 2.0),
            "audio_language" => p.audio_language = v.into(),
            "repeat" => choice(&mut p.repeat, REPEAT),
            "shuffle" => flag(&mut p.shuffle),
            "at_end" => choice(&mut p.at_end, AT_END),
            "start_fullscreen" => flag(&mut p.start_fullscreen),
            "keep_awake" => flag(&mut p.keep_awake),
            "aspect" if !v.is_empty() => p.aspect = v.into(),
            "device" => p.device = v.into(),
            "startup_volume" => choice(&mut p.startup_volume, STARTUP_VOLUME),
            "volume" => number(&mut p.volume, 0.0, 2.0),
            "volume_step" => {
                if let Ok(n) = v.parse::<u32>()
                    && (1..=50).contains(&n)
                {
                    p.volume_step = n;
                }
            }
            "eq_preset" => p.eq_preset = v.into(),
            "two_pass" => flag(&mut p.two_pass),
            "stereo" => {
                if let Ok(n) = v.parse::<usize>() {
                    p.stereo = n.min(4);
                }
            }
            "subtitles" => flag(&mut p.subtitles),
            "sub_language" => p.sub_language = v.into(),
            "auto_subs" => flag(&mut p.auto_subs),
            "sub_size" => number(&mut p.sub_size, 0.5, 2.0),
            "sub_bold" => flag(&mut p.sub_bold),
            "sub_color" => colour(&mut p.sub_color),
            "sub_opacity" => number(&mut p.sub_opacity, 0.1, 1.0),
            "outline" => choice(&mut p.outline, OUTLINE),
            "outline_color" => colour(&mut p.outline_color),
            "sub_shadow" => flag(&mut p.sub_shadow),
            "show_hidden" => flag(&mut p.show_hidden),
            "hw_decoding" => flag(&mut p.hw_decoding),
            _ => {}
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_every_setting() {
        let p = Prefs {
            theme: 2,
            accent: 0x144297,
            on_top: true,
            window: Some(([-1200, 40, 1600, 900], true)),
            auto_hide: 1,
            hide_after: 6.0,
            save_recent: false,
            skip: [1, 5, 30, 600],
            click: 1,
            double_click: 1,
            wheel: 1,
            reverse_wheel: true,
            resume: 1,
            autoplay: false,
            speed: 1.25,
            audio_language: "jpn".into(),
            repeat: 2,
            shuffle: true,
            at_end: 1,
            start_fullscreen: true,
            keep_awake: false,
            aspect: "16:9".into(),
            device: "Speakers (Realtek)".into(),
            startup_volume: 3,
            volume: 0.4,
            volume_step: 5,
            eq_preset: "Rock".into(),
            two_pass: true,
            stereo: 3,
            subtitles: false,
            sub_language: "eng".into(),
            auto_subs: false,
            sub_size: 1.5,
            sub_bold: true,
            sub_color: 0xFFE14D,
            sub_opacity: 0.5,
            outline: 3,
            outline_color: 0x808080,
            sub_shadow: true,
            show_hidden: true,
            hw_decoding: false,
            ..Prefs::default()
        };
        let dir = std::env::temp_dir().join(format!("myvlc-prefs-{}", std::process::id()));
        p.save(&dir).unwrap();
        assert_eq!(Prefs::load(&dir), p);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bad_lines_keep_their_defaults() {
        let p = parse(
            "auto_hide = sometimes\nspeed = 9\nskip = 1 2\nsub_color = blue\nvolume = NaN\n\
             garbage\non_top = yes\nhide_after = 4",
        );
        let d = Prefs::default();
        assert_eq!(p.auto_hide, d.auto_hide);
        assert_eq!(p.speed, 2.0, "clamped to the slider's end");
        assert_eq!(p.skip, d.skip);
        assert_eq!(p.sub_color, d.sub_color);
        assert_eq!(p.volume, d.volume);
        assert_eq!(p.on_top, d.on_top);
        assert_eq!(p.hide_after, 4.0);
    }

    #[test]
    fn startup_volume_remembers_or_is_fixed() {
        let mut p = Prefs {
            volume: 0.3,
            ..Prefs::default()
        };
        assert_eq!(p.startup_volume(), 0.3);
        p.startup_volume = 2;
        assert_eq!(p.startup_volume(), 0.5);
    }
}

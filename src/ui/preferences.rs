//! The Settings window, from the design's "Settings - ..." frames: categories down the left, the
//! chosen category's options on the right, and a search box over all of them. It edits its own
//! copy of `Prefs`; each change goes out whole as [`PrefsAction::Changed`], and main.rs applies
//! and saves it.
//!
//! Only settings wushang acts on are listed. Extensions are wushang's optional add-ons (libass), not
//! VLC's Lua extensions, which wushang doesn't run. General has the design's theme and accent rows
//! but not its interface language, as there is only English. The design's (i) marks are left
//! out, as there is nothing behind them. Sizes are DIPs from the 1046x864 frames. On a short window the
//! frame gets shorter first (the options scroll, the sidebar tightens), then scales down.

use std::time::Instant;

use super::dialog::TextField;
use super::*;
use crate::dsp::EQ_PRESETS;
use crate::prefs::{self, Choices, Prefs};
use crate::session::LANGUAGES;
use panels::{ASPECT, STEREO};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    General,
    Interface,
    Privacy,
    Controls,
    Playback,
    Video,
    Audio,
    Subtitles,
    Library,
    Network,
    Performance,
    Recording,
    Extensions,
}

/// The sidebar's groups, as in the design.
const GROUPS: [(&str, &[Category]); 3] = [
    (
        "App",
        &[
            Category::General,
            Category::Interface,
            Category::Privacy,
            Category::Controls,
        ],
    ),
    (
        "Media",
        &[
            Category::Playback,
            Category::Video,
            Category::Audio,
            Category::Subtitles,
            Category::Library,
        ],
    ),
    (
        "System",
        &[
            Category::Network,
            Category::Performance,
            Category::Recording,
            Category::Extensions,
        ],
    ),
];

impl Category {
    fn name(self) -> &'static str {
        match self {
            Category::General => "General",
            Category::Interface => "Interface",
            Category::Privacy => "Privacy",
            Category::Controls => "Controls",
            Category::Playback => "Playback",
            Category::Video => "Video",
            Category::Audio => "Audio",
            Category::Subtitles => "Subtitles",
            Category::Library => "Library",
            Category::Network => "Network",
            Category::Performance => "Performance",
            Category::Recording => "Recording",
            Category::Extensions => "Extensions",
        }
    }

    /// The header over the category's options.
    fn title(self) -> &'static str {
        match self {
            Category::Interface => "Interface Settings",
            Category::Privacy => "Privacy Settings",
            Category::Controls => "Mouse & Keyboard Controls",
            Category::Playback => "Playback Preferences",
            Category::Video => "Video Defaults",
            Category::Audio => "Audio Defaults",
            Category::Subtitles => "Subtitle Defaults",
            Category::Library => "Playlists, Folders & Files",
            Category::Network => "Network Settings",
            Category::Performance => "Performance Settings",
            Category::General => "Global Settings",
            Category::Recording => "Recording & Streaming",
            Category::Extensions => "Extensions",
        }
    }

    fn icon(self) -> usize {
        GROUPS
            .iter()
            .flat_map(|(_, c)| c.iter())
            .position(|c| *c == self)
            .unwrap_or(0)
    }
}

/// What the window asks main.rs to do.
#[derive(Clone, Debug, PartialEq)]
pub enum PrefsAction {
    /// Apply and save these settings.
    Changed(Box<Prefs>),
    ClearHistory,
    ShowShortcuts,
}

/// Every option the window shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ctl {
    Theme,
    Accent,
    OnTop,
    RememberWindow,
    AutoHide,
    HideAfter,
    SaveRecent,
    ClearHistory,
    Shortcuts,
    /// Very short, short, medium and long.
    Skip(u8),
    Click,
    DoubleClick,
    Wheel,
    ReverseWheel,
    Resume,
    Autoplay,
    Speed,
    AudioLanguage,
    Repeat,
    Shuffle,
    AtEnd,
    StartFullscreen,
    KeepAwake,
    Aspect,
    Device,
    StartupVolume,
    VolumeStep,
    EqPreset,
    TwoPass,
    Stereo,
    Subtitles,
    SubLanguage,
    AutoSubs,
    SubSize,
    Bold,
    SubColor,
    SubOpacity,
    Outline,
    OutlineColor,
    Shadow,
    ShowHidden,
    HwDecoding,
    NetReconnect,
    NetTimeout,
    SnapDir,
    SnapFormat,
    RecordDir,
    RecordFormat,
    StreamPort,
    SubStyles,
    EmbeddedFonts,
}

const SPEED_TICKS: [&str; 8] = ["0.25", "0.5", "0.75", "1", "1.25", "1.5", "1.75", "2"];
const HIDE_TICKS: [&str; 3] = ["2 s", "6 s", "10 s"];
const SIZE_TICKS: [&str; 4] = ["50%", "100%", "150%", "200%"];
const OPACITY_TICKS: [&str; 2] = ["10%", "100%"];
const HW: [&str; 2] = ["Automatic", "Off"];

fn labels(list: Choices) -> Vec<String> {
    list.iter().map(|c| c.1.to_owned()).collect()
}

fn seconds_text(s: u32) -> String {
    if s >= 60 && s.is_multiple_of(60) {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

fn language_names() -> Vec<String> {
    std::iter::once("File default".to_owned())
        .chain(LANGUAGES.iter().map(|(_, name)| (*name).to_owned()))
        .collect()
}

/// The language list's row for an ISO 639 code: 0 is "File default".
fn language_row(code: &str) -> usize {
    LANGUAGES
        .iter()
        .position(|(codes, _)| codes.contains(&code))
        .map_or(0, |i| i + 1)
}

fn language_code(row: usize) -> String {
    row.checked_sub(1)
        .and_then(|i| LANGUAGES.get(i))
        .map_or_else(String::new, |(codes, _)| codes[0].to_owned())
}

impl Ctl {
    fn flag(self, p: &Prefs) -> Option<bool> {
        Some(match self {
            Ctl::OnTop => p.on_top,
            Ctl::RememberWindow => p.remember_window,
            Ctl::SaveRecent => p.save_recent,
            Ctl::ReverseWheel => p.reverse_wheel,
            Ctl::Autoplay => p.autoplay,
            Ctl::Shuffle => p.shuffle,
            Ctl::StartFullscreen => p.start_fullscreen,
            Ctl::KeepAwake => p.keep_awake,
            Ctl::TwoPass => p.two_pass,
            Ctl::Subtitles => p.subtitles,
            Ctl::AutoSubs => p.auto_subs,
            Ctl::Bold => p.sub_bold,
            Ctl::Shadow => p.sub_shadow,
            Ctl::ShowHidden => p.show_hidden,
            Ctl::NetReconnect => p.net_reconnect,
            Ctl::SubStyles => p.sub_styles,
            Ctl::EmbeddedFonts => p.embedded_fonts,
            _ => return None,
        })
    }

    fn set_flag(self, p: &mut Prefs, on: bool) {
        let f = match self {
            Ctl::OnTop => &mut p.on_top,
            Ctl::RememberWindow => &mut p.remember_window,
            Ctl::SaveRecent => &mut p.save_recent,
            Ctl::ReverseWheel => &mut p.reverse_wheel,
            Ctl::Autoplay => &mut p.autoplay,
            Ctl::Shuffle => &mut p.shuffle,
            Ctl::StartFullscreen => &mut p.start_fullscreen,
            Ctl::KeepAwake => &mut p.keep_awake,
            Ctl::TwoPass => &mut p.two_pass,
            Ctl::Subtitles => &mut p.subtitles,
            Ctl::AutoSubs => &mut p.auto_subs,
            Ctl::Bold => &mut p.sub_bold,
            Ctl::Shadow => &mut p.sub_shadow,
            Ctl::ShowHidden => &mut p.show_hidden,
            Ctl::NetReconnect => &mut p.net_reconnect,
            Ctl::SubStyles => &mut p.sub_styles,
            Ctl::EmbeddedFonts => &mut p.embedded_fonts,
            _ => return,
        };
        *f = on;
    }

    /// Range, default and step of a slider.
    fn range(self) -> Option<(f32, f32, f32, f32)> {
        Some(match self {
            Ctl::HideAfter => (2.0, 10.0, 3.0, 1.0),
            Ctl::Speed => (0.25, 2.0, 1.0, 0.05),
            Ctl::SubSize => (0.5, 2.0, 1.0, 0.05),
            Ctl::SubOpacity => (0.1, 1.0, 1.0, 0.05),
            _ => return None,
        })
    }

    pub fn is_slider(self) -> bool {
        self.range().is_some()
    }

    fn value(self, p: &Prefs) -> f32 {
        match self {
            Ctl::HideAfter => p.hide_after,
            Ctl::Speed => p.speed,
            Ctl::SubSize => p.sub_size,
            Ctl::SubOpacity => p.sub_opacity,
            _ => 0.0,
        }
    }

    fn set_value(self, p: &mut Prefs, v: f32) {
        let Some((min, max, _, step)) = self.range() else {
            return;
        };
        // Dividing last keeps 1.15 from becoming 1.1500001.
        let v = ((v.clamp(min, max) / step).round() / (1.0 / step)).clamp(min, max);
        match self {
            Ctl::HideAfter => p.hide_after = v,
            Ctl::Speed => p.speed = v,
            Ctl::SubSize => p.sub_size = v,
            Ctl::SubOpacity => p.sub_opacity = v,
            _ => {}
        }
    }

    /// The slider's value as the label beside it shows it.
    fn value_text(self, p: &Prefs) -> String {
        match self {
            Ctl::HideAfter => format!("{} s", p.hide_after.round()),
            Ctl::Speed => {
                let s = format!("{:.2}", p.speed);
                format!("{}x", s.trim_end_matches('0').trim_end_matches('.'))
            }
            Ctl::SubSize => format!("{:.0}%", p.sub_size * 100.0),
            Ctl::SubOpacity => format!("{:.0}%", p.sub_opacity * 100.0),
            _ => String::new(),
        }
    }

    /// The choices of a control that opens a list.
    fn options(self, devices: &[String]) -> Option<Vec<String>> {
        Some(match self {
            Ctl::AutoHide => labels(prefs::AUTO_HIDE),
            Ctl::Skip(_) => prefs::SKIPS.iter().map(|s| seconds_text(*s)).collect(),
            Ctl::Click => labels(prefs::CLICK),
            Ctl::DoubleClick => labels(prefs::DOUBLE_CLICK),
            Ctl::Wheel => labels(prefs::WHEEL),
            Ctl::Resume => labels(prefs::RESUME),
            Ctl::AudioLanguage | Ctl::SubLanguage => language_names(),
            Ctl::Repeat => labels(prefs::REPEAT),
            Ctl::AtEnd => labels(prefs::AT_END),
            Ctl::Aspect => ASPECT.iter().map(|s| (*s).to_owned()).collect(),
            Ctl::Device => std::iter::once("System default".to_owned())
                .chain(devices.iter().skip(1).cloned())
                .collect(),
            Ctl::StartupVolume => labels(prefs::STARTUP_VOLUME),
            Ctl::VolumeStep => prefs::VOLUME_STEPS
                .iter()
                .map(|s| format!("{s}%"))
                .collect(),
            Ctl::EqPreset => std::iter::once("Off")
                .chain(EQ_PRESETS.iter().map(|p| p.name))
                .chain([prefs::EQ_CUSTOM])
                .map(str::to_owned)
                .collect(),
            Ctl::Stereo => STEREO.iter().map(|s| (*s).to_owned()).collect(),
            Ctl::SubColor | Ctl::OutlineColor => {
                prefs::COLORS.iter().map(|c| c.1.to_owned()).collect()
            }
            Ctl::Outline => labels(prefs::OUTLINE),
            Ctl::HwDecoding => HW.iter().map(|s| (*s).to_owned()).collect(),
            Ctl::NetTimeout => labels(prefs::NET_TIMEOUT),
            Ctl::SnapDir => labels(prefs::SNAP_DIR),
            Ctl::SnapFormat => labels(prefs::SNAP_FORMAT),
            Ctl::RecordDir => labels(prefs::RECORD_DIR),
            Ctl::RecordFormat => labels(prefs::RECORD_FORMAT),
            Ctl::StreamPort => labels(prefs::STREAM_PORT),
            _ => return None,
        })
    }

    pub fn has_options(self) -> bool {
        self.options(&[]).is_some()
    }

    fn current(self, p: &Prefs, devices: &[String]) -> Option<usize> {
        let colour = |c: u32| prefs::COLORS.iter().position(|(rgb, _)| *rgb == c);
        match self {
            Ctl::Theme => Some(p.theme),
            Ctl::Accent => theme::ACCENTS.iter().position(|a| *a == p.accent),
            Ctl::AutoHide => Some(p.auto_hide),
            Ctl::Skip(i) => {
                let s = p.skip[usize::from(i)];
                prefs::SKIPS.iter().position(|v| *v == s)
            }
            Ctl::Click => Some(p.click),
            Ctl::DoubleClick => Some(p.double_click),
            Ctl::Wheel => Some(p.wheel),
            Ctl::Resume => Some(p.resume),
            Ctl::AudioLanguage => Some(language_row(&p.audio_language)),
            Ctl::SubLanguage => Some(language_row(&p.sub_language)),
            Ctl::Repeat => Some(p.repeat),
            Ctl::AtEnd => Some(p.at_end),
            Ctl::Aspect => ASPECT.iter().position(|a| *a == p.aspect),
            // The device list starts with the system default, as the tracks panel's does.
            Ctl::Device if p.device.is_empty() => Some(0),
            Ctl::Device => devices
                .iter()
                .skip(1)
                .position(|d| *d == p.device)
                .map(|i| i + 1),
            Ctl::StartupVolume => Some(p.startup_volume),
            Ctl::VolumeStep => prefs::VOLUME_STEPS.iter().position(|v| *v == p.volume_step),
            Ctl::EqPreset if p.eq_preset.is_empty() => Some(0),
            Ctl::EqPreset if p.eq_preset == prefs::EQ_CUSTOM => Some(EQ_PRESETS.len() + 1),
            Ctl::EqPreset => EQ_PRESETS
                .iter()
                .position(|e| e.name == p.eq_preset)
                .map(|i| i + 1),
            Ctl::Stereo => Some(p.stereo),
            Ctl::SubColor => colour(p.sub_color),
            Ctl::OutlineColor => colour(p.outline_color),
            Ctl::Outline => Some(p.outline),
            Ctl::HwDecoding => Some(usize::from(!p.hw_decoding)),
            Ctl::NetTimeout => {
                (prefs::NET_TIMEOUT.iter()).position(|c| c.0.parse::<u32>() == Ok(p.net_timeout))
            }
            Ctl::SnapDir => Some(p.snap_dir),
            Ctl::SnapFormat => Some(p.snap_format),
            Ctl::RecordDir => Some(p.record_dir),
            Ctl::RecordFormat => Some(p.record_format),
            Ctl::StreamPort => {
                (prefs::STREAM_PORT.iter()).position(|c| c.0.parse::<u16>() == Ok(p.stream_port))
            }
            _ => None,
        }
    }

    fn pick(self, p: &mut Prefs, i: usize, devices: &[String]) {
        match self {
            Ctl::Theme => p.theme = i.min(prefs::THEME.len() - 1),
            Ctl::Accent => {
                if let Some(a) = theme::ACCENTS.get(i) {
                    p.accent = *a;
                }
            }
            Ctl::AutoHide => p.auto_hide = i,
            Ctl::Skip(n) => {
                if let Some(s) = prefs::SKIPS.get(i) {
                    p.skip[usize::from(n)] = *s;
                }
            }
            Ctl::Click => p.click = i,
            Ctl::DoubleClick => p.double_click = i,
            Ctl::Wheel => p.wheel = i,
            Ctl::Resume => p.resume = i,
            Ctl::AudioLanguage => p.audio_language = language_code(i),
            Ctl::SubLanguage => p.sub_language = language_code(i),
            Ctl::Repeat => p.repeat = i,
            Ctl::AtEnd => p.at_end = i,
            Ctl::Aspect => {
                if let Some(a) = ASPECT.get(i) {
                    p.aspect = (*a).to_owned();
                }
            }
            Ctl::Device => {
                p.device = (i > 0)
                    .then(|| devices.get(i).cloned())
                    .flatten()
                    .unwrap_or_default();
            }
            Ctl::StartupVolume => p.startup_volume = i,
            Ctl::VolumeStep => {
                if let Some(s) = prefs::VOLUME_STEPS.get(i) {
                    p.volume_step = *s;
                }
            }
            Ctl::EqPreset => {
                p.eq_preset = match i.checked_sub(1) {
                    Some(i) if i == EQ_PRESETS.len() => prefs::EQ_CUSTOM.to_owned(),
                    i => i
                        .and_then(|i| EQ_PRESETS.get(i))
                        .map_or_else(String::new, |e| e.name.to_owned()),
                };
            }
            Ctl::Stereo => p.stereo = i.min(STEREO.len() - 1),
            Ctl::SubColor => {
                if let Some((rgb, _)) = prefs::COLORS.get(i) {
                    p.sub_color = *rgb;
                }
            }
            Ctl::OutlineColor => {
                if let Some((rgb, _)) = prefs::COLORS.get(i) {
                    p.outline_color = *rgb;
                }
            }
            Ctl::Outline => p.outline = i,
            Ctl::HwDecoding => p.hw_decoding = i == 0,
            Ctl::NetTimeout => {
                if let Some(Ok(s)) = prefs::NET_TIMEOUT.get(i).map(|c| c.0.parse()) {
                    p.net_timeout = s;
                }
            }
            Ctl::SnapDir => p.snap_dir = i,
            Ctl::SnapFormat => p.snap_format = i,
            Ctl::RecordDir => p.record_dir = i,
            Ctl::RecordFormat => p.record_format = i,
            Ctl::StreamPort => {
                if let Some(Ok(port)) = prefs::STREAM_PORT.get(i).map(|c| c.0.parse()) {
                    p.stream_port = port;
                }
            }
            _ => {}
        }
    }

    /// What a closed list shows: the current choice, or what the setting holds when no row of
    /// the list matches it (a hand-edited skip of 45 s, a device that went away).
    fn text(self, p: &Prefs, devices: &[String]) -> String {
        match self {
            Ctl::Skip(i) => seconds_text(p.skip[usize::from(i)]),
            Ctl::Device if !p.device.is_empty() => p.device.clone(),
            Ctl::VolumeStep => format!("{}%", p.volume_step),
            Ctl::SubColor => format!("#{:06X}", p.sub_color),
            Ctl::OutlineColor => format!("#{:06X}", p.outline_color),
            _ => {
                let options = self.options(devices).unwrap_or_default();
                self.current(p, devices)
                    .and_then(|i| options.get(i).cloned())
                    .unwrap_or_default()
            }
        }
    }

    fn swatch(self, p: &Prefs) -> Option<u32> {
        match self {
            Ctl::SubColor => Some(p.sub_color),
            Ctl::OutlineColor => Some(p.outline_color),
            _ => None,
        }
    }
}

const W: f32 = 1046.0;
const H: f32 = 864.0;
/// Shorter than this, the frame scales down instead of getting shorter.
const MIN_H: f32 = 560.0;
const SIDEBAR_W: f32 = 216.0;
const HEADER: f32 = 50.0;
/// The sidebar's rounded box.
const BOX_TOP: f32 = 57.0;
const BOX_INSET: f32 = 8.0;
/// Room the sidebar's groups need at full size, from the box's top.
const SIDEBAR_NEEDS: f32 = 749.0;
const LABEL_X: f32 = 240.0;
const CTRL_L: f32 = 705.0;
const CTRL_R: f32 = W - 24.0;
const RADIUS: f32 = 8.0;
const LIST_ROW: f32 = 36.0;
const SEGMENT_GAP: f32 = 3.0;
/// The accent swatches, their selection box and the strip's inset, from "Settings - global".
const SWATCH: f32 = 20.0;
const SWATCH_BOX: f32 = 30.0;
const SWATCH_PAD: f32 = 6.0;
const LIST_PAD: f32 = 6.0;

// Sampled from the "Settings - playback" frame.
const BG_SIDEBAR: u32 = 0x0D0D0D;
const BG_CONTENT: u32 = 0x0F0F0F;
const BG_HEADER: u32 = 0x090909;
const BG_BOX: u32 = 0x181818;
const BG_SEARCH: u32 = 0x161616;
const BG_SELECTED: u32 = 0x222222;
const BG_FIELD: u32 = 0x191919;
const BG_BUTTON: u32 = 0x1F1F1F;
const BG_KNOB: u32 = 0x404040;
const LINE_FIELD: u32 = 0x171717;
const FG_TITLE: u32 = 0xAAAAAA;
const FG_HEADING: u32 = 0xD3D3D3;
const FG_GROUP: u32 = 0x707070;
const FG_SELECTED: u32 = 0xF5F5F5;
const FG_PLACEHOLDER: u32 = 0x424242;
const FG_TICK: u32 = 0x6D6D6D;
/// Disabled categories and rows show the design's look at this opacity.
const DIM: f32 = 0.35;

/// The open Settings window.
pub struct State {
    pub prefs: Prefs,
    /// Output devices, the system default first, as the tracks panel lists them.
    devices: Vec<String>,
    pub category: Category,
    scroll: f32,
    list: Option<Ctl>,
    list_scroll: f32,
    search: TextField,
    /// The search box was clicked or typed in; only then does its caret show.
    focused: bool,
    blink: Instant,
    blink_on: bool,
    /// A key went down since the window opened, so the WM_CHAR of the key that opened it (the
    /// P of Ctrl+P is a control character, but a menu's Enter isn't) stays out of the search.
    typing: bool,
}

impl State {
    pub fn new(prefs: Prefs, devices: Vec<String>, category: Category) -> Self {
        State {
            prefs,
            devices,
            category,
            scroll: 0.0,
            list: None,
            list_scroll: 0.0,
            search: TextField::default(),
            focused: false,
            blink: Instant::now(),
            blink_on: true,
            typing: false,
        }
    }

    fn query(&self) -> String {
        self.search.text.trim().to_lowercase()
    }

    fn caret_visible(&self) -> bool {
        self.focused && (self.blink.elapsed().as_millis() / 530).is_multiple_of(2)
    }
}

/// What a click lands on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// The window outside its controls, or the dimmed window around it.
    Body,
    Close,
    Search,
    Category(Category),
    Ctl(Ctl),
    /// A row of the open choice list.
    Choice(usize),
    /// One segment or swatch of a row of them.
    Pick(Ctl, usize),
}

#[derive(Clone, Debug)]
enum Kind {
    Heading(String),
    Label(&'static str),
    /// A slider's value, right-aligned before the slider.
    Value,
    Toggle,
    Choice,
    /// A slider with a tick under each of its inner labels; the number of labels.
    Slider(usize),
    Ticks(&'static [&'static str]),
    Button(&'static str),
    /// Side-by-side choices, the selected one lit, as the design's Dark / Light / System.
    Segments(Choices),
    /// The accent colours in a strip.
    Swatches,
    /// "No settings match", centred.
    Empty,
}

#[derive(Clone, Debug)]
struct Row {
    kind: Kind,
    r: Rect,
    ctl: Option<Ctl>,
    dim: bool,
}

impl Row {
    fn clickable(&self) -> bool {
        !self.dim
            && self.ctl.is_some()
            && matches!(
                self.kind,
                Kind::Toggle
                    | Kind::Choice
                    | Kind::Slider(_)
                    | Kind::Button(_)
                    | Kind::Segments(_)
                    | Kind::Swatches
            )
    }

    /// The segments or swatch cells of a row of them, by index.
    fn picks(&self) -> Vec<Rect> {
        let b = self.r;
        match self.kind {
            Kind::Segments(list) => {
                let n = list.len() as f32;
                let w = (b.w() - SEGMENT_GAP * (n - 1.0)) / n;
                (0..list.len())
                    .map(|i| {
                        let l = b.l + (w + SEGMENT_GAP) * i as f32;
                        Rect::new(l, b.t, l + w, b.b)
                    })
                    .collect()
            }
            Kind::Swatches => {
                let n = theme::ACCENTS.len();
                let pitch = (b.w() - 2.0 * SWATCH_PAD - SWATCH) / (n - 1) as f32;
                let cy = (b.t + b.b) / 2.0;
                (0..n)
                    .map(|i| {
                        let cx = b.l + SWATCH_PAD + SWATCH / 2.0 + pitch * i as f32;
                        Rect::around(cx, cy, SWATCH_BOX)
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn hit_rect(&self) -> Rect {
        match self.kind {
            // A thin track is hard to grab; the thumb's height counts.
            Kind::Slider(_) => Rect::new(self.r.l, self.r.t - 6.0, self.r.r, self.r.b + 6.0),
            _ => self.r,
        }
    }
}

#[derive(Clone, Debug)]
struct List {
    ctl: Ctl,
    frame: Rect,
    /// Rows already moved by the list's scroll.
    rows: Vec<(String, Rect)>,
    current: Option<usize>,
    max_scroll: f32,
}

/// Where the open window sits and what it draws, in window units (0..W, 0..h).
#[derive(Clone, Default)]
pub struct Geometry {
    /// Window DIPs = origin + window units * scale.
    origin: (f32, f32),
    scale: f32,
    size: (f32, f32),
    window: (f32, f32),
    title: &'static str,
    search: Rect,
    close: Rect,
    /// The sidebar's group labels (text, centre y) and categories (highlight rect).
    groups: Vec<(&'static str, f32)>,
    items: Vec<(Category, Rect)>,
    sidebar_box: Rect,
    /// The scrolling area right of the sidebar, under the header.
    content: Rect,
    rows: Vec<Row>,
    pub max_scroll: f32,
    list: Option<List>,
}

impl Geometry {
    fn local(&self, x: f32, y: f32) -> (f32, f32) {
        (
            (x - self.origin.0) / self.scale,
            (y - self.origin.1) / self.scale,
        )
    }

    pub fn hit(&self, x: f32, y: f32) -> Target {
        let (x, y) = self.local(x, y);
        if let Some(list) = &self.list
            && list.frame.contains(x, y)
        {
            let row = list.rows.iter().position(|(_, r)| r.contains(x, y));
            return row.map_or(Target::Body, Target::Choice);
        }
        if self.close.contains(x, y) {
            return Target::Close;
        }
        if self.search.contains(x, y) {
            return Target::Search;
        }
        if let Some((c, _)) = self.items.iter().find(|(_, r)| r.contains(x, y)) {
            return Target::Category(*c);
        }
        if self.content.contains(x, y)
            && let Some(row) = self
                .rows
                .iter()
                .rev()
                .find(|r| r.clickable() && r.hit_rect().contains(x, y))
            && let Some(ctl) = row.ctl
        {
            if matches!(row.kind, Kind::Segments(_) | Kind::Swatches) {
                return match row.picks().iter().position(|r| r.contains(x, y)) {
                    Some(i) => Target::Pick(ctl, i),
                    None => Target::Body,
                };
            }
            return Target::Ctl(ctl);
        }
        Target::Body
    }

    /// The value a slider takes with the pointer at window x.
    fn slider_value(&self, ctl: Ctl, x: f32) -> Option<f32> {
        let (min, max, _, _) = ctl.range()?;
        let (x, _) = self.local(x, 0.0);
        let row = self
            .rows
            .iter()
            .find(|r| r.ctl == Some(ctl) && matches!(r.kind, Kind::Slider(_)))?;
        let f = ((x - row.r.l - 4.0) / (row.r.w() - 8.0)).clamp(0.0, 1.0);
        Some(min + f * (max - min))
    }

    fn over_list(&self, x: f32, y: f32) -> bool {
        let (x, y) = self.local(x, y);
        self.list.as_ref().is_some_and(|l| l.frame.contains(x, y))
    }
}

/// Places a category's sections and rows top to bottom, y from the window's top before
/// scrolling. While searching it keeps only the rows that match, under "Category / Section".
struct Builder<'a> {
    rows: Vec<Row>,
    /// The bottom of the last thing placed.
    last: f32,
    query: &'a str,
    category: Category,
    section: &'static str,
    section_shown: bool,
    prefs: &'a Prefs,
}

impl Builder<'_> {
    /// The centre y of something `above` tall over its centre and `below` under it.
    fn place(&mut self, above: f32, below: f32) -> f32 {
        let cy = self.last + above;
        self.last = cy + below;
        cy
    }

    fn section(&mut self, name: &'static str) {
        self.section = name;
        self.section_shown = false;
    }

    fn matches(&self, label: &str) -> bool {
        let q = self.query;
        q.is_empty()
            || [label, self.section, self.category.name()]
                .iter()
                .any(|s| s.to_lowercase().contains(q))
    }

    fn push(&mut self, kind: Kind, r: Rect, ctl: Option<Ctl>, dim: bool) {
        self.rows.push(Row { kind, r, ctl, dim });
    }

    /// The label and centre y of a row, after its section heading if that isn't shown yet.
    /// None when searching and the row doesn't match.
    fn start(&mut self, label: &'static str, above: f32, below: f32, dim: bool) -> Option<f32> {
        if !self.matches(label) {
            return None;
        }
        if !self.section_shown {
            self.section_shown = true;
            let text = if self.query.is_empty() {
                self.section.to_owned()
            } else {
                format!("{} / {}", self.category.name(), self.section)
            };
            let cy = self.place(37.0, 20.0);
            let r = Rect::new(LABEL_X, cy - 14.0, CTRL_R, cy + 14.0);
            self.push(Kind::Heading(text), r, None, false);
        }
        let cy = self.place(above, below);
        let r = Rect::new(LABEL_X, cy - 12.0, CTRL_L - 90.0, cy + 12.0);
        self.push(Kind::Label(label), r, None, dim);
        Some(cy)
    }

    fn toggle(&mut self, label: &'static str, ctl: Ctl) {
        self.toggle_if(label, ctl, true);
    }

    /// A toggle, dimmed and inert unless `on`.
    fn toggle_if(&mut self, label: &'static str, ctl: Ctl, on: bool) {
        if let Some(cy) = self.start(label, 23.0, 23.0, !on) {
            let r = Rect::new(CTRL_R - 64.0, cy - 14.0, CTRL_R, cy + 14.0);
            self.push(Kind::Toggle, r, Some(ctl), !on);
        }
    }

    fn choice(&mut self, label: &'static str, ctl: Ctl) {
        if let Some(cy) = self.start(label, 23.0, 23.0, false) {
            let r = Rect::new(CTRL_L, cy - 16.5, CTRL_R, cy + 16.5);
            self.push(Kind::Choice, r, Some(ctl), false);
        }
    }

    fn segments(&mut self, label: &'static str, ctl: Ctl, list: Choices) {
        if let Some(cy) = self.start(label, 23.0, 23.0, false) {
            let r = Rect::new(CTRL_L, cy - 15.5, CTRL_R, cy + 15.5);
            self.push(Kind::Segments(list), r, Some(ctl), false);
        }
    }

    fn swatches(&mut self, label: &'static str, ctl: Ctl) {
        if let Some(cy) = self.start(label, 23.0, 23.0, false) {
            let r = Rect::new(CTRL_L, cy - 16.5, CTRL_R, cy + 16.5);
            self.push(Kind::Swatches, r, Some(ctl), false);
        }
    }

    fn button(&mut self, label: &'static str, text: &'static str, ctl: Ctl) {
        if let Some(cy) = self.start(label, 23.0, 23.0, false) {
            let r = Rect::new(CTRL_R - 100.0, cy - 16.5, CTRL_R, cy + 16.5);
            self.push(Kind::Button(text), r, Some(ctl), false);
        }
    }

    /// A slider over its tick labels, with the label and value beside them, as the design's
    /// "Default playback speed".
    fn slider(&mut self, label: &'static str, ctl: Ctl, ticks: &'static [&'static str], dim: bool) {
        if let Some(cy) = self.start(label, 32.0, 30.0, dim) {
            let value = Rect::new(CTRL_L - 90.0, cy - 12.0, CTRL_L - 16.0, cy + 12.0);
            self.push(Kind::Value, value, Some(ctl), dim);
            let track = Rect::new(CTRL_L, cy - 20.0, CTRL_R, cy - 6.0);
            self.push(Kind::Slider(ticks.len()), track, Some(ctl), dim);
            let labels = Rect::new(CTRL_L, cy + 8.0, CTRL_R, cy + 24.0);
            self.push(Kind::Ticks(ticks), labels, None, dim);
        }
    }

    /// Adds a category's options.
    fn category(&mut self, c: Category) {
        self.category = c;
        let p = self.prefs;
        match c {
            Category::General => {
                self.section("Appearance");
                self.segments("Theme", Ctl::Theme, prefs::THEME);
                self.swatches("Accent color", Ctl::Accent);
            }
            Category::Interface => {
                self.section("Window");
                self.toggle("Keep window above other windows", Ctl::OnTop);
                self.toggle("Remember window size and position", Ctl::RememberWindow);
                self.section("Playback Controls");
                self.choice("Auto-hide the controls", Ctl::AutoHide);
                let never = p.auto_hide == 2;
                self.slider("Hide after", Ctl::HideAfter, &HIDE_TICKS, never);
            }
            Category::Privacy => {
                self.section("History");
                self.toggle("Save recently played items", Ctl::SaveRecent);
                self.button("Clear history now", "Clear", Ctl::ClearHistory);
            }
            Category::Controls => {
                self.section("Keyboard Shortcuts");
                self.button("Keyboard shortcuts", "Show", Ctl::Shortcuts);
                self.section("Skip Amounts");
                self.choice("Very short skip (Alt + Left/Right)", Ctl::Skip(0));
                self.choice("Short skip (Left/Right)", Ctl::Skip(1));
                self.choice("Medium skip (Shift + Left/Right)", Ctl::Skip(2));
                self.choice("Long skip (Ctrl + Left/Right)", Ctl::Skip(3));
                self.section("Mouse");
                self.choice("Single click on the video", Ctl::Click);
                self.choice("Double click on the video", Ctl::DoubleClick);
                self.choice("Scroll wheel on the video", Ctl::Wheel);
                self.toggle("Reverse scroll direction", Ctl::ReverseWheel);
            }
            Category::Playback => {
                self.section("Startup");
                self.choice("Continue where you left off", Ctl::Resume);
                self.toggle("Start playing as soon as a file opens", Ctl::Autoplay);
                self.slider("Default playback speed", Ctl::Speed, &SPEED_TICKS, false);
                self.section("Tracks");
                self.choice("Preferred audio language", Ctl::AudioLanguage);
                self.section("Order");
                self.choice("Repeat", Ctl::Repeat);
                self.toggle("Shuffle", Ctl::Shuffle);
                self.choice("When playback finishes", Ctl::AtEnd);
            }
            Category::Video => {
                self.section("Output");
                self.toggle("Start videos in fullscreen", Ctl::StartFullscreen);
                self.toggle("Stop the screensaver during playback", Ctl::KeepAwake);
                self.section("Framing");
                self.choice("Aspect ratio", Ctl::Aspect);
            }
            Category::Audio => {
                self.section("Output");
                self.choice("Output device", Ctl::Device);
                self.section("Volume");
                self.choice("Startup volume", Ctl::StartupVolume);
                self.choice("Volume step (Up/Down)", Ctl::VolumeStep);
                self.section("Effects");
                self.choice("Equalizer preset", Ctl::EqPreset);
                self.toggle("Two-pass equalizer", Ctl::TwoPass);
                self.choice("Stereo mode", Ctl::Stereo);
            }
            Category::Subtitles => {
                self.section("Tracks & Languages");
                self.toggle("Enable subtitles", Ctl::Subtitles);
                self.choice("Preferred language", Ctl::SubLanguage);
                self.section("Loading Subtitles");
                self.toggle("Load subtitle files automatically", Ctl::AutoSubs);
                self.section("Appearance");
                self.slider("Font size", Ctl::SubSize, &SIZE_TICKS, false);
                self.toggle("Bold", Ctl::Bold);
                self.choice("Text color", Ctl::SubColor);
                self.slider("Text opacity", Ctl::SubOpacity, &OPACITY_TICKS, false);
                self.choice("Outline thickness", Ctl::Outline);
                self.choice("Outline color", Ctl::OutlineColor);
                self.toggle("Shadow", Ctl::Shadow);
            }
            Category::Library => {
                self.section("Opening Files");
                self.toggle("Show hidden files", Ctl::ShowHidden);
            }
            Category::Performance => {
                self.section("Decoding");
                self.choice("Hardware-accelerated decoding", Ctl::HwDecoding);
            }
            Category::Network => {
                self.section("Streams");
                self.toggle("Reconnect when a stream drops", Ctl::NetReconnect);
                self.choice("Give up on a stalled connection after", Ctl::NetTimeout);
            }
            Category::Recording => {
                self.section("Snapshots");
                self.choice("Save snapshots to", Ctl::SnapDir);
                self.choice("Snapshot format", Ctl::SnapFormat);
                self.section("Recordings");
                self.choice("Save recordings to", Ctl::RecordDir);
                self.choice("Recording format", Ctl::RecordFormat);
                self.section("Streaming");
                self.choice("Stream on port", Ctl::StreamPort);
            }
            Category::Extensions => {
                // Without libass-9.dll there is nothing to turn on.
                let libass = crate::ass::available();
                self.section("Styled Subtitles (libass)");
                self.toggle_if(
                    "Draw ASS and SSA with their own styling",
                    Ctl::SubStyles,
                    libass,
                );
                let fonts = libass && p.sub_styles;
                self.toggle_if(
                    "Use fonts carried by Matroska files",
                    Ctl::EmbeddedFonts,
                    fonts,
                );
            }
        }
    }
}

impl Layout {
    /// Lays out the open Settings window over the page.
    pub fn add_preferences(&mut self, st: &State) {
        let (ww, wh) = (self.width, self.height);
        let margin = 16.0;
        let scale = ((ww - 2.0 * margin) / W)
            .min((wh - 2.0 * margin) / MIN_H)
            .clamp(0.1, 1.0);
        let h = ((wh - 2.0 * margin) / scale).clamp(MIN_H, H);

        // The sidebar tightens to fit a shorter frame.
        let room = h - BOX_TOP - BOX_INSET - 6.0;
        let f = (room / SIDEBAR_NEEDS).min(1.0);
        let pitch = 48.0 * f;
        let mut groups = Vec::new();
        let mut items = Vec::new();
        let mut y = 23.0;
        for (gi, (name, cats)) in GROUPS.iter().enumerate() {
            if gi > 0 {
                y += 45.0;
            }
            groups.push((*name, BOX_TOP + y * f));
            y += 44.0;
            for (i, c) in cats.iter().enumerate() {
                if i > 0 {
                    y += 48.0;
                }
                let cy = BOX_TOP + y * f;
                let half = (pitch - 4.0).min(40.0) / 2.0;
                items.push((*c, Rect::new(14.0, cy - half, SIDEBAR_W - 14.0, cy + half)));
            }
        }

        let content = Rect::new(SIDEBAR_W, HEADER, W, h);
        let query = st.query();
        let build = |scroll: f32| {
            let mut b = Builder {
                rows: Vec::new(),
                last: HEADER + 1.0 - scroll,
                query: &query,
                category: st.category,
                section: "",
                section_shown: true,
                prefs: &st.prefs,
            };
            if query.is_empty() {
                b.category(st.category);
            } else {
                for c in GROUPS.iter().flat_map(|(_, c)| c.iter()) {
                    b.category(*c);
                }
                if b.rows.is_empty() {
                    let r = Rect::new(SIDEBAR_W, HEADER + 40.0, W, HEADER + 80.0);
                    b.push(Kind::Empty, r, None, false);
                }
            }
            let end = b.last + scroll + 40.0;
            (b.rows, (end - h).max(0.0))
        };
        let (mut rows, max_scroll) = build(st.scroll);
        if st.scroll > max_scroll {
            rows = build(max_scroll).0;
        }
        let list = st
            .list
            .and_then(|ctl| choice_list(ctl, &rows, st, h, st.list_scroll));

        self.preferences = Some(Geometry {
            origin: (
                ((ww - W * scale) / 2.0).round(),
                ((wh - h * scale) / 2.0).round(),
            ),
            scale,
            size: (W, h),
            window: (ww, wh),
            title: if query.is_empty() {
                st.category.title()
            } else {
                "Search Results"
            },
            search: Rect::new(BOX_INSET, 7.0, SIDEBAR_W - BOX_INSET, 41.0),
            close: Rect::around(W - 30.0, 24.0, 32.0),
            groups,
            items,
            sidebar_box: Rect::new(BOX_INSET, BOX_TOP, SIDEBAR_W - BOX_INSET, h - BOX_INSET),
            content,
            rows,
            max_scroll,
            list,
        });
    }
}

/// The open control's choices under it, or over it when there's no room below.
fn choice_list(ctl: Ctl, rows: &[Row], st: &State, h: f32, scroll: f32) -> Option<List> {
    let anchor = rows
        .iter()
        .find(|r| r.ctl == Some(ctl) && matches!(r.kind, Kind::Choice))?
        .r;
    let items = ctl.options(&st.devices)?;
    let full = items.len() as f32 * LIST_ROW + 2.0 * LIST_PAD;
    let (min, max) = (HEADER + 4.0, h - 8.0);
    let below = max - (anchor.b + 4.0);
    let above = anchor.t - 4.0 - min;
    let (t, lh) = if full <= below || below >= above {
        (anchor.b + 4.0, full.min(below))
    } else {
        let lh = full.min(above);
        (anchor.t - 4.0 - lh, lh)
    };
    let frame = Rect::new(anchor.l, t, anchor.r, t + lh);
    let max_scroll = (full - lh).max(0.0);
    let scroll = scroll.clamp(0.0, max_scroll);
    let rows = items
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let y = t + LIST_PAD + LIST_ROW * i as f32 - scroll;
            (
                text,
                Rect::new(frame.l + 4.0, y, frame.r - 4.0, y + LIST_ROW),
            )
        })
        .collect();
    Some(List {
        ctl,
        frame,
        rows,
        current: ctl.current(&st.prefs, &st.devices),
        max_scroll,
    })
}

impl Ui {
    /// Opens the Settings window on `category`; popups, menus and the sidebar panel close.
    pub fn open_preferences(&mut self, prefs: Prefs, devices: Vec<String>, category: Category) {
        self.close_menu();
        self.panels.show(None, None);
        self.drag = None;
        self.pressed = None;
        self.preferences = Some(State::new(prefs, devices, category));
        self.relayout = true;
        self.wake();
    }

    pub fn close_preferences(&mut self) {
        if self.preferences.take().is_some() {
            self.drag = None;
            self.relayout = true;
        }
    }

    fn prefs_changed(&mut self) -> Option<Action> {
        let st = self.preferences.as_ref()?;
        self.relayout = true;
        Some(Action::Preferences(PrefsAction::Changed(Box::new(
            st.prefs.clone(),
        ))))
    }

    /// A click in the window: switching, lists and searching happen here, changes go out.
    pub(super) fn preferences_click(&mut self, target: Target) -> Option<Action> {
        let st = self.preferences.as_mut()?;
        self.relayout = true;
        // An open list takes the next click: a choice, or anywhere else to close it.
        if let Some(ctl) = st.list.take() {
            if let Target::Choice(i) = target {
                ctl.pick(&mut st.prefs, i, &st.devices);
                return self.prefs_changed();
            }
            return None;
        }
        match target {
            Target::Search => {
                st.focused = true;
                st.blink = Instant::now();
                None
            }
            Target::Body | Target::Choice(_) => None,
            Target::Close => {
                self.close_preferences();
                None
            }
            Target::Category(c) => {
                st.category = c;
                st.scroll = 0.0;
                st.search = TextField::default();
                None
            }
            Target::Pick(ctl, i) => {
                ctl.pick(&mut st.prefs, i, &st.devices);
                self.prefs_changed()
            }
            Target::Ctl(Ctl::ClearHistory) => Some(Action::Preferences(PrefsAction::ClearHistory)),
            Target::Ctl(Ctl::Shortcuts) => Some(Action::Preferences(PrefsAction::ShowShortcuts)),
            Target::Ctl(ctl) if ctl.has_options() => {
                st.list = Some(ctl);
                st.list_scroll = 0.0;
                None
            }
            Target::Ctl(ctl) => {
                let on = ctl.flag(&st.prefs)?;
                ctl.set_flag(&mut st.prefs, !on);
                self.prefs_changed()
            }
        }
    }

    /// A press in the window: a slider follows the pointer from here; a quick second press
    /// puts it back to its default.
    pub(super) fn preferences_down(&mut self, target: Target, x: f32) -> Option<Action> {
        let Target::Ctl(ctl) = target else {
            return None;
        };
        let st = self.preferences.as_mut()?;
        if !ctl.is_slider() || st.list.is_some() {
            return None;
        }
        let double = self
            .last_pref_slider
            .is_some_and(|(c, at)| c == ctl && at.elapsed() < DOUBLE_CLICK);
        if double {
            self.last_pref_slider = None;
            self.pressed = None;
            let (_, _, default, _) = ctl.range()?;
            ctl.set_value(&mut st.prefs, default);
            return self.prefs_changed();
        }
        self.last_pref_slider = Some((ctl, Instant::now()));
        self.drag = Some(Hit::Preferences(target));
        self.preferences_drag(x)
    }

    /// Dragging a slider.
    pub(super) fn preferences_drag(&mut self, x: f32) -> Option<Action> {
        let Some(Hit::Preferences(Target::Ctl(ctl))) = self.drag else {
            return None;
        };
        let v = self.layout.preferences.as_ref()?.slider_value(ctl, x)?;
        let st = self.preferences.as_mut()?;
        let before = ctl.value(&st.prefs);
        ctl.set_value(&mut st.prefs, v);
        if ctl.value(&st.prefs) == before {
            return None;
        }
        self.prefs_changed()
    }

    /// A key while the window is open: typing searches, Esc closes the list, then clears the
    /// search, then closes the window.
    pub fn preferences_key(&mut self, key: VIRTUAL_KEY, shift: bool, ctrl: bool) {
        let Some(st) = self.preferences.as_mut() else {
            return;
        };
        st.typing = true;
        self.relayout = true;
        let before = st.search.text.clone();
        let f = &mut st.search;
        match key {
            VK_ESCAPE if st.list.is_some() => st.list = None,
            VK_ESCAPE if !f.text.is_empty() => {
                *f = TextField::default();
                st.scroll = 0.0;
            }
            VK_ESCAPE => self.close_preferences(),
            VK_LEFT => f.left(shift, ctrl),
            VK_RIGHT => f.right(shift, ctrl),
            VK_HOME => f.move_to(0, shift),
            VK_END => f.move_to(usize::MAX, shift),
            VK_BACK => f.backspace(ctrl),
            VK_DELETE => f.delete(ctrl),
            VK_A if ctrl => f.select_all(),
            _ => return,
        }
        if let Some(st) = self.preferences.as_mut() {
            st.blink = Instant::now();
            // New results start at the top.
            if st.search.text != before {
                st.scroll = 0.0;
                st.list = None;
            }
        }
    }

    /// A typed character goes to the search box. False when the window isn't open.
    pub fn preferences_char(&mut self, c: u32) -> bool {
        let Some(st) = self.preferences.as_mut() else {
            return false;
        };
        if let Some(c) = char::from_u32(c).filter(|c| !c.is_control())
            && st.typing
            && st.search.text.chars().count() < 60
        {
            st.search.insert(c.encode_utf8(&mut [0; 4]));
            st.focused = true;
            st.list = None;
            st.scroll = 0.0;
            st.blink = Instant::now();
            self.relayout = true;
        }
        true
    }

    /// The wheel scrolls the open list when over it, else the options.
    pub fn scroll_preferences(&mut self, x: f32, y: f32, notches: f32) {
        let (Some(st), Some(g)) = (self.preferences.as_mut(), self.layout.preferences.as_ref())
        else {
            return;
        };
        let dy = -60.0 * notches;
        match &g.list {
            Some(l) if g.over_list(x, y) => {
                st.list_scroll = (st.list_scroll + dy).clamp(0.0, l.max_scroll);
            }
            Some(_) => {}
            None => st.scroll = (st.scroll + dy).clamp(0.0, g.max_scroll),
        }
        self.relayout = true;
    }

    /// True when the search caret's blink turned since the last call, so the window repaints.
    pub fn preferences_tick(&mut self) -> bool {
        let Some(st) = self.preferences.as_mut() else {
            return false;
        };
        let on = st.caret_visible();
        std::mem::replace(&mut st.blink_on, on) != on
    }
}

/// Brushes, fonts and icons of the window.
pub(super) struct Resources {
    veil: ID2D1SolidColorBrush,
    shadow: ID2D1SolidColorBrush,
    sidebar: ID2D1SolidColorBrush,
    content: ID2D1SolidColorBrush,
    header: ID2D1SolidColorBrush,
    box_bg: ID2D1SolidColorBrush,
    search_bg: ID2D1SolidColorBrush,
    selected: ID2D1SolidColorBrush,
    field: ID2D1SolidColorBrush,
    field_line: ID2D1SolidColorBrush,
    button: ID2D1SolidColorBrush,
    knob: ID2D1SolidColorBrush,
    hover: ID2D1SolidColorBrush,
    selection: ID2D1SolidColorBrush,
    title_fg: ID2D1SolidColorBrush,
    heading_fg: ID2D1SolidColorBrush,
    label_fg: ID2D1SolidColorBrush,
    group_fg: ID2D1SolidColorBrush,
    selected_fg: ID2D1SolidColorBrush,
    placeholder_fg: ID2D1SolidColorBrush,
    tick_fg: ID2D1SolidColorBrush,
    swatch: ID2D1SolidColorBrush,
    /// The content colour at 1 - DIM, laid over dimmed rows.
    content_veil: ID2D1SolidColorBrush,
    title: IDWriteTextFormat,
    heading: IDWriteTextFormat,
    label: IDWriteTextFormat,
    value: IDWriteTextFormat,
    item: IDWriteTextFormat,
    group: IDWriteTextFormat,
    centred: IDWriteTextFormat,
    small_l: IDWriteTextFormat,
    small_c: IDWriteTextFormat,
    small_r: IDWriteTextFormat,
    toggle: IDWriteTextFormat,
    search_icon: Svg,
    close: Svg,
    /// By `Category::icon`: (normal, selected).
    icons: Vec<(Svg, Svg)>,
}

impl Resources {
    pub(super) fn new(
        ctx: &ID2D1DeviceContext5,
        dwrite: &IDWriteFactory6,
        fonts: &IDWriteFontCollection2,
    ) -> Result<Self> {
        let brush =
            |rgb: u32, alpha: f32| unsafe { ctx.CreateSolidColorBrush(&color(rgb, alpha), None) };
        let format = |size: f32, weight, align| text_format(dwrite, fonts, size, weight, align);
        let (regular, medium) = (DWRITE_FONT_WEIGHT_REGULAR, DWRITE_FONT_WEIGHT_MEDIUM);
        let (lead, centre, trail) = (
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_TEXT_ALIGNMENT_CENTER,
            DWRITE_TEXT_ALIGNMENT_TRAILING,
        );
        let tint = |bytes: &[u8], rgb: u32| {
            let text = String::from_utf8_lossy(bytes).replace("#8F8F8F", &format!("#{rgb:06X}"));
            svg(ctx, text.as_bytes())
        };
        let icon = |bytes: &[u8]| -> Result<(Svg, Svg)> {
            Ok((tint(bytes, FG_LABEL)?, tint(bytes, ACCENT)?))
        };
        Ok(Resources {
            veil: brush(0x000000, 0.5)?,
            shadow: brush(0x000000, 0.12)?,
            sidebar: brush(BG_SIDEBAR, 1.0)?,
            content: brush(BG_CONTENT, 1.0)?,
            header: brush(BG_HEADER, 1.0)?,
            box_bg: brush(BG_BOX, 1.0)?,
            search_bg: brush(BG_SEARCH, 1.0)?,
            selected: brush(BG_SELECTED, 1.0)?,
            field: brush(BG_FIELD, 1.0)?,
            field_line: brush(LINE_FIELD, 1.0)?,
            button: brush(BG_BUTTON, 1.0)?,
            knob: brush(BG_KNOB, 1.0)?,
            hover: brush(0xFFFFFF, 0.04)?,
            selection: brush(ACCENT, 0.35)?,
            title_fg: brush(FG_TITLE, 1.0)?,
            heading_fg: brush(FG_HEADING, 1.0)?,
            label_fg: brush(FG_LABEL, 1.0)?,
            group_fg: brush(FG_GROUP, 1.0)?,
            selected_fg: brush(FG_SELECTED, 1.0)?,
            placeholder_fg: brush(FG_PLACEHOLDER, 1.0)?,
            tick_fg: brush(FG_TICK, 1.0)?,
            swatch: brush(0xFFFFFF, 1.0)?,
            content_veil: brush(BG_CONTENT, 1.0 - DIM)?,
            title: format(17.0, regular, lead)?,
            heading: format(16.0, medium, lead)?,
            label: format(14.0, regular, lead)?,
            value: format(14.0, regular, trail)?,
            item: format(15.0, regular, lead)?,
            group: format(14.0, regular, lead)?,
            centred: format(14.0, regular, centre)?,
            small_l: format(11.0, regular, lead)?,
            small_c: format(11.0, regular, centre)?,
            small_r: format(11.0, regular, trail)?,
            toggle: format(11.0, regular, centre)?,
            search_icon: tint(include_bytes!("../../assets/icons/search.svg"), FG_DIM)?,
            close: tint(include_bytes!("../../assets/icons/close.svg"), FG_LABEL)?,
            icons: vec![
                icon(include_bytes!("../../assets/icons/prefs_general.svg"))?,
                icon(include_bytes!("../../assets/icons/prefs_interface.svg"))?,
                icon(include_bytes!("../../assets/icons/prefs_privacy.svg"))?,
                icon(include_bytes!("../../assets/icons/prefs_controls.svg"))?,
                icon(include_bytes!("../../assets/icons/prefs_playback.svg"))?,
                icon(include_bytes!("../../assets/icons/tab_video.svg"))?,
                icon(include_bytes!("../../assets/icons/audio.svg"))?,
                icon(include_bytes!("../../assets/icons/subtitles.svg"))?,
                icon(include_bytes!("../../assets/icons/folder.svg"))?,
                icon(include_bytes!("../../assets/icons/prefs_network.svg"))?,
                icon(include_bytes!("../../assets/icons/prefs_performance.svg"))?,
                icon(include_bytes!("../../assets/icons/prefs_recording.svg"))?,
                icon(include_bytes!("../../assets/icons/prefs_extensions.svg"))?,
            ],
        })
    }
}

impl Painter {
    /// Dims the window and paints the Settings window on top.
    pub(super) fn paint_preferences(
        &mut self,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        g: &Geometry,
        st: &State,
    ) {
        let rt: &ID2D1RenderTarget = ctx;
        let r = &self.prefs;
        let base = Matrix3x2 {
            M11: g.scale,
            M12: 0.0,
            M21: 0.0,
            M22: g.scale,
            M31: g.origin.0,
            M32: g.origin.1,
        };
        let (w, h) = g.size;
        let frame = Rect::new(0.0, 0.0, w, h);
        let hover = |t: Target| s.hover == Some(Hit::Preferences(t));
        unsafe {
            let window = Rect::new(0.0, 0.0, g.window.0, g.window.1);
            rt.FillRectangle(&window.d2d(), &r.veil);
            ctx.SetTransform(&base);
            for spread in [12.0, 8.0, 4.0] {
                rt.FillRoundedRectangle(&rounded(frame.inset(-spread), RADIUS + spread), &r.shadow);
            }
            // Content colour under everything, then the sidebar column and the header band
            // clipped to the frame's rounded corners.
            rt.FillRoundedRectangle(&rounded(frame, RADIUS), &r.content);
            let clip = |area: Rect, brush: &ID2D1SolidColorBrush| {
                rt.PushAxisAlignedClip(&area.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
                rt.FillRoundedRectangle(&rounded(frame, RADIUS), brush);
                rt.PopAxisAlignedClip();
            };
            clip(Rect::new(0.0, 0.0, SIDEBAR_W, h), &r.sidebar);
            clip(Rect::new(SIDEBAR_W, 0.0, w, HEADER), &r.header);

            self.prefs_search(rt, ctx, g, st, hover(Target::Search));
            self.put(
                rt,
                g.title,
                &r.title,
                Rect::new(LABEL_X, 0.0, w - 60.0, 48.0),
                &r.title_fg,
            );
            if hover(Target::Close) {
                rt.FillRoundedRectangle(&rounded(g.close, 4.0), &r.hover);
            }
            let (cx, cy) = centre(g.close);
            self.prefs_glyph(ctx, &base, &r.close, cx, cy, 24.0);

            // The sidebar.
            rt.FillRoundedRectangle(&rounded(g.sidebar_box, RADIUS), &r.box_bg);
            rt.PushAxisAlignedClip(&g.sidebar_box.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            for (name, cy) in &g.groups {
                let tr = Rect::new(24.0, cy - 12.0, SIDEBAR_W - 16.0, cy + 12.0);
                self.put(rt, name, &r.group, tr, &r.group_fg);
            }
            let searching = !st.search.text.trim().is_empty();
            for (c, item) in &g.items {
                let on = *c == st.category && !searching;
                if on {
                    rt.FillRoundedRectangle(&rounded(*item, 6.0), &r.selected);
                } else if hover(Target::Category(*c)) {
                    rt.FillRoundedRectangle(&rounded(*item, 6.0), &r.hover);
                }
                let (_, cy) = centre(*item);
                let (normal, lit) = &r.icons[c.icon()];
                self.prefs_glyph(ctx, &base, if on { lit } else { normal }, 34.0, cy, 24.0);
                let fg = if on { &r.selected_fg } else { &r.label_fg };
                let tr = Rect::new(58.0, cy - 12.0, item.r - 8.0, cy + 12.0);
                self.put(rt, c.name(), &r.item, tr, fg);
            }
            rt.PopAxisAlignedClip();

            // The options.
            rt.PushAxisAlignedClip(&g.content.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            for row in g.rows.iter().filter(|row| row.r.intersects(&g.content)) {
                self.prefs_row(rt, ctx, &base, s, g, st, row);
            }
            rt.PopAxisAlignedClip();
            if let Some(list) = &g.list {
                self.prefs_list(rt, ctx, &base, s, list);
            }
            ctx.SetTransform(&Matrix3x2::identity());
        }
    }

    /// An icon in window units: `base` is the window's transform.
    pub(super) unsafe fn prefs_glyph(
        &self,
        ctx: &ID2D1DeviceContext5,
        base: &Matrix3x2,
        svg: &Svg,
        cx: f32,
        cy: f32,
        size: f32,
    ) {
        let s = size / svg.width;
        let local = Matrix3x2 {
            M11: s,
            M12: 0.0,
            M21: 0.0,
            M22: s,
            M31: cx - size / 2.0,
            M32: cy - size / 2.0,
        };
        let m = Matrix3x2 {
            M11: local.M11 * base.M11,
            M12: 0.0,
            M21: 0.0,
            M22: local.M22 * base.M22,
            M31: local.M31 * base.M11 + base.M31,
            M32: local.M32 * base.M22 + base.M32,
        };
        unsafe {
            ctx.SetTransform(&m);
            ctx.DrawSvgDocument(&svg.doc);
            ctx.SetTransform(base);
        }
    }

    unsafe fn prefs_search(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        g: &Geometry,
        st: &State,
        hovered: bool,
    ) {
        let r = &self.prefs;
        let b = g.search;
        let base = Matrix3x2 {
            M11: g.scale,
            M12: 0.0,
            M21: 0.0,
            M22: g.scale,
            M31: g.origin.0,
            M32: g.origin.1,
        };
        unsafe {
            rt.FillRoundedRectangle(&rounded(b, 6.0), &r.search_bg);
            if hovered {
                rt.FillRoundedRectangle(&rounded(b, 6.0), &r.hover);
            }
            let (_, cy) = centre(b);
            self.prefs_glyph(ctx, &base, &r.search_icon, b.l + 22.0, cy, 24.0);
            let area = Rect::new(b.l + 42.0, b.t, b.r - 10.0, b.b);
            rt.PushAxisAlignedClip(&area.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            let f = &st.search;
            let caret_x = self.measure(&f.text[..f.caret()], &r.label);
            // Keep the caret in view once the text outgrows the box.
            let x0 = area.l - (caret_x - (area.w() - 2.0)).max(0.0);
            if f.text.is_empty() {
                self.put(rt, "Search", &r.label, area, &r.placeholder_fg);
            } else {
                let sel = f.selection();
                if !sel.is_empty() {
                    let a = self.measure(&f.text[..sel.start], &r.label);
                    let z = self.measure(&f.text[..sel.end], &r.label);
                    let band = Rect::new(x0 + a, cy - 10.0, x0 + z, cy + 10.0);
                    rt.FillRectangle(&band.d2d(), &r.selection);
                }
                let tr = Rect::new(x0, area.t, x0 + 10_000.0, area.b);
                self.put(rt, &f.text, &r.label, tr, &r.heading_fg);
            }
            if st.caret_visible() {
                let x = (x0 + caret_x).round();
                rt.FillRectangle(
                    &Rect::new(x, cy - 9.0, x + 1.0, cy + 9.0).d2d(),
                    &r.label_fg,
                );
            }
            rt.PopAxisAlignedClip();
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn prefs_row(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        base: &Matrix3x2,
        s: &Scene,
        g: &Geometry,
        st: &State,
        row: &Row,
    ) {
        let r = &self.prefs;
        let b = row.r;
        let (_, cy) = centre(b);
        let p = &st.prefs;
        let hovered = row.clickable()
            && row
                .ctl
                .is_some_and(|c| s.hover == Some(Hit::Preferences(Target::Ctl(c))));
        unsafe {
            match &row.kind {
                Kind::Heading(text) => self.put(rt, text, &r.heading, b, &r.heading_fg),
                Kind::Label(text) => self.put(rt, text, &r.label, b, &r.label_fg),
                Kind::Value => {
                    let text = row.ctl.map(|c| c.value_text(p)).unwrap_or_default();
                    self.put(rt, &text, &r.value, b, &r.tick_fg);
                }
                Kind::Empty => self.put(rt, "No settings match", &r.centred, b, &r.tick_fg),
                Kind::Toggle => {
                    let on = row.ctl.and_then(|c| c.flag(p)).unwrap_or(false);
                    rt.FillRoundedRectangle(&rounded(b, 6.0), &r.header);
                    let inner = b.inset(2.0);
                    let (knob, label) = if on {
                        (
                            Rect::new(inner.r - 28.0, inner.t, inner.r, inner.b),
                            Rect::new(inner.l, inner.t, inner.r - 30.0, inner.b),
                        )
                    } else {
                        (
                            Rect::new(inner.l, inner.t, inner.l + 28.0, inner.b),
                            Rect::new(inner.l + 30.0, inner.t, inner.r, inner.b),
                        )
                    };
                    let (bg, fg) = if on {
                        (&self.played, &self.accent)
                    } else {
                        (&r.field, &r.tick_fg)
                    };
                    rt.FillRoundedRectangle(&rounded(label, 4.0), bg);
                    let knob_brush = if on { &self.accent } else { &r.knob };
                    rt.FillRoundedRectangle(&rounded(knob, 4.0), knob_brush);
                    self.put(rt, if on { "ON" } else { "OFF" }, &r.toggle, label, fg);
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(b, 6.0), &r.hover);
                    }
                }
                Kind::Choice => {
                    let ctl = row.ctl;
                    let open = ctl.is_some() && ctl == g.list.as_ref().map(|l| l.ctl);
                    rt.FillRoundedRectangle(&rounded(b, 6.0), &r.field);
                    rt.DrawRoundedRectangle(&rounded(b.inset(0.5), 6.0), &r.field_line, 1.0, None);
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(b, 6.0), &r.hover);
                    }
                    let swatch = ctl.and_then(|c| c.swatch(p));
                    let text_r = b.r - if swatch.is_some() { 72.0 } else { 44.0 };
                    let tr = Rect::new(b.l + 15.0, b.t, text_r, b.b);
                    let text = ctl.map(|c| c.text(p, &st.devices)).unwrap_or_default();
                    self.put(rt, &text, &r.label, tr, &r.label_fg);
                    if let Some(rgb) = swatch {
                        let sw = Rect::around(b.r - 54.0, cy, 16.0);
                        r.swatch.SetColor(&color(theme::fixed(rgb), 1.0));
                        rt.FillRoundedRectangle(&rounded(sw, 2.0), &r.swatch);
                        rt.DrawRoundedRectangle(
                            &rounded(sw.inset(0.5), 2.0),
                            &r.tick_fg,
                            1.0,
                            None,
                        );
                    }
                    let chevron = if open {
                        &self.panels.chevron_up
                    } else {
                        &self.panels.chevron_down
                    };
                    self.prefs_glyph(ctx, base, chevron, b.r - 27.0, cy, 24.0);
                }
                Kind::Segments(list) => {
                    let ctl = row.ctl;
                    let current = ctl.and_then(|c| c.current(p, &st.devices));
                    for (i, seg) in row.picks().into_iter().enumerate() {
                        let on = current == Some(i);
                        let (bg, fg) = if on {
                            (&self.played, &self.accent)
                        } else {
                            (&r.field, &r.label_fg)
                        };
                        rt.FillRoundedRectangle(&rounded(seg, 6.0), bg);
                        let picked = ctl.map(|c| Hit::Preferences(Target::Pick(c, i)));
                        if !on && s.hover == picked {
                            rt.FillRoundedRectangle(&rounded(seg, 6.0), &r.hover);
                        }
                        let text = list.get(i).map_or("", |c| c.1);
                        self.put(rt, text, &r.centred, seg, fg);
                    }
                }
                Kind::Swatches => {
                    let ctl = row.ctl;
                    let current = ctl.and_then(|c| c.current(p, &st.devices));
                    rt.FillRoundedRectangle(&rounded(b, 6.0), &r.header);
                    for (i, cell) in row.picks().into_iter().enumerate() {
                        let picked = ctl.map(|c| Hit::Preferences(Target::Pick(c, i)));
                        if current == Some(i) {
                            rt.FillRoundedRectangle(&rounded(cell, 4.0), &self.played);
                        } else if s.hover == picked {
                            rt.FillRoundedRectangle(&rounded(cell, 4.0), &r.hover);
                        }
                        let (cx, cy) = centre(cell);
                        // As in the design: the picked colour bright, the others dimmed.
                        let alpha = if current == Some(i) { 1.0 } else { 0.62 };
                        r.swatch
                            .SetColor(&color(theme::fixed(theme::ACCENTS[i]), alpha));
                        let sw = Rect::around(cx, cy, SWATCH);
                        rt.FillRoundedRectangle(&rounded(sw, 2.0), &r.swatch);
                    }
                }
                Kind::Button(text) => {
                    rt.FillRoundedRectangle(&rounded(b, 6.0), &r.button);
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(b, 6.0), &r.hover);
                    }
                    self.put(rt, text, &r.centred, b, &r.heading_fg);
                }
                Kind::Slider(labels) => {
                    let ctl = row.ctl;
                    let (min, max, _, _) = ctl.and_then(Ctl::range).unwrap_or((0.0, 1.0, 0.0, 0.1));
                    let v = ctl.map_or(min, |c| c.value(p));
                    let f = ((v - min) / (max - min)).clamp(0.0, 1.0);
                    let at = |f: f32| b.l + 4.0 + (b.w() - 8.0) * f;
                    let x = at(f);
                    rt.FillRoundedRectangle(&rounded(b, 4.0), &r.header);
                    let fill = Rect::new(b.l, b.t, x, b.b);
                    if fill.w() > 0.5 {
                        rt.FillRoundedRectangle(&rounded(fill, 4.0), &self.played);
                    }
                    // A tick over each inner label, as the design's speed slider has.
                    let labels = *labels;
                    for i in 1..labels.saturating_sub(1) {
                        let tx = at(i as f32 / (labels - 1) as f32);
                        let tick = Rect::new(tx - 0.5, cy - 3.0, tx + 0.5, cy + 3.0);
                        let lit = tx <= x;
                        let brush = if lit {
                            &self.panels.tick_on
                        } else {
                            &self.panels.tick
                        };
                        rt.FillRectangle(&tick.d2d(), brush);
                    }
                    let thumb = Rect::new(x - 5.0, cy - 12.0, x + 5.0, cy + 12.0);
                    rt.FillRoundedRectangle(&rounded(thumb, 3.0), &self.panels.thumb);
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(thumb, 3.0), &r.hover);
                    }
                }
                Kind::Ticks(labels) => {
                    let n = labels.len();
                    for (i, text) in labels.iter().enumerate() {
                        let x = b.l + 4.0 + (b.w() - 8.0) * i as f32 / (n - 1).max(1) as f32;
                        let (format, tr) = if i == 0 {
                            (&r.small_l, Rect::new(b.l, b.t, b.l + 60.0, b.b))
                        } else if i + 1 == n {
                            (&r.small_r, Rect::new(b.r - 60.0, b.t, b.r, b.b))
                        } else {
                            (&r.small_c, Rect::new(x - 30.0, b.t, x + 30.0, b.b))
                        };
                        self.put(rt, text, format, tr, &r.tick_fg);
                    }
                }
            }
            if row.dim {
                rt.FillRectangle(&b.inset(-6.0).d2d(), &r.content_veil);
            }
        }
    }

    unsafe fn prefs_list(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        base: &Matrix3x2,
        s: &Scene,
        list: &List,
    ) {
        let p = &self.panels;
        let f = list.frame;
        unsafe {
            for spread in [6.0, 4.0, 2.0] {
                rt.FillRoundedRectangle(&rounded(f.inset(-spread), 6.0 + spread), &p.shadow);
            }
            rt.FillRoundedRectangle(&rounded(f, 6.0), &p.list_bg);
            rt.DrawRoundedRectangle(&rounded(f.inset(0.5), 6.0), &p.list_line, 1.0, None);
            rt.PushAxisAlignedClip(&f.inset(1.0).d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            for (i, (text, r)) in list.rows.iter().enumerate() {
                if !r.intersects(&f) {
                    continue;
                }
                let current = list.current == Some(i);
                if s.hover == Some(Hit::Preferences(Target::Choice(i))) {
                    rt.FillRoundedRectangle(&rounded(*r, 4.0), &self.hover);
                }
                let tr = Rect::new(r.l + 12.0, r.t, r.r - 36.0, r.b);
                let brush = if current { &p.label } else { &p.row };
                self.put(rt, text, &p.text, tr, brush);
                if current {
                    let (_, cy) = centre(*r);
                    self.prefs_glyph(ctx, base, &p.check, r.r - 20.0, cy, 22.0);
                }
            }
            rt.PopAxisAlignedClip();
        }
    }
}

fn centre(r: Rect) -> (f32, f32) {
    ((r.l + r.r) / 2.0, (r.t + r.b) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(category: Category, search: &str, height: f32) -> (Layout, State) {
        let mut st = State::new(Prefs::default(), vec!["Default".into()], category);
        st.search = TextField::new(search);
        let mut layout = Layout::new(1920.0, height, true, false);
        layout.add_preferences(&st);
        (layout, st)
    }

    fn labels(g: &Geometry) -> Vec<&'static str> {
        g.rows
            .iter()
            .filter_map(|r| match r.kind {
                Kind::Label(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn playback_matches_the_design_at_full_size() {
        let (layout, _) = open(Category::Playback, "", 1080.0);
        let g = layout.preferences.as_ref().unwrap();
        assert_eq!(g.scale, 1.0);
        assert_eq!(g.size, (W, H));
        // The frame's measured positions: the first heading, its first rows, the slider row.
        let heading = g
            .rows
            .iter()
            .find(|r| matches!(r.kind, Kind::Heading(_)))
            .unwrap();
        assert_eq!(centre(heading.r).1, 88.0);
        let row = |label: &str| {
            let r = g
                .rows
                .iter()
                .find(|r| matches!(r.kind, Kind::Label(t) if t == label));
            centre(r.unwrap().r).1
        };
        assert_eq!(row("Continue where you left off"), 131.0);
        assert_eq!(row("Start playing as soon as a file opens"), 177.0);
        assert_eq!(row("Default playback speed"), 232.0);
        // The sidebar: General and Playback where the design has them.
        let item = |c| centre(g.items.iter().find(|(i, _)| *i == c).unwrap().1).1;
        assert_eq!(item(Category::General), 124.0);
        assert_eq!(item(Category::Playback), 357.0);
        assert_eq!(item(Category::Extensions), 782.0);
    }

    #[test]
    fn a_short_window_shortens_then_scales() {
        let (layout, _) = open(Category::Subtitles, "", 720.0);
        let g = layout.preferences.as_ref().unwrap();
        assert_eq!(g.scale, 1.0);
        assert_eq!(g.size.1, 720.0 - 32.0);
        assert!(
            g.max_scroll > 0.0,
            "subtitle options scroll in a short frame"
        );
        let last = g.items.last().unwrap().1;
        assert!(last.b <= g.sidebar_box.b, "the sidebar tightens to fit");

        let (layout, _) = open(Category::Subtitles, "", 450.0);
        let g = layout.preferences.as_ref().unwrap();
        assert!(g.scale < 1.0);
        assert_eq!(g.size.1, MIN_H);
    }

    #[test]
    fn search_finds_options_in_every_category() {
        let (layout, _) = open(Category::Playback, "color", 1080.0);
        let g = layout.preferences.as_ref().unwrap();
        assert_eq!(labels(g), ["Accent color", "Text color", "Outline color"]);
        assert_eq!(g.title, "Search Results");
        let (layout, _) = open(Category::Playback, "zzz", 1080.0);
        let g = layout.preferences.as_ref().unwrap();
        assert!(g.rows.iter().any(|r| matches!(r.kind, Kind::Empty)));
    }

    #[test]
    fn controls_change_the_settings() {
        let mut ui = Ui::new();
        ui.layout = Layout::new(1920.0, 1080.0, true, false);
        ui.open_preferences(
            Prefs::default(),
            vec!["Default".into(), "USB".into()],
            Category::Audio,
        );
        ui.layout.add_preferences(ui.preferences.as_ref().unwrap());
        // A list opens, and its row sets the setting.
        assert!(ui.preferences_click(Target::Ctl(Ctl::Device)).is_none());
        let Some(Action::Preferences(PrefsAction::Changed(p))) =
            ui.preferences_click(Target::Choice(1))
        else {
            panic!("picking a device changes the settings");
        };
        assert_eq!(p.device, "USB");
        // A toggle flips.
        let Some(Action::Preferences(PrefsAction::Changed(p))) =
            ui.preferences_click(Target::Ctl(Ctl::TwoPass))
        else {
            panic!("a toggle changes the settings");
        };
        assert!(p.two_pass);
        // Every category opens, Recording among them.
        let g = ui.layout.preferences.as_ref().unwrap();
        let (recording, item) = g.items[11];
        assert_eq!(recording, Category::Recording);
        let (cx, cy) = centre(item);
        let hit = g.hit(g.origin.0 + cx * g.scale, g.origin.1 + cy * g.scale);
        assert_eq!(hit, Target::Category(Category::Recording));
    }

    #[test]
    fn theme_and_accent_pick_by_segment_and_swatch() {
        let mut ui = Ui::new();
        ui.layout = Layout::new(1920.0, 1080.0, true, false);
        ui.open_preferences(Prefs::default(), vec!["Default".into()], Category::General);
        ui.layout.add_preferences(ui.preferences.as_ref().unwrap());
        let g = ui.layout.preferences.as_ref().unwrap();
        let at = |kind: fn(&Kind) -> bool, i: usize| {
            let row = g.rows.iter().find(|r| kind(&r.kind)).unwrap();
            let (cx, cy) = centre(row.picks()[i]);
            g.hit(g.origin.0 + cx * g.scale, g.origin.1 + cy * g.scale)
        };
        let light = at(|k| matches!(k, Kind::Segments(_)), 1);
        assert_eq!(light, Target::Pick(Ctl::Theme, 1));
        let blue = at(|k| matches!(k, Kind::Swatches), 5);
        assert_eq!(blue, Target::Pick(Ctl::Accent, 5));
        // The swatches end at the controls' right edge, as the design's strip does.
        let strip = g
            .rows
            .iter()
            .find(|r| matches!(r.kind, Kind::Swatches))
            .unwrap();
        let last = strip.picks()[8];
        assert_eq!(centre(last).0 + SWATCH / 2.0, CTRL_R - SWATCH_PAD);
        ui.preferences_click(light);
        let Some(Action::Preferences(PrefsAction::Changed(p))) = ui.preferences_click(blue) else {
            panic!("a swatch changes the settings");
        };
        assert_eq!((p.theme, p.accent), (1, theme::ACCENTS[5]));
    }

    #[test]
    fn sliders_snap_to_their_step() {
        let mut p = Prefs::default();
        Ctl::Speed.set_value(&mut p, 1.137);
        assert_eq!(p.speed, 1.15);
        Ctl::HideAfter.set_value(&mut p, 40.0);
        assert_eq!(p.hide_after, 10.0);
        assert_eq!(Ctl::HideAfter.value_text(&p), "10 s");
    }
}

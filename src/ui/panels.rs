//! The player's stage-3 popups: TRACKS (video, audio and subtitle tracks, the output device),
//! opened by the control bar's audio and subtitles buttons, and SETTINGS (playback, video, audio
//! and subtitle options), opened by the gear. Both sit at the window's bottom right over the
//! video. Sizes are DIPs measured from the design's stage-3 frames, colours sampled from them.
//!
//! Drawn as in the design but disabled (dimmed, no action), because myvlc can't do them yet:
//! Show Subtitle Timeline on Seekbar, Reverse, Audio Visualizer, HDR, Resolution, Render,
//! Subtitle Speed, Subtitle Duration Factor, Embedded Sub Format, Subtitle Font, Text and Outline
//! Color, and the Default Preset footer. Open Advanced Settings opens the Settings window. The external tracks' pencil and
//! the stereo "Test" button keep their design look but do nothing.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackTab {
    Video,
    Audio,
    Subtitles,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsTab {
    Playback,
    Video,
    Audio,
    Subtitles,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Panel {
    Tracks(TrackTab),
    Settings(SettingsTab),
}

/// Everything the panels show; main.rs fills it from the session, audio effects and video
/// settings each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelData {
    pub video_tracks: Vec<String>,
    pub video_selected: Option<usize>,
    pub audio_tracks: Vec<String>,
    pub audio_selected: Option<usize>,
    pub sub_tracks: Vec<String>,
    pub sub_selected: Option<usize>,
    pub external_subs: Vec<String>,
    pub external_selected: Option<usize>,
    pub devices: Vec<String>,
    pub device_selected: usize,
    pub speed: f32,
    /// Seconds.
    pub audio_delay: f64,
    pub sub_delay: f64,
    pub eq_presets: Vec<String>,
    pub eq_preset: Option<usize>,
    /// dB, -20..20.
    pub eq_bands: [f32; 10],
    pub eq_preamp: f32,
    pub two_pass: bool,
    /// Index into `STEREO`.
    pub stereo: usize,
    /// 0 none, 1 horizontal, 2 vertical.
    pub flip: u8,
    /// Degrees: 0, 90, 180 or 270.
    pub rotate: i32,
    /// Indices into `ZOOM`, `ASPECT` and `CROP`.
    pub zoom: usize,
    pub aspect: usize,
    pub crop: usize,
    /// Each -1..1, 0 = unchanged.
    pub brightness: f32,
    pub contrast: f32,
    pub hue: f32,
    pub saturation: f32,
    /// Index into `SLEEP`.
    pub sleep_timer: usize,
    pub show_statistics: bool,
    /// 0.5..2.
    pub sub_size: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PanelAction {
    SelectTrack(TrackTab, Option<usize>),
    SelectExternal(Option<usize>),
    AddSubtitleFile,
    SelectDevice(usize),
    RefreshDevices,
    SetSpeed(f32),
    SetAudioDelay(f64),
    SetSubDelay(f64),
    SetEqPreset(Option<usize>),
    SetEqBand(usize, f32),
    SetPreamp(f32),
    SetTwoPass(bool),
    SetStereo(usize),
    SetFlip(u8),
    SetRotate(i32),
    SetZoom(usize),
    SetAspect(usize),
    SetCrop(usize),
    SetBrightness(f32),
    SetContrast(f32),
    SetHue(f32),
    SetSaturation(f32),
    SetSleepTimer(usize),
    SetShowStatistics(bool),
    SetSubSize(f32),
    ResetVideo,
}

/// "1:1 Original" is the picture at its own shape, fitted to the window (VLC's normal view).
pub const ZOOM: [&str; 3] = ["1:1 Original", "Fill", "2x"];
pub const ASPECT: [&str; 7] = ["Default", "16:9", "4:3", "21:9", "1:1", "16:10", "5:4"];
pub const CROP: [&str; 6] = ["Default", "16:9", "4:3", "21:9", "1.85:1", "2.39:1"];
pub const SLEEP: [&str; 6] = ["Off", "15 min", "30 min", "45 min", "1 hour", "End of file"];
pub const STEREO: [&str; 5] = ["Stereo", "Mono", "Left", "Right", "Reverse"];
const ROTATE: [&str; 4] = ["0 deg", "90 deg", "180 deg", "270 deg"];
/// VLC's ten equalizer bands.
const BANDS: [&str; 10] = [
    "60 Hz", "170 Hz", "310 Hz", "600 Hz", "1 kHz", "3 kHz", "6 kHz", "12 kHz", "14 kHz", "16 kHz",
];
const SPEED_TICKS: [&str; 8] = ["0.25", "0.5", "0.75", "1", "1.25", "1.5", "1.75", "2"];
const DELAY_TICKS: [&str; 3] = ["-10 s", "0 s", "+10 s"];
const SIZE_TICKS: [&str; 4] = ["50%", "100%", "150%", "200%"];

const W: f32 = 400.0;
/// Gap to the window's right edge and above the control bar.
const MARGIN: f32 = 20.0;
const TRACKS_H: f32 = 522.0;
const SETTINGS_TOP: f32 = 61.0;
const RADIUS: f32 = 8.0;
const HEADER: f32 = 50.0;
const SETTINGS_FOOTER: f32 = 125.0;
/// Room under the tracks list for "Show Preferences".
const TRACKS_FOOTER: f32 = 58.0;
/// Inset of fields, dropdowns and sliders from the panel's sides.
const PAD: f32 = 20.0;
/// Left edge of settings labels.
const LABEL_X: f32 = 24.0;
const ROW: f32 = 44.0;
const DROPDOWN_H: f32 = 46.0;
const FIELD_W: f32 = 140.0;
const FIELD_H: f32 = 36.0;
const LIST_ROW: f32 = 36.0;
const LIST_PAD: f32 = 6.0;
/// The open filter box: its "Collapse Settings" header, then label and slider per filter.
const BOX_HEAD: f32 = 46.0;
const BOX_ITEM: f32 = 75.0;
const BOX_EQ_ROW: f32 = 40.0;

// Colours sampled from the stage-3 frames.
const BG_BODY_PANEL: u32 = 0x0F0F0F;
const FG_HEADING: u32 = 0xA8A8A8;
const KNOB: u32 = 0x404040;
const TOGGLE_TEXT_BG: u32 = 0x181818;
const FIELD_LINE: u32 = 0x171717;
const DIVIDER_LINE: u32 = 0x171717;
/// The sliders' thumb, a deeper orange than the control bar's accent.
const THUMB: u32 = 0xB6640E;
/// A tick inside a slider's orange fill.
const TICK_ON: u32 = 0x4A2C12;
const LIST_BG: u32 = 0x1B1B1B;
const LIST_LINE: u32 = 0x262626;
/// Disabled rows show the design's look at this opacity.
const DIM: f32 = 0.4;

impl Panel {
    fn tabs(self) -> usize {
        match self {
            Panel::Tracks(_) => 3,
            Panel::Settings(_) => 4,
        }
    }

    fn tab(self) -> usize {
        match self {
            Panel::Tracks(t) => t as usize,
            Panel::Settings(t) => t as usize,
        }
    }

    fn with_tab(self, i: usize) -> Panel {
        const TRACKS: [TrackTab; 3] = [TrackTab::Video, TrackTab::Audio, TrackTab::Subtitles];
        const SETTINGS: [SettingsTab; 4] = [
            SettingsTab::Playback,
            SettingsTab::Video,
            SettingsTab::Audio,
            SettingsTab::Subtitles,
        ];
        match self {
            Panel::Tracks(_) => Panel::Tracks(TRACKS[i.min(2)]),
            Panel::Settings(_) => Panel::Settings(SETTINGS[i.min(3)]),
        }
    }
}

/// Every control in the panels that reacts or shows a live value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ctl {
    Track(TrackTab, Option<usize>),
    External(usize),
    AddSubtitle,
    Device(usize),
    RefreshDevices,
    ShowPreferences,
    /// "Open Advanced Settings": the Settings window at the open tab's category.
    AdvancedSettings,
    ShowStatistics,
    SleepTimer,
    Speed,
    Flip(u8),
    Rotate,
    Zoom,
    Aspect,
    Crop,
    ResetVideo,
    /// Expands the video filters.
    Essential,
    Brightness,
    Contrast,
    Hue,
    Saturation,
    AudioTrack,
    AudioLanguage,
    Stereo,
    AudioDelay,
    EqPreset,
    /// Expands the equalizer bands.
    Equalizer,
    Preamp,
    Band(u8),
    TwoPass,
    SubTrack,
    SubLanguage,
    SubDelay,
    SubSize,
}

fn track_name(names: &[String], selected: Option<usize>) -> String {
    selected
        .and_then(|i| names.get(i))
        .map_or_else(|| "None".to_owned(), Clone::clone)
}

/// Rounds to the nearest 1/`n`; dividing last keeps 0.73 from printing as 0.72999996.
fn round_to(v: f32, n: f32) -> f32 {
    (v * n).round() / n
}

impl Ctl {
    /// Range and default of a slider.
    fn range(self) -> Option<(f32, f32, f32)> {
        Some(match self {
            Ctl::Speed => (0.25, 2.0, 1.0),
            Ctl::AudioDelay | Ctl::SubDelay => (-10.0, 10.0, 0.0),
            Ctl::Brightness | Ctl::Contrast | Ctl::Hue | Ctl::Saturation => (-1.0, 1.0, 0.0),
            Ctl::Preamp | Ctl::Band(_) => (-20.0, 20.0, 0.0),
            Ctl::SubSize => (0.5, 2.0, 1.0),
            _ => return None,
        })
    }

    fn value(self, d: &PanelData) -> f32 {
        match self {
            Ctl::Speed => d.speed,
            Ctl::AudioDelay => d.audio_delay as f32,
            Ctl::SubDelay => d.sub_delay as f32,
            Ctl::Brightness => d.brightness,
            Ctl::Contrast => d.contrast,
            Ctl::Hue => d.hue,
            Ctl::Saturation => d.saturation,
            Ctl::Preamp => d.eq_preamp,
            Ctl::Band(i) => d.eq_bands[usize::from(i)],
            Ctl::SubSize => d.sub_size,
            _ => 0.0,
        }
    }

    /// The action for a slider moved to `v`, snapped to a sensible step.
    pub fn set(self, v: f32) -> Option<PanelAction> {
        Some(match self {
            Ctl::Speed => PanelAction::SetSpeed(round_to(v, 20.0)),
            Ctl::AudioDelay => PanelAction::SetAudioDelay(f64::from(round_to(v, 20.0))),
            Ctl::SubDelay => PanelAction::SetSubDelay(f64::from(round_to(v, 20.0))),
            Ctl::Brightness => PanelAction::SetBrightness(round_to(v, 100.0)),
            Ctl::Contrast => PanelAction::SetContrast(round_to(v, 100.0)),
            Ctl::Hue => PanelAction::SetHue(round_to(v, 100.0)),
            Ctl::Saturation => PanelAction::SetSaturation(round_to(v, 100.0)),
            Ctl::Preamp => PanelAction::SetPreamp(round_to(v, 10.0)),
            Ctl::Band(i) => PanelAction::SetEqBand(usize::from(i), round_to(v, 10.0)),
            Ctl::SubSize => PanelAction::SetSubSize(round_to(v, 20.0)),
            _ => return None,
        })
    }

    /// The slider's default, for a double-click.
    pub fn reset(self) -> Option<PanelAction> {
        self.set(self.range()?.2)
    }

    pub fn is_slider(self) -> bool {
        self.range().is_some()
    }

    /// Whether a click opens a choice list.
    pub fn has_options(self) -> bool {
        self.options(&PanelData::default()).is_some()
    }

    /// The choices of a control that opens a list.
    fn options(self, d: &PanelData) -> Option<Vec<String>> {
        let list = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect();
        let tracks = |names: &[String]| {
            let mut v = vec!["None".to_owned()];
            v.extend(names.iter().cloned());
            v
        };
        Some(match self {
            Ctl::SleepTimer => list(&SLEEP),
            Ctl::Rotate => list(&ROTATE),
            Ctl::Zoom => list(&ZOOM),
            Ctl::Aspect => list(&ASPECT),
            Ctl::Crop => list(&CROP),
            Ctl::Stereo => list(&STEREO),
            Ctl::AudioTrack => tracks(&d.audio_tracks),
            Ctl::SubTrack => tracks(&d.sub_tracks),
            Ctl::EqPreset => tracks(&d.eq_presets),
            _ => return None,
        })
    }

    fn current(self, d: &PanelData) -> Option<usize> {
        let track = |s: Option<usize>| Some(s.map_or(0, |i| i + 1));
        match self {
            Ctl::SleepTimer => Some(d.sleep_timer),
            Ctl::Rotate => Some((d.rotate.rem_euclid(360) / 90) as usize),
            Ctl::Zoom => Some(d.zoom),
            Ctl::Aspect => Some(d.aspect),
            Ctl::Crop => Some(d.crop),
            Ctl::Stereo => Some(d.stereo),
            Ctl::AudioTrack => track(d.audio_selected),
            Ctl::SubTrack => track(d.sub_selected),
            Ctl::EqPreset => track(d.eq_preset),
            _ => None,
        }
    }

    /// The action for choice `i` of the control's list.
    pub fn pick(self, i: usize) -> Option<PanelAction> {
        Some(match self {
            Ctl::SleepTimer => PanelAction::SetSleepTimer(i),
            Ctl::Rotate => PanelAction::SetRotate(i as i32 * 90),
            Ctl::Zoom => PanelAction::SetZoom(i),
            Ctl::Aspect => PanelAction::SetAspect(i),
            Ctl::Crop => PanelAction::SetCrop(i),
            Ctl::Stereo => PanelAction::SetStereo(i),
            Ctl::AudioTrack => PanelAction::SelectTrack(TrackTab::Audio, i.checked_sub(1)),
            Ctl::SubTrack => PanelAction::SelectTrack(TrackTab::Subtitles, i.checked_sub(1)),
            Ctl::EqPreset => PanelAction::SetEqPreset(i.checked_sub(1)),
            _ => return None,
        })
    }

    fn toggled(self, d: &PanelData) -> bool {
        match self {
            Ctl::ShowStatistics => d.show_statistics,
            Ctl::TwoPass => d.two_pass,
            _ => false,
        }
    }

    /// Whether the control is the current choice: a checked row or the active segment.
    fn selected(self, d: &PanelData) -> bool {
        match self {
            Ctl::Track(TrackTab::Video, i) => d.video_selected == i,
            Ctl::Track(TrackTab::Audio, i) => d.audio_selected == i,
            Ctl::Track(TrackTab::Subtitles, i) => d.sub_selected == i,
            Ctl::External(i) => d.external_selected == Some(i),
            Ctl::Device(i) => d.device_selected == i,
            Ctl::Flip(k) => d.flip == k,
            _ => false,
        }
    }

    /// What a click does for the controls that act straight away.
    pub fn click(self, d: &PanelData) -> Option<PanelAction> {
        Some(match self {
            Ctl::Track(tab, i) => PanelAction::SelectTrack(tab, i),
            // A second click on the chosen file turns it off; there is no "None" row.
            Ctl::External(i) => {
                PanelAction::SelectExternal((d.external_selected != Some(i)).then_some(i))
            }
            Ctl::AddSubtitle => PanelAction::AddSubtitleFile,
            Ctl::Device(i) => PanelAction::SelectDevice(i),
            Ctl::RefreshDevices => PanelAction::RefreshDevices,
            Ctl::ShowStatistics => PanelAction::SetShowStatistics(!d.show_statistics),
            Ctl::TwoPass => PanelAction::SetTwoPass(!d.two_pass),
            Ctl::Flip(k) => PanelAction::SetFlip(k),
            Ctl::ResetVideo => PanelAction::ResetVideo,
            _ => return None,
        })
    }

    /// The text in a field or dropdown that follows the data.
    fn text(self, d: &PanelData) -> String {
        let pick = |items: &[&str], i: usize| items.get(i).copied().unwrap_or("").to_owned();
        match self {
            Ctl::SleepTimer => pick(&SLEEP, d.sleep_timer),
            Ctl::Rotate => format!("{} deg", d.rotate),
            Ctl::Zoom => pick(&ZOOM, d.zoom),
            Ctl::Aspect => pick(&ASPECT, d.aspect),
            Ctl::Crop => pick(&CROP, d.crop),
            Ctl::Stereo => pick(&STEREO, d.stereo),
            Ctl::Speed => {
                let s = format!("{:.2}", d.speed);
                format!("{}x", s.trim_end_matches('0').trim_end_matches('.'))
            }
            Ctl::AudioDelay => format!("{:.3} s", d.audio_delay),
            Ctl::SubDelay => format!("{:.3} s", d.sub_delay),
            Ctl::AudioTrack => d
                .audio_selected
                .map_or("None".into(), |i| (i + 1).to_string()),
            Ctl::SubTrack => d
                .sub_selected
                .map_or("None".into(), |i| (i + 1).to_string()),
            Ctl::AudioLanguage => track_name(&d.audio_tracks, d.audio_selected),
            Ctl::SubLanguage => track_name(&d.sub_tracks, d.sub_selected),
            Ctl::EqPreset => track_name(&d.eq_presets, d.eq_preset),
            Ctl::SubSize => format!("{:.0}%", d.sub_size * 100.0),
            _ => String::new(),
        }
    }
}

/// Which popup is open and how it is scrolled; lives in `Ui`.
#[derive(Clone, Debug)]
pub struct State {
    pub open: Option<Panel>,
    /// The control bar button that opened it, drawn in the accent colour.
    pub opener: Option<Hit>,
    pub scroll: f32,
    pub essential: bool,
    pub equalizer: bool,
    /// The control whose choice list is open.
    pub list: Option<Ctl>,
    pub list_scroll: f32,
    /// The gear reopens settings where they were left.
    pub settings_tab: SettingsTab,
}

impl Default for State {
    fn default() -> Self {
        State {
            open: None,
            opener: None,
            scroll: 0.0,
            essential: false,
            equalizer: false,
            list: None,
            list_scroll: 0.0,
            settings_tab: SettingsTab::Playback,
        }
    }
}

/// What the pointer is over inside a popup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Tab(u8),
    Ctl(Ctl),
    /// A row of the open choice list.
    Choice(usize),
    /// Anywhere else on the popup: it swallows the click.
    Body,
}

#[derive(Clone, Copy, Debug)]
enum Value {
    Fixed(&'static str),
    /// `Ctl::text` of the row's control.
    Live,
}

#[derive(Clone, Copy, Debug)]
enum Glyph {
    Refresh,
    Pencil,
    Plus,
}

#[derive(Clone, Debug)]
enum Kind {
    Heading(&'static str),
    /// Dim text: setting labels, section titles.
    Label(&'static str),
    /// A track or device, checked when chosen.
    Choice(String),
    Divider,
    Toggle,
    /// `up`: the chevron points up, as on the footer's preset menu.
    Dropdown {
        value: Value,
        up: bool,
    },
    Field {
        value: Value,
        swatch: Option<u32>,
    },
    /// The dark well behind the segments.
    Segments,
    Segment(&'static str),
    Slider {
        ticks: u8,
        centre: bool,
    },
    TickLabels(&'static [&'static str]),
    /// "Expand Settings", or the open box's "Collapse Settings" header.
    Expand(bool),
    /// The open filter box's background.
    Box,
    Button(&'static str),
    Icon(Glyph),
    /// "Show Preferences".
    Footer(&'static str),
    /// "Open Advanced Settings".
    Link(&'static str),
}

#[derive(Clone, Debug)]
struct Row {
    kind: Kind,
    r: Rect,
    ctl: Option<Ctl>,
    /// Hit-testable; display-only and disabled rows aren't.
    click: bool,
    dim: bool,
}

impl Row {
    fn hit_rect(&self) -> Rect {
        match self.kind {
            // A thin track is hard to grab; the thumb's height counts.
            Kind::Slider { .. } => Rect::new(self.r.l, self.r.t - 6.0, self.r.r, self.r.b + 6.0),
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

/// Where a popup's parts sit; part of `Layout`.
#[derive(Clone, Default, Debug)]
pub struct Geometry {
    pub panel: Option<Panel>,
    pub opener: Option<Hit>,
    pub frame: Rect,
    header: Rect,
    tabs: Vec<Rect>,
    /// The scrolling area, between the tabs and the footer.
    content: Rect,
    rows: Vec<Row>,
    footer: Vec<Row>,
    footer_top: f32,
    pub max_scroll: f32,
    list: Option<List>,
    /// What the rows were laid out from: a change needs a new layout.
    data: Option<PanelData>,
}

/// Places rows, with y measured from the panel's top in the design.
struct Builder<'a> {
    rows: Vec<Row>,
    frame: Rect,
    /// Panel top minus the scroll.
    top: f32,
    s: &'a State,
}

impl Builder<'_> {
    fn rect(&self, l: f32, cy: f32, r: f32, h: f32) -> Rect {
        let y = self.top + cy;
        Rect::new(self.frame.l + l, y - h / 2.0, self.frame.l + r, y + h / 2.0)
    }

    fn push(&mut self, kind: Kind, r: Rect, ctl: Option<Ctl>, click: bool, dim: bool) {
        self.rows.push(Row {
            kind,
            r,
            ctl,
            click,
            dim,
        });
    }

    fn heading(&mut self, text: &'static str, cy: f32) {
        let r = self.rect(LABEL_X - 1.0, cy, W - PAD, 24.0);
        self.push(Kind::Heading(text), r, None, false, false);
    }

    fn label(&mut self, text: &'static str, x: f32, cy: f32, dim: bool) {
        let r = self.rect(x, cy, W - PAD, 24.0);
        self.push(Kind::Label(text), r, None, false, dim);
    }

    fn divider(&mut self, cy: f32) {
        let r = self.rect(0.0, cy, W, 1.0);
        self.push(Kind::Divider, r, None, false, false);
    }

    /// A label with an OFF/ON switch at the right; disabled without a control.
    fn toggle(&mut self, label: &'static str, cy: f32, ctl: Option<Ctl>) {
        let dim = ctl.is_none();
        self.label(label, LABEL_X, cy, dim);
        let r = self.rect(W - PAD - 68.0, cy, W - PAD, 32.0);
        self.push(Kind::Toggle, r, ctl, !dim, dim);
    }

    /// A label with a menu at the right, or a full-width menu without a label.
    fn dropdown(&mut self, label: Option<&'static str>, cy: f32, ctl: Option<Ctl>, value: Value) {
        let dim = ctl.is_none();
        let l = match label {
            Some(text) => {
                self.label(text, LABEL_X, cy, dim);
                160.0
            }
            None => PAD,
        };
        let r = self.rect(l, cy, W - PAD, DROPDOWN_H);
        let kind = Kind::Dropdown { value, up: false };
        self.push(kind, r, ctl, !dim, dim);
    }

    /// A label with a value field at the right. Fields whose control has choices open a list.
    fn field(&mut self, label: &'static str, cy: f32, ctl: Option<Ctl>, value: Value, dim: bool) {
        self.label(label, LABEL_X, cy, dim);
        let r = self.rect(W - PAD - FIELD_W, cy, W - PAD, FIELD_H);
        let click = !dim && ctl.is_some_and(Ctl::has_options);
        let swatch = match value {
            Value::Fixed(text) if text.starts_with('#') => u32::from_str_radix(&text[1..], 16).ok(),
            _ => None,
        };
        self.push(Kind::Field { value, swatch }, r, ctl, click, dim);
    }

    fn slider(&mut self, ctl: Option<Ctl>, cy: f32, l: f32, r: f32, ticks: u8, centre: bool) {
        let dim = ctl.is_none();
        let rect = self.rect(l, cy, r, 14.0);
        self.push(Kind::Slider { ticks, centre }, rect, ctl, !dim, dim);
    }

    fn ticks(&mut self, labels: &'static [&'static str], cy: f32, dim: bool) {
        let r = self.rect(PAD, cy, W - PAD, 16.0);
        self.push(Kind::TickLabels(labels), r, None, false, dim);
    }

    fn button(&mut self, text: &'static str, cy: f32, ctl: Option<Ctl>) {
        let r = self.rect(W - PAD - 69.0, cy, W - PAD, 36.0);
        let click = ctl.is_some();
        self.push(Kind::Button(text), r, ctl, click, false);
    }

    fn choice(&mut self, text: String, cy: f32, ctl: Ctl) {
        let r = self.rect(12.0, cy, W - 12.0, ROW);
        self.push(Kind::Choice(text), r, Some(ctl), true, false);
    }

    fn icon(&mut self, glyph: Glyph, cx: f32, cy: f32, ctl: Option<Ctl>) {
        let r = self.rect(cx - 16.0, cy, cx + 16.0, 32.0);
        let click = ctl.is_some();
        self.push(Kind::Icon(glyph), r, ctl, click, false);
    }

    /// "Expand Settings" for a filter group at `cy`, or its open box with `fill` placing the
    /// contents from the box's top. Returns the bottom of the group.
    fn group(
        &mut self,
        cy: f32,
        ctl: Ctl,
        open: bool,
        h: f32,
        fill: impl FnOnce(&mut Self, f32),
    ) -> f32 {
        let top = cy - DROPDOWN_H / 2.0;
        if !open {
            let r = self.rect(PAD, cy, W - PAD, DROPDOWN_H);
            self.push(Kind::Expand(false), r, Some(ctl), true, false);
            return top + DROPDOWN_H;
        }
        let r = self.rect(PAD, top + h / 2.0, W - PAD, h);
        self.push(Kind::Box, r, None, false, false);
        let head = self.rect(PAD, top + BOX_HEAD / 2.0, W - PAD, BOX_HEAD);
        self.push(Kind::Expand(true), head, Some(ctl), true, false);
        fill(self, top);
        top + h
    }
}

fn tracks(b: &mut Builder, tab: TrackTab, d: &PanelData) -> f32 {
    let names = match tab {
        TrackTab::Video => &d.video_tracks,
        TrackTab::Audio => &d.audio_tracks,
        TrackTab::Subtitles => &d.sub_tracks,
    };
    let ctl = |i| Ctl::Track(tab, i);
    b.label("Embedded Tracks", 28.0, 80.0, false);
    let mut y = 127.5;
    b.choice("None".into(), y, ctl(None));
    for (i, name) in names.iter().enumerate() {
        y += ROW;
        b.choice(name.clone(), y, ctl(Some(i)));
    }
    match tab {
        TrackTab::Video => {}
        TrackTab::Audio => {
            b.divider(y + 34.0);
            let label = y + 66.0;
            b.label("Output Device", 28.0, label, false);
            b.icon(Glyph::Refresh, 359.0, label, Some(Ctl::RefreshDevices));
            y = label + 47.5 - ROW;
            for (i, name) in d.devices.iter().enumerate() {
                y += ROW;
                b.choice(name.clone(), y, Ctl::Device(i));
            }
        }
        TrackTab::Subtitles => {
            let label = y + 64.0;
            b.label("External Tracks", 28.0, label, false);
            b.icon(Glyph::Pencil, 315.0, label, None);
            b.icon(Glyph::Plus, 359.0, label, Some(Ctl::AddSubtitle));
            y = label + 47.5 - ROW;
            for (i, name) in d.external_subs.iter().enumerate() {
                y += ROW;
                b.choice(name.clone(), y, Ctl::External(i));
            }
        }
    }
    y + ROW / 2.0
}

fn playback(b: &mut Builder) -> f32 {
    b.toggle("Show Subtitle Timeline on Seekbar", 87.5, None);
    b.toggle("Show Statistics", 139.5, Some(Ctl::ShowStatistics));
    b.toggle("Reverse", 191.5, None);
    b.dropdown(
        Some("Sleep Timer"),
        251.0,
        Some(Ctl::SleepTimer),
        Value::Live,
    );
    b.dropdown(
        Some("Audio Visualizer"),
        319.0,
        None,
        Value::Fixed("Disabled"),
    );
    b.field("Speed", 386.0, Some(Ctl::Speed), Value::Live, false);
    b.slider(Some(Ctl::Speed), 433.0, PAD, W - PAD, 6, false);
    b.ticks(&SPEED_TICKS, 463.0, false);
    478.0
}

fn video(b: &mut Builder) -> f32 {
    b.heading("Defaults", 80.0);
    b.button("Reset", 80.0, Some(Ctl::ResetVideo));
    b.label("Flip", LABEL_X, 125.0, false);
    let well = b.rect(PAD, 174.0, W - PAD, 40.0);
    b.push(Kind::Segments, well, None, false, false);
    let seg = (well.w() - 2.0 - 4.0) / 3.0;
    for (k, text) in ["None", "Horizontal", "Vertical"].into_iter().enumerate() {
        let l = well.l + 1.0 + (seg + 2.0) * k as f32;
        let r = Rect::new(l, well.t + 1.0, l + seg, well.b - 1.0);
        b.push(
            Kind::Segment(text),
            r,
            Some(Ctl::Flip(k as u8)),
            true,
            false,
        );
    }
    b.field("Rotate", 241.5, Some(Ctl::Rotate), Value::Live, false);
    b.dropdown(Some("Zoom"), 309.0, Some(Ctl::Zoom), Value::Live);
    b.dropdown(Some("HDR"), 377.0, None, Value::Fixed("Auto"));
    b.dropdown(
        Some("Resolution"),
        445.0,
        None,
        Value::Fixed("Best Available"),
    );
    b.dropdown(Some("Aspect Ratio"), 513.0, Some(Ctl::Aspect), Value::Live);
    b.dropdown(Some("Crop"), 581.0, Some(Ctl::Crop), Value::Live);
    b.dropdown(Some("Render"), 649.0, None, Value::Fixed("Local"));
    b.divider(692.5);
    b.heading("Filters", 722.0);
    b.label("Essential", LABEL_X, 761.0, false);
    let filters = [
        ("Brightness", Ctl::Brightness),
        ("Contrast", Ctl::Contrast),
        ("Hue", Ctl::Hue),
        ("Saturation", Ctl::Saturation),
    ];
    let h = 66.0 + BOX_ITEM * 3.0 + 33.0 + 28.0;
    let open = b.s.essential;
    b.group(814.0, Ctl::Essential, open, h, |b, top| {
        for (i, (name, ctl)) in filters.into_iter().enumerate() {
            let y = top + 66.0 + BOX_ITEM * i as f32;
            b.label(name, PAD + 16.0, y, false);
            b.slider(Some(ctl), y + 33.0, PAD + 16.0, W - PAD - 16.0, 0, false);
        }
    }) + 20.0
}

fn audio(b: &mut Builder) -> f32 {
    b.heading("Defaults", 80.0);
    b.field(
        "Audio Track",
        124.5,
        Some(Ctl::AudioTrack),
        Value::Live,
        false,
    );
    b.field(
        "Audio Language",
        177.5,
        Some(Ctl::AudioLanguage),
        Value::Live,
        false,
    );
    b.label("Stereo Mode", LABEL_X, 238.0, false);
    // ponytail: "Test" has nothing to play yet; it keeps the design's look.
    b.button("Test", 238.0, None);
    b.dropdown(None, 297.5, Some(Ctl::Stereo), Value::Live);
    b.divider(341.0);
    b.heading("Synchronization", 372.0);
    b.field(
        "Audio Track Delay",
        420.0,
        Some(Ctl::AudioDelay),
        Value::Live,
        false,
    );
    b.slider(Some(Ctl::AudioDelay), 467.5, PAD, W - PAD, 0, true);
    b.ticks(&DELAY_TICKS, 494.0, false);
    b.divider(525.0);
    b.heading("Filters", 555.0);
    b.label("Equalizer", LABEL_X, 595.0, false);
    b.dropdown(None, 645.5, Some(Ctl::EqPreset), Value::Live);
    let rows = 1 + BANDS.len();
    let h = BOX_HEAD + 20.0 + BOX_EQ_ROW * rows as f32 - 6.0;
    let open = b.s.equalizer;
    let bottom = b.group(704.0, Ctl::Equalizer, open, h, |b, top| {
        let names = std::iter::once("Preamp").chain(BANDS);
        let ctls = std::iter::once(Ctl::Preamp).chain((0..10).map(Ctl::Band));
        for (i, (name, ctl)) in names.zip(ctls).enumerate() {
            let y = top + BOX_HEAD + 14.0 + BOX_EQ_ROW * i as f32;
            let label = b.rect(PAD + 16.0, y, PAD + 86.0, 24.0);
            b.push(Kind::Label(name), label, None, false, false);
            b.slider(Some(ctl), y, PAD + 90.0, W - PAD - 16.0, 0, true);
        }
    });
    // "2 Pass" sits 58 under the closed group's centre.
    let pass = bottom - DROPDOWN_H / 2.0 + 58.0;
    b.toggle("2 Pass", pass, Some(Ctl::TwoPass));
    b.divider(pass + 43.0);
    pass + 60.0
}

fn subtitles(b: &mut Builder) -> f32 {
    b.heading("Defaults", 80.0);
    b.field(
        "Subtitle Track",
        124.5,
        Some(Ctl::SubTrack),
        Value::Live,
        false,
    );
    b.field(
        "Subtitle Language",
        177.5,
        Some(Ctl::SubLanguage),
        Value::Live,
        false,
    );
    b.divider(211.0);
    b.heading("Synchronization", 241.0);
    b.field(
        "Subtitle Track Delay",
        289.0,
        Some(Ctl::SubDelay),
        Value::Live,
        false,
    );
    b.slider(Some(Ctl::SubDelay), 332.5, PAD, W - PAD, 0, true);
    b.ticks(&DELAY_TICKS, 359.0, false);
    b.field(
        "Subtitle Speed",
        404.0,
        None,
        Value::Fixed("1.000 fps"),
        true,
    );
    b.slider(None, 448.0, PAD, W - PAD, 0, false);
    b.field(
        "Subtitle Duration Factor",
        493.0,
        None,
        Value::Fixed("0.000"),
        true,
    );
    b.slider(None, 538.0, PAD, W - PAD, 0, false);
    b.divider(570.5);
    b.heading("Appearance", 601.0);
    // Not in the design: the one appearance setting myvlc has.
    b.field("Text Size", 646.0, Some(Ctl::SubSize), Value::Live, false);
    b.slider(Some(Ctl::SubSize), 690.0, PAD, W - PAD, 0, false);
    b.ticks(&SIZE_TICKS, 717.0, false);
    b.toggle("Embedded Sub Format", 762.0, None);
    b.field("Subtitle Font", 814.0, None, Value::Fixed("Inter"), true);
    b.field("Text Color", 867.0, None, Value::Fixed("#B8B8B8"), true);
    b.field("Outline Color", 918.0, None, Value::Fixed("#0C0C0C"), true);
    950.0
}

impl Layout {
    /// Adds the open popup to a player layout, built from the state and data it shows.
    pub fn add_panel(&mut self, state: &State, data: &PanelData) {
        let Some(panel) = state.open else { return };
        let (w, h) = (self.width, self.height);
        let bottom = h - self.tools.bar - MARGIN;
        let top = match panel {
            Panel::Tracks(_) => bottom - TRACKS_H,
            Panel::Settings(_) => SETTINGS_TOP,
        }
        .max(TITLE_BAR + FRAME);
        let frame = Rect::new(w - MARGIN - W, top, w - MARGIN, bottom.max(top + 200.0));
        let header = Rect::new(frame.l, frame.t, frame.r, frame.t + HEADER);
        let footer_top = match panel {
            Panel::Tracks(_) => frame.b - TRACKS_FOOTER,
            Panel::Settings(_) => frame.b - SETTINGS_FOOTER,
        };
        let content = Rect::new(frame.l, header.b, frame.r, footer_top);
        let build = |scroll: f32| {
            let mut b = Builder {
                rows: Vec::new(),
                frame,
                top: frame.t - scroll,
                s: state,
            };
            let end = match panel {
                Panel::Tracks(t) => tracks(&mut b, t, data),
                Panel::Settings(SettingsTab::Playback) => playback(&mut b),
                Panel::Settings(SettingsTab::Video) => video(&mut b),
                Panel::Settings(SettingsTab::Audio) => audio(&mut b),
                Panel::Settings(SettingsTab::Subtitles) => subtitles(&mut b),
            };
            (b.rows, (end - (content.b - frame.t)).max(0.0))
        };
        let (mut rows, max_scroll) = build(state.scroll);
        if state.scroll > max_scroll {
            rows = build(max_scroll).0;
        }

        let mut b = Builder {
            rows: Vec::new(),
            frame,
            top: frame.t,
            s: state,
        };
        match panel {
            Panel::Tracks(_) => {
                let r = Rect::new(frame.l + PAD, frame.b - 50.0, frame.r - PAD, frame.b - 18.0);
                b.push(
                    Kind::Footer("Show Preferences"),
                    r,
                    Some(Ctl::ShowPreferences),
                    true,
                    false,
                );
            }
            Panel::Settings(_) => {
                let y = footer_top - frame.t;
                let r = b.rect(PAD, y + 41.0, W - PAD, 42.0);
                let preset = Kind::Dropdown {
                    value: Value::Fixed("Default Preset"),
                    up: true,
                };
                b.push(preset, r, None, false, true);
                let r = b.rect(PAD, y + 89.0, W - PAD, 24.0);
                let ctl = Some(Ctl::AdvancedSettings);
                b.push(Kind::Link("Open Advanced Settings"), r, ctl, true, false);
            }
        }

        let tabs = (0..panel.tabs())
            .map(|i| tab_rect(panel, header, i))
            .collect();
        let list = state
            .list
            .and_then(|ctl| self.choice_list(ctl, &rows, data, state.list_scroll));
        self.panels = Geometry {
            panel: Some(panel),
            opener: state.opener,
            frame,
            header,
            tabs,
            content,
            rows,
            footer: b.rows,
            footer_top,
            max_scroll,
            list,
            data: Some(data.clone()),
        };
    }

    /// The open control's choices, under its field, or over it when there's no room below.
    fn choice_list(&self, ctl: Ctl, rows: &[Row], d: &PanelData, scroll: f32) -> Option<List> {
        let anchor = rows.iter().find(|r| r.ctl == Some(ctl) && r.click)?.r;
        let items = ctl.options(d)?;
        let full = items.len() as f32 * LIST_ROW + 2.0 * LIST_PAD;
        let (min, max) = (TITLE_BAR + 8.0, self.height - self.tools.bar - 8.0);
        let below = max - (anchor.b + 4.0);
        let above = anchor.t - 4.0 - min;
        let (t, h) = if full <= below || below >= above {
            (anchor.b + 4.0, full.min(below))
        } else {
            let h = full.min(above);
            (anchor.t - 4.0 - h, h)
        };
        let frame = Rect::new(anchor.l, t, anchor.r, t + h);
        let max_scroll = (full - h).max(0.0);
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
            current: ctl.current(d),
            max_scroll,
        })
    }
}

fn tab_rect(panel: Panel, header: Rect, i: usize) -> Rect {
    // Tracks has three equal tabs; settings' four hug their labels, spread between 14 and 386.
    let (l, w) = match panel {
        Panel::Tracks(_) => (14.0 + 130.0 * i as f32, 112.0),
        Panel::Settings(_) => {
            const LEFT: [f32; 4] = [14.0, 121.5, 208.0, 295.0];
            const WIDTH: [f32; 4] = [91.0, 70.0, 70.0, 91.0];
            (LEFT[i], WIDTH[i])
        }
    };
    Rect::new(header.l + l, header.t, header.l + l + w, header.b)
}

impl Geometry {
    pub fn hit(&self, x: f32, y: f32) -> Option<Target> {
        if let Some(list) = &self.list
            && list.frame.contains(x, y)
        {
            let row = list.rows.iter().position(|(_, r)| r.contains(x, y));
            return Some(row.map_or(Target::Body, Target::Choice));
        }
        if !self.frame.contains(x, y) {
            return None;
        }
        if let Some(i) = self.tabs.iter().position(|r| r.contains(x, y)) {
            return Some(Target::Tab(i as u8));
        }
        let hit = |rows: &[Row]| {
            rows.iter()
                .rev()
                .find(|r| r.click && r.hit_rect().contains(x, y))
                .and_then(|r| r.ctl)
        };
        let ctl = if self.content.contains(x, y) {
            hit(&self.rows)
        } else {
            hit(&self.footer)
        };
        Some(ctl.map_or(Target::Body, Target::Ctl))
    }

    /// The value a slider takes with the pointer at `x`.
    pub fn slider_value(&self, ctl: Ctl, x: f32) -> Option<f32> {
        let (min, max, _) = ctl.range()?;
        let row = self
            .rows
            .iter()
            .find(|r| r.ctl == Some(ctl) && matches!(r.kind, Kind::Slider { .. }))?;
        let f = ((x - row.r.l - 4.0) / (row.r.w() - 8.0)).clamp(0.0, 1.0);
        Some(min + f * (max - min))
    }

    pub fn list_open(&self) -> bool {
        self.list.is_some()
    }

    pub fn over_list(&self, x: f32, y: f32) -> bool {
        self.list.as_ref().is_some_and(|l| l.frame.contains(x, y))
    }

    pub fn list_max_scroll(&self) -> f32 {
        self.list.as_ref().map_or(0.0, |l| l.max_scroll)
    }

    pub fn list_ctl(&self) -> Option<Ctl> {
        self.list.as_ref().map(|l| l.ctl)
    }

    pub fn data(&self) -> Option<&PanelData> {
        self.data.as_ref()
    }

    /// The tab a "Show Preferences" click opens in settings.
    pub fn preferences_tab(&self) -> SettingsTab {
        match self.panel {
            Some(Panel::Tracks(TrackTab::Video)) => SettingsTab::Video,
            Some(Panel::Tracks(TrackTab::Subtitles)) => SettingsTab::Subtitles,
            _ => SettingsTab::Audio,
        }
    }
}

impl State {
    /// The panel a control bar button opens.
    pub fn for_button(&self, hit: Hit) -> Option<Panel> {
        match hit {
            Hit::Audio => Some(Panel::Tracks(TrackTab::Audio)),
            Hit::Subtitles => Some(Panel::Tracks(TrackTab::Subtitles)),
            Hit::Settings => Some(Panel::Settings(self.settings_tab)),
            _ => None,
        }
    }

    pub fn show(&mut self, panel: Option<Panel>, opener: Option<Hit>) {
        if let Some(Panel::Settings(tab)) = panel {
            self.settings_tab = tab;
        }
        self.open = panel;
        self.opener = opener;
        self.scroll = 0.0;
        self.list = None;
        self.list_scroll = 0.0;
    }

    pub fn switch_tab(&mut self, i: usize) {
        if let Some(p) = self.open {
            let opener = self.opener;
            self.show(Some(p.with_tab(i)), opener);
        }
    }
}

/// Brushes, fonts and icons the popups use.
pub(super) struct Resources {
    body: ID2D1SolidColorBrush,
    header: ID2D1SolidColorBrush,
    panel: ID2D1SolidColorBrush,
    box_bg: ID2D1SolidColorBrush,
    footer_bg: ID2D1SolidColorBrush,
    heading: ID2D1SolidColorBrush,
    pub(super) label: ID2D1SolidColorBrush,
    pub(super) row: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    knob: ID2D1SolidColorBrush,
    toggle_text_bg: ID2D1SolidColorBrush,
    field_line: ID2D1SolidColorBrush,
    divider: ID2D1SolidColorBrush,
    line: ID2D1SolidColorBrush,
    pub(super) thumb: ID2D1SolidColorBrush,
    pub(super) tick: ID2D1SolidColorBrush,
    pub(super) tick_on: ID2D1SolidColorBrush,
    button_bg: ID2D1SolidColorBrush,
    pub(super) list_bg: ID2D1SolidColorBrush,
    pub(super) list_line: ID2D1SolidColorBrush,
    hover: ID2D1SolidColorBrush,
    pub(super) shadow: ID2D1SolidColorBrush,
    swatch: ID2D1SolidColorBrush,
    veil: ID2D1SolidColorBrush,
    tab: IDWriteTextFormat,
    pub(super) text: IDWriteTextFormat,
    centred: IDWriteTextFormat,
    segment: IDWriteTextFormat,
    heading_font: IDWriteTextFormat,
    small_l: IDWriteTextFormat,
    small_c: IDWriteTextFormat,
    small_r: IDWriteTextFormat,
    pub(super) check: Svg,
    pub(super) chevron_down: Svg,
    pub(super) chevron_up: Svg,
    chevron_right: Svg,
    pencil: Svg,
    refresh: Svg,
    plus: Svg,
    gear: Svg,
    /// Tab icons, (inactive, active).
    tab_icons: [(Svg, Svg); 4],
    /// The control bar's buttons while their popup is open.
    pub(super) audio_on: Svg,
    pub(super) subtitles_on: Svg,
    pub(super) settings_on: Svg,
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
        let regular = DWRITE_FONT_WEIGHT_REGULAR;
        let (lead, centre, trail) = (
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_TEXT_ALIGNMENT_CENTER,
            DWRITE_TEXT_ALIGNMENT_TRAILING,
        );
        let tint = |bytes: &[u8], rgb: u32| {
            let text = String::from_utf8_lossy(bytes).replace("#8F8F8F", &format!("#{rgb:06X}"));
            svg(ctx, text.as_bytes())
        };
        let tab = |bytes: &[u8]| -> Result<(Svg, Svg)> {
            Ok((tint(bytes, FG_DIM)?, tint(bytes, ACCENT)?))
        };
        Ok(Resources {
            body: brush(BG_BODY_PANEL, 1.0)?,
            header: brush(BG_B2, 1.0)?,
            panel: brush(BG_PANEL, 1.0)?,
            box_bg: brush(BG_A3, 1.0)?,
            footer_bg: brush(BG_BODY, 1.0)?,
            heading: brush(FG_HEADING, 1.0)?,
            label: brush(FG_LABEL, 1.0)?,
            row: brush(FG_A2, 1.0)?,
            dim: brush(FG_DIM, 1.0)?,
            knob: brush(KNOB, 1.0)?,
            toggle_text_bg: brush(TOGGLE_TEXT_BG, 1.0)?,
            field_line: brush(FIELD_LINE, 1.0)?,
            divider: brush(DIVIDER_LINE, 1.0)?,
            line: brush(LINE, 1.0)?,
            thumb: brush(THUMB, 1.0)?,
            tick: brush(BG_A3, 1.0)?,
            tick_on: brush(TICK_ON, 1.0)?,
            button_bg: brush(ACCENT, 0.06)?,
            list_bg: brush(LIST_BG, 1.0)?,
            list_line: brush(LIST_LINE, 1.0)?,
            hover: brush(0xFFFFFF, 0.04)?,
            shadow: brush(0x000000, 0.07)?,
            swatch: brush(0xFFFFFF, 1.0)?,
            veil: brush(BG_BODY_PANEL, 1.0 - DIM)?,
            tab: format(12.0, regular, lead)?,
            text: format(14.0, regular, lead)?,
            centred: format(14.0, regular, centre)?,
            segment: format(13.0, regular, centre)?,
            heading_font: format(16.0, DWRITE_FONT_WEIGHT_MEDIUM, lead)?,
            small_l: format(11.0, regular, lead)?,
            small_c: format(11.0, regular, centre)?,
            small_r: format(11.0, regular, trail)?,
            check: svg(ctx, include_bytes!("../../assets/icons/check.svg"))?,
            chevron_down: svg(ctx, include_bytes!("../../assets/icons/chevron_down.svg"))?,
            chevron_up: svg(ctx, include_bytes!("../../assets/icons/chevron_up.svg"))?,
            chevron_right: svg(ctx, include_bytes!("../../assets/icons/chevron_right.svg"))?,
            pencil: tint(include_bytes!("../../assets/icons/pencil.svg"), FG_DIM)?,
            refresh: tint(include_bytes!("../../assets/icons/refresh.svg"), FG_DIM)?,
            plus: tint(include_bytes!("../../assets/icons/nav_add.svg"), FG_DIM)?,
            gear: tint(
                include_bytes!("../../assets/icons/settings_gear.svg"),
                FG_DIM,
            )?,
            tab_icons: [
                tab(include_bytes!("../../assets/icons/tab_playback.svg"))?,
                tab(include_bytes!("../../assets/icons/tab_video.svg"))?,
                tab(include_bytes!("../../assets/icons/audio.svg"))?,
                tab(include_bytes!("../../assets/icons/subtitles.svg"))?,
            ],
            audio_on: tint(include_bytes!("../../assets/icons/audio.svg"), ACCENT)?,
            subtitles_on: svg(ctx, include_bytes!("../../assets/icons/subtitles_on.svg"))?,
            settings_on: tint(
                include_bytes!("../../assets/icons/playback_settings.svg"),
                ACCENT,
            )?,
        })
    }
}

impl Painter {
    pub(super) unsafe fn put(
        &self,
        rt: &ID2D1RenderTarget,
        text: &str,
        format: &IDWriteTextFormat,
        r: Rect,
        brush: &ID2D1SolidColorBrush,
    ) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            rt.DrawText(
                &utf16,
                format,
                &r.d2d(),
                brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        };
    }

    /// Draws a 24-DIP icon scaled to `size`, centred on (cx, cy).
    pub(super) unsafe fn glyph(
        &self,
        ctx: &ID2D1DeviceContext5,
        svg: &Svg,
        cx: f32,
        cy: f32,
        size: f32,
    ) {
        let s = size / svg.width;
        let m = Matrix3x2 {
            M11: s,
            M12: 0.0,
            M21: 0.0,
            M22: s,
            M31: cx - size / 2.0,
            M32: cy - size / 2.0,
        };
        unsafe {
            ctx.SetTransform(&m);
            ctx.DrawSvgDocument(&svg.doc);
            ctx.SetTransform(&Matrix3x2::identity());
        }
    }

    /// The open popup. Runs after the control bar, so it covers the video and nothing else.
    pub(super) unsafe fn panel(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        d: &PanelData,
    ) {
        let (g, p) = (&s.layout.panels, &self.panels);
        let Some(panel) = g.panel else { return };
        let f = g.frame;
        unsafe {
            // A soft edge, so the panel holds on bright video.
            for spread in [8.0, 6.0, 4.0, 2.0] {
                rt.FillRoundedRectangle(&rounded(f.inset(-spread), RADIUS + spread), &p.shadow);
            }
            rt.FillRoundedRectangle(&rounded(f, RADIUS), &p.body);
            // The header and the settings footer follow the panel's rounded corners.
            rt.PushAxisAlignedClip(&g.header.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            rt.FillRoundedRectangle(&rounded(f, RADIUS), &p.header);
            rt.PopAxisAlignedClip();
            if let Panel::Settings(_) = panel {
                let foot = Rect::new(f.l, g.footer_top, f.r, f.b);
                rt.PushAxisAlignedClip(&foot.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
                rt.FillRoundedRectangle(&rounded(f, RADIUS), &p.footer_bg);
                rt.PopAxisAlignedClip();
                let line = Rect::new(f.l, g.footer_top, f.r, g.footer_top + 1.0);
                rt.FillRectangle(&line.d2d(), &p.line);
            }
            self.tabs(rt, ctx, s, panel);

            rt.PushAxisAlignedClip(&g.content.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            for row in g.rows.iter().filter(|r| r.r.intersects(&g.content)) {
                self.row(rt, ctx, s, d, row);
            }
            rt.PopAxisAlignedClip();
            for row in &g.footer {
                self.row(rt, ctx, s, d, row);
            }
            if let Some(list) = &g.list {
                self.list(rt, ctx, s, list);
            }
        }
    }

    unsafe fn tabs(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        panel: Panel,
    ) {
        let (g, p) = (&s.layout.panels, &self.panels);
        let names: &[&str] = match panel {
            Panel::Tracks(_) => &["Video", "Audio", "Subtitles"],
            Panel::Settings(_) => &["Playback", "Video", "Audio", "Subtitles"],
        };
        // Tracks has no Playback tab, so its icons start one later.
        let first_icon = match panel {
            Panel::Tracks(_) => 1,
            Panel::Settings(_) => 0,
        };
        let active = panel.tab();
        for (i, (name, r)) in names.iter().zip(&g.tabs).enumerate() {
            let on = i == active;
            let hovered = s.hover == Some(Hit::Popup(Target::Tab(i as u8)));
            let text_w = self.measure(name, &p.tab);
            let content = 20.0 + 7.0 + text_w;
            let cx = (r.l + r.r) / 2.0;
            let x = cx - content / 2.0;
            let cy = r.t + 25.0;
            let (off, lit) = &p.tab_icons[i + first_icon];
            unsafe {
                self.glyph(ctx, if on { lit } else { off }, x + 10.0, cy, 20.0);
                let brush = if on || hovered { &p.row } else { &p.dim };
                let tr = Rect::new(x + 27.0, cy - 10.0, x + 27.0 + text_w + 2.0, cy + 10.0);
                self.put(rt, name, &p.tab, tr, brush);
                if on {
                    let w = content + 12.0;
                    let line = Rect::new(cx - w / 2.0, r.b - 2.0, cx + w / 2.0, r.b);
                    rt.FillRoundedRectangle(&rounded(line, 1.0), &self.accent);
                }
            }
        }
    }

    unsafe fn row(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        d: &PanelData,
        row: &Row,
    ) {
        let p = &self.panels;
        let r = row.r;
        let (cx, cy) = ((r.l + r.r) / 2.0, (r.t + r.b) / 2.0);
        let hovered = row.click
            && row
                .ctl
                .is_some_and(|c| s.hover == Some(Hit::Popup(Target::Ctl(c))));
        let value_text = |v: Value| match v {
            Value::Fixed(t) => t.to_owned(),
            Value::Live => row.ctl.map(|c| c.text(d)).unwrap_or_default(),
        };
        unsafe {
            match &row.kind {
                Kind::Heading(text) => self.put(rt, text, &p.heading_font, r, &p.heading),
                Kind::Label(text) => self.put(rt, text, &p.text, r, &p.dim),
                Kind::Choice(text) => {
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(r, 6.0), &p.hover);
                    }
                    if row.ctl.is_some_and(|c| c.selected(d)) {
                        self.glyph(ctx, &p.check, r.l + 23.0, cy, 24.0);
                    }
                    let tr = Rect::new(r.l + 49.0, r.t, r.r - 12.0, r.b);
                    self.put(rt, text, &p.text, tr, &p.label);
                }
                Kind::Divider => rt.FillRectangle(&r.d2d(), &p.divider),
                Kind::Toggle => {
                    let on = row.ctl.is_some_and(|c| c.toggled(d));
                    rt.FillRoundedRectangle(&rounded(r, 6.0), &p.header);
                    let inner = r.inset(2.0);
                    let (knob, label) = if on {
                        (
                            Rect::new(inner.r - 28.0, inner.t, inner.r, inner.b),
                            Rect::new(inner.l, inner.t, inner.r - 29.0, inner.b),
                        )
                    } else {
                        (
                            Rect::new(inner.l, inner.t, inner.l + 28.0, inner.b),
                            Rect::new(inner.l + 29.0, inner.t, inner.r, inner.b),
                        )
                    };
                    let (bg, fg) = if on {
                        (&self.played, &self.accent)
                    } else {
                        (&p.toggle_text_bg, &p.dim)
                    };
                    rt.FillRoundedRectangle(&rounded(label, 4.0), bg);
                    let knob_brush = if on { &self.accent } else { &p.knob };
                    rt.FillRoundedRectangle(&rounded(knob, 4.0), knob_brush);
                    self.put(rt, if on { "ON" } else { "OFF" }, &p.small_c, label, fg);
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(r, 6.0), &p.hover);
                    }
                }
                Kind::Dropdown { value, up } => {
                    let open = row.ctl.is_some() && row.ctl == s.layout.panels.list_ctl();
                    let well = if *up { &p.header } else { &p.panel };
                    rt.FillRoundedRectangle(&rounded(r, 6.0), well);
                    if !*up {
                        rt.DrawRoundedRectangle(&rounded(r.inset(0.5), 6.0), &p.header, 1.0, None);
                    }
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(r, 6.0), &p.hover);
                    }
                    let tr = Rect::new(r.l + 16.0, r.t, r.r - 44.0, r.b);
                    self.put(rt, &value_text(*value), &p.text, tr, &p.row);
                    let chevron = if *up || open {
                        &p.chevron_up
                    } else {
                        &p.chevron_down
                    };
                    self.glyph(ctx, chevron, r.r - 26.0, cy, 24.0);
                }
                Kind::Field { value, swatch } => {
                    // The design draws the speed readout quieter: no outline, dim text.
                    let quiet = row.ctl == Some(Ctl::Speed);
                    rt.FillRoundedRectangle(&rounded(r, 6.0), &p.header);
                    if !quiet {
                        let outline = rounded(r.inset(0.5), 6.0);
                        rt.DrawRoundedRectangle(&outline, &p.field_line, 1.0, None);
                    }
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(r, 6.0), &p.hover);
                    }
                    let tr = Rect::new(r.l + 13.0, r.t, r.r - 10.0, r.b);
                    let fg = if quiet { &p.dim } else { &p.row };
                    self.put(rt, &value_text(*value), &p.text, tr, fg);
                    if let Some(rgb) = swatch {
                        let sw = Rect::around(r.r - 17.0, cy, 18.0);
                        p.swatch.SetColor(&color(theme::fixed(*rgb), 1.0));
                        rt.FillRoundedRectangle(&rounded(sw, 2.0), &p.swatch);
                        rt.DrawRoundedRectangle(&rounded(sw.inset(0.5), 2.0), &p.dim, 1.0, None);
                    }
                }
                Kind::Segments => rt.FillRoundedRectangle(&rounded(r, 6.0), &p.header),
                Kind::Segment(text) => {
                    let on = row.ctl.is_some_and(|c| c.selected(d));
                    let bg = if on { &self.played } else { &p.panel };
                    rt.FillRoundedRectangle(&rounded(r, 5.0), bg);
                    if hovered && !on {
                        rt.FillRoundedRectangle(&rounded(r, 5.0), &p.hover);
                    }
                    let fg = if on { &self.accent } else { &p.dim };
                    self.put(rt, text, &p.segment, r, fg);
                }
                Kind::Slider { ticks, centre } => {
                    self.slider(rt, s, d, row, *ticks, *centre);
                }
                Kind::TickLabels(labels) => {
                    let n = labels.len();
                    for (i, text) in labels.iter().enumerate() {
                        let x = r.l + 4.0 + (r.w() - 8.0) * i as f32 / (n - 1) as f32;
                        let (format, tr) = if i == 0 {
                            (&p.small_l, Rect::new(r.l, r.t, r.l + 60.0, r.b))
                        } else if i + 1 == n {
                            (&p.small_r, Rect::new(r.r - 60.0, r.t, r.r, r.b))
                        } else {
                            (&p.small_c, Rect::new(x - 30.0, r.t, x + 30.0, r.b))
                        };
                        self.put(rt, text, format, tr, &p.dim);
                    }
                }
                Kind::Expand(open) => {
                    if !open {
                        rt.FillRoundedRectangle(&rounded(r, 6.0), &p.header);
                    }
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(r, 6.0), &p.hover);
                    }
                    let text = if *open {
                        "Collapse Settings"
                    } else {
                        "Expand Settings"
                    };
                    let tr = Rect::new(r.l + 16.0, r.t, r.r - 44.0, r.b);
                    self.put(rt, text, &p.text, tr, &p.dim);
                    let chevron = if *open {
                        &p.chevron_down
                    } else {
                        &p.chevron_right
                    };
                    self.glyph(ctx, chevron, r.r - 21.0, cy, 24.0);
                }
                Kind::Box => rt.FillRoundedRectangle(&rounded(r, 6.0), &p.box_bg),
                Kind::Button(text) => {
                    rt.FillRoundedRectangle(&rounded(r, 6.0), &p.button_bg);
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(r, 6.0), &p.hover);
                    }
                    self.put(rt, text, &p.centred, r, &self.accent);
                }
                Kind::Icon(glyph) => {
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(r, 4.0), &self.hover);
                    }
                    let svg = match glyph {
                        Glyph::Refresh => &p.refresh,
                        Glyph::Pencil => &p.pencil,
                        Glyph::Plus => &p.plus,
                    };
                    self.glyph(ctx, svg, cx, cy, 24.0);
                }
                Kind::Footer(text) => {
                    rt.FillRoundedRectangle(&rounded(r, 6.0), &p.header);
                    if hovered {
                        rt.FillRoundedRectangle(&rounded(r, 6.0), &p.hover);
                    }
                    self.put(rt, text, &p.centred, r, &p.dim);
                }
                Kind::Link(text) => {
                    let w = self.measure(text, &p.text);
                    let x = cx - (w + 26.0) / 2.0;
                    self.glyph(ctx, &p.gear, x + 9.0, cy, 20.0);
                    let tr = Rect::new(x + 26.0, r.t, x + 26.0 + w + 2.0, r.b);
                    let fg = if hovered { &p.label } else { &p.dim };
                    self.put(rt, text, &p.text, tr, fg);
                }
            }
            // Dimmed: the panel's own background over it at 1 - DIM, which looks like the
            // row drawn at DIM opacity without the cost of a layer.
            if row.dim {
                rt.FillRectangle(&r.inset(-1.0).d2d(), &p.veil);
            }
        }
    }

    unsafe fn slider(
        &self,
        rt: &ID2D1RenderTarget,
        s: &Scene,
        d: &PanelData,
        row: &Row,
        ticks: u8,
        centre: bool,
    ) {
        let p = &self.panels;
        let r = row.r;
        let cy = (r.t + r.b) / 2.0;
        let (min, max, _) = row.ctl.and_then(Ctl::range).unwrap_or((0.0, 1.0, 0.0));
        let v = row.ctl.map_or(min, |c| c.value(d));
        let f = ((v - min) / (max - min)).clamp(0.0, 1.0);
        let at = |f: f32| r.l + 4.0 + (r.w() - 8.0) * f;
        let x = at(f);
        unsafe {
            rt.FillRoundedRectangle(&rounded(r, 4.0), &p.header);
            let from = if centre { at(0.5) } else { r.l };
            let fill = Rect::new(from.min(x), r.t, from.max(x), r.b);
            if fill.w() > 0.5 {
                rt.FillRoundedRectangle(&rounded(fill, 4.0), &self.played);
            }
            for i in 1..=ticks {
                let tx = at(f32::from(i) / f32::from(ticks + 1));
                let lit = tx >= fill.l && tx <= fill.r;
                let tick = Rect::new(tx - 0.5, cy - 3.0, tx + 0.5, cy + 3.0);
                rt.FillRectangle(&tick.d2d(), if lit { &p.tick_on } else { &p.tick });
            }
            let thumb = Rect::new(x - 4.0, cy - 12.0, x + 4.0, cy + 12.0);
            rt.FillRoundedRectangle(&rounded(thumb, 3.0), &p.thumb);
            let dragging = row
                .ctl
                .is_some_and(|c| s.hover == Some(Hit::Popup(Target::Ctl(c))));
            if dragging {
                rt.FillRoundedRectangle(&rounded(thumb, 3.0), &p.hover);
            }
        }
    }

    unsafe fn list(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
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
                if s.hover == Some(Hit::Popup(Target::Choice(i))) {
                    rt.FillRoundedRectangle(&rounded(*r, 4.0), &self.hover);
                }
                let tr = Rect::new(r.l + 12.0, r.t, r.r - 36.0, r.b);
                let brush = if current { &p.label } else { &p.row };
                self.put(rt, text, &p.text, tr, brush);
                if current {
                    self.glyph(ctx, &p.check, r.r - 20.0, (r.t + r.b) / 2.0, 22.0);
                }
            }
            rt.PopAxisAlignedClip();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> PanelData {
        PanelData {
            audio_tracks: vec!["English".into(), "Japanese".into()],
            audio_selected: Some(1),
            devices: vec!["Speakers".into(), "Headphones".into()],
            speed: 1.0,
            sub_size: 1.0,
            ..Default::default()
        }
    }

    fn open(panel: Panel) -> Layout {
        let mut l = Layout::new(1920.0, 1080.0, true, false);
        let state = State {
            open: Some(panel),
            ..Default::default()
        };
        l.add_panel(&state, &data());
        l
    }

    #[test]
    fn panels_match_the_design_at_1920() {
        let l = open(Panel::Tracks(TrackTab::Audio));
        let g = &l.panels;
        assert_eq!(g.frame, Rect::new(1500.0, 484.0, 1900.0, 1006.0));
        assert_eq!(g.tabs[1], Rect::new(1644.0, 484.0, 1756.0, 534.0));
        // None, English, Japanese, then the devices under "Output Device".
        let at = |x: f32, y: f32| l.hit(x, y);
        let row = |cy: f32| at(1700.0, 484.0 + cy);
        assert_eq!(
            row(127.5),
            Some(Hit::Popup(Target::Ctl(Ctl::Track(TrackTab::Audio, None))))
        );
        assert_eq!(
            row(215.5),
            Some(Hit::Popup(Target::Ctl(Ctl::Track(
                TrackTab::Audio,
                Some(1)
            ))))
        );
        assert_eq!(row(329.5), Some(Hit::Popup(Target::Ctl(Ctl::Device(0)))));
        assert_eq!(
            row(488.0),
            Some(Hit::Popup(Target::Ctl(Ctl::ShowPreferences)))
        );
        assert_eq!(
            at(1500.0 + 359.0, 484.0 + 282.0),
            Some(Hit::Popup(Target::Ctl(Ctl::RefreshDevices)))
        );
        // Outside the panel the video still takes clicks.
        assert_eq!(at(1000.0, 500.0), Some(Hit::Video));

        let l = open(Panel::Settings(SettingsTab::Playback));
        let g = &l.panels;
        assert_eq!(g.frame, Rect::new(1500.0, 61.0, 1900.0, 1006.0));
        assert_eq!(g.content, Rect::new(1500.0, 111.0, 1900.0, 881.0));
        assert_eq!(g.max_scroll, 0.0);
        let speed = g.slider_value(Ctl::Speed, 1500.0 + 200.0).unwrap();
        assert!((speed - 1.125).abs() < 1e-3, "{speed}");
        assert_eq!(
            l.hit(1500.0 + 340.0, 61.0 + 139.5),
            Some(Hit::Popup(Target::Ctl(Ctl::ShowStatistics)))
        );
        // Disabled rows swallow clicks without acting.
        assert_eq!(
            l.hit(1500.0 + 340.0, 61.0 + 87.5),
            Some(Hit::Popup(Target::Body))
        );
    }

    #[test]
    fn video_settings_scroll_and_lists_open_below() {
        let mut l = Layout::new(1920.0, 1080.0, true, false);
        let mut state = State {
            open: Some(Panel::Settings(SettingsTab::Video)),
            essential: true,
            list: Some(Ctl::Zoom),
            ..Default::default()
        };
        l.add_panel(&state, &data());
        assert!(l.panels.max_scroll > 300.0);
        let list = l.panels.list.as_ref().unwrap();
        assert_eq!(list.frame.t, 61.0 + 309.0 + 23.0 + 4.0);
        assert_eq!(list.rows.len(), ZOOM.len());
        // Scrolling past the end clamps to it.
        state.scroll = 10_000.0;
        state.list = None;
        l.add_panel(&state, &data());
        let last = l.panels.rows.last().unwrap();
        assert!(last.r.b <= l.panels.content.b + 1.0);
    }
}

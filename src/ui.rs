//! The interface from Juxtopposed's "VLC Redesign" (Figma community file, CC BY 4.0), drawn with
//! Direct2D and DirectWrite on top of the video: the player page (title bar, control bar and
//! hide-UI mode) here, the library page in `browse`, the tracks and settings popups in `panels`.
//! Sizes are DIPs measured from the design's 1920x1080 frames.

mod browse;
pub mod dialog;
pub mod panels;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub use browse::Browse;
pub use dialog::{Dialog, DialogAction};
pub use panels::{Panel, PanelAction, PanelData, TrackTab};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use crate::library::Section;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::{Error, Result, w};
use windows_numerics::Matrix3x2;

const TITLE_BAR: f32 = 40.0;
const CONTROL_BAR: f32 = 54.0;
/// Gap between the window edge and the rounded video area.
const FRAME: f32 = 10.0;
const VIDEO_RADIUS: f32 = 10.0;
/// While playing, the bars hide after this long without mouse movement.
const HIDE_AFTER: Duration = Duration::from_secs(3);
/// After a seek-bar drag, keep showing the dropped position while the seek lands.
const SEEK_SETTLE: Duration = Duration::from_millis(600);

// Dark theme colour tokens from the design file.
const BG_A3: u32 = 0x1B1B1B;
const BG_B2: u32 = 0x090909;
const BG_ACCENT_BRIGHT: u32 = 0x371F06;
const FG_A2: u32 = 0x8F8F8F;
const ACCENT: u32 = 0xD37918;
// Sampled from the library frames.
const BG_BODY: u32 = 0x0E0E0E;
const BG_PANEL: u32 = 0x191919;
const BG_SEARCH: u32 = 0x151515;
const LINE: u32 = 0x1D1D1D;
const FG_LABEL: u32 = 0x969696;
const FG_DIM: u32 = 0x6B6B6B;
const FG_FAINT: u32 = 0x404040;
const FG_DISABLED: u32 = 0x353535;

/// Volume level against the width of the volume wedge's fill, from the design's 0-200% variants.
const VOLUME_STEPS: [(f32, f32); 13] = [
    (0.0, 1.0),
    (0.1, 7.0),
    (0.2, 14.0),
    (0.3, 20.0),
    (0.4, 26.0),
    (0.5, 33.0),
    (0.6, 41.0),
    (0.7, 48.0),
    (0.8, 53.0),
    (0.9, 58.0),
    (1.0, 63.0),
    (1.5, 75.0),
    (2.0, 88.0),
];

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Rect {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Rect {
    pub const fn new(l: f32, t: f32, r: f32, b: f32) -> Self {
        Rect { l, t, r, b }
    }

    pub fn w(&self) -> f32 {
        self.r - self.l
    }

    pub fn h(&self) -> f32 {
        self.b - self.t
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }

    fn intersects(&self, o: &Rect) -> bool {
        self.l < o.r && o.l < self.r && self.t < o.b && o.t < self.b
    }

    fn inset(&self, d: f32) -> Rect {
        Rect::new(self.l + d, self.t + d, self.r - d, self.b - d)
    }

    /// A `size` square centred on (x, y).
    fn around(x: f32, y: f32, size: f32) -> Rect {
        Rect::new(
            x - size / 2.0,
            y - size / 2.0,
            x + size / 2.0,
            y + size / 2.0,
        )
    }

    pub fn scaled(&self, s: f32) -> Rect {
        Rect::new(self.l * s, self.t * s, self.r * s, self.b * s)
    }

    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F {
            left: self.l,
            top: self.t,
            right: self.r,
            bottom: self.b,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    More,
    Sidebar,
    Minimize,
    Maximize,
    Fullscreen,
    Close,
    Previous,
    Play,
    Next,
    Speaker,
    Audio,
    Subtitles,
    Settings,
    Playlist,
    Seek,
    Volume,
    /// Empty title bar: drags the window.
    Caption,
    Video,
    /// The folder breadcrumb in the player's title bar: opens that folder in the library.
    Folder,
    // Library page.
    Nav(Section),
    AddItem,
    Back,
    Forward,
    Crumb(usize),
    Sort,
    ViewMode,
    Search,
    AddHere,
    HeaderMore,
    /// An entry of `Browse::entries`.
    Entry(usize),
    /// The mini player: back to the player page.
    Card,
    /// The overlay panel's "open in library" button.
    OpenLibrary,
    /// The overlay panel outside its buttons and entries.
    Panel,
    /// The tracks or settings popup.
    Popup(panels::Target),
    /// The open dialog, which takes every click.
    Dialog(dialog::Target),
}

impl Hit {
    fn in_bars(self) -> bool {
        self != Hit::Video
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Page {
    #[default]
    Library,
    Player,
}

/// Where everything sits for one window size.
#[derive(Clone, Default)]
pub struct Layout {
    pub page: Page,
    pub bars: bool,
    pub width: f32,
    pub height: f32,
    pub video: Rect,
    buttons: Vec<(Hit, Rect)>,
    track: Rect,
    current: Rect,
    duration: Rect,
    volume: Rect,
    browse: browse::Geometry,
    panels: panels::Geometry,
    dialog: Option<dialog::Geometry>,
}

impl Layout {
    pub fn new(width: f32, height: f32, bars: bool, fullscreen: bool) -> Self {
        if !bars {
            let inset = if fullscreen { 0.0 } else { FRAME };
            let video = Rect::new(inset, inset, width - inset, height - inset);
            return Layout {
                page: Page::Player,
                bars,
                width,
                height,
                video,
                ..Default::default()
            };
        }
        let (w, cy) = (width, height - CONTROL_BAR / 2.0);
        let top = |x: f32, size: f32| Rect::new(x, 6.0, x + size, 34.0);
        let icon = |x: f32| Rect::new(x, cy - 16.0, x + 30.0, cy + 16.0);
        let buttons = vec![
            (Hit::More, top(51.0, 28.0)),
            (Hit::Sidebar, top(89.0, 28.0)),
            (Hit::Minimize, top(w - 150.0, 36.0)),
            (Hit::Maximize, top(w - 114.0, 36.0)),
            (Hit::Fullscreen, top(w - 78.0, 36.0)),
            (Hit::Close, top(w - 42.0, 36.0)),
            (Hit::Previous, icon(20.0)),
            (Hit::Play, Rect::new(58.0, cy - 14.0, 112.0, cy + 14.0)),
            (Hit::Next, icon(120.0)),
            (
                Hit::Speaker,
                Rect::new(w - 324.0, cy - 12.0, w - 300.0, cy + 12.0),
            ),
            (Hit::Audio, icon(w - 184.0)),
            (Hit::Subtitles, icon(w - 142.0)),
            (Hit::Settings, icon(w - 100.0)),
            (Hit::Playlist, icon(w - 50.0)),
            (Hit::Folder, Rect::new(139.0, 6.0, 300.0, 34.0)),
        ];
        Layout {
            page: Page::Player,
            bars,
            width,
            height,
            video: Rect::new(FRAME, TITLE_BAR + 1.0, w - FRAME, height - CONTROL_BAR),
            buttons,
            current: Rect::new(170.0, cy - 14.0, 246.0, cy + 14.0),
            track: Rect::new(254.0, cy - 14.0, w - 428.0, cy + 14.0),
            duration: Rect::new(w - 420.0, cy - 14.0, w - 344.0, cy + 14.0),
            volume: Rect::new(w - 292.0, cy - 14.0, w - 204.0, cy + 14.0),
            browse: Default::default(),
            panels: Default::default(),
            dialog: None,
        }
    }

    pub fn has_overlay(&self) -> bool {
        self.browse.overlay.is_some()
    }

    /// The title bar's window buttons, shared by both pages.
    fn window_buttons(w: f32) -> [(Hit, Rect); 5] {
        let top = |x: f32, size: f32| Rect::new(x, 6.0, x + size, 34.0);
        [
            (Hit::More, top(51.0, 28.0)),
            (Hit::Minimize, top(w - 150.0, 36.0)),
            (Hit::Maximize, top(w - 114.0, 36.0)),
            (Hit::Fullscreen, top(w - 78.0, 36.0)),
            (Hit::Close, top(w - 42.0, 36.0)),
        ]
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if let Some(d) = &self.dialog {
            return Some(Hit::Dialog(d.hit(x, y)));
        }
        if self.page == Page::Library {
            return self.browse_hit(x, y);
        }
        if let Some(target) = self.panels.hit(x, y) {
            return Some(Hit::Popup(target));
        }
        if let Some(hit) = self.overlay_hit(x, y) {
            return hit;
        }
        if let Some((hit, _)) = self.buttons.iter().find(|(_, r)| r.contains(x, y)) {
            return Some(*hit);
        }
        if self.bars && self.track.contains(x, y) {
            Some(Hit::Seek)
        } else if self.bars && self.volume.contains(x, y) {
            Some(Hit::Volume)
        } else if self.video.contains(x, y) {
            Some(Hit::Video)
        } else if self.bars && y < TITLE_BAR {
            Some(Hit::Caption)
        } else {
            None
        }
    }

    fn button(&self, hit: Hit) -> Rect {
        self.buttons
            .iter()
            .find(|(h, _)| *h == hit)
            .map(|(_, r)| *r)
            .unwrap_or_default()
    }

    fn seek_fraction(&self, x: f32) -> f64 {
        f64::from(((x - self.track.l - 2.0) / (self.track.w() - 4.0)).clamp(0.0, 1.0))
    }
}

pub enum Action {
    Click(Hit),
    /// Fraction of the duration.
    SeekTo(f64),
    SetVolume(f32),
    Panel(PanelAction),
    Dialog(DialogAction),
}

/// Pointer state. Lives on the UI thread; the window procedure feeds it and the render loop
/// reads it.
pub struct Ui {
    pub layout: Layout,
    pub hover: Option<Hit>,
    pressed: Option<Hit>,
    drag: Option<Hit>,
    seek_preview: Option<(f64, Instant)>,
    last_activity: Instant,
    pointer: (f32, f32),
    /// Something changed that needs a repaint even without a new video frame.
    pub dirty: bool,
    /// Something changed that moves things around: rebuild the layout before painting.
    pub relayout: bool,
    pub page: Page,
    pub browse: Browse,
    /// The player's sidebar panel is open.
    pub overlay: bool,
    /// The tracks or settings popup.
    pub panels: panels::State,
    /// The last slider pressed and when, to spot a double-click that resets it.
    last_slider: Option<(panels::Ctl, Instant)>,
    /// The open dialog, if any.
    pub dialog: Option<dialog::State>,
}

/// Two presses on a slider this close together reset it.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

impl Ui {
    pub fn new() -> Self {
        Ui {
            layout: Layout::default(),
            hover: None,
            pressed: None,
            drag: None,
            seek_preview: None,
            last_activity: Instant::now(),
            pointer: (f32::NAN, f32::NAN),
            dirty: true,
            relayout: true,
            page: Page::Library,
            browse: Browse::new(),
            overlay: false,
            panels: panels::State::default(),
            last_slider: None,
            dialog: None,
        }
    }

    /// Switches pages; the pointer may now be over something else.
    pub fn show(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.panels.show(None, None);
            self.hover = None;
            self.relayout = true;
            self.wake();
        }
    }

    /// Scrolls the library by `dy` DIPs; the layout clamps it.
    pub fn scroll(&mut self, dy: f32) {
        self.browse.scroll = (self.browse.scroll + dy).clamp(0.0, self.layout.browse.max_scroll);
        self.relayout = true;
    }

    /// Whether the pointer is over the player's overlay panel.
    pub fn over_overlay(&self) -> bool {
        let (x, y) = self.pointer;
        self.layout.browse.overlay.is_some_and(|p| p.contains(x, y))
    }

    /// Whether (x, y) is over the tracks or settings popup, or its open choice list. The wheel
    /// passes its own position: the pointer may never have moved over the window.
    pub fn over_panel(&self, x: f32, y: f32) -> bool {
        self.layout.panels.hit(x, y).is_some()
    }

    /// Scrolls the popup, or its open choice list when (x, y) is over it, by `dy` DIPs.
    pub fn scroll_panel(&mut self, x: f32, y: f32, dy: f32) {
        let g = &self.layout.panels;
        let p = &mut self.panels;
        if g.over_list(x, y) {
            p.list_scroll = (p.list_scroll + dy).clamp(0.0, g.list_max_scroll());
        } else if !g.list_open() {
            p.scroll = (p.scroll + dy).clamp(0.0, g.max_scroll);
        }
        self.relayout = true;
    }

    /// Esc: closes the open choice list, else the popup. False when neither was open.
    pub fn close_panel(&mut self) -> bool {
        if self.panels.list.take().is_some() {
            self.relayout = true;
            return true;
        }
        if self.panels.open.is_none() {
            return false;
        }
        self.panels.show(None, None);
        self.relayout = true;
        true
    }

    /// Whether the popup's layout is out of date: another panel or tab, or other data.
    pub fn panel_stale(&self, data: &PanelData) -> bool {
        let g = &self.layout.panels;
        self.panels.open != g.panel || (g.panel.is_some() && g.data() != Some(data))
    }

    /// What a click on the popup does. Choosing, expanding and switching happen here; changes
    /// to playback go out as actions.
    fn panel_click(&mut self, target: panels::Target) -> Option<Action> {
        use panels::{Ctl, Target};
        let p = &mut self.panels;
        self.relayout = true;
        match target {
            Target::Body => None,
            Target::Tab(i) => {
                p.switch_tab(usize::from(i));
                None
            }
            Target::Choice(i) => {
                let ctl = p.list.take()?;
                ctl.pick(i).map(Action::Panel)
            }
            Target::Ctl(Ctl::Essential) => {
                p.essential = !p.essential;
                None
            }
            Target::Ctl(Ctl::Equalizer) => {
                p.equalizer = !p.equalizer;
                None
            }
            Target::Ctl(Ctl::ShowPreferences) => {
                let tab = self.layout.panels.preferences_tab();
                p.show(Some(Panel::Settings(tab)), Some(Hit::Settings));
                None
            }
            Target::Ctl(ctl) if ctl.has_options() => {
                p.list = (p.list != Some(ctl)).then_some(ctl);
                p.list_scroll = 0.0;
                None
            }
            Target::Ctl(ctl) => ctl.click(self.layout.panels.data()?).map(Action::Panel),
        }
    }

    /// Re-runs hit testing at the last pointer position, after a relayout moved things under it.
    pub fn rehover(&mut self) {
        let (x, y) = self.pointer;
        let hover = if x.is_nan() {
            None
        } else {
            self.layout.hit(x, y)
        };
        if hover != self.hover {
            self.hover = hover;
            self.dirty = true;
        }
    }

    /// Bars stay up while paused, while the pointer is over them or moved recently, and while
    /// dragging; otherwise playback goes to the design's hide-UI mode.
    pub fn bars_visible(&self, paused: bool) -> bool {
        self.page == Page::Library
            || self.overlay
            || self.panels.open.is_some()
            || self.dialog.is_some()
            || paused
            || self.drag.is_some()
            || self.hover.is_some_and(Hit::in_bars)
            || self.last_activity.elapsed() < HIDE_AFTER
    }

    /// The seek position to draw instead of the clock while dragging or right after a drop.
    pub fn seek_preview(&self) -> Option<f64> {
        let (fraction, at) = self.seek_preview?;
        (self.drag == Some(Hit::Seek) || at.elapsed() < SEEK_SETTLE).then_some(fraction)
    }

    /// True once when a seek preview runs out, so a paused player repaints the real position.
    pub fn settle_seek_preview(&mut self) -> bool {
        let expired = self.seek_preview.is_some() && self.seek_preview().is_none();
        if expired {
            self.seek_preview = None;
        }
        expired
    }

    pub fn on_move(&mut self, x: f32, y: f32) -> Option<Action> {
        // Windows also sends WM_MOUSEMOVE when the cursor is hidden or the window changes under
        // a still pointer. Only real movement counts as activity, or the bars would never hide.
        if (x, y) != self.pointer {
            self.pointer = (x, y);
            self.last_activity = Instant::now();
        }
        let hover = self.layout.hit(x, y);
        if hover != self.hover {
            self.hover = hover;
            self.dirty = true;
        }
        match self.drag {
            Some(Hit::Seek) => {
                self.seek_preview = Some((self.layout.seek_fraction(x), Instant::now()));
                self.dirty = true;
                None
            }
            Some(Hit::Volume) => Some(Action::SetVolume(volume_for_width(
                x - self.layout.volume.l,
            ))),
            Some(Hit::Popup(panels::Target::Ctl(ctl))) => self.slide(ctl, x),
            Some(Hit::Dialog(_)) => self.dialog_drag(x),
            _ => None,
        }
    }

    /// The action for a popup slider with the pointer at `x`.
    fn slide(&self, ctl: panels::Ctl, x: f32) -> Option<Action> {
        let v = self.layout.panels.slider_value(ctl, x)?;
        ctl.set(v).map(Action::Panel)
    }

    pub fn on_down(&mut self, x: f32, y: f32) -> Option<Action> {
        self.on_move(x, y);
        self.pressed = self.hover;
        match self.hover {
            Some(Hit::Seek) => {
                self.drag = Some(Hit::Seek);
                self.seek_preview = Some((self.layout.seek_fraction(x), Instant::now()));
                None
            }
            Some(Hit::Volume) => {
                self.drag = Some(Hit::Volume);
                Some(Action::SetVolume(volume_for_width(
                    x - self.layout.volume.l,
                )))
            }
            // A slider follows the pointer from the press; a quick second press resets it.
            Some(hit @ Hit::Popup(panels::Target::Ctl(ctl)))
                if ctl.is_slider() && !self.layout.panels.list_open() =>
            {
                let double = self
                    .last_slider
                    .is_some_and(|(c, at)| c == ctl && at.elapsed() < DOUBLE_CLICK);
                if double {
                    self.last_slider = None;
                    self.pressed = None;
                    return ctl.reset().map(Action::Panel);
                }
                self.last_slider = Some((ctl, Instant::now()));
                self.drag = Some(hit);
                self.slide(ctl, x)
            }
            Some(Hit::Dialog(target)) => self.dialog_down(target, x),
            _ => None,
        }
    }

    pub fn on_up(&mut self, x: f32, y: f32) -> Option<Action> {
        let hit = self.layout.hit(x, y);
        let pressed = self.pressed.take();
        self.dirty = true;
        match self.drag.take() {
            Some(Hit::Seek) => {
                let fraction = self.layout.seek_fraction(x);
                self.seek_preview = Some((fraction, Instant::now()));
                Some(Action::SeekTo(fraction))
            }
            Some(_) => None,
            // An open choice list takes the next click: a choice, or anywhere else to close it.
            None if self.panels.list.is_some()
                && !matches!(hit, Some(Hit::Popup(panels::Target::Choice(_)))) =>
            {
                self.panels.list = None;
                self.relayout = true;
                None
            }
            None if hit != pressed => None,
            None => match hit {
                Some(Hit::Caption | Hit::Seek | Hit::Volume | Hit::Panel) => None,
                Some(Hit::Popup(target)) => self.panel_click(target),
                Some(Hit::Dialog(target)) => self.dialog_click(target),
                Some(h @ (Hit::Audio | Hit::Subtitles | Hit::Settings)) => {
                    // Its own button closes a popup; another button switches to its popup.
                    let open = self.panels.opener != Some(h);
                    let panel = self.panels.for_button(h).filter(|_| open);
                    self.panels.show(panel, panel.map(|_| h));
                    self.relayout = true;
                    Some(Action::Click(h))
                }
                // A click on the video closes the popup.
                Some(Hit::Video) => {
                    if self.panels.open.is_some() {
                        self.panels.show(None, None);
                        self.relayout = true;
                    }
                    Some(Action::Click(Hit::Video))
                }
                Some(h) => Some(Action::Click(h)),
                None => None,
            },
        }
    }

    pub fn on_leave(&mut self) {
        self.pointer = (f32::NAN, f32::NAN);
        if self.hover.take().is_some() {
            self.dirty = true;
        }
    }

    pub fn wake(&mut self) {
        self.last_activity = Instant::now();
    }
}

/// Everything the painter needs for one frame.
pub struct Scene<'a> {
    pub layout: &'a Layout,
    pub hover: Option<Hit>,
    /// Seconds.
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub volume: f32,
    pub muted: bool,
    pub title: &'a str,
    pub folder: &'a str,
    /// The open file's name, for the mini player; empty when nothing is open.
    pub file: &'a str,
    /// An audio-only file: its cover (or a music glyph) goes where the video would.
    pub art: Option<&'a std::path::Path>,
    pub fullscreen: bool,
    pub has_frame: bool,
    pub browse: &'a Browse,
    /// What the tracks and settings popups show.
    pub panel: Option<&'a PanelData>,
    /// The open dialog's text, files and caret.
    pub dialog: Option<&'a dialog::State>,
    /// Subtitle cues to show now, and how.
    pub cues: &'a [crate::subs::Cue],
    pub caption_style: &'a crate::captions::CaptionStyle,
    /// Playback statistics, drawn over the video's top-left corner when turned on.
    pub stats: Option<&'a str>,
}

fn fill_width(volume: f32) -> f32 {
    let v = volume.clamp(0.0, 2.0);
    let i = VOLUME_STEPS
        .iter()
        .position(|&(level, _)| level >= v)
        .unwrap_or(VOLUME_STEPS.len() - 1);
    if i == 0 {
        return VOLUME_STEPS[0].1;
    }
    let ((v0, w0), (v1, w1)) = (VOLUME_STEPS[i - 1], VOLUME_STEPS[i]);
    w0 + (w1 - w0) * (v - v0) / (v1 - v0)
}

fn volume_for_width(width: f32) -> f32 {
    let wd = width.clamp(VOLUME_STEPS[0].1, 88.0);
    let i = VOLUME_STEPS
        .iter()
        .position(|&(_, w)| w >= wd)
        .unwrap_or(VOLUME_STEPS.len() - 1);
    if i == 0 {
        return 0.0;
    }
    let ((v0, w0), (v1, w1)) = (VOLUME_STEPS[i - 1], VOLUME_STEPS[i]);
    v0 + (v1 - v0) * (wd - w0) / (w1 - w0)
}

fn clock_text(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn color(rgb: u32, alpha: f32) -> D2D1_COLOR_F {
    let c = |shift: u32| ((rgb >> shift) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F {
        r: c(16),
        g: c(8),
        b: c(0),
        a: alpha,
    }
}

fn rounded(r: Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: r.d2d(),
        radiusX: radius,
        radiusY: radius,
    }
}

fn translate(x: f32, y: f32) -> Matrix3x2 {
    Matrix3x2 {
        M11: 1.0,
        M12: 0.0,
        M21: 0.0,
        M22: 1.0,
        M31: x,
        M32: y,
    }
}

struct Svg {
    doc: ID2D1SvgDocument,
    width: f32,
}

macro_rules! icons {
    ($($name:ident = $file:literal),* $(,)?) => {
        struct Icons { $($name: Svg),* }
        impl Icons {
            fn load(ctx: &ID2D1DeviceContext5) -> Result<Self> {
                Ok(Icons { $($name: svg(ctx, include_bytes!(concat!("../assets/icons/", $file)))?),* })
            }
        }
    };
}

icons! {
    logo = "logo.svg",
    more = "more.svg",
    sidebar = "sidebar.svg",
    divider = "divider.svg",
    folder = "folder.svg",
    minimize = "minimize.svg",
    maximize = "maximize.svg",
    fullscreen = "fullscreen.svg",
    fullscreen_exit = "fullscreen_exit.svg",
    close = "close.svg",
    play = "play.svg",
    pause = "pause.svg",
    previous = "previous.svg",
    next = "next.svg",
    speaker_high = "speaker.svg",
    speaker_medium = "speaker_medium.svg",
    speaker_low = "speaker_low.svg",
    speaker_muted = "speaker_muted.svg",
    audio = "audio.svg",
    subtitles = "subtitles.svg",
    settings = "playback_settings.svg",
    playlist = "playlist.svg",
    volume_track = "volume_track.svg",
    volume_fill = "volume_fill.svg",
    home = "nav_home.svg",
    videos = "nav_videos.svg",
    music = "nav_music.svg",
    network = "nav_network.svg",
    playlists = "nav_playlists.svg",
    recent = "nav_recent.svg",
    add = "nav_add.svg",
    back = "arrow_left.svg",
    forward = "arrow_right.svg",
    sort = "sort.svg",
    view_grid = "view_grid.svg",
    view_list = "view_list.svg",
    search = "search.svg",
    more_panel = "more_panel.svg",
}

/// The design's active state: the same glyph in the accent colour.
struct ActiveIcons {
    home: Svg,
    videos: Svg,
    music: Svg,
    network: Svg,
    playlists: Svg,
    recent: Svg,
}

impl ActiveIcons {
    fn load(ctx: &ID2D1DeviceContext5) -> Result<Self> {
        let tint = |bytes: &[u8]| {
            let text = String::from_utf8_lossy(bytes).replace("#8F8F8F", "#D37918");
            svg(ctx, text.as_bytes())
        };
        Ok(ActiveIcons {
            home: tint(include_bytes!("../assets/icons/nav_home.svg"))?,
            videos: tint(include_bytes!("../assets/icons/nav_videos.svg"))?,
            music: tint(include_bytes!("../assets/icons/nav_music.svg"))?,
            network: tint(include_bytes!("../assets/icons/nav_network.svg"))?,
            playlists: tint(include_bytes!("../assets/icons/nav_playlists.svg"))?,
            recent: tint(include_bytes!("../assets/icons/nav_recent.svg"))?,
        })
    }
}

/// Parses an icon exported from Figma. The viewport is the root element's width and height.
fn svg(ctx: &ID2D1DeviceContext5, bytes: &[u8]) -> Result<Svg> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::from(E_FAIL))?;
    let attr = |name: &str| -> f32 {
        let start = text
            .find(&format!(" {name}=\""))
            .map(|i| i + name.len() + 3)
            .unwrap_or(0);
        text[start..]
            .split('"')
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(24.0)
    };
    let (width, height) = (attr("width"), attr("height"));
    // Direct2D's SVG parser rejects the CSS that Figma adds for the web.
    let cleaned = text.replace(" style=\"display: block;\"", "");
    let stream =
        unsafe { SHCreateMemStream(Some(cleaned.as_bytes())) }.ok_or(Error::from(E_FAIL))?;
    let doc = unsafe { ctx.CreateSvgDocument(&stream, D2D_SIZE_F { width, height })? };
    Ok(Svg { doc, width })
}

/// Inter, embedded so the text matches the design without installing anything.
fn inter(dwrite: &IDWriteFactory6) -> Result<IDWriteFontCollection2> {
    const FONTS: [&[u8]; 3] = [
        include_bytes!("../assets/fonts/Inter-Regular.ttf"),
        include_bytes!("../assets/fonts/Inter-Medium.ttf"),
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    ];
    unsafe {
        let loader = dwrite.CreateInMemoryFontFileLoader()?;
        dwrite.RegisterFontFileLoader(&loader)?;
        let builder = dwrite.CreateFontSetBuilder()?;
        // The newest builder interface's AddFontFile takes a path; the in-memory file needs v1's.
        let builder1: &IDWriteFontSetBuilder1 = &builder;
        for data in FONTS {
            let file = loader.CreateInMemoryFontFileReference(
                dwrite,
                data.as_ptr().cast(),
                data.len() as u32,
                None,
            )?;
            builder1.AddFontFile(&file)?;
        }
        dwrite.CreateFontCollectionFromFontSet(
            &builder.CreateFontSet()?,
            DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC,
        )
    }
}

/// One line of vertically centred Inter, cut with an ellipsis when it doesn't fit.
fn text_format(
    dwrite: &IDWriteFactory6,
    fonts: &IDWriteFontCollection2,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    // IDWriteFactory6 overloads CreateTextFormat with font axes; the classic one is on IDWriteFactory.
    let dwrite: &IDWriteFactory = dwrite;
    unsafe {
        let format = dwrite.CreateTextFormat(
            w!("Inter"),
            fonts,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-us"),
        )?;
        format.SetTextAlignment(align)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        format.SetTrimming(&trimming, &dwrite.CreateEllipsisTrimmingSign(&format)?)?;
        Ok(format)
    }
}

/// Device-bound drawing resources, created once per Direct2D context.
pub struct Painter {
    factory: ID2D1Factory1,
    icons: Icons,
    bg: ID2D1SolidColorBrush,
    fg: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    track: ID2D1SolidColorBrush,
    played: ID2D1SolidColorBrush,
    hover: ID2D1SolidColorBrush,
    black: ID2D1SolidColorBrush,
    times: IDWriteTextFormat,
    title: IDWriteTextFormat,
    folder: IDWriteTextFormat,
    /// The window minus the rounded video area, rebuilt when either changes.
    frame: Option<((f32, f32, Rect), ID2D1PathGeometry1)>,
    dwrite: IDWriteFactory6,
    lib: browse::Resources,
    panels: panels::Resources,
    dialog: dialog::Resources,
    /// Thumbnails by file; None means the file has no picture.
    thumbs: HashMap<PathBuf, Option<ID2D1Bitmap>>,
    /// Visible files without a thumbnail yet, collected while painting.
    wanted: Vec<PathBuf>,
    captions: crate::captions::CaptionRenderer,
    stats_text: IDWriteTextFormat,
    stats_bg: ID2D1SolidColorBrush,
    /// Where the renderer put the picture this frame (after crop, zoom and rotation), in DIPs.
    pub picture: Option<Rect>,
}

impl Painter {
    pub fn new(ctx: &ID2D1DeviceContext5, factory: ID2D1Factory1) -> Result<Self> {
        let dwrite: IDWriteFactory6 = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let fonts = inter(&dwrite)?;
        let brush =
            |rgb: u32, alpha: f32| unsafe { ctx.CreateSolidColorBrush(&color(rgb, alpha), None) };
        let format = |size: f32, align| {
            text_format(&dwrite, &fonts, size, DWRITE_FONT_WEIGHT_REGULAR, align)
        };
        Ok(Painter {
            icons: Icons::load(ctx)?,
            bg: brush(BG_A3, 1.0)?,
            fg: brush(FG_A2, 1.0)?,
            accent: brush(ACCENT, 1.0)?,
            track: brush(BG_B2, 1.0)?,
            played: brush(BG_ACCENT_BRIGHT, 1.0)?,
            hover: brush(0xFFFFFF, 0.06)?,
            black: brush(0x000000, 1.0)?,
            times: format(15.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
            title: format(14.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
            folder: format(14.0, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            frame: None,
            lib: browse::Resources::new(ctx, &factory, &dwrite, &fonts)?,
            panels: panels::Resources::new(ctx, &dwrite, &fonts)?,
            dialog: dialog::Resources::new(ctx, &factory, &dwrite, &fonts)?,
            captions: crate::captions::CaptionRenderer::new(ctx, &dwrite, Some(&fonts))?,
            stats_text: {
                let f = text_format(
                    &dwrite,
                    &fonts,
                    13.0,
                    DWRITE_FONT_WEIGHT_REGULAR,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                )?;
                unsafe {
                    f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
                    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
                }
                f
            },
            stats_bg: unsafe { ctx.CreateSolidColorBrush(&color(0x000000, 0.6), None)? },
            picture: None,
            factory,
            dwrite,
            thumbs: HashMap::new(),
            wanted: Vec::new(),
        })
    }

    /// Width of `text` in DIPs when set in `format`.
    fn measure(&self, text: &str, format: &IDWriteTextFormat) -> f32 {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.dwrite
                .CreateTextLayout(&utf16, format, 10_000.0, 100.0)
                .and_then(|l| {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    l.GetMetrics(&mut m)
                        .map(|()| m.widthIncludingTrailingWhitespace)
                })
                .unwrap_or(0.0)
        }
    }

    /// Takes a finished thumbnail. BGRA rows, `width * 4` bytes each.
    pub fn add_thumbnail(
        &mut self,
        ctx: &ID2D1DeviceContext5,
        path: PathBuf,
        picture: Option<(u32, u32, &[u8])>,
    ) {
        let bitmap = picture.and_then(|(width, height, bgra)| unsafe {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let rt: &ID2D1RenderTarget = ctx;
            rt.CreateBitmap(
                D2D_SIZE_U { width, height },
                Some(bgra.as_ptr().cast()),
                width * 4,
                &props,
            )
            .ok()
        });
        self.thumbs.insert(path, bitmap);
    }

    /// Files the last paint showed without a thumbnail.
    pub fn take_wanted(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.wanted)
    }

    /// Lays out the library page; it needs DirectWrite to measure the breadcrumb.
    pub fn library_layout(&self, width: f32, height: f32, browse: &Browse, card: bool) -> Layout {
        browse::layout(self, width, height, browse, card)
    }

    /// Adds the sidebar overlay panel to a player layout.
    pub fn add_overlay(&self, layout: &mut Layout, browse: &Browse) {
        browse::overlay(self, layout, browse);
    }

    /// Paints the interface over the video, which the renderer has already drawn into the
    /// video area, and the open dialog over both. Runs between BeginDraw and EndDraw.
    pub fn paint(&mut self, ctx: &ID2D1DeviceContext5, s: &Scene) -> Result<()> {
        self.paint_page(ctx, s)?;
        match (&s.layout.dialog, s.dialog) {
            (Some(g), Some(d)) => self.paint_dialog(ctx, s, g, d),
            _ => Ok(()),
        }
    }

    fn paint_page(&mut self, ctx: &ID2D1DeviceContext5, s: &Scene) -> Result<()> {
        if s.layout.page == Page::Library {
            return self.paint_library(ctx, s);
        }
        let rt: &ID2D1RenderTarget = ctx;
        let l = s.layout;
        unsafe {
            if !s.has_frame {
                rt.FillRectangle(&l.video.d2d(), &self.black);
                self.art(rt, ctx, s, l.video)?;
            }
            rt.FillGeometry(&self.frame_geometry(l)?, &self.bg, None);
            // Subtitles sit on the picture itself, or on the video area for audio files.
            let picture = self.picture.unwrap_or(l.video);
            if !s.cues.is_empty() {
                rt.PushAxisAlignedClip(&l.video.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
                let drawn = self.captions.draw(
                    ctx,
                    (picture.l, picture.t, picture.r, picture.b),
                    s.cues,
                    s.caption_style,
                );
                rt.PopAxisAlignedClip();
                drawn?;
            }
            if let Some(stats) = s.stats {
                // Four lines of 13 px text in a box sized to them.
                let lines = stats.lines().count() as f32;
                let r = Rect::new(
                    l.video.l + 12.0,
                    l.video.t + 12.0,
                    l.video.l + 332.0,
                    l.video.t + 32.0 + 17.0 * lines,
                );
                rt.FillRoundedRectangle(&rounded(r, 6.0), &self.stats_bg);
                let utf16: Vec<u16> = stats.encode_utf16().collect();
                rt.DrawText(
                    &utf16,
                    &self.stats_text,
                    &r.inset(10.0).d2d(),
                    &self.fg,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            if !l.bars {
                return Ok(());
            }
            self.title_bar(rt, ctx, s);
            self.control_bar(rt, ctx, s);
            if let Some(panel) = l.browse.overlay {
                self.overlay(rt, ctx, s, panel)?;
            }
            if let Some(data) = s.panel {
                self.panel(rt, ctx, s, data);
            }
        }
        Ok(())
    }

    fn frame_geometry(&mut self, l: &Layout) -> Result<ID2D1PathGeometry1> {
        let key = (l.width, l.height, l.video);
        if let Some((k, geometry)) = &self.frame
            && *k == key
        {
            return Ok(geometry.clone());
        }
        // Fullscreen hide-UI mode shows the video edge to edge, without rounded corners.
        let radius = match l.page {
            Page::Library => 4.0,
            Page::Player if l.video.l <= 0.0 => 0.0,
            Page::Player => VIDEO_RADIUS,
        };
        unsafe {
            let window = self
                .factory
                .CreateRectangleGeometry(&Rect::new(0.0, 0.0, l.width, l.height).d2d())?;
            let video = self
                .factory
                .CreateRoundedRectangleGeometry(&rounded(l.video, radius))?;
            let path = self.factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            window.CombineWithGeometry(
                &video,
                D2D1_COMBINE_MODE_EXCLUDE,
                None,
                D2D1_DEFAULT_FLATTENING_TOLERANCE,
                &sink,
            )?;
            sink.Close()?;
            self.frame = Some((key, path.clone()));
            Ok(path)
        }
    }

    unsafe fn icon(&self, ctx: &ID2D1DeviceContext5, svg: &Svg, x: f32, y: f32, mirrored: bool) {
        // The design mirrors some icons (rotate 180 + flip vertically, which is a horizontal flip).
        let m = if mirrored {
            Matrix3x2 {
                M11: -1.0,
                M12: 0.0,
                M21: 0.0,
                M22: 1.0,
                M31: x + svg.width,
                M32: y,
            }
        } else {
            translate(x, y)
        };
        unsafe {
            ctx.SetTransform(&m);
            ctx.DrawSvgDocument(&svg.doc);
            ctx.SetTransform(&Matrix3x2::identity());
        }
    }

    unsafe fn hover_box(&self, rt: &ID2D1RenderTarget, s: &Scene, hit: Hit) {
        if s.hover == Some(hit) {
            unsafe { rt.FillRoundedRectangle(&rounded(s.layout.button(hit), 4.0), &self.hover) };
        }
    }

    unsafe fn text(&self, rt: &ID2D1RenderTarget, text: &str, format: &IDWriteTextFormat, r: Rect) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            rt.DrawText(
                &utf16,
                format,
                &r.d2d(),
                &self.fg,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        };
    }

    unsafe fn title_bar(&self, rt: &ID2D1RenderTarget, ctx: &ID2D1DeviceContext5, s: &Scene) {
        let (i, w) = (&self.icons, s.layout.width);
        let player = s.layout.page == Page::Player;
        unsafe {
            self.icon(ctx, &i.logo, 13.0, 8.0, false);
            self.hover_box(rt, s, Hit::More);
            self.icon(ctx, &i.more, 56.0, 11.0, true);
            // The library page has its own sidebar and breadcrumb.
            if player {
                self.hover_box(rt, s, Hit::Sidebar);
                self.icon(ctx, &i.sidebar, 91.0, 8.0, true);
                self.icon(ctx, &i.divider, 117.0, 8.0, true);
                self.icon(ctx, &i.folder, 143.0, 8.0, true);
                self.text(
                    rt,
                    s.folder,
                    &self.folder,
                    Rect::new(171.0, 0.0, 300.0, TITLE_BAR),
                );
            }
            let title = if s.title.is_empty() { "myvlc" } else { s.title };
            self.text(
                rt,
                title,
                &self.title,
                Rect::new(300.0, 0.0, w - 300.0, TITLE_BAR),
            );
            let full = if s.fullscreen {
                &i.fullscreen_exit
            } else {
                &i.fullscreen
            };
            let buttons = [
                (Hit::Minimize, &i.minimize),
                (Hit::Maximize, &i.maximize),
                (Hit::Fullscreen, full),
                (Hit::Close, &i.close),
            ];
            for (hit, svg) in buttons {
                self.hover_box(rt, s, hit);
                self.icon(ctx, svg, s.layout.button(hit).l + 6.0, 8.0, true);
            }
        }
    }

    unsafe fn control_bar(&self, rt: &ID2D1RenderTarget, ctx: &ID2D1DeviceContext5, s: &Scene) {
        let (i, l) = (&self.icons, s.layout);
        let cy = l.height - CONTROL_BAR / 2.0;
        unsafe {
            self.hover_box(rt, s, Hit::Previous);
            self.icon(ctx, &i.previous, 23.0, cy - 12.0, false);
            rt.FillRoundedRectangle(&rounded(l.button(Hit::Play), 6.0), &self.accent);
            self.icon(
                ctx,
                if s.paused { &i.play } else { &i.pause },
                75.0,
                cy - 12.0,
                false,
            );
            self.hover_box(rt, s, Hit::Next);
            self.icon(ctx, &i.next, 123.0, cy - 12.0, false);

            self.text(rt, &clock_text(s.position), &self.times, l.current);
            self.text(rt, &clock_text(s.duration), &self.times, l.duration);
            // Seek bar: dark track, a played section and an orange handle at its end.
            let t = l.track;
            rt.FillRoundedRectangle(&rounded(t, 6.0), &self.track);
            let fraction = if s.duration > 0.0 {
                (s.position / s.duration).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let played = (fraction as f32 * (t.w() - 4.0)).max(8.0);
            let fill = Rect::new(t.l + 2.0, t.t + 2.0, t.l + 2.0 + played, t.b - 2.0);
            rt.FillRoundedRectangle(&rounded(fill, 4.0), &self.played);
            rt.FillRoundedRectangle(
                &rounded(Rect::new(fill.r - 8.0, fill.t, fill.r, fill.b), 4.0),
                &self.accent,
            );

            let speaker = match (s.muted, s.volume) {
                (true, _) => &i.speaker_muted,
                (_, v) if v <= 0.0 => &i.speaker_muted,
                (_, v) if v < 0.34 => &i.speaker_low,
                (_, v) if v < 0.67 => &i.speaker_medium,
                _ => &i.speaker_high,
            };
            self.icon(ctx, speaker, l.button(Hit::Speaker).l, cy - 12.0, true);
            // Volume wedge: the track, then the gradient fill clipped to the level.
            let v = l.volume;
            self.icon(ctx, &i.volume_track, v.l, v.t + 0.32, false);
            if !s.muted {
                let clip = Rect::new(v.l, v.t, v.l + fill_width(s.volume), v.b);
                rt.PushAxisAlignedClip(&clip.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
                self.icon(ctx, &i.volume_fill, v.l + 1.5, v.t + 1.72, false);
                rt.PopAxisAlignedClip();
            }

            // The button whose popup is open turns orange.
            let on = |hit| l.panels.opener == Some(hit);
            let p = &self.panels;
            for (hit, svg) in [
                (
                    Hit::Audio,
                    if on(Hit::Audio) {
                        &p.audio_on
                    } else {
                        &i.audio
                    },
                ),
                (
                    Hit::Subtitles,
                    if on(Hit::Subtitles) {
                        &p.subtitles_on
                    } else {
                        &i.subtitles
                    },
                ),
                (
                    Hit::Settings,
                    if on(Hit::Settings) {
                        &p.settings_on
                    } else {
                        &i.settings
                    },
                ),
                (Hit::Playlist, &i.playlist),
            ] {
                self.hover_box(rt, s, hit);
                self.icon(ctx, svg, l.button(hit).l + 3.0, cy - 12.0, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_the_design_at_1920() {
        let l = Layout::new(1920.0, 1080.0, true, false);
        assert_eq!(l.video, Rect::new(10.0, 41.0, 1910.0, 1026.0));
        assert_eq!(l.track, Rect::new(254.0, 1039.0, 1492.0, 1067.0));
        assert_eq!(
            l.button(Hit::Playlist),
            Rect::new(1870.0, 1037.0, 1900.0, 1069.0)
        );
        assert_eq!(l.hit(80.0, 1050.0), Some(Hit::Play));
        assert_eq!(l.hit(960.0, 20.0), Some(Hit::Caption));
    }

    #[test]
    fn volume_wedge_round_trips() {
        for v in [0.0, 0.25, 0.5, 1.0, 1.7, 2.0] {
            assert!((volume_for_width(fill_width(v)) - v).abs() < 1e-4, "{v}");
        }
        assert_eq!(fill_width(1.0), 63.0);
    }
}

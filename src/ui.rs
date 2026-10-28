//! The interface from Juxtopposed's "VLC Redesign" (Figma community file, CC BY 4.0), drawn with
//! Direct2D and DirectWrite on top of the video: the player page (title bar, control bar and
//! hide-UI mode) here, the library page in `browse`, the tracks and settings popups in `panels`,
//! the popup menus in `menu`.
//! Sizes are DIPs measured from the design's 1920x1080 frames.

mod browse;
pub mod customize;
pub mod dialog;
pub mod menu;
pub mod mini;
pub mod onboarding;
pub mod panels;
pub mod preferences;
pub mod queue;
pub mod theme;
pub mod toolbar;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub use browse::Browse;
pub use dialog::{Dialog, DialogAction};
pub use menu::MenuAction;
pub use panels::{Panel, PanelAction, PanelData, TrackTab};
pub use preferences::{Category, PrefsAction};
pub use queue::{QueueAction, QueueData, QueueItem};
pub use toolbar::{Item as ToolItem, Toolbar};
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
    /// The library's mini player card outside its buttons: back to the player page.
    Card,
    /// The mini player's close: stops playback.
    MiniClose,
    /// The mini player's expand: back to the full player.
    MiniExpand,
    /// The overlay panel's "open in library" button.
    OpenLibrary,
    /// The overlay panel outside its buttons and entries.
    Panel,
    /// The tracks or settings popup.
    Popup(panels::Target),
    /// The queue panel.
    Queue(queue::Target),
    /// An open menu, over everything else.
    Menu(menu::Target),
    /// The open dialog, which takes every click.
    Dialog(dialog::Target),
    /// The Settings window, which takes every click outside an open dialog.
    Preferences(preferences::Target),
    /// The first-run welcome, which takes every click outside an open dialog.
    Onboarding(onboarding::Target),
    /// A bar item the design's customize palette adds.
    Tool(toolbar::Item),
    /// Customize Player and the bars under it, which take every click while it's open.
    Customize(customize::Target),
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
    /// The mini player window.
    Mini,
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
    /// The bars' items and groups, on the player page.
    tools: toolbar::Bars,
    /// The mini player card: the library's while something plays, or the whole mini window.
    card: Option<mini::Card>,
    browse: browse::Geometry,
    panels: panels::Geometry,
    queue: queue::Geometry,
    menu: menu::Geometry,
    dialog: Option<dialog::Geometry>,
    preferences: Option<preferences::Geometry>,
    onboarding: Option<onboarding::Geometry>,
    customize: Option<customize::Geometry>,
}

impl Layout {
    /// The player page with the bars myvlc starts with.
    #[cfg(test)]
    pub fn new(width: f32, height: f32, bars: bool, fullscreen: bool) -> Self {
        Self::player(width, height, bars, fullscreen, &Toolbar::default())
    }

    /// The player page with `toolbar`'s bars.
    pub fn player(width: f32, height: f32, bars: bool, fullscreen: bool, t: &Toolbar) -> Self {
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
        let w = width;
        let tools = toolbar::layout(t, w, height);
        let top = |x: f32, size: f32| Rect::new(x, 6.0, x + size, 34.0);
        let mut buttons = vec![
            (Hit::Minimize, top(w - 150.0, 36.0)),
            (Hit::Maximize, top(w - 114.0, 36.0)),
            (Hit::Fullscreen, top(w - 78.0, 36.0)),
            (Hit::Close, top(w - 42.0, 36.0)),
        ];
        let mut volume = Rect::default();
        for p in &tools.placed {
            let r = p.r;
            match p.item {
                // The speaker and its wedge, which drags like the seek bar.
                ToolItem::Volume => {
                    let cy = (r.t + r.b) / 2.0;
                    buttons.push((
                        Hit::Speaker,
                        Rect::new(r.l, cy - 12.0, r.l + 24.0, cy + 12.0),
                    ));
                    volume = Rect::new(r.l + 32.0, r.t, r.r, r.b);
                }
                item => {
                    if let Some(hit) = item.hit() {
                        buttons.push((hit, r));
                    }
                }
            }
        }
        Layout {
            page: Page::Player,
            bars,
            width,
            height,
            video: Rect::new(FRAME, TITLE_BAR + 1.0, w - FRAME, height - tools.bar),
            buttons,
            current: tools.current,
            track: tools.track,
            duration: tools.duration,
            volume,
            tools,
            ..Default::default()
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
        if let Some(c) = &self.customize {
            return Some(Hit::Customize(c.hit(x, y)));
        }
        if let Some(o) = &self.onboarding {
            return Some(Hit::Onboarding(o.hit(x, y)));
        }
        if let Some(p) = &self.preferences {
            return Some(Hit::Preferences(p.hit(x, y)));
        }
        if let Some(target) = self.menu.hit(x, y) {
            return Some(Hit::Menu(target));
        }
        if self.page == Page::Library {
            return self.browse_hit(x, y);
        }
        if self.page == Page::Mini {
            if let Some(target) = self.queue.hit(x, y) {
                return Some(Hit::Queue(target));
            }
            return self.card.and_then(|c| c.hit(x, y));
        }
        if let Some(target) = self.panels.hit(x, y) {
            return Some(Hit::Popup(target));
        }
        if let Some(target) = self.queue.hit(x, y) {
            return Some(Hit::Queue(target));
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
    Queue(QueueAction),
    Menu(MenuAction),
    /// Opens the menu for what was clicked (the "..." button, the sort button, the video or an
    /// entry) at (x, y); main.rs builds it, because it needs the library and the open file.
    OpenMenu(Hit, f32, f32),
    Dialog(DialogAction),
    Preferences(PrefsAction),
    Onboarding(onboarding::OnboardingAction),
    /// Customize Player's Save: keep these bars.
    SaveToolbar(Toolbar),
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
    /// The queue panel.
    pub queue: queue::State,
    /// The last slider pressed and when, to spot a double-click that resets it.
    last_slider: Option<(panels::Ctl, Instant)>,
    /// The open popup menu.
    pub menu: menu::State,
    /// Hide UI (Ctrl+\): the bars stay hidden until the mouse moves.
    hidden: bool,
    /// The open dialog, if any.
    pub dialog: Option<dialog::State>,
    /// The open Settings window, if any.
    pub preferences: Option<preferences::State>,
    last_pref_slider: Option<(preferences::Ctl, Instant)>,
    /// The first-run welcome, while it's open.
    pub onboarding: Option<onboarding::State>,
    /// What the bars hold, and Customize Player while it's open.
    pub toolbar: Toolbar,
    pub customize: Option<customize::State>,
    /// From Settings: when the bars hide on their own (an index of `prefs::AUTO_HIDE`), and
    /// after how long without mouse movement.
    pub auto_hide: usize,
    pub hide_after: Duration,
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
            queue: queue::State::default(),
            last_slider: None,
            menu: menu::State::default(),
            hidden: false,
            dialog: None,
            preferences: None,
            last_pref_slider: None,
            onboarding: None,
            toolbar: Toolbar::default(),
            customize: None,
            auto_hide: 0,
            hide_after: Duration::from_secs(3),
        }
    }

    /// Switches pages; the pointer may now be over something else.
    pub fn show(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.close_menu();
            self.panels.show(None, None);
            self.queue.open = false;
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
        self.layout.panels.hit(x, y).is_some() || self.layout.queue.hit(x, y).is_some()
    }

    /// Scrolls the popup, or its open choice list when (x, y) is over it, by `dy` DIPs.
    pub fn scroll_panel(&mut self, x: f32, y: f32, dy: f32) {
        if self.layout.queue.hit(x, y).is_some() {
            let max = self.layout.queue.max_scroll;
            self.queue.scroll = (self.queue.scroll + dy).clamp(0.0, max);
            self.relayout = true;
            return;
        }
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
        if self.queue.open {
            self.toggle_queue();
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
            Target::Ctl(Ctl::AdvancedSettings) => {
                use panels::SettingsTab;
                let category = match p.open {
                    Some(Panel::Settings(SettingsTab::Video)) => Category::Video,
                    Some(Panel::Settings(SettingsTab::Audio)) => Category::Audio,
                    Some(Panel::Settings(SettingsTab::Subtitles)) => Category::Subtitles,
                    _ => Category::Playback,
                };
                Some(Action::Menu(MenuAction::Preferences(category)))
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
    /// dragging; otherwise playback goes to the design's hide-UI mode. Settings can keep them
    /// up always, or outside fullscreen.
    pub fn bars_visible(&self, paused: bool, fullscreen: bool) -> bool {
        if self.hidden && !self.menu.is_open() {
            return false;
        }
        let stay = match self.auto_hide {
            1 => !fullscreen,
            2 => true,
            _ => false,
        };
        stay || self.page != Page::Player
            || self.menu.is_open()
            || self.overlay
            || self.panels.open.is_some()
            || self.queue.open
            || self.dialog.is_some()
            || self.preferences.is_some()
            || self.onboarding.is_some()
            || self.customize.is_some()
            || paused
            || self.drag.is_some()
            || self.hover.is_some_and(Hit::in_bars)
            || self.last_activity.elapsed() < self.hide_after
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
            self.hidden = false;
        }
        let hover = self.layout.hit(x, y);
        if hover != self.hover {
            self.hover = hover;
            self.dirty = true;
        }
        if self.menu.is_open() {
            let target = match hover {
                Some(Hit::Menu(t)) => Some(t),
                _ => None,
            };
            if self.menu.hover(target, Instant::now()) {
                self.sync_menu();
            }
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
            Some(Hit::Preferences(_)) => self.preferences_drag(x),
            Some(Hit::Customize(_)) => {
                self.customize_drag(x, y);
                None
            }
            _ => {
                self.queue_drag(y);
                None
            }
        }
    }

    /// The action for a popup slider with the pointer at `x`.
    fn slide(&self, ctl: panels::Ctl, x: f32) -> Option<Action> {
        let v = self.layout.panels.slider_value(ctl, x)?;
        ctl.set(v).map(Action::Panel)
    }

    pub fn on_down(&mut self, x: f32, y: f32) -> Option<Action> {
        self.on_move(x, y);
        // A press outside an open menu only closes it.
        if self.menu.is_open() && !matches!(self.hover, Some(Hit::Menu(_))) {
            self.close_menu();
            self.pressed = None;
            return None;
        }
        self.pressed = self.hover;
        self.queue_press(y);
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
            Some(Hit::Preferences(target)) => self.preferences_down(target, x),
            Some(Hit::Customize(target)) => {
                self.customize_down(target, x, y);
                None
            }
            _ => None,
        }
    }

    pub fn on_up(&mut self, x: f32, y: f32) -> Option<Action> {
        let hit = self.layout.hit(x, y);
        let pressed = self.pressed.take();
        self.dirty = true;
        self.queue.press = None;
        match self.drag.take() {
            Some(Hit::Seek) => {
                let fraction = self.layout.seek_fraction(x);
                self.seek_preview = Some((fraction, Instant::now()));
                Some(Action::SeekTo(fraction))
            }
            Some(Hit::Queue(queue::Target::Row(from))) => self.queue_drop(from),
            Some(Hit::Customize(_)) => {
                self.customize_drop(x, y);
                None
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
                Some(Hit::Preferences(target)) => self.preferences_click(target),
                Some(Hit::Onboarding(target)) => self.onboarding_click(target),
                Some(Hit::Customize(target)) => self.customize_click(target),
                Some(h @ Hit::Queue(queue::Target::More(_))) if self.queue_rows_menu() => {
                    Some(Action::OpenMenu(h, x, y))
                }
                Some(Hit::Queue(target)) => self.queue_click(target),
                Some(Hit::Playlist) => {
                    self.toggle_queue();
                    Some(Action::Click(Hit::Playlist))
                }
                Some(Hit::Menu(target)) => {
                    let action = self.menu.click(target);
                    self.menu_changed();
                    action.map(Action::Menu)
                }
                // Menus open under their button.
                Some(h @ (Hit::More | Hit::Sort)) => {
                    let r = self.layout.button(h);
                    Some(Action::OpenMenu(h, r.l, r.b + 4.0))
                }
                Some(h @ (Hit::Audio | Hit::Subtitles | Hit::Settings)) => {
                    // Its own button closes a popup; another button switches to its popup.
                    let open = self.panels.opener != Some(h);
                    let panel = self.panels.for_button(h).filter(|_| open);
                    self.panels.show(panel, panel.map(|_| h));
                    if panel.is_some() {
                        self.queue.open = false;
                    }
                    self.relayout = true;
                    Some(Action::Click(h))
                }
                // A click on the video closes the popup; only a click that closed nothing
                // goes on (to play or pause, if Settings say so).
                Some(Hit::Video) => {
                    let mut closed = false;
                    if self.panels.open.is_some() {
                        self.panels.show(None, None);
                        self.relayout = true;
                        closed = true;
                    }
                    if self.queue.open {
                        self.toggle_queue();
                        closed = true;
                    }
                    (!closed).then_some(Action::Click(Hit::Video))
                }
                Some(h) => Some(Action::Click(h)),
                None => None,
            },
        }
    }

    /// A right-click: the video, library entries and queue rows have context menus.
    pub fn on_right(&mut self, x: f32, y: f32) -> Option<Action> {
        self.on_move(x, y);
        if matches!(self.hover, Some(Hit::Menu(_))) {
            return None;
        }
        self.close_menu();
        match self.layout.hit(x, y)? {
            hit @ (Hit::Video | Hit::Entry(_)) => Some(Action::OpenMenu(hit, x, y)),
            Hit::Queue(queue::Target::Row(i) | queue::Target::More(i))
                if self.queue_rows_menu() =>
            {
                Some(Action::OpenMenu(Hit::Queue(queue::Target::More(i)), x, y))
            }
            _ => None,
        }
    }

    /// Opens a menu at (x, y) for `hit`; an entry stays highlighted under its menu.
    pub fn open_menu(
        &mut self,
        kind: menu::Kind,
        items: Vec<menu::Item>,
        at: (f32, f32),
        hit: Hit,
    ) {
        self.menu.open(kind, items, at.0, at.1);
        self.browse.context = match hit {
            Hit::Entry(i) => Some(i),
            _ => None,
        };
        self.sync_menu();
    }

    pub fn close_menu(&mut self) {
        if self.menu.is_open() {
            self.menu.close();
            self.menu_changed();
        }
    }

    /// Rebuilds the menu's geometry, after the menu, the window or the layout changed.
    pub fn sync_menu(&mut self) {
        self.layout.menu = menu::Geometry::new(&self.menu, self.layout.width, self.layout.height);
        self.dirty = true;
        self.rehover();
    }

    fn menu_changed(&mut self) {
        if !self.menu.is_open() {
            self.browse.context = None;
        }
        self.sync_menu();
    }

    /// A key for the open menu.
    pub fn menu_key(&mut self, key: menu::Key) -> Option<Action> {
        let action = self.menu.key(key);
        self.menu_changed();
        if self.menu.reveal(&self.layout.menu) {
            self.sync_menu();
        }
        action.map(Action::Menu)
    }

    /// Opens or swaps a submenu whose hover delay ran out; the render loop calls it.
    pub fn menu_tick(&mut self) {
        if self.menu.tick(Instant::now()) {
            self.sync_menu();
        }
    }

    /// Whether (x, y) is over the open menu.
    pub fn over_menu(&self, x: f32, y: f32) -> bool {
        self.layout.menu.contains(x, y)
    }

    pub fn scroll_menu(&mut self, x: f32, y: f32, dy: f32) {
        self.menu.scroll(&self.layout.menu, x, y, dy);
        self.sync_menu();
    }

    /// Hide UI: the bars, the popup and the sidebar panel go until the mouse moves.
    pub fn hide_bars(&mut self) {
        self.hidden = true;
        self.overlay = false;
        self.panels.show(None, None);
        self.relayout = true;
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
    /// A stream with no end, like radio: its length reads "Live".
    pub live: bool,
    pub paused: bool,
    pub volume: f32,
    pub muted: bool,
    pub title: &'a str,
    pub folder: &'a str,
    /// The open file's name, for the mini player; empty when nothing is open.
    pub file: &'a str,
    /// An audio-only file: its cover (or a music glyph) goes where the video would.
    pub art: Option<&'a std::path::Path>,
    /// The open file, for the bars' thumbnail; None for streams, discs and cameras.
    pub media: Option<&'a std::path::Path>,
    /// The bars' artist and playlist names: the track's artist tag, and the playlist the queue
    /// came from. Empty when there is none.
    pub artist: &'a str,
    pub playlist: &'a str,
    pub fullscreen: bool,
    pub has_frame: bool,
    pub browse: &'a Browse,
    /// What the tracks and settings popups show.
    pub panel: Option<&'a PanelData>,
    /// What the queue panel shows, while it is open.
    pub queue: Option<&'a QueueData>,
    /// The open dialog's text, files and caret.
    pub dialog: Option<&'a dialog::State>,
    /// The open Settings window's settings and search.
    pub preferences: Option<&'a preferences::State>,
    /// The first-run welcome, while it's open.
    pub onboarding: Option<&'a onboarding::State>,
    /// Customize Player, while it's open.
    pub customize: Option<&'a customize::State>,
    /// For the bars' repeat, shuffle and favourite items.
    pub repeat: crate::queue::Repeat,
    pub shuffle: bool,
    pub favorite: bool,
    /// The bars' record item lights while recording.
    pub recording: bool,
    /// Subtitle cues to show now, and how.
    pub cues: &'a [crate::subs::Cue],
    /// Where the subtitles have lines (seconds), marked along the seek bar; empty when off.
    pub sub_marks: &'a [(f64, f64)],
    /// The audio visualizer for an audio file: 1 with spectrum levels (0..1), 2 with the latest
    /// samples (-1..1).
    pub visual: Option<(usize, &'a [f32])>,
    pub caption_style: &'a crate::captions::CaptionStyle,
    /// Playback statistics, drawn over the video's top-left corner when turned on.
    pub stats: Option<&'a str>,
    /// A line shown over the video for a moment, such as where a snapshot went.
    pub notice: Option<&'a str>,
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

/// A design colour in the active theme (see [`theme`]).
fn color(rgb: u32, alpha: f32) -> D2D1_COLOR_F {
    let rgb = theme::map(rgb);
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
                let mut icons = Icons { $($name: svg(ctx, include_bytes!(concat!("../assets/icons/", $file)))?),* };
                // VLC's cone stays orange whatever the accent, and the volume meter's green to red
                // is a level scale, not the accent.
                icons.logo = svg_as_is(ctx, include_bytes!("../assets/icons/logo.svg"))?;
                icons.volume_fill = svg_as_is(ctx, include_bytes!("../assets/icons/volume_fill.svg"))?;
                Ok(icons)
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

/// Parses an icon exported from Figma, in the active theme's colours.
fn svg(ctx: &ID2D1DeviceContext5, bytes: &[u8]) -> Result<Svg> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::from(E_FAIL))?;
    svg_as_is(ctx, theme::svg(text).as_bytes())
}

/// Parses an icon exported from Figma. The viewport is the root element's width and height.
fn svg_as_is(ctx: &ID2D1DeviceContext5, bytes: &[u8]) -> Result<Svg> {
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
    /// The seek bar's subtitle marks.
    marks: ID2D1SolidColorBrush,
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
    queue: queue::Resources,
    menu: menu::Resources,
    dialog: dialog::Resources,
    prefs: preferences::Resources,
    mini: mini::Resources,
    onboard: onboarding::Resources,
    custom: customize::Resources,
    /// The bar items' icons, and their lit versions for repeat, shuffle and favourite.
    tools: HashMap<ToolItem, Svg>,
    tools_on: HashMap<ToolItem, Svg>,
    /// The seconds on the skip items.
    skip_text: IDWriteTextFormat,
    /// Thumbnails by file; None means the file has no picture.
    thumbs: HashMap<PathBuf, Option<ID2D1Bitmap>>,
    /// Visible files without a thumbnail yet, collected while painting.
    wanted: Vec<PathBuf>,
    /// The bars' thumbnail, wanted while painting the bars, which only borrow the painter.
    bar_wanted: std::cell::Cell<Option<PathBuf>>,
    captions: crate::captions::CaptionRenderer,
    stats_text: IDWriteTextFormat,
    stats_bg: ID2D1SolidColorBrush,
    stats_fg: ID2D1SolidColorBrush,
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
            marks: brush(0xFFFFFF, 0.35)?,
            hover: brush(0xFFFFFF, 0.06)?,
            black: brush(0x000000, 1.0)?,
            times: format(15.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
            title: format(14.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
            folder: format(14.0, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            frame: None,
            lib: browse::Resources::new(ctx, &factory, &dwrite, &fonts)?,
            panels: panels::Resources::new(ctx, &dwrite, &fonts)?,
            queue: queue::Resources::new(ctx, &dwrite, &fonts)?,
            menu: menu::Resources::new(ctx, &dwrite, &fonts)?,
            dialog: dialog::Resources::new(ctx, &factory, &dwrite, &fonts)?,
            prefs: preferences::Resources::new(ctx, &dwrite, &fonts)?,
            mini: mini::Resources::new(ctx, &dwrite, &fonts)?,
            onboard: onboarding::Resources::new(ctx, &dwrite, &fonts)?,
            custom: customize::Resources::new(ctx, &factory, &dwrite, &fonts)?,
            tools: tool_icons(ctx, false)?,
            tools_on: tool_icons(ctx, true)?,
            skip_text: text_format(
                &dwrite,
                &fonts,
                7.5,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            )?,
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
            // Over the video, so the same in every theme.
            stats_bg: brush(0x000000, 0.6)?,
            stats_fg: brush(theme::fixed(FG_A2), 1.0)?,
            picture: None,
            factory,
            dwrite,
            thumbs: HashMap::new(),
            wanted: Vec::new(),
            bar_wanted: Default::default(),
        })
    }

    pub fn factory(&self) -> ID2D1Factory1 {
        self.factory.clone()
    }

    /// Takes what a painter made in another theme has that isn't colour: thumbnails, the
    /// current captions and where the picture is.
    pub fn carry_over(&mut self, old: &mut Painter) {
        std::mem::swap(&mut self.thumbs, &mut old.thumbs);
        std::mem::swap(&mut self.captions, &mut old.captions);
        self.picture = old.picture;
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
        let mut wanted = std::mem::take(&mut self.wanted);
        wanted.extend(self.bar_wanted.take());
        wanted
    }

    /// Lays out the library page; it needs DirectWrite to measure the breadcrumb.
    pub fn library_layout(
        &self,
        width: f32,
        height: f32,
        browse: &Browse,
        card: Option<bool>,
    ) -> Layout {
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
        if let Some(g) = &s.layout.customize {
            self.paint_customize(ctx, s, g);
        }
        unsafe { self.menu(ctx, s) };
        if let (Some(g), Some(p)) = (&s.layout.preferences, s.preferences) {
            self.paint_preferences(ctx, s, g, p);
        }
        if let Some(g) = &s.layout.onboarding {
            self.paint_onboarding(ctx, s, g);
        }
        match (&s.layout.dialog, s.dialog) {
            (Some(g), Some(d)) => self.paint_dialog(ctx, s, g, d),
            _ => Ok(()),
        }
    }

    /// The notice, one line at the top right of `area`, in the statistics' box.
    fn notice(&self, rt: &ID2D1RenderTarget, s: &Scene, area: Rect) {
        let Some(notice) = s.notice else { return };
        let w = (self.measure(notice, &self.stats_text) + 20.0).min(area.w() - 24.0);
        let r = Rect::new(
            area.r - 12.0 - w,
            area.t + 12.0,
            area.r - 12.0,
            area.t + 46.0,
        );
        let utf16: Vec<u16> = notice.encode_utf16().collect();
        unsafe {
            rt.FillRoundedRectangle(&rounded(r, 6.0), &self.stats_bg);
            rt.DrawText(
                &utf16,
                &self.stats_text,
                &Rect::new(r.l + 10.0, r.t + 8.0, r.r - 10.0, r.b).d2d(),
                &self.stats_fg,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    fn paint_page(&mut self, ctx: &ID2D1DeviceContext5, s: &Scene) -> Result<()> {
        match s.layout.page {
            // Notices, such as a conversion's progress, show over the library too.
            Page::Library => {
                self.paint_library(ctx, s)?;
                self.notice(ctx, s, s.layout.browse.content);
                return Ok(());
            }
            Page::Mini => return self.paint_mini(ctx, s),
            Page::Player => {}
        }
        let rt: &ID2D1RenderTarget = ctx;
        let l = s.layout;
        unsafe {
            if !s.has_frame {
                rt.FillRectangle(&l.video.d2d(), &self.black);
                match s.visual {
                    Some(visual) => self.visualizer(rt, visual, l.video)?,
                    None => self.art(rt, ctx, s, l.video)?,
                }
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
                    &self.stats_fg,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            self.notice(rt, s, l.video);
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
            if let Some(data) = s.queue {
                self.queue_panel(rt, ctx, s, data);
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
            Page::Library | Page::Mini => 4.0,
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

    /// The subtitle timeline: a thin line along the seek bar's bottom under each subtitle,
    /// lines closer than a pixel merged.
    unsafe fn sub_marks(&self, rt: &ID2D1RenderTarget, s: &Scene, t: Rect) {
        if s.duration <= 0.0 {
            return;
        }
        let x = |at: f64| t.l + 2.0 + (at / s.duration).clamp(0.0, 1.0) as f32 * (t.w() - 4.0);
        let mut run: Option<(f32, f32)> = None;
        let draw = |(l, r): (f32, f32)| {
            let mark = Rect::new(l, t.b - 6.0, r.max(l + 1.5), t.b - 3.0);
            unsafe { rt.FillRectangle(&mark.d2d(), &self.marks) };
        };
        for &(start, end) in s.sub_marks {
            let (l, r) = (x(start), x(end));
            run = match run {
                Some((rl, rr)) if l <= rr + 1.0 => Some((rl, rr.max(r))),
                Some(done) => {
                    draw(done);
                    Some((l, r))
                }
                None => Some((l, r)),
            };
        }
        run.map(draw);
    }

    /// The audio visualizer filling `area`: spectrum bars rising from the bottom third, or the
    /// waveform across the middle.
    unsafe fn visualizer(
        &self,
        rt: &ID2D1RenderTarget,
        (mode, v): (usize, &[f32]),
        area: Rect,
    ) -> Result<()> {
        if v.is_empty() {
            return Ok(());
        }
        let (w, base) = (area.w() * 0.8, area.t + area.h() * 0.7);
        let left = area.l + (area.w() - w) / 2.0;
        unsafe {
            if mode == 1 {
                let step = w / v.len() as f32;
                for (i, level) in v.iter().enumerate() {
                    let h = (level * area.h() * 0.45).max(2.0);
                    let l = left + step * i as f32 + step * 0.15;
                    let bar = Rect::new(l, base - h, l + step * 0.7, base);
                    rt.FillRoundedRectangle(&rounded(bar, 2.0), &self.accent);
                }
            } else {
                let path = self.factory.CreatePathGeometry()?;
                let sink = path.Open()?;
                let cy = area.t + area.h() / 2.0;
                let amp = area.h() * 0.3;
                let point = |i: usize| windows_numerics::Vector2 {
                    X: left + w * i as f32 / (v.len() - 1).max(1) as f32,
                    Y: cy - v[i].clamp(-1.0, 1.0) * amp,
                };
                sink.BeginFigure(point(0), D2D1_FIGURE_BEGIN_HOLLOW);
                for i in 1..v.len() {
                    sink.AddLine(point(i));
                }
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                sink.Close()?;
                rt.DrawGeometry(&path, &self.accent, 2.0, None);
            }
        }
        Ok(())
    }

    unsafe fn title_bar(&self, rt: &ID2D1RenderTarget, ctx: &ID2D1DeviceContext5, s: &Scene) {
        let (i, w) = (&self.icons, s.layout.width);
        unsafe {
            self.icon(ctx, &i.logo, 13.0, 8.0, false);
            // The library page has its own sidebar and breadcrumb; the player's title bar group
            // is whatever Customize Player put there.
            if s.layout.page == Page::Player {
                for p in s
                    .layout
                    .tools
                    .placed
                    .iter()
                    .filter(|p| p.zone == toolbar::Zone::Title)
                {
                    self.tool(rt, ctx, s, p);
                }
            } else {
                self.hover_box(rt, s, Hit::More);
                self.icon(ctx, &i.more, 56.0, 11.0, true);
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
        let l = s.layout;
        unsafe {
            self.text(rt, &clock_text(s.position), &self.times, l.current);
            let total = if s.live {
                "Live".to_owned()
            } else {
                clock_text(s.duration)
            };
            self.text(rt, &total, &self.times, l.duration);
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
            self.sub_marks(rt, s, t);
            rt.FillRoundedRectangle(
                &rounded(Rect::new(fill.r - 8.0, fill.t, fill.r, fill.b), 4.0),
                &self.accent,
            );
            for p in l
                .tools
                .placed
                .iter()
                .filter(|p| p.zone != toolbar::Zone::Title)
            {
                self.tool(rt, ctx, s, p);
            }
        }
    }

    /// One bar item where the layout put it.
    unsafe fn tool(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        p: &toolbar::Placed,
    ) {
        let (i, l, r) = (&self.icons, s.layout, p.r);
        let cy = (r.t + r.b) / 2.0;
        // The button whose popup is open turns orange.
        let on = |hit| l.panels.opener == Some(hit);
        let pp = &self.panels;
        unsafe {
            match p.item {
                ToolItem::More => {
                    self.hover_box(rt, s, Hit::More);
                    self.icon(ctx, &i.more, r.l + 5.0, cy - 9.0, true);
                }
                ToolItem::Sidebar => {
                    self.hover_box(rt, s, Hit::Sidebar);
                    self.icon(ctx, &i.sidebar, r.l + 2.0, cy - 12.0, true);
                }
                ToolItem::Divider => {
                    self.icon(ctx, &i.divider, r.l + (r.w() - 24.0) / 2.0, cy - 12.0, true)
                }
                ToolItem::Spacer => {}
                // A stream, disc or camera has no folder; neither icon nor name then.
                ToolItem::Folder if s.folder.is_empty() => {}
                ToolItem::Folder => {
                    self.icon(ctx, &i.folder, r.l + 2.0, cy - 12.0, true);
                    let tr = Rect::new(r.l + 30.0, cy - 20.0, r.r, cy + 20.0);
                    self.text(rt, s.folder, &self.folder, tr);
                }
                ToolItem::FileName => {
                    let tr = Rect::new(r.l, cy - 20.0, r.r, cy + 20.0);
                    self.text(rt, s.file, &self.folder, tr);
                }
                ToolItem::ArtistName | ToolItem::PlaylistName => {
                    let text = match p.item {
                        ToolItem::ArtistName => s.artist,
                        _ => s.playlist,
                    };
                    if !text.is_empty() {
                        self.hover_box(rt, s, Hit::Tool(p.item));
                        let tr = Rect::new(r.l + 6.0, cy - 20.0, r.r - 6.0, cy + 20.0);
                        self.text(rt, text, &self.folder, tr);
                    }
                }
                ToolItem::Thumbnail => self.bar_thumbnail(rt, ctx, s, r, cy),
                ToolItem::Previous => {
                    self.hover_box(rt, s, Hit::Previous);
                    self.icon(ctx, &i.previous, r.l + 3.0, cy - 12.0, false);
                }
                ToolItem::Play => {
                    rt.FillRoundedRectangle(&rounded(r, 6.0), &self.accent);
                    let svg = if s.paused { &i.play } else { &i.pause };
                    self.icon(ctx, svg, r.l + 17.0, cy - 12.0, false);
                }
                ToolItem::Next => {
                    self.hover_box(rt, s, Hit::Next);
                    self.icon(ctx, &i.next, r.l + 3.0, cy - 12.0, false);
                }
                ToolItem::Volume => {
                    let speaker = match (s.muted, s.volume) {
                        (true, _) => &i.speaker_muted,
                        (_, v) if v <= 0.0 => &i.speaker_muted,
                        (_, v) if v < 0.34 => &i.speaker_low,
                        (_, v) if v < 0.67 => &i.speaker_medium,
                        _ => &i.speaker_high,
                    };
                    self.icon(ctx, speaker, r.l, cy - 12.0, true);
                    // Volume wedge: the track, then the gradient fill clipped to the level.
                    let v = l.volume;
                    self.icon(ctx, &i.volume_track, v.l, v.t + 0.32, false);
                    if !s.muted {
                        let clip = Rect::new(v.l, v.t, v.l + fill_width(s.volume), v.b);
                        rt.PushAxisAlignedClip(&clip.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
                        self.icon(ctx, &i.volume_fill, v.l + 1.5, v.t + 1.72, false);
                        rt.PopAxisAlignedClip();
                    }
                }
                ToolItem::Audio | ToolItem::Subtitles | ToolItem::Settings | ToolItem::Queue => {
                    let (hit, svg) = match p.item {
                        ToolItem::Audio if on(Hit::Audio) => (Hit::Audio, &pp.audio_on),
                        ToolItem::Audio => (Hit::Audio, &i.audio),
                        ToolItem::Subtitles if on(Hit::Subtitles) => {
                            (Hit::Subtitles, &pp.subtitles_on)
                        }
                        ToolItem::Subtitles => (Hit::Subtitles, &i.subtitles),
                        ToolItem::Settings if on(Hit::Settings) => (Hit::Settings, &pp.settings_on),
                        ToolItem::Settings => (Hit::Settings, &i.settings),
                        _ if l.queue.open => (Hit::Playlist, &self.queue.playlist_on),
                        _ => (Hit::Playlist, &i.playlist),
                    };
                    self.hover_box(rt, s, hit);
                    self.icon(ctx, svg, r.l + 3.0, cy - 12.0, false);
                }
                item => {
                    self.hover_box(rt, s, Hit::Tool(item));
                    let lit = match item {
                        ToolItem::Repeat => s.repeat != crate::queue::Repeat::Off,
                        ToolItem::Shuffle => s.shuffle,
                        ToolItem::Favorite => s.favorite,
                        ToolItem::Record => s.recording,
                        ToolItem::Hdr => s.panel.is_some_and(|p| p.hdr),
                        ToolItem::RightSidebar => l.queue.open,
                        _ => false,
                    };
                    let svg = match (item, s.repeat) {
                        (ToolItem::Repeat, crate::queue::Repeat::One) => &self.queue.repeat_one_on,
                        _ if lit => &self.tools_on[&item],
                        _ => self.tool_icon(item),
                    };
                    let x = r.l + (r.w() - 24.0) / 2.0;
                    self.icon(ctx, svg, x, cy - 12.0, false);
                    self.tool_number(rt, item, x + 12.0, cy);
                }
            }
        }
    }

    /// The bars' thumbnail of the open file: its picture (a video's frame, a song's cover) in a
    /// 16:9 box, or the box with the file's kind of glyph until there is one.
    unsafe fn bar_thumbnail(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        r: Rect,
        cy: f32,
    ) {
        let Some(path) = s.media else { return };
        let (w, h) = (48.0, 27.0);
        let x = r.l + (r.w() - w) / 2.0;
        let box_ = Rect::new(x, cy - h / 2.0, x + w, cy + h / 2.0);
        unsafe {
            match self.thumbs.get(path) {
                Some(Some(bitmap)) => {
                    let size = bitmap.GetSize();
                    if let Ok(brush) = ctx.CreateBitmapBrush(bitmap, None, None) {
                        brush.SetTransform(&browse::cover(box_, size.width, size.height));
                        rt.FillRoundedRectangle(&rounded(box_, 4.0), &brush);
                    }
                }
                // Until it has one, or for a song without a cover: the library tile's look.
                known => {
                    if known.is_none() {
                        self.bar_wanted.set(Some(path.to_owned()));
                    }
                    let audio =
                        crate::library::media_kind(path) == Some(crate::library::MediaKind::Audio);
                    let fill = if audio { &self.lib.panel } else { &self.black };
                    rt.FillRoundedRectangle(&rounded(box_, 4.0), fill);
                    if audio {
                        let x = box_.l + (w - 24.0) / 2.0;
                        self.icon(ctx, &self.icons.music, x, cy - 12.0, false);
                    }
                }
            }
        }
    }

    /// A bar item's icon for the palette and the bars.
    fn tool_icon(&self, item: ToolItem) -> &Svg {
        &self.tools[&item]
    }

    /// The seconds inside the skip items' arrows.
    unsafe fn tool_number(&self, rt: &ID2D1RenderTarget, item: ToolItem, cx: f32, cy: f32) {
        let n = match item {
            ToolItem::Forward10 | ToolItem::Back10 => "10",
            ToolItem::Forward15 | ToolItem::Back15 => "15",
            _ => return,
        };
        let r = Rect::new(cx - 8.0, cy - 4.0, cx + 8.0, cy + 8.0);
        unsafe { self.text(rt, n, &self.skip_text, r) };
    }
}

/// The bar items' icons, grey or (`lit`) in the accent for the ones that light up. The menus'
/// two-tone icons come out grey, as the palette shows them.
fn tool_icons(ctx: &ID2D1DeviceContext5, lit: bool) -> Result<HashMap<ToolItem, Svg>> {
    use toolbar::Item as T;
    let table: [(T, &[u8]); 41] = [
        (T::More, include_bytes!("../assets/icons/more.svg")),
        (T::Sidebar, include_bytes!("../assets/icons/sidebar.svg")),
        (T::Folder, include_bytes!("../assets/icons/menu_info.svg")),
        (T::Previous, include_bytes!("../assets/icons/previous.svg")),
        (T::Play, include_bytes!("../assets/icons/menu_play.svg")),
        (T::Next, include_bytes!("../assets/icons/next.svg")),
        (T::Volume, include_bytes!("../assets/icons/speaker.svg")),
        (T::Audio, include_bytes!("../assets/icons/audio.svg")),
        (
            T::Subtitles,
            include_bytes!("../assets/icons/subtitles.svg"),
        ),
        (
            T::Settings,
            include_bytes!("../assets/icons/playback_settings.svg"),
        ),
        (T::Queue, include_bytes!("../assets/icons/playlist.svg")),
        (T::Spacer, include_bytes!("../assets/icons/tool_spacer.svg")),
        (T::Divider, include_bytes!("../assets/icons/divider.svg")),
        (T::Speed, include_bytes!("../assets/icons/tool_speed.svg")),
        (
            T::Forward10,
            include_bytes!("../assets/icons/tool_forward.svg"),
        ),
        (T::Back10, include_bytes!("../assets/icons/tool_back.svg")),
        (
            T::Forward15,
            include_bytes!("../assets/icons/tool_forward.svg"),
        ),
        (T::Back15, include_bytes!("../assets/icons/tool_back.svg")),
        (T::Stop, include_bytes!("../assets/icons/menu_stop.svg")),
        (T::Repeat, include_bytes!("../assets/icons/repeat.svg")),
        (T::Shuffle, include_bytes!("../assets/icons/shuffle.svg")),
        (T::Record, include_bytes!("../assets/icons/menu_record.svg")),
        (
            T::Snapshot,
            include_bytes!("../assets/icons/tool_snapshot.svg"),
        ),
        (
            T::MiniPlayer,
            include_bytes!("../assets/icons/menu_mini.svg"),
        ),
        (T::Hdr, include_bytes!("../assets/icons/tool_hdr.svg")),
        (T::Zoom, include_bytes!("../assets/icons/tool_zoom.svg")),
        (
            T::RightSidebar,
            include_bytes!("../assets/icons/sidebar.svg"),
        ),
        (T::Quit, include_bytes!("../assets/icons/menu_quit.svg")),
        (
            T::Preferences,
            include_bytes!("../assets/icons/menu_settings.svg"),
        ),
        (T::Home, include_bytes!("../assets/icons/nav_home.svg")),
        (
            T::Thumbnail,
            include_bytes!("../assets/icons/tool_thumbnail.svg"),
        ),
        (T::FileName, include_bytes!("../assets/icons/menu_info.svg")),
        (
            T::ArtistName,
            include_bytes!("../assets/icons/menu_info.svg"),
        ),
        (
            T::PlaylistName,
            include_bytes!("../assets/icons/menu_info.svg"),
        ),
        (
            T::Properties,
            include_bytes!("../assets/icons/menu_info.svg"),
        ),
        (T::Replay, include_bytes!("../assets/icons/tool_replay.svg")),
        (T::Chapters, include_bytes!("../assets/icons/chapters.svg")),
        (
            T::Favorite,
            include_bytes!("../assets/icons/menu_heart.svg"),
        ),
        (T::Bookmark, include_bytes!("../assets/icons/bookmark.svg")),
        (
            T::Bookmarks,
            include_bytes!("../assets/icons/tool_bookmarks.svg"),
        ),
        (
            T::Playlists,
            include_bytes!("../assets/icons/nav_playlists.svg"),
        ),
    ];
    let (from, to) = if lit {
        ("#8F8F8F", "#D37918")
    } else {
        ("#D37918", "#8F8F8F")
    };
    table
        .iter()
        .map(|(item, bytes)| {
            let text = String::from_utf8_lossy(bytes).replace(from, to);
            Ok((*item, svg(ctx, text.as_bytes())?))
        })
        .collect()
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

//! Popup menus from the design's menu frames: the title bar's "..." menu, the player's
//! right-click menu, the library's file, folder and playlist context menus, and the sorting
//! options. One generic popup draws them all: rows with an icon, a label, a shortcut, a submenu
//! chevron or a check mark, dividers and disabled rows. Submenus open on hover. Sizes are DIPs
//! from the design's menu frames, colours sampled from them.
//!
//! The menus only describe what a choice means ([`MenuAction`]); main.rs builds them from the
//! app's state when one opens and carries out the choice.

use std::path::{Path, PathBuf};

use super::browse::SortMode;
use super::panels::{ASPECT, CROP, PanelAction, PanelData, SLEEP, STEREO, SettingsTab, ZOOM};
use super::*;
use crate::convert::Target as Format;
use crate::library::{Entry, Place};
use crate::net::{NetKind, is_address};

const ROW: f32 = 48.0;
/// Space between the frame and the first and last rows, and the rows' inset from the sides.
const PAD: f32 = 4.0;
const RADIUS: f32 = 8.0;
const ROW_RADIUS: f32 = 6.0;
/// Icon centre, label start and the label start of menus without icons, from the frame's left.
const ICON_X: f32 = 30.0;
const LABEL_X: f32 = 52.0;
const PLAIN_X: f32 = 18.0;
/// Shortcut text's right edge, and the chevron or check mark's centre, from the frame's right.
const KEY_R: f32 = 20.0;
const MARK_R: f32 = 31.0;
const SUB_W: f32 = 265.0;
/// Gap between a menu and its submenu.
const SUB_GAP: f32 = 2.0;
/// Room kept between a menu and the window's edges.
const EDGE: f32 = 4.0;
/// Hovering another row waits this long before swapping the open submenu, so the pointer can
/// cross other rows on its way into the submenu.
const SWITCH_DELAY: Duration = Duration::from_millis(300);

// Sampled from the menu frames.
const LABEL_ON: u32 = 0xA8A8A8;
/// The hover row, #1F1F1F over the #191919 panel.
const HOVER_ALPHA: f32 = 0.026;
/// Disabled rows show at this opacity.
const DIM: f32 = 0.4;

/// Every choice the menus offer. main.rs carries them out; the ones marked there belong to
/// features other parts of the app add.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuAction {
    /// Pick files to play; the first plays and the rest follow it.
    OpenFiles,
    OpenNetwork,
    /// Pick a folder and play its media in order.
    OpenFolder,
    /// A file from Open Recent: plays it with its folder as the queue.
    PlayRecent(PathBuf),
    /// Opens the settings panel at a tab.
    Settings(SettingsTab),
    /// Opens the Settings window at a category.
    Preferences(crate::ui::Category),
    ShowShortcuts,
    ShowAbout,
    Quit,
    TogglePause,
    Stop,
    Previous,
    Next,
    /// Hides the bars until the mouse moves.
    HideUi,
    /// Into the mini player window, or back out of it.
    MiniPlayer,
    /// Opens Customize Player.
    CustomizePlayer,
    /// Saves the frame on screen as a picture.
    Snapshot,
    /// Starts recording what plays, or stops.
    Record,
    /// Starts streaming what plays to other devices, or stops.
    Stream,
    /// Plays a file and streams it.
    StreamFile(PathBuf),
    /// Plays a capture device: its address and its name.
    OpenCapture(String, String),
    /// Converts a file into an MP4 or an MP3.
    Convert(PathBuf, Format),
    /// Converts the open file into an MP4 (Ctrl+R).
    ConvertOpen,
    CancelConvert,
    /// Switches to the named settings profile.
    Profile(String),
    /// Asks for a name and adds a profile.
    NewProfile,
    /// A change the tracks and settings panels also make.
    Panel(PanelAction),
    /// A change the queue panel also makes.
    Queue(crate::ui::queue::QueueAction),
    ShowQueue,
    ToggleRepeat,
    ToggleShuffle,
    /// A library file: plays it with the page it is on as the queue.
    Play(PathBuf),
    AddToQueue(Vec<PathBuf>),
    AddToPlaylist(String, Vec<PathBuf>),
    NewPlaylist(Vec<PathBuf>),
    ShowInExplorer(PathBuf),
    /// Opens the file's folder in the library.
    GoToSource(PathBuf),
    /// Picks a subtitle file and plays the file with it.
    AddSubtitleTo(PathBuf),
    /// Picks an audio file and plays the file with its sound.
    AddAudioTo(PathBuf),
    /// Takes a saved stream or podcast off the Network page.
    ForgetAddress(NetKind, String),
    /// The properties of a file or folder.
    Properties(PathBuf),
    MarkWatched(PathBuf),
    Favorite(PathBuf),
    /// Picks a folder and adds it to a library section.
    AddFolder(crate::library::Section),
    /// Reads the page's folders again.
    Resync,
    PlayPlaylist(String),
    ShufflePlaylist(String),
    ExportPlaylist(String),
    FavoritePlaylist(String),
    RenamePlaylist(String),
    DeletePlaylist(String),
    /// Takes a file out of the playlist page it was right-clicked on.
    RemoveFromPlaylist(String, PathBuf),
    Sort(SortMode),
}

/// Which menu: it sets the width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Main,
    Player,
    File,
    Folder,
    Playlist,
    Sort,
}

impl Kind {
    fn width(self) -> f32 {
        match self {
            Kind::Main => 375.0,
            Kind::Player => 290.0,
            Kind::File | Kind::Folder | Kind::Playlist | Kind::Sort => 265.0,
        }
    }
}

macro_rules! menu_icons {
    ($($name:ident = $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Icon { $($name),* }
        /// The menu icons, in `Icon` order.
        fn load_icons(ctx: &ID2D1DeviceContext5) -> Result<Vec<Svg>> {
            Ok(vec![$(svg(ctx, include_bytes!(concat!("../../assets/icons/", $file)))?),*])
        }
    };
}

menu_icons! {
    OpenFile = "menu_open_file.svg",
    Folder = "menu_folder.svg",
    AddFolder = "menu_add_folder.svg",
    Network = "menu_network.svg",
    Disc = "menu_disc.svg",
    Capture = "menu_capture.svg",
    Convert = "menu_convert.svg",
    Recent = "menu_recent.svg",
    Profile = "menu_profile.svg",
    Keyboard = "menu_keyboard.svg",
    Brush = "menu_brush.svg",
    Settings = "menu_settings.svg",
    Info = "menu_info.svg",
    Play = "menu_play.svg",
    Stop = "menu_stop.svg",
    Previous = "menu_previous.svg",
    Next = "menu_next.svg",
    Record = "menu_record.svg",
    HideUi = "menu_hide_ui.svg",
    Mini = "menu_mini.svg",
    Playback = "menu_playback.svg",
    Video = "menu_video.svg",
    Audio = "menu_audio.svg",
    Subtitle = "menu_subtitle.svg",
    Playlist = "menu_playlist.svg",
    Quit = "menu_quit.svg",
    Watched = "menu_watched.svg",
    Heart = "menu_heart.svg",
    Queue = "menu_queue.svg",
    Shuffle = "menu_shuffle.svg",
    Save = "menu_save.svg",
    Resync = "menu_resync.svg",
    Add = "nav_add.svg",
    GoTo = "arrow_right.svg",
    Delete = "menu_delete.svg",
}

/// One row of a menu, or a divider line.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub label: String,
    pub icon: Option<Icon>,
    /// Shortcut text at the right, empty for none.
    pub key: &'static str,
    pub action: Option<MenuAction>,
    pub sub: Vec<Item>,
    /// Draws the submenu chevron, even while disabled.
    pub opens: bool,
    pub checked: bool,
    pub enabled: bool,
    pub divider: bool,
}

impl Item {
    pub fn new(label: impl Into<String>, action: MenuAction) -> Self {
        Item {
            label: label.into(),
            icon: None,
            key: "",
            action: Some(action),
            sub: Vec::new(),
            opens: false,
            checked: false,
            enabled: true,
            divider: false,
        }
    }

    /// A row that opens `items`; disabled when there are none.
    pub fn submenu(label: impl Into<String>, items: Vec<Item>) -> Self {
        Item {
            action: None,
            enabled: !items.is_empty(),
            sub: items,
            opens: true,
            ..Item::new(label, MenuAction::Quit)
        }
    }

    /// A row drawn dimmed that does nothing: something myvlc can't do yet.
    pub fn off(label: impl Into<String>) -> Self {
        Item {
            action: None,
            enabled: false,
            ..Item::new(label, MenuAction::Quit)
        }
    }

    pub fn divider() -> Self {
        Item {
            divider: true,
            ..Item::off("")
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn key(mut self, key: &'static str) -> Self {
        self.key = key;
        self
    }

    pub fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }

    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on && (self.action.is_some() || !self.sub.is_empty());
        self
    }

    /// Hover, keyboard and clicks reach it.
    fn live(&self) -> bool {
        self.enabled && !self.divider
    }

    fn height(&self) -> f32 {
        if self.divider { 1.0 } else { ROW }
    }
}

/// Keys an open menu takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
}

/// What the pointer is over in an open menu. Levels count from the top menu, 0, into submenus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// A row that isn't a divider, disabled ones included: they swallow the click.
    Row(usize, usize),
    /// The padding or a divider of a level.
    Body(usize),
}

impl Target {
    fn level(self) -> usize {
        match self {
            Target::Row(level, _) | Target::Body(level) => level,
        }
    }
}

#[derive(Clone, Debug)]
struct Open {
    kind: Kind,
    items: Vec<Item>,
    at: (f32, f32),
    /// The row of each level whose submenu is open; levels = path.len() + 1.
    path: Vec<usize>,
    /// The highlighted row of each level.
    active: Vec<Option<usize>>,
    scroll: Vec<f32>,
    /// A submenu swap waiting for the pointer to rest: the level, the row whose submenu opens
    /// (None just closes the level's submenu) and since when.
    pending: Option<(usize, Option<usize>, Instant)>,
}

impl Open {
    fn level_items(&self, level: usize) -> &[Item] {
        let mut items = &self.items[..];
        for &row in &self.path[..level] {
            items = &items[row].sub;
        }
        items
    }

    /// Closes submenus deeper than `level`.
    fn truncate(&mut self, level: usize) {
        self.path.truncate(level);
        self.active.truncate(level + 1);
        self.scroll.truncate(level + 1);
    }

    /// Opens the submenu of `row` on `level`, closing any other submenu from that level.
    fn open_sub(&mut self, level: usize, row: usize) {
        self.truncate(level);
        self.active[level] = Some(row);
        self.path.push(row);
        self.active.push(None);
        self.scroll.push(0.0);
        self.pending = None;
    }

    /// The next live row on `level` from `from`, stepping by `step` (1 or -1) and wrapping.
    fn step(&self, level: usize, from: Option<usize>, step: isize) -> Option<usize> {
        let items = self.level_items(level);
        let n = items.len() as isize;
        let mut i = from.map_or(if step > 0 { -1 } else { n }, |i| i as isize);
        for _ in 0..n {
            i = (i + step).rem_euclid(n);
            if items[i as usize].live() {
                return Some(i as usize);
            }
        }
        None
    }

    /// The level keys act on: the deepest one with a highlighted row.
    fn focus(&self) -> usize {
        (0..self.active.len())
            .rev()
            .find(|&l| self.active[l].is_some())
            .unwrap_or(0)
    }
}

/// The open menu, if any; lives in `Ui`.
#[derive(Clone, Debug, Default)]
pub struct State {
    open: Option<Open>,
}

impl State {
    /// Opens `items` with its top-left corner at (x, y), or flipped to stay in the window.
    pub fn open(&mut self, kind: Kind, items: Vec<Item>, x: f32, y: f32) {
        // The sorting options open on the current choice, as in the design.
        let active = (kind == Kind::Sort)
            .then(|| items.iter().position(|i| i.checked))
            .flatten();
        self.open = Some(Open {
            kind,
            items,
            at: (x, y),
            path: Vec::new(),
            active: vec![active],
            scroll: vec![0.0],
            pending: None,
        });
    }

    pub fn close(&mut self) {
        self.open = None;
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The pointer moved over `target`, or off the menu. True when something changed.
    pub fn hover(&mut self, target: Option<Target>, now: Instant) -> bool {
        let Some(o) = self.open.as_mut() else {
            return false;
        };
        let before = (o.path.clone(), o.active.clone(), o.pending.is_some());
        match target {
            // A stale target from before the menu changed.
            Some(t) if t.level() > o.path.len() => {}
            Some(t) => {
                let level = t.level();
                // Inside a submenu: its parent rows stay chosen and a pending swap is off.
                for l in 0..level {
                    o.active[l] = Some(o.path[l]);
                }
                if o.pending.is_some_and(|(l, ..)| l < level) {
                    o.pending = None;
                }
                match t {
                    Target::Body(_) => {}
                    Target::Row(level, row) => {
                        let item = &o.level_items(level)[row];
                        let (live, opens) = (item.live(), item.opens && item.live());
                        let open_here = o.path.get(level).copied();
                        if open_here.is_none() || live {
                            o.active[level] = live.then_some(row);
                        }
                        match open_here {
                            Some(r) if r == row => o.pending = None,
                            None if opens => o.open_sub(level, row),
                            None => {}
                            Some(_) => {
                                let next = opens.then_some(row);
                                if o.pending.map(|(l, r, _)| (l, r)) != Some((level, next)) {
                                    o.pending = Some((level, next, now));
                                }
                            }
                        }
                    }
                }
            }
            // Off the menu: forget the highlight, and close submenus once the pointer stays away.
            None => {
                let deepest = o.active.len() - 1;
                o.active[deepest] = None;
                if !o.path.is_empty() && o.pending.is_none_or(|(l, ..)| l != 0) {
                    o.pending = Some((0, None, now));
                }
            }
        }
        before != (o.path.clone(), o.active.clone(), o.pending.is_some())
    }

    /// Applies a pending submenu swap once its delay has passed. True when it did.
    pub fn tick(&mut self, now: Instant) -> bool {
        let Some(o) = self.open.as_mut() else {
            return false;
        };
        let Some((level, row, since)) = o.pending else {
            return false;
        };
        if now.duration_since(since) < SWITCH_DELAY {
            return false;
        }
        o.pending = None;
        match row {
            Some(row) => o.open_sub(level, row),
            None => {
                o.truncate(level);
                if level == 0 {
                    o.active[0] = None;
                }
            }
        }
        true
    }

    /// A click on the menu: opens a submenu, or returns the row's action and closes the menu.
    pub fn click(&mut self, target: Target) -> Option<MenuAction> {
        let o = self.open.as_mut()?;
        let Target::Row(level, row) = target else {
            return None;
        };
        let item = o.level_items(level).get(row)?;
        if !item.live() {
            return None;
        }
        if item.opens {
            o.open_sub(level, row);
            return None;
        }
        let action = item.action.clone();
        self.open = None;
        action
    }

    pub fn key(&mut self, key: Key) -> Option<MenuAction> {
        let o = self.open.as_mut()?;
        o.pending = None;
        let level = o.focus();
        let current = o.active[level];
        match key {
            Key::Up | Key::Down => {
                let step = if key == Key::Down { 1 } else { -1 };
                // Moving off a row closes its submenu.
                o.truncate(level);
                o.active[level] = o.step(level, current, step);
                None
            }
            Key::Right | Key::Enter => {
                let row = current?;
                let item = &o.level_items(level)[row];
                if item.opens {
                    o.open_sub(level, row);
                    let first = o.step(level + 1, None, 1);
                    o.active[level + 1] = first;
                    None
                } else if key == Key::Enter {
                    self.click(Target::Row(level, row))
                } else {
                    None
                }
            }
            Key::Left => {
                if level > 0 {
                    o.truncate(level - 1);
                }
                None
            }
            Key::Escape => {
                if o.path.is_empty() {
                    self.open = None;
                } else {
                    let deepest = o.path.len() - 1;
                    o.truncate(deepest);
                }
                None
            }
        }
    }

    /// Scrolls the level under (x, y) by `dy`, when it is taller than the window.
    pub fn scroll(&mut self, g: &Geometry, x: f32, y: f32, dy: f32) {
        let Some(o) = self.open.as_mut() else { return };
        if let Some(l) = g.levels.iter().rposition(|l| l.frame.contains(x, y)) {
            o.scroll[l] = (o.scroll[l] + dy).clamp(0.0, g.levels[l].max_scroll);
        }
    }

    /// Scrolls the keyboard's row into view. True when it moved.
    pub fn reveal(&mut self, g: &Geometry) -> bool {
        let Some(o) = self.open.as_mut() else {
            return false;
        };
        let level = o.focus();
        let (Some(row), Some(l)) = (o.active[level], g.levels.get(level)) else {
            return false;
        };
        let Some((_, r)) = l.rows.iter().find(|(i, _)| *i == row) else {
            return false;
        };
        let dy = if r.t < l.clip.t {
            r.t - l.clip.t
        } else if r.b > l.clip.b {
            r.b - l.clip.b
        } else {
            return false;
        };
        o.scroll[level] = (o.scroll[level] + dy).clamp(0.0, l.max_scroll);
        true
    }
}

/// Where a `w` x `h` menu opened at (x, y) goes: right of and below the point, flipped left or up
/// where it would leave `bounds`, and pushed inside when it fits neither way. A menu taller than
/// `bounds` gets their height and scrolls.
pub fn place(x: f32, y: f32, w: f32, h: f32, bounds: Rect) -> Rect {
    let h = h.min(bounds.h());
    let l = if x + w <= bounds.r {
        x
    } else if x - w >= bounds.l {
        x - w
    } else {
        (bounds.r - w).max(bounds.l)
    };
    let t = if y + h <= bounds.b {
        y
    } else if y - h >= bounds.t {
        y - h
    } else {
        (bounds.b - h).max(bounds.t)
    };
    Rect::new(l, t, l + w, t + h)
}

/// A submenu beside `parent` with its first row level with `row`: on the right, or on the left
/// when the right has no room. It slides up rather than flipping when it would run off the bottom.
pub fn place_sub(parent: Rect, row: Rect, w: f32, h: f32, bounds: Rect) -> Rect {
    let h = h.min(bounds.h());
    let right = parent.r + SUB_GAP;
    let left = parent.l - SUB_GAP - w;
    let l = if right + w <= bounds.r {
        right
    } else if left >= bounds.l {
        left
    } else {
        (bounds.r - w).max(bounds.l)
    };
    let t = (row.t - PAD).min(bounds.b - h).max(bounds.t);
    Rect::new(l, t, l + w, t + h)
}

#[derive(Clone, Debug)]
struct Level {
    frame: Rect,
    /// Where rows show: the frame minus its padding.
    clip: Rect,
    /// Rows by item index, already moved by the level's scroll.
    rows: Vec<(usize, Rect)>,
    items: Vec<Item>,
    /// No icons on this level: labels start at the left.
    plain: bool,
    active: Option<usize>,
    max_scroll: f32,
}

/// Where the open menu's levels sit; part of `Layout`.
#[derive(Clone, Default, Debug)]
pub struct Geometry {
    levels: Vec<Level>,
}

impl Geometry {
    pub fn new(state: &State, width: f32, height: f32) -> Geometry {
        let Some(o) = &state.open else {
            return Geometry::default();
        };
        let bounds = Rect::new(EDGE, EDGE, width - EDGE, height - EDGE);
        let mut levels: Vec<Level> = Vec::new();
        for level in 0..=o.path.len() {
            let items = o.level_items(level);
            let content = 2.0 * PAD + items.iter().map(Item::height).sum::<f32>();
            let frame = match levels.last() {
                None => place(o.at.0, o.at.1, o.kind.width(), content, bounds),
                Some(parent) => {
                    let row = o.path[level - 1];
                    let r = parent.rows.iter().find(|(i, _)| *i == row).map(|(_, r)| *r);
                    place_sub(
                        parent.frame,
                        r.unwrap_or(parent.frame),
                        SUB_W,
                        content,
                        bounds,
                    )
                }
            };
            let max_scroll = (content - frame.h()).max(0.0);
            let scroll = o.scroll[level].clamp(0.0, max_scroll);
            let mut y = frame.t + PAD - scroll;
            let rows = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let r = Rect::new(frame.l + PAD, y, frame.r - PAD, y + item.height());
                    y = r.b;
                    (i, r)
                })
                .collect();
            levels.push(Level {
                frame,
                clip: Rect::new(frame.l, frame.t + PAD, frame.r, frame.b - PAD),
                rows,
                items: items
                    .iter()
                    .map(|i| Item {
                        sub: Vec::new(),
                        ..i.clone()
                    })
                    .collect(),
                plain: items.iter().all(|i| i.icon.is_none()),
                active: o.active[level],
                max_scroll,
            });
        }
        Geometry { levels }
    }

    /// The deepest level under (x, y) wins: submenus sit over their parents.
    pub fn hit(&self, x: f32, y: f32) -> Option<Target> {
        let (n, level) = self
            .levels
            .iter()
            .enumerate()
            .rev()
            .find(|(_, l)| l.frame.contains(x, y))?;
        let row = level
            .rows
            .iter()
            .filter(|(i, r)| !level.items[*i].divider && r.contains(x, y))
            .find(|_| level.clip.contains(x, y));
        Some(row.map_or(Target::Body(n), |(i, _)| Target::Row(n, *i)))
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.levels.iter().any(|l| l.frame.contains(x, y))
    }
}

/// The title bar's "..." menu. `recent` is the newest first; `profile` is the one in force.
/// What the main menu's Convert offers.
pub enum Converting<'a> {
    /// Nothing open that can be converted.
    Nothing,
    /// The open file.
    File(&'a Path),
    /// A conversion is running; Convert becomes Stop Converting.
    Running,
}

pub fn main_menu(
    recent: &[PathBuf],
    profiles: &[String],
    profile: &str,
    devices: &[crate::capture::Device],
    converting: Converting,
) -> Vec<Item> {
    // A submenu row has its arrow where the shortcut would go.
    let convert = match converting {
        Converting::Nothing => Item::off("Convert").key("Ctrl + R"),
        Converting::File(path) => Item::submenu("Convert", convert_menu(path)),
        Converting::Running => Item::new("Stop Converting", MenuAction::CancelConvert),
    };
    let devices = devices
        .iter()
        .map(|d| {
            Item::new(
                d.name.clone(),
                MenuAction::OpenCapture(d.address.clone(), d.name.clone()),
            )
        })
        .collect();
    let recent = recent
        .iter()
        .take(10)
        .map(|p| Item::new(file_name(p), MenuAction::PlayRecent(p.clone())))
        .collect();
    let mut profile_items: Vec<Item> = (profiles.iter())
        .map(|name| {
            Item::new(name.clone(), MenuAction::Profile(name.clone())).checked(name == profile)
        })
        .collect();
    profile_items.push(Item::divider());
    profile_items.push(Item::new("New", MenuAction::NewProfile).icon(Icon::Add));
    vec![
        Item::new("Open File(s)", MenuAction::OpenFiles)
            .icon(Icon::OpenFile)
            .key("Ctrl + O"),
        Item::new("Open Folder", MenuAction::OpenFolder)
            .icon(Icon::Folder)
            .key("Ctrl + F"),
        Item::new("Open Network Stream", MenuAction::OpenNetwork)
            .icon(Icon::Network)
            .key("Ctrl + N"),
        // ponytail: discs need libdvdnav and libbluray, which the FFmpeg build leaves out.
        Item::off("Open Disc").icon(Icon::Disc).key("Ctrl + D"),
        Item::submenu("Open Capture Device", devices).icon(Icon::Capture),
        convert.icon(Icon::Convert),
        Item::submenu("Open Recent", recent).icon(Icon::Recent),
        Item::divider(),
        Item::submenu(format!("Profile ({profile})"), profile_items).icon(Icon::Profile),
        Item::new("Keyboard Shortcuts", MenuAction::ShowShortcuts).icon(Icon::Keyboard),
        Item::new("Customize Player", MenuAction::CustomizePlayer).icon(Icon::Brush),
        Item::new(
            "Settings",
            MenuAction::Preferences(crate::ui::Category::Playback),
        )
        .icon(Icon::Settings)
        .key("Ctrl + P"),
        Item::new("About myvlc", MenuAction::ShowAbout)
            .icon(Icon::Info)
            .key("Shift + F1"),
    ]
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or(p.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// "None" and the tracks of one kind, the chosen one checked.
fn tracks(tab: TrackTab, names: &[String], selected: Option<usize>) -> Vec<Item> {
    let pick = |i| MenuAction::Panel(PanelAction::SelectTrack(tab, i));
    let mut items = vec![Item::new("None", pick(None)).checked(selected.is_none())];
    items.extend(
        names
            .iter()
            .enumerate()
            .map(|(i, name)| Item::new(name.clone(), pick(Some(i))).checked(selected == Some(i))),
    );
    items
}

/// A list of choices, each a panel action, the current one checked.
fn choices(names: &[&str], current: usize, action: fn(usize) -> PanelAction) -> Vec<Item> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| Item::new(*name, MenuAction::Panel(action(i))).checked(i == current))
        .collect()
}

/// The player's right-click menu, from what the panels show.
pub fn player_menu(d: &PanelData, paused: bool) -> Vec<Item> {
    let panel = MenuAction::Panel;
    let speeds = [
        (0.5, "0.5x"),
        (0.75, "0.75x"),
        (1.0, "Normal"),
        (1.25, "1.25x"),
        (1.5, "1.5x"),
        (2.0, "2x"),
    ];
    let mut playback: Vec<Item> = speeds
        .iter()
        .map(|&(v, label)| {
            Item::new(label, panel(PanelAction::SetSpeed(v))).checked((d.speed - v).abs() < 0.01)
        })
        .collect();
    playback.push(Item::divider());
    playback.push(Item::submenu(
        "Sleep Timer",
        choices(&SLEEP, d.sleep_timer, PanelAction::SetSleepTimer),
    ));

    // Flip and rotation work as toggles: choosing the current one turns it off.
    let flip = |k: u8, label| {
        let on = d.flip == k;
        Item::new(label, panel(PanelAction::SetFlip(if on { 0 } else { k }))).checked(on)
    };
    let rotate = |deg: i32, label| {
        let on = d.rotate == deg;
        Item::new(
            label,
            panel(PanelAction::SetRotate(if on { 0 } else { deg })),
        )
        .checked(on)
    };
    let video = vec![
        Item::submenu(
            "Video Track",
            tracks(TrackTab::Video, &d.video_tracks, d.video_selected),
        ),
        Item::new("Take Snapshot", MenuAction::Snapshot).key("Shift + S"),
        Item::divider(),
        flip(1, "Flip Horizontally"),
        flip(2, "Flip Vertically"),
        Item::divider(),
        rotate(90, "Rotate 90\u{b0}"),
        rotate(180, "Rotate 180\u{b0}"),
        rotate(270, "Rotate 270\u{b0}"),
        Item::divider(),
        Item::submenu("Zoom", choices(&ZOOM, d.zoom, PanelAction::SetZoom)),
        Item::submenu(
            "Aspect Ratio",
            choices(&ASPECT, d.aspect, PanelAction::SetAspect),
        ),
        Item::submenu("Crop", choices(&CROP, d.crop, PanelAction::SetCrop)),
    ];

    let devices = d
        .devices
        .iter()
        .enumerate()
        .map(|(i, name)| {
            Item::new(name.clone(), panel(PanelAction::SelectDevice(i)))
                .checked(i == d.device_selected)
        })
        .collect();
    let audio = vec![
        Item::new("Add Audio File...", panel(PanelAction::AddAudioFile)),
        Item::divider(),
        Item::submenu(
            "Audio Track",
            tracks(TrackTab::Audio, &d.audio_tracks, d.audio_selected),
        ),
        Item::submenu(
            "Stereo Mode",
            choices(&STEREO, d.stereo, PanelAction::SetStereo),
        ),
        Item::submenu("Audio Device", devices),
    ];

    let none = d.sub_selected.is_none() && d.external_selected.is_none();
    let mut subtitle = vec![
        Item::new("Add Subtitle File...", panel(PanelAction::AddSubtitleFile)),
        Item::divider(),
    ];
    subtitle.extend(tracks(TrackTab::Subtitles, &d.sub_tracks, d.sub_selected));
    subtitle[2].checked = none;
    subtitle.extend(d.external_subs.iter().enumerate().map(|(i, name)| {
        Item::new(name.clone(), panel(PanelAction::SelectExternal(Some(i))))
            .checked(d.external_selected == Some(i))
    }));

    let tabs = [
        ("Playback", SettingsTab::Playback),
        ("Video", SettingsTab::Video),
        ("Audio", SettingsTab::Audio),
        ("Subtitles", SettingsTab::Subtitles),
    ];
    let settings = tabs
        .iter()
        .map(|&(label, tab)| Item::new(label, MenuAction::Settings(tab)))
        .collect();
    let playlist = vec![
        Item::new("Show Queue", MenuAction::ShowQueue).icon(Icon::Queue),
        Item::new("Repeat", MenuAction::ToggleRepeat).icon(Icon::Resync),
        Item::new("Shuffle", MenuAction::ToggleShuffle).icon(Icon::Shuffle),
    ];
    let open = vec![
        Item::new("Open File(s)", MenuAction::OpenFiles)
            .icon(Icon::OpenFile)
            .key("Ctrl + O"),
        Item::new("Open Folder", MenuAction::OpenFolder)
            .icon(Icon::Folder)
            .key("Ctrl + F"),
    ];

    vec![
        Item::new(
            if paused { "Play" } else { "Pause" },
            MenuAction::TogglePause,
        )
        .icon(Icon::Play)
        .key("Space"),
        Item::new("Stop", MenuAction::Stop)
            .icon(Icon::Stop)
            .key("S"),
        Item::new("Previous Item", MenuAction::Previous)
            .icon(Icon::Previous)
            .key("P"),
        Item::new("Next Item", MenuAction::Next)
            .icon(Icon::Next)
            .key("N"),
        Item::new(
            if d.recording {
                "Stop Recording"
            } else {
                "Record"
            },
            MenuAction::Record,
        )
        .icon(Icon::Record)
        .key("R"),
        match &d.streaming {
            None => Item::new("Stream to Other Devices", MenuAction::Stream),
            // The address to open elsewhere, shown while it serves.
            Some((url, players)) => Item::submenu(
                "Streaming",
                vec![
                    Item::off(url.clone()),
                    Item::off(match players {
                        0 => "No players yet".to_owned(),
                        1 => "1 player connected".to_owned(),
                        n => format!("{n} players connected"),
                    }),
                    Item::new("Stop Streaming", MenuAction::Stream),
                ],
            ),
        }
        .icon(Icon::Network),
        Item::divider(),
        Item::new("Hide UI", MenuAction::HideUi)
            .icon(Icon::HideUi)
            .key("Ctrl + \\"),
        Item::new("Mini Player", MenuAction::MiniPlayer)
            .icon(Icon::Mini)
            .key("Ctrl + M"),
        // Here too, since Customize Player can take the "..." button out of the bars.
        Item::new("Customize Player", MenuAction::CustomizePlayer).icon(Icon::Brush),
        Item::divider(),
        Item::submenu("Playback", playback).icon(Icon::Playback),
        Item::submenu("Video", video).icon(Icon::Video),
        Item::submenu("Audio", audio).icon(Icon::Audio),
        Item::submenu("Subtitle", subtitle).icon(Icon::Subtitle),
        Item::submenu("More Settings", settings).icon(Icon::Settings),
        Item::submenu("Playlist", playlist).icon(Icon::Playlist),
        Item::submenu("Open Media", open).icon(Icon::OpenFile),
        Item::divider(),
        Item::new("Quit myvlc", MenuAction::Quit)
            .icon(Icon::Quit)
            .key("Ctrl + Q"),
    ]
}

/// The context menu of a library entry: files, streams, folders and playlists have their own.
/// `playlists` fills "Add to Playlist"; `items` are a playlist's files; `place` is the page the
/// entry is on: a playlist page adds "Remove from Playlist", the Network page "Remove".
pub fn entry_menu(
    entry: &Entry,
    playlists: &[String],
    items: &[PathBuf],
    place: &Place,
) -> Option<(Kind, Vec<Item>)> {
    let within = match place {
        Place::Playlist(name) => Some(name.as_str()),
        _ => None,
    };
    match entry {
        Entry::Media { path, .. } if is_address(path) => {
            let saved = *place == Place::Network;
            Some((Kind::File, stream_menu(path, playlists, saved)))
        }
        Entry::Media { path, .. } => Some((Kind::File, file_menu(path, playlists, within))),
        Entry::Folder {
            place: Place::Playlist(name),
            ..
        } => Some((Kind::Playlist, playlist_menu(name, items))),
        Entry::Folder {
            place: Place::Podcast(url),
            ..
        } => {
            let forget = MenuAction::ForgetAddress(NetKind::Podcast, url.clone());
            Some((
                Kind::Folder,
                vec![Item::new("Remove Podcast", forget).icon(Icon::Delete)],
            ))
        }
        Entry::Folder {
            place: Place::Podcasts | Place::Radio,
            ..
        } => None,
        Entry::Folder { place, .. } => Some((Kind::Folder, folder_menu(place))),
        Entry::Header { .. } | Entry::Action { .. } => None,
    }
}

/// "Add to Playlist"'s submenu for `path`.
fn add_to_playlist(path: &Path, playlists: &[String]) -> Vec<Item> {
    let one = || vec![path.to_owned()];
    let mut add: Vec<Item> = playlists
        .iter()
        .map(|name| Item::new(name.clone(), MenuAction::AddToPlaylist(name.clone(), one())))
        .collect();
    if !add.is_empty() {
        add.push(Item::divider());
    }
    add.push(Item::new("New Playlist...", MenuAction::NewPlaylist(one())));
    add
}

/// A stream address: no file to show, probe or mark watched. A `saved` one can be removed.
fn stream_menu(path: &Path, playlists: &[String], saved: bool) -> Vec<Item> {
    let mut items = vec![
        Item::new("Play", MenuAction::Play(path.to_owned())).icon(Icon::Play),
        Item::new(
            "Add to Queue",
            MenuAction::AddToQueue(vec![path.to_owned()]),
        )
        .icon(Icon::Queue),
        Item::submenu("Add to Playlist", add_to_playlist(path, playlists)).icon(Icon::Playlist),
    ];
    if saved {
        let url = path.to_string_lossy().into_owned();
        let forget = MenuAction::ForgetAddress(NetKind::Stream, url);
        items.push(Item::new("Remove", forget).icon(Icon::Delete));
    }
    items
}

/// Convert's choices for `path`.
fn convert_menu(path: &Path) -> Vec<Item> {
    vec![
        Item::new(
            "MP4 (H.264 + AAC)",
            MenuAction::Convert(path.to_owned(), Format::Mp4),
        ),
        Item::new(
            "MP3 (sound only)",
            MenuAction::Convert(path.to_owned(), Format::Mp3),
        ),
    ]
}

fn file_menu(path: &Path, playlists: &[String], within: Option<&str>) -> Vec<Item> {
    let one = || vec![path.to_owned()];
    let add = add_to_playlist(path, playlists);
    let mut items = vec![
        Item::new("Play", MenuAction::Play(path.to_owned())).icon(Icon::Play),
        Item::new("Mark as Watched", MenuAction::MarkWatched(path.to_owned())).icon(Icon::Watched),
        Item::new("Favorite", MenuAction::Favorite(path.to_owned())).icon(Icon::Heart),
        Item::new("Stream", MenuAction::StreamFile(path.to_owned())).icon(Icon::Network),
        Item::submenu("Convert", convert_menu(path)).icon(Icon::Convert),
        Item::new("Add to Queue", MenuAction::AddToQueue(one())).icon(Icon::Queue),
        Item::submenu("Add to Playlist", add).icon(Icon::Playlist),
        Item::new(
            "Show in File Explorer",
            MenuAction::ShowInExplorer(path.to_owned()),
        )
        .icon(Icon::Folder),
        Item::new("Go to Source", MenuAction::GoToSource(path.to_owned())).icon(Icon::GoTo),
        Item::new(
            "Add Subtitle Tracks",
            MenuAction::AddSubtitleTo(path.to_owned()),
        )
        .icon(Icon::Subtitle),
        Item::new("Add Audio Tracks", MenuAction::AddAudioTo(path.to_owned())).icon(Icon::Audio),
        Item::new(
            "Properties & Metadata",
            MenuAction::Properties(path.to_owned()),
        )
        .icon(Icon::Info),
    ];
    if let Some(name) = within {
        let remove = MenuAction::RemoveFromPlaylist(name.to_owned(), path.to_owned());
        items.insert(
            7,
            Item::new("Remove from Playlist", remove).icon(Icon::Delete),
        );
    }
    items
}

/// The menu of queue row `i`, the design's file menu for `path`: Play plays that row, "Play Next"
/// queues it after the one playing (`current`), and "Remove from Queue" takes Add to Queue's place.
pub fn queue_menu(
    i: usize,
    path: &Path,
    current: Option<usize>,
    playlists: &[String],
) -> Vec<Item> {
    use crate::ui::queue::QueueAction;
    let mut items = if is_address(path) {
        stream_menu(path, playlists, false)
    } else {
        file_menu(path, playlists, None)
    };
    items[0].action = Some(MenuAction::Queue(QueueAction::Play(i)));
    if let Some(add) = items.iter_mut().find(|item| item.label == "Add to Queue") {
        *add = Item::new(
            "Remove from Queue",
            MenuAction::Queue(QueueAction::Remove(i)),
        )
        .icon(Icon::Delete);
    }
    if current.is_some_and(|c| c != i) {
        let play_next = MenuAction::Queue(QueueAction::PlayNext(i));
        items.insert(1, Item::new("Play Next", play_next).icon(Icon::Next));
    }
    items
}

fn folder_menu(place: &Place) -> Vec<Item> {
    let dir = match place {
        Place::Folder(_, dir) => Some(dir.clone()),
        _ => None,
    };
    let info = match dir {
        Some(dir) => Item::new("Info", MenuAction::Properties(dir)),
        None => Item::off("Info"),
    };
    vec![
        Item::new("Add File", MenuAction::OpenFiles).icon(Icon::Add),
        Item::new("Add Folder", MenuAction::AddFolder(place.section())).icon(Icon::AddFolder),
        // ponytail: editing a folder's tracks needs per-folder track settings, which don't exist.
        Item::off("Edit Subtitle Tracks").icon(Icon::Subtitle),
        Item::off("Edit Audio Tracks").icon(Icon::Audio),
        Item::new("Resync", MenuAction::Resync).icon(Icon::Resync),
        info.icon(Icon::Info),
    ]
}

fn playlist_menu(name: &str, items: &[PathBuf]) -> Vec<Item> {
    let has = !items.is_empty();
    let name = || name.to_owned();
    vec![
        Item::new("Play", MenuAction::PlayPlaylist(name()))
            .icon(Icon::Play)
            .enabled(has),
        Item::new("Favorite", MenuAction::FavoritePlaylist(name())).icon(Icon::Heart),
        Item::new("Shuffle", MenuAction::ShufflePlaylist(name()))
            .icon(Icon::Shuffle)
            .enabled(has),
        Item::new("Export Playlist", MenuAction::ExportPlaylist(name())).icon(Icon::Save),
        Item::new("Add to Queue", MenuAction::AddToQueue(items.to_vec()))
            .icon(Icon::Queue)
            .enabled(has),
        // ponytail: subtitle and audio tracks for a whole playlist have nowhere to live yet.
        Item::off("Add Subtitle Tracks").icon(Icon::Subtitle),
        Item::off("Add Audio Tracks").icon(Icon::Audio),
        Item::new("Properties & Metadata", MenuAction::RenamePlaylist(name())).icon(Icon::Info),
        Item::divider(),
        Item::new("Delete Playlist", MenuAction::DeletePlaylist(name())).icon(Icon::Delete),
    ]
}

/// The sorting options, the current one checked. The artist sorts need files with artists.
pub fn sort_menu(current: SortMode, artists: bool, albums: bool) -> Vec<Item> {
    SortMode::ALL
        .iter()
        .map(|&m| {
            let usable = match m {
                SortMode::ArtistAz | SortMode::ArtistZa => artists,
                SortMode::AlbumAz | SortMode::AlbumZa => albums,
                _ => true,
            };
            Item::new(m.label(), MenuAction::Sort(m))
                .checked(m == current)
                .enabled(usable)
        })
        .collect()
}

/// Brushes, fonts and icons the menus use.
pub(super) struct Resources {
    panel: ID2D1SolidColorBrush,
    hover: ID2D1SolidColorBrush,
    shadow: ID2D1SolidColorBrush,
    line: ID2D1SolidColorBrush,
    label: ID2D1SolidColorBrush,
    label_on: ID2D1SolidColorBrush,
    key: ID2D1SolidColorBrush,
    veil: ID2D1SolidColorBrush,
    text: IDWriteTextFormat,
    key_text: IDWriteTextFormat,
    icons: Vec<Svg>,
    chevron: Svg,
    check: Svg,
}

impl Resources {
    pub(super) fn new(
        ctx: &ID2D1DeviceContext5,
        dwrite: &IDWriteFactory6,
        fonts: &IDWriteFontCollection2,
    ) -> Result<Self> {
        let brush =
            |rgb: u32, alpha: f32| unsafe { ctx.CreateSolidColorBrush(&color(rgb, alpha), None) };
        let format = |align| text_format(dwrite, fonts, 14.0, DWRITE_FONT_WEIGHT_REGULAR, align);
        Ok(Resources {
            panel: brush(BG_PANEL, 1.0)?,
            hover: brush(0xFFFFFF, HOVER_ALPHA)?,
            shadow: brush(0x000000, 0.06)?,
            line: brush(LINE, 1.0)?,
            label: brush(FG_LABEL, 1.0)?,
            label_on: brush(LABEL_ON, 1.0)?,
            key: brush(FG_DIM, 1.0)?,
            veil: brush(BG_PANEL, 1.0 - DIM)?,
            text: format(DWRITE_TEXT_ALIGNMENT_LEADING)?,
            key_text: format(DWRITE_TEXT_ALIGNMENT_TRAILING)?,
            icons: load_icons(ctx)?,
            chevron: svg(ctx, include_bytes!("../../assets/icons/chevron_right.svg"))?,
            check: svg(ctx, include_bytes!("../../assets/icons/check.svg"))?,
        })
    }
}

impl Painter {
    /// The open menu over everything else.
    pub(super) unsafe fn menu(&self, ctx: &ID2D1DeviceContext5, s: &Scene) {
        let rt: &ID2D1RenderTarget = ctx;
        let m = &self.menu;
        for level in &s.layout.menu.levels {
            let f = level.frame;
            unsafe {
                // The design's soft shadow, darkest at the edge.
                for spread in [12.0, 10.0, 8.0, 6.0, 4.0, 2.0] {
                    rt.FillRoundedRectangle(&rounded(f.inset(-spread), RADIUS + spread), &m.shadow);
                }
                rt.FillRoundedRectangle(&rounded(f, RADIUS), &m.panel);
                rt.PushAxisAlignedClip(&level.clip.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
                for (i, r) in level.rows.iter().filter(|(_, r)| r.intersects(&level.clip)) {
                    self.menu_row(
                        rt,
                        ctx,
                        level,
                        &level.items[*i],
                        *r,
                        level.active == Some(*i),
                    );
                }
                rt.PopAxisAlignedClip();
            }
        }
    }

    unsafe fn menu_text(
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

    unsafe fn menu_row(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        level: &Level,
        item: &Item,
        r: Rect,
        active: bool,
    ) {
        let (m, f) = (&self.menu, level.frame);
        let cy = (r.t + r.b) / 2.0;
        unsafe {
            if item.divider {
                let line = Rect::new(f.l, r.t, f.r, r.b);
                rt.FillRectangle(&line.d2d(), &m.line);
                return;
            }
            let lit = active && item.enabled;
            if lit {
                rt.FillRoundedRectangle(&rounded(r, ROW_RADIUS), &m.hover);
            }
            if let Some(icon) = item.icon {
                self.icon(
                    ctx,
                    &m.icons[icon as usize],
                    f.l + ICON_X - 12.0,
                    cy - 12.0,
                    false,
                );
            }
            let mut right = f.r - 16.0;
            if !item.key.is_empty() {
                let kr = Rect::new(f.l + LABEL_X, r.t, f.r - KEY_R, r.b);
                self.menu_text(rt, item.key, &m.key_text, kr, &m.key);
                right = f.r - KEY_R - self.measure(item.key, &m.key_text) - 12.0;
            }
            // In a menu with icons, an icon-less row's check takes the icon's place, as in the
            // design's Profile submenu.
            let check_left = item.checked && item.icon.is_none() && !level.plain;
            if check_left {
                self.icon(ctx, &m.check, f.l + ICON_X - 12.0, cy - 12.0, false);
            }
            if item.opens {
                self.icon(ctx, &m.chevron, f.r - MARK_R - 12.0, cy - 12.0, false);
                right = f.r - MARK_R - 16.0;
            } else if item.checked && !check_left {
                self.icon(ctx, &m.check, f.r - MARK_R - 12.0, cy - 12.0, false);
                right = f.r - MARK_R - 16.0;
            }
            let x = if level.plain { PLAIN_X } else { LABEL_X };
            let brush = if lit || item.checked {
                &m.label_on
            } else {
                &m.label
            };
            self.menu_text(
                rt,
                &item.label,
                &m.text,
                Rect::new(f.l + x, r.t, right, r.b),
                brush,
            );
            // Dimmed: the panel over it at 1 - DIM, like drawing the row at DIM opacity.
            if !item.enabled {
                rt.FillRectangle(&r.d2d(), &m.veil);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Rect = Rect::new(0.0, 0.0, 1000.0, 700.0);

    #[test]
    fn menus_open_at_the_point_and_flip_at_the_edges() {
        // Room on both sides: right of and below the point.
        assert_eq!(
            place(100.0, 50.0, 265.0, 300.0, WINDOW),
            Rect::new(100.0, 50.0, 365.0, 350.0)
        );
        // Near the right edge it opens to the left, near the bottom upwards.
        assert_eq!(
            place(900.0, 600.0, 265.0, 300.0, WINDOW),
            Rect::new(635.0, 300.0, 900.0, 600.0)
        );
        // Fits neither way: pushed inside.
        assert_eq!(
            place(200.0, 350.0, 265.0, 500.0, WINDOW),
            Rect::new(200.0, 200.0, 465.0, 700.0)
        );
        // Taller than the window: as tall as the window, scrolling.
        assert_eq!(place(10.0, 10.0, 290.0, 900.0, WINDOW).h(), 700.0);
    }

    #[test]
    fn submenus_open_beside_their_row_and_flip_left() {
        let parent = Rect::new(100.0, 100.0, 390.0, 600.0);
        let row = Rect::new(104.0, 300.0, 386.0, 348.0);
        let sub = place_sub(parent, row, SUB_W, 200.0, WINDOW);
        assert_eq!(sub, Rect::new(392.0, 296.0, 657.0, 496.0));
        // At the right edge it goes left of the parent.
        let parent = Rect::new(700.0, 100.0, 990.0, 600.0);
        let sub = place_sub(parent, row, SUB_W, 200.0, WINDOW);
        assert_eq!(sub.r, 698.0);
        // Near the bottom it slides up to fit.
        let low = Rect::new(704.0, 640.0, 986.0, 688.0);
        assert_eq!(place_sub(parent, low, SUB_W, 200.0, WINDOW).b, 700.0);
    }

    fn sample() -> Vec<Item> {
        vec![
            Item::new("One", MenuAction::Stop),
            Item::off("Off"),
            Item::divider(),
            Item::submenu(
                "More",
                vec![
                    Item::new("A", MenuAction::Next),
                    Item::new("B", MenuAction::Previous),
                ],
            ),
            Item::submenu("Other", vec![Item::new("C", MenuAction::Quit)]),
        ]
    }

    #[test]
    fn layout_follows_the_design_rows() {
        let mut s = State::default();
        s.open(
            Kind::Main,
            main_menu(&[], &[], "Movies", &[], Converting::Nothing),
            51.0,
            38.0,
        );
        let g = Geometry::new(&s, 1600.0, 900.0);
        let level = &g.levels[0];
        // 12 rows of 48 and one divider, inside 4 DIPs of padding: the design's 375 x 585.
        assert_eq!(level.frame, Rect::new(51.0, 38.0, 426.0, 623.0));
        assert_eq!(g.hit(100.0, 38.0 + 4.0 + 24.0), Some(Target::Row(0, 0)));
        assert_eq!(g.hit(100.0, 38.0 + 4.0 + 7.0 * 48.0), Some(Target::Body(0)));
        assert_eq!(
            g.hit(100.0, 38.0 + 5.0 + 7.0 * 48.0),
            Some(Target::Row(0, 8))
        );
        assert_eq!(g.hit(40.0, 100.0), None);
    }

    #[test]
    fn hover_opens_submenus_and_keys_walk_them() {
        let mut s = State::default();
        s.open(Kind::File, sample(), 0.0, 0.0);
        let t0 = Instant::now();
        // Hovering a submenu row opens it at once.
        assert!(s.hover(Some(Target::Row(0, 3)), t0));
        assert_eq!(s.open.as_ref().unwrap().path, vec![3]);
        // Another submenu row swaps only after the delay; entering the open submenu cancels it.
        s.hover(Some(Target::Row(0, 4)), t0);
        assert!(!s.tick(t0 + Duration::from_millis(100)));
        s.hover(Some(Target::Row(1, 0)), t0 + Duration::from_millis(150));
        assert!(!s.tick(t0 + Duration::from_secs(1)));
        assert_eq!(s.open.as_ref().unwrap().path, vec![3]);
        s.hover(Some(Target::Row(0, 4)), t0);
        assert!(s.tick(t0 + Duration::from_secs(1)));
        assert_eq!(s.open.as_ref().unwrap().path, vec![4]);
        // Leaving the menu closes the submenu after the delay.
        s.hover(None, t0);
        assert!(s.tick(t0 + Duration::from_secs(1)));
        assert!(s.open.as_ref().unwrap().path.is_empty());

        // Down skips disabled rows and dividers; Right opens; Enter picks.
        let mut s = State::default();
        s.open(Kind::File, sample(), 0.0, 0.0);
        s.key(Key::Down);
        s.key(Key::Down);
        assert_eq!(s.open.as_ref().unwrap().active, vec![Some(3)]);
        s.key(Key::Right);
        assert_eq!(s.open.as_ref().unwrap().active, vec![Some(3), Some(0)]);
        s.key(Key::Down);
        assert_eq!(s.key(Key::Enter), Some(MenuAction::Previous));
        assert!(!s.is_open());

        // Esc closes the submenu, then the menu; a disabled row ignores clicks.
        let mut s = State::default();
        s.open(Kind::File, sample(), 0.0, 0.0);
        assert_eq!(s.click(Target::Row(0, 1)), None);
        assert_eq!(s.click(Target::Row(0, 3)), None);
        s.key(Key::Escape);
        assert!(s.is_open());
        s.key(Key::Escape);
        assert!(!s.is_open());
    }

    #[test]
    fn menus_offer_the_right_choices() {
        let d = PanelData {
            speed: 1.5,
            audio_tracks: vec!["English".into()],
            audio_selected: Some(0),
            ..Default::default()
        };
        let items = player_menu(&d, true);
        assert_eq!(items[0].label, "Play");
        let playback = &items.iter().find(|i| i.label == "Playback").unwrap().sub;
        assert!(playback.iter().find(|i| i.label == "1.5x").unwrap().checked);
        assert!(items[4].label == "Record" && items[4].enabled);

        // Settings opens without a file; Open Recent needs recent files.
        let main = main_menu(&[], &[], "Movies", &[], Converting::Nothing);
        assert!(main.iter().find(|i| i.label == "Settings").unwrap().enabled);
        assert!(
            !main
                .iter()
                .find(|i| i.label == "Open Recent")
                .unwrap()
                .enabled
        );

        // The Profile row names the profile in force and checks it in its submenu.
        let names = ["Movies".to_owned(), "Work".to_owned()];
        let main = main_menu(&[], &names, "Work", &[], Converting::Nothing);
        let profile = main.iter().find(|i| i.label == "Profile (Work)").unwrap();
        let checked: Vec<_> = profile.sub.iter().filter(|i| i.checked).collect();
        assert_eq!(checked.len(), 1);
        assert_eq!(checked[0].action, Some(MenuAction::Profile("Work".into())));
        assert_eq!(
            profile.sub.last().unwrap().action,
            Some(MenuAction::NewProfile)
        );

        // A queue row's menu plays and removes that row, and offers to play it next.
        use crate::ui::queue::QueueAction;
        let q = queue_menu(4, Path::new("a.mkv"), Some(1), &[]);
        assert_eq!(q[0].action, Some(MenuAction::Queue(QueueAction::Play(4))));
        assert_eq!(
            q[1].action,
            Some(MenuAction::Queue(QueueAction::PlayNext(4)))
        );
        assert!(
            q.iter()
                .any(|i| i.action == Some(MenuAction::Queue(QueueAction::Remove(4))))
        );
        assert!(!q.iter().any(|i| i.label == "Add to Queue"));
        // The one playing, or nothing playing: no "Play Next".
        let has_next = |q: Vec<Item>| q.iter().any(|i| i.label == "Play Next");
        assert!(!has_next(queue_menu(1, Path::new("a.mkv"), Some(1), &[])));
        assert!(!has_next(queue_menu(1, Path::new("a.mkv"), None, &[])));

        let sort = sort_menu(SortMode::NewestFirst, false, true);
        assert!(sort[2].checked);
        assert!(!sort[6].enabled, "artist sorts need artists");
        assert!(sort[8].enabled && sort[9].label == "Album Z-A");
    }
}

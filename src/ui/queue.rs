//! The play queue panel, opened by the control bar's playlist button: QUEUE (the files previous
//! and next step through, with sort, repeat, shuffle, remove and add), BOOKMARKS and CHAPTERS of
//! the playing file. It takes the settings panel's place at the window's right. Sizes are DIPs
//! measured from the design's queue frame, colours sampled from it. The mini player shows the
//! same list in its own style: a "Queue" title instead of the tabs, and sort, remove and add.

use super::*;
use crate::queue::Repeat;

const W: f32 = 400.0;
/// Gap to the window's right edge and above the control bar.
const MARGIN: f32 = 20.0;
const TOP: f32 = 61.0;
const RADIUS: f32 = 8.0;
const FIRST_ROW: f32 = 7.5;
const ROW: f32 = 44.0;

/// Where a queue's parts sit, from its frame's left and top.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Style {
    /// The mini player's: a title instead of tabs, and no shadow.
    mini: bool,
    header: f32,
    /// Centre of the "4 items - 2h 3m" line and its buttons.
    toolbar_y: f32,
    /// Top of the scrolling list, and of its first row inside it.
    list_top: f32,
    row_l: f32,
    row_r: f32,
    /// Centre of the drag handle, left edge of the name, right edge of the duration.
    handle_x: f32,
    name_x: f32,
    time_r: f32,
    /// Centre of the hovered row's "..." button.
    more_x: f32,
    /// Toolbar button centres: sort, repeat, shuffle, minus, plus.
    tool_x: [f32; 5],
}

const PANEL: Style = Style {
    mini: false,
    header: 50.0,
    toolbar_y: 85.0,
    list_top: 108.0,
    row_l: 12.0,
    row_r: 388.0,
    handle_x: 35.5,
    name_x: 62.5,
    time_r: 374.0,
    more_x: 362.0,
    tool_x: [211.0, 251.0, 291.5, 331.5, 368.0],
};

/// Measured from the Miniplayer component's queue; it has no repeat or shuffle.
const MINI: Style = Style {
    mini: true,
    header: 58.0,
    toolbar_y: 91.0,
    list_top: 111.0,
    row_l: 20.0,
    row_r: 371.0,
    handle_x: 43.5,
    name_x: 70.0,
    time_r: 358.0,
    more_x: 346.0,
    tool_x: [262.0, 0.0, 0.0, 307.0, 351.0],
};

impl Default for Style {
    fn default() -> Self {
        PANEL
    }
}
/// The pointer moves this far from a press on a row before it becomes a drag.
const DRAG_START: f32 = 4.0;

/// The hovered row's text, brighter than the others.
const FG_HOVER: u32 = 0xBDBDBD;
const BG_BODY_PANEL: u32 = 0x0F0F0F;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Queue,
    Bookmarks,
    Chapters,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Queue, Tab::Bookmarks, Tab::Chapters];
}

/// One row of the Queue tab.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueItem {
    pub name: String,
    /// Seconds; None until probed, or when the file has no length.
    pub duration: Option<f64>,
    /// Watched to the end before.
    pub watched: bool,
}

/// What the panel shows; main.rs fills it while the panel is open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueueData {
    pub items: Vec<QueueItem>,
    pub current: Option<usize>,
    pub repeat: Repeat,
    pub shuffle: bool,
    /// The playing file's bookmarks: name and seconds.
    pub bookmarks: Vec<(String, f64)>,
    /// The playing file's chapters: start in seconds and title.
    pub chapters: Vec<(f64, String)>,
}

impl QueueData {
    fn rows(&self, tab: Tab) -> usize {
        match tab {
            Tab::Queue => self.items.len(),
            Tab::Bookmarks => self.bookmarks.len(),
            Tab::Chapters => self.chapters.len(),
        }
    }
}

/// Changes to the queue and playback that main.rs applies.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum QueueAction {
    Play(usize),
    /// Moves item `.0` so it ends up at index `.1`.
    Move(usize, usize),
    /// Moves an item to play right after the current one.
    PlayNext(usize),
    Remove(usize),
    /// Opens the file picker and appends what it returns.
    AddFiles,
    Sort,
    CycleRepeat,
    ToggleShuffle,
    /// A bookmark at the current position of the playing file.
    AddBookmark,
    RemoveBookmark(usize),
    /// A bookmark's or chapter's position, in seconds.
    SeekTo(f64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Sort,
    Repeat,
    Shuffle,
    Remove,
    Add,
}

/// What the pointer is over inside the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Tab(u8),
    Tool(Tool),
    Row(usize),
    /// The hovered queue row's "..." button.
    More(usize),
    /// Anywhere else on the panel: it swallows the click.
    Body,
}

/// Whether the panel is open, its tab, scroll and selection; lives in `Ui`.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub open: bool,
    pub tab: Tab,
    pub scroll: f32,
    /// The row last clicked, which minus removes.
    pub selected: Option<usize>,
    /// A queue row pressed and the pointer's y then; moving on from there starts a drag.
    pub press: Option<(usize, f32)>,
    /// While dragging: the row and the gap between rows (0 to len) it would drop into.
    pub drag: Option<(usize, usize)>,
}

/// Where the panel's parts sit; part of `Layout`.
#[derive(Clone, Default, Debug)]
pub struct Geometry {
    pub open: bool,
    style: Style,
    tab: Tab,
    frame: Rect,
    header: Rect,
    tabs: Vec<Rect>,
    tools: Vec<(Tool, Rect)>,
    list: Rect,
    /// Already moved by the scroll.
    rows: Vec<Rect>,
    /// The first row's top, moved by the scroll, for drop gaps.
    rows_top: f32,
    pub max_scroll: f32,
    selected: Option<usize>,
    drag: Option<(usize, usize)>,
    /// What the rows were laid out from: a change needs a new layout.
    data: Option<QueueData>,
}

impl Layout {
    /// Adds the queue panel to a player layout when it is open.
    pub fn add_queue(&mut self, state: &State, data: &QueueData) {
        if !state.open {
            self.queue = Geometry::default();
            return;
        }
        let (w, h) = (self.width, self.height);
        let top = TOP.max(TITLE_BAR + FRAME);
        let bottom = (h - self.tools.bar - MARGIN).max(top + 200.0);
        let frame = Rect::new(w - MARGIN - W, top, w - MARGIN, bottom);
        self.queue = queue_geometry(PANEL, frame, state, state.tab, data);
    }

    /// Adds the mini player card's queue when its queue button is on. It always lists the queue,
    /// whichever tab the player's panel was left on.
    pub fn add_mini_queue(&mut self, state: &State, data: &QueueData) {
        self.queue = match self.card.and_then(|c| c.queue_frame()) {
            Some(frame) if state.open => queue_geometry(MINI, frame, state, Tab::Queue, data),
            _ => Geometry::default(),
        };
    }
}

fn queue_geometry(st: Style, frame: Rect, state: &State, tab: Tab, data: &QueueData) -> Geometry {
    {
        let header = Rect::new(frame.l, frame.t, frame.r, frame.t + st.header);
        // Three equal tabs, as in the tracks popup.
        let tabs = (0..if st.mini { 0 } else { 3 })
            .map(|i| {
                let l = frame.l + 14.0 + 130.0 * i as f32;
                Rect::new(l, header.t, l + 112.0, header.b)
            })
            .collect();
        let tool = |t: Tool, i: usize| {
            (
                t,
                Rect::around(frame.l + st.tool_x[i], frame.t + st.toolbar_y, 32.0),
            )
        };
        let tools = match tab {
            Tab::Queue if st.mini => vec![
                tool(Tool::Sort, 0),
                tool(Tool::Remove, 3),
                tool(Tool::Add, 4),
            ],
            Tab::Queue => vec![
                tool(Tool::Sort, 0),
                tool(Tool::Repeat, 1),
                tool(Tool::Shuffle, 2),
                tool(Tool::Remove, 3),
                tool(Tool::Add, 4),
            ],
            Tab::Bookmarks => vec![tool(Tool::Remove, 3), tool(Tool::Add, 4)],
            Tab::Chapters => Vec::new(),
        };
        let list = Rect::new(frame.l, frame.t + st.list_top, frame.r, frame.b - 8.0);
        let n = data.rows(tab);
        let content = FIRST_ROW + ROW * n as f32 + 8.0;
        let max_scroll = (content - list.h()).max(0.0);
        let rows_top = list.t + FIRST_ROW - state.scroll.clamp(0.0, max_scroll);
        let rows = (0..n)
            .map(|i| {
                let t = rows_top + ROW * i as f32;
                Rect::new(frame.l + st.row_l, t, frame.l + st.row_r, t + ROW)
            })
            .collect();
        Geometry {
            open: true,
            style: st,
            tab,
            frame,
            header,
            tabs,
            tools,
            list,
            rows,
            rows_top,
            max_scroll,
            selected: state.selected,
            drag: state.drag,
            data: Some(data.clone()),
        }
    }
}

impl Geometry {
    pub fn hit(&self, x: f32, y: f32) -> Option<Target> {
        if !self.open || !self.frame.contains(x, y) {
            return None;
        }
        if let Some(i) = self.tabs.iter().position(|r| r.contains(x, y)) {
            return Some(Target::Tab(i as u8));
        }
        if let Some((t, _)) = self.tools.iter().find(|(_, r)| r.contains(x, y)) {
            return Some(Target::Tool(*t));
        }
        if self.list.contains(x, y)
            && let Some(i) = self.rows.iter().position(|r| r.contains(x, y))
        {
            let more = (x - (self.frame.l + self.style.more_x)).abs() < 16.0;
            return Some(if more && self.tab == Tab::Queue {
                Target::More(i)
            } else {
                Target::Row(i)
            });
        }
        Some(Target::Body)
    }

    /// The gap between rows nearest to `y`, from 0 (before the first) to the row count.
    fn gap_at(&self, y: f32) -> usize {
        let g = ((y - self.rows_top) / ROW).round().max(0.0) as usize;
        g.min(self.rows.len())
    }
}

/// "4 items - 2h 3m", or just the count while no length is known.
fn summary(d: &QueueData) -> String {
    let n = d.items.len();
    let count = format!("{n} item{}", if n == 1 { "" } else { "s" });
    let total: f64 = d.items.iter().filter_map(|i| i.duration).sum();
    if total < 1.0 {
        return count;
    }
    let s = total.round() as u64;
    let length = match (s / 3600, s / 60 % 60) {
        (0, 0) => format!("{s}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m}m"),
    };
    format!("{count} - {length}")
}

/// A row's length: "23:06", or "1:02:03" from an hour on.
fn duration_text(seconds: f64) -> String {
    let s = seconds.round() as u64;
    match s / 3600 {
        0 => format!("{}:{:02}", s / 60, s % 60),
        h => format!("{h}:{:02}:{:02}", s / 60 % 60, s % 60),
    }
}

/// "3 bookmarks"; nothing for none, since the list itself says so.
fn plural(n: usize, word: &str) -> String {
    match n {
        0 => String::new(),
        n => format!("{n} {word}{}", if n == 1 { "" } else { "s" }),
    }
}

impl Ui {
    /// The playlist button: opens or closes the queue. It shares the right side with the tracks
    /// and settings popups, so opening one closes the other.
    pub fn toggle_queue(&mut self) {
        self.queue.open = !self.queue.open;
        if self.queue.open {
            self.panels.show(None, None);
        }
        (self.queue.press, self.queue.drag) = (None, None);
        self.relayout = true;
    }

    /// Whether the panel's layout is out of date.
    pub fn queue_stale(&self, data: Option<&QueueData>) -> bool {
        let (g, s) = (&self.layout.queue, &self.queue);
        // The mini player's queue always shows the Queue tab.
        let tab = if g.style.mini { Tab::Queue } else { s.tab };
        s.open != g.open
            || (s.open
                && (g.tab != tab
                    || g.selected != s.selected
                    || g.drag != s.drag
                    || g.data.as_ref() != data))
    }

    /// Whether the panel shows the queue's files, which have a menu, rather than bookmarks or
    /// chapters.
    pub(super) fn queue_rows_menu(&self) -> bool {
        self.layout.queue.tab == Tab::Queue
    }

    /// What a click on the panel does. Tabs and selection change here; the rest goes out.
    pub(super) fn queue_click(&mut self, target: Target) -> Option<Action> {
        let s = &mut self.queue;
        let g = &self.layout.queue;
        let d = g.data.as_ref()?;
        self.relayout = true;
        let action = match (target, g.tab) {
            (Target::Tab(i), _) => {
                s.tab = Tab::ALL[usize::from(i).min(2)];
                (s.scroll, s.selected) = (0.0, None);
                return None;
            }
            (Target::Tool(Tool::Sort), _) => {
                s.selected = None;
                QueueAction::Sort
            }
            (Target::Tool(Tool::Repeat), _) => QueueAction::CycleRepeat,
            (Target::Tool(Tool::Shuffle), _) => QueueAction::ToggleShuffle,
            (Target::Tool(Tool::Add), Tab::Bookmarks) => QueueAction::AddBookmark,
            (Target::Tool(Tool::Add), _) => QueueAction::AddFiles,
            (Target::Tool(Tool::Remove), tab) => {
                let i = s.selected.take()?;
                match tab {
                    Tab::Bookmarks => QueueAction::RemoveBookmark(i),
                    _ => QueueAction::Remove(i),
                }
            }
            (Target::Row(i), tab) => {
                // Chapters have no minus, so nothing to select for.
                s.selected = (tab != Tab::Chapters).then_some(i);
                match tab {
                    Tab::Queue => QueueAction::Play(i),
                    Tab::Bookmarks => QueueAction::SeekTo(d.bookmarks.get(i)?.1),
                    Tab::Chapters => QueueAction::SeekTo(d.chapters.get(i)?.0),
                }
            }
            // "..." opens the row's menu, which Ui::on_up asks main.rs for.
            (Target::More(_) | Target::Body, _) => return None,
        };
        Some(Action::Queue(action))
    }

    /// A press on a queue row may become a drag.
    pub(super) fn queue_press(&mut self, y: f32) {
        if let Some(Hit::Queue(Target::Row(i))) = self.hover
            && self.queue.tab == Tab::Queue
        {
            self.queue.press = Some((i, y));
        }
    }

    /// Starts a drag once the pointer has moved off the pressed row's spot, then follows it
    /// with the drop gap.
    pub(super) fn queue_drag(&mut self, y: f32) {
        if self.drag.is_none()
            && let Some((i, from_y)) = self.queue.press
            && (y - from_y).abs() > DRAG_START
        {
            self.drag = Some(Hit::Queue(Target::Row(i)));
        }
        if let Some(Hit::Queue(Target::Row(from))) = self.drag {
            let drag = Some((from, self.layout.queue.gap_at(y)));
            if self.queue.drag != drag {
                self.queue.drag = drag;
                self.relayout = true;
            }
        }
    }

    /// Drops a dragged row into the gap under the pointer.
    pub(super) fn queue_drop(&mut self, from: usize) -> Option<Action> {
        let (_, gap) = self.queue.drag.take()?;
        self.relayout = true;
        // Taking the row out first shifts the gaps after it up by one.
        let to = if gap > from { gap - 1 } else { gap };
        (to != from).then(|| {
            self.queue.selected = Some(to);
            Action::Queue(QueueAction::Move(from, to))
        })
    }
}

/// Brushes, fonts and icons the queue panel uses.
pub(super) struct Resources {
    body: ID2D1SolidColorBrush,
    header: ID2D1SolidColorBrush,
    shadow: ID2D1SolidColorBrush,
    selected: ID2D1SolidColorBrush,
    label: ID2D1SolidColorBrush,
    bright: ID2D1SolidColorBrush,
    row: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    faint: ID2D1SolidColorBrush,
    /// Over the row being dragged, so it reads as lifted out.
    veil: ID2D1SolidColorBrush,
    tab: IDWriteTextFormat,
    text: IDWriteTextFormat,
    text_r: IDWriteTextFormat,
    centred: IDWriteTextFormat,
    /// Tab icons, (inactive, active).
    tab_icons: [(Svg, Svg); 3],
    sort: Svg,
    repeat: Svg,
    repeat_on: Svg,
    pub(super) repeat_one_on: Svg,
    shuffle: Svg,
    shuffle_on: Svg,
    minus: Svg,
    minus_off: Svg,
    plus: Svg,
    handle: Svg,
    handle_dim: Svg,
    more: Svg,
    bookmark: Svg,
    /// The control bar's playlist button while the panel is open.
    pub(super) playlist_on: Svg,
}

impl Resources {
    pub(super) fn new(
        ctx: &ID2D1DeviceContext5,
        dwrite: &IDWriteFactory6,
        fonts: &IDWriteFontCollection2,
    ) -> Result<Self> {
        let brush =
            |rgb: u32, alpha: f32| unsafe { ctx.CreateSolidColorBrush(&color(rgb, alpha), None) };
        let format =
            |size: f32, align| text_format(dwrite, fonts, size, DWRITE_FONT_WEIGHT_REGULAR, align);
        let tint = |bytes: &[u8], rgb: u32| {
            let text = String::from_utf8_lossy(bytes).replace("#8F8F8F", &format!("#{rgb:06X}"));
            svg(ctx, text.as_bytes())
        };
        let tab = |bytes: &[u8]| -> Result<(Svg, Svg)> {
            Ok((tint(bytes, FG_DIM)?, tint(bytes, ACCENT)?))
        };
        // The library's sort icon, without the dot that marks a reversed order there.
        let sort = {
            let text = String::from_utf8_lossy(include_bytes!("../../assets/icons/sort.svg"));
            let text = match (text.find("<circle"), text.find("</g>")) {
                (Some(a), Some(b)) if a < b => format!("{}{}", &text[..a], &text[b..]),
                _ => text.into_owned(),
            };
            svg(ctx, text.as_bytes())?
        };
        let more = String::from_utf8_lossy(include_bytes!("../../assets/icons/more_panel.svg"))
            .replace("#6B6B6B", &format!("#{FG_A2:06X}"));
        let repeat = include_bytes!("../../assets/icons/repeat.svg");
        let shuffle = include_bytes!("../../assets/icons/shuffle.svg");
        let minus = include_bytes!("../../assets/icons/minus.svg");
        let handle = include_bytes!("../../assets/icons/drag_handle.svg");
        Ok(Resources {
            body: brush(BG_BODY_PANEL, 1.0)?,
            header: brush(BG_B2, 1.0)?,
            shadow: brush(0x000000, 0.07)?,
            selected: brush(0xFFFFFF, 0.04)?,
            label: brush(FG_LABEL, 1.0)?,
            bright: brush(FG_HOVER, 1.0)?,
            row: brush(FG_A2, 1.0)?,
            dim: brush(FG_DIM, 1.0)?,
            faint: brush(FG_FAINT, 1.0)?,
            veil: brush(BG_BODY_PANEL, 0.6)?,
            tab: format(12.0, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            text: format(14.0, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            text_r: format(14.0, DWRITE_TEXT_ALIGNMENT_TRAILING)?,
            centred: format(14.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
            tab_icons: [
                tab(include_bytes!("../../assets/icons/queue.svg"))?,
                tab(include_bytes!("../../assets/icons/bookmark.svg"))?,
                tab(include_bytes!("../../assets/icons/chapters.svg"))?,
            ],
            sort,
            repeat: tint(repeat, FG_DIM)?,
            repeat_on: tint(repeat, ACCENT)?,
            repeat_one_on: tint(include_bytes!("../../assets/icons/repeat_one.svg"), ACCENT)?,
            shuffle: tint(shuffle, FG_DIM)?,
            shuffle_on: tint(shuffle, ACCENT)?,
            minus: tint(minus, FG_DIM)?,
            minus_off: tint(minus, FG_FAINT)?,
            plus: tint(include_bytes!("../../assets/icons/nav_add.svg"), FG_DIM)?,
            handle: svg(ctx, handle)?,
            handle_dim: tint(handle, FG_DIM)?,
            more: svg(ctx, more.as_bytes())?,
            bookmark: tint(include_bytes!("../../assets/icons/bookmark.svg"), FG_DIM)?,
            playlist_on: tint(include_bytes!("../../assets/icons/playlist.svg"), ACCENT)?,
        })
    }
}

impl Painter {
    /// The open queue panel. Runs after the control bar, so it covers the video and nothing else.
    pub(super) unsafe fn queue_panel(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        d: &QueueData,
    ) {
        let (g, p) = (&s.layout.queue, &self.queue);
        if !g.open {
            return;
        }
        let (f, st) = (g.frame, g.style);
        unsafe {
            // A soft edge, so the panel holds on bright video. The mini player's queue is part
            // of its card, which already has one.
            if !st.mini {
                for spread in [8.0, 6.0, 4.0, 2.0] {
                    let edge = rounded(f.inset(-spread), RADIUS + spread);
                    rt.FillRoundedRectangle(&edge, &p.shadow);
                }
                rt.FillRoundedRectangle(&rounded(f, RADIUS), &p.body);
            }
            rt.PushAxisAlignedClip(&g.header.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            rt.FillRoundedRectangle(&rounded(f, RADIUS), &p.header);
            rt.PopAxisAlignedClip();
            if st.mini {
                self.put(rt, "Queue", &p.centred, g.header, &p.row);
            } else {
                self.queue_tabs(rt, ctx, s);
            }
            self.queue_toolbar(rt, ctx, s, d);

            rt.PushAxisAlignedClip(&g.list.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            if g.rows.is_empty() {
                let empty = match g.tab {
                    Tab::Queue => "No items",
                    Tab::Bookmarks => "No bookmarks",
                    Tab::Chapters => "No chapters",
                };
                let t = g.list.t + FIRST_ROW;
                let r = Rect::new(f.l + st.row_l, t, f.l + st.row_r, t + ROW);
                self.put(rt, empty, &p.centred, r, &p.dim);
            }
            for (i, r) in g.rows.iter().enumerate() {
                if !r.intersects(&g.list) {
                    continue;
                }
                let hovered = matches!(
                    s.hover,
                    Some(Hit::Queue(Target::Row(h) | Target::More(h))) if h == i
                );
                if g.selected == Some(i) {
                    rt.FillRoundedRectangle(&rounded(*r, 6.0), &p.selected);
                }
                match g.tab {
                    Tab::Queue => self.queue_row(rt, ctx, s, d, i, *r, hovered),
                    Tab::Bookmarks => {
                        let (name, at) = &d.bookmarks[i];
                        let cy = (r.t + r.b) / 2.0;
                        self.glyph(ctx, &p.bookmark, f.l + st.handle_x, cy, 20.0);
                        let fg = if hovered { &p.bright } else { &p.label };
                        self.row_text(rt, &st, f.l, r, name, &clock_text(*at), fg, &p.dim);
                    }
                    Tab::Chapters => {
                        let (start, title) = &d.chapters[i];
                        let end = d.chapters.get(i + 1).map_or(f64::INFINITY, |c| c.0);
                        let playing = (*start..end).contains(&s.position);
                        let fg = match (playing, hovered) {
                            (true, _) => &self.accent,
                            (_, true) => &p.bright,
                            _ => &p.label,
                        };
                        let x = f.l + st.handle_x;
                        let number = Rect::new(x - 16.0, r.t, x + 16.0, r.b);
                        self.put(rt, &(i + 1).to_string(), &p.centred, number, &p.dim);
                        self.row_text(rt, &st, f.l, r, title, &clock_text(*start), fg, &p.dim);
                    }
                }
            }
            if let Some((from, gap)) = g.drag {
                if let Some(r) = g.rows.get(from) {
                    rt.FillRectangle(&r.d2d(), &p.veil);
                }
                let y = g.rows_top + ROW * gap as f32;
                let line = Rect::new(f.l + st.row_l, y - 1.0, f.l + st.row_r, y + 1.0);
                rt.FillRoundedRectangle(&rounded(line, 1.0), &self.accent);
            }
            rt.PopAxisAlignedClip();
        }
    }

    /// The name at the left of a row and `right` (a length or time) at its right edge. `l` is
    /// the queue's left edge.
    #[allow(clippy::too_many_arguments)]
    unsafe fn row_text(
        &self,
        rt: &ID2D1RenderTarget,
        st: &Style,
        l: f32,
        r: &Rect,
        name: &str,
        right: &str,
        fg: &ID2D1SolidColorBrush,
        right_fg: &ID2D1SolidColorBrush,
    ) {
        let p = &self.queue;
        unsafe {
            let tr = Rect::new(l + st.name_x, r.t, l + st.time_r - 80.0, r.b);
            self.put(rt, name, &p.text, tr, fg);
            let tr = Rect::new(l + st.time_r - 76.0, r.t, l + st.time_r, r.b);
            self.put(rt, right, &p.text_r, tr, right_fg);
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn queue_row(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        d: &QueueData,
        i: usize,
        r: Rect,
        hovered: bool,
    ) {
        let p = &self.queue;
        let st = s.layout.queue.style;
        let item = &d.items[i];
        let l = s.layout.queue.frame.l;
        let cy = (r.t + r.b) / 2.0;
        let current = d.current == Some(i);
        let (fg, right_fg) = match (current, hovered, item.watched) {
            (true, ..) => (&self.accent, &p.dim),
            (_, true, _) => (&p.bright, &p.dim),
            (.., true) => (&p.dim, &p.faint),
            _ => (&p.label, &p.dim),
        };
        unsafe {
            let handle = if item.watched && !current {
                &p.handle_dim
            } else {
                &p.handle
            };
            self.glyph(ctx, handle, l + st.handle_x, cy, 24.0);
            let length = item.duration.map(duration_text).unwrap_or_default();
            let right = if hovered { "" } else { &length };
            self.row_text(rt, &st, l, &r, &item.name, right, fg, right_fg);
            if hovered {
                if s.hover == Some(Hit::Queue(Target::More(i))) {
                    let b = Rect::around(l + st.more_x, cy, 28.0);
                    rt.FillRoundedRectangle(&rounded(b, 4.0), &self.hover);
                }
                self.glyph(ctx, &p.more, l + st.more_x, cy, 18.0);
            }
            // The playing row's progress, as an orange underline.
            if current {
                let track = Rect::new(l + st.name_x, r.b - 7.0, l + st.time_r, r.b - 5.0);
                rt.FillRoundedRectangle(&rounded(track, 1.0), &self.played);
                let fraction = if s.duration > 0.0 {
                    (s.position / s.duration).clamp(0.0, 1.0) as f32
                } else {
                    0.0
                };
                let fill = Rect::new(track.l, track.t, track.l + track.w() * fraction, track.b);
                if fill.w() > 0.5 {
                    rt.FillRoundedRectangle(&rounded(fill, 1.0), &self.accent);
                }
            }
        }
    }

    unsafe fn queue_tabs(&self, rt: &ID2D1RenderTarget, ctx: &ID2D1DeviceContext5, s: &Scene) {
        let (g, p) = (&s.layout.queue, &self.queue);
        let names = ["Queue", "Bookmarks", "Chapters"];
        for (i, (name, r)) in names.iter().zip(&g.tabs).enumerate() {
            let on = Tab::ALL[i] == g.tab;
            let hovered = s.hover == Some(Hit::Queue(Target::Tab(i as u8)));
            let text_w = self.measure(name, &p.tab);
            let content = 20.0 + 7.0 + text_w;
            let cx = (r.l + r.r) / 2.0;
            let x = cx - content / 2.0;
            let cy = r.t + 25.0;
            let (off, lit) = &p.tab_icons[i];
            unsafe {
                self.glyph(ctx, if on { lit } else { off }, x + 10.0, cy, 20.0);
                let brush = if on || hovered { &p.row } else { &p.dim };
                let tr = Rect::new(x + 27.0, cy - 10.0, x + 27.0 + text_w + 2.0, cy + 10.0);
                self.put(rt, name, &p.tab, tr, brush);
                // The design underlines the whole tab here, not just its label.
                if on {
                    let line = Rect::new(r.l, r.b - 2.0, r.r, r.b);
                    rt.FillRoundedRectangle(&rounded(line, 1.0), &self.accent);
                }
            }
        }
    }

    unsafe fn queue_toolbar(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        d: &QueueData,
    ) {
        let (g, p) = (&s.layout.queue, &self.queue);
        let f = g.frame;
        let cy = f.t + g.style.toolbar_y;
        let text = match g.tab {
            Tab::Queue => summary(d),
            Tab::Bookmarks => plural(d.bookmarks.len(), "bookmark"),
            Tab::Chapters => plural(d.chapters.len(), "chapter"),
        };
        unsafe {
            let tr = Rect::new(
                f.l + g.style.handle_x - 7.5,
                cy - 12.0,
                f.l + 190.0,
                cy + 12.0,
            );
            self.put(rt, &text, &p.text, tr, &p.dim);
            for (tool, r) in &g.tools {
                let svg = match tool {
                    Tool::Sort => &p.sort,
                    Tool::Repeat => match d.repeat {
                        Repeat::Off => &p.repeat,
                        Repeat::All => &p.repeat_on,
                        Repeat::One => &p.repeat_one_on,
                    },
                    Tool::Shuffle if d.shuffle => &p.shuffle_on,
                    Tool::Shuffle => &p.shuffle,
                    Tool::Remove if g.selected.is_some() => &p.minus,
                    Tool::Remove => &p.minus_off,
                    Tool::Add => &p.plus,
                };
                let enabled = *tool != Tool::Remove || g.selected.is_some();
                if enabled && s.hover == Some(Hit::Queue(Target::Tool(*tool))) {
                    rt.FillRoundedRectangle(&rounded(*r, 4.0), &self.hover);
                }
                let (cx, cy) = ((r.l + r.r) / 2.0, (r.t + r.b) / 2.0);
                // The sort icon is drawn on a 26-wide canvas.
                let size = if *tool == Tool::Sort { 26.0 } else { 24.0 };
                self.glyph(ctx, svg, cx, cy, size);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> QueueData {
        let item = |name: &str, duration| QueueItem {
            name: name.into(),
            duration,
            watched: false,
        };
        QueueData {
            items: vec![
                item("The Wand_S01E11.mp4", Some(1386.0)),
                item("The Wand_S01E12.mp4", Some(1458.0)),
                item("The Wand_S01E13.mp4", Some(1500.0)),
                item("The Wand_S01E14.mp4", Some(1355.0)),
            ],
            current: Some(0),
            bookmarks: vec![("Bookmark 1".into(), 754.0)],
            ..Default::default()
        }
    }

    fn open(tab: Tab) -> Layout {
        let mut l = Layout::new(1920.0, 1080.0, true, false);
        let state = State {
            open: true,
            tab,
            ..Default::default()
        };
        l.add_queue(&state, &data());
        l
    }

    #[test]
    fn panel_matches_the_design_at_1920() {
        let l = open(Tab::Queue);
        let g = &l.queue;
        assert_eq!(g.frame, Rect::new(1500.0, 61.0, 1900.0, 1006.0));
        assert_eq!(g.tabs[1], Rect::new(1644.0, 61.0, 1756.0, 111.0));
        assert_eq!(summary(&data()), "4 items - 1h 34m");
        // Rows centre at 137.5 + 44 i from the panel's top, as in the design.
        let row = |i: usize| l.hit(1700.0, 61.0 + 137.5 + ROW * i as f32);
        assert_eq!(row(0), Some(Hit::Queue(Target::Row(0))));
        assert_eq!(row(3), Some(Hit::Queue(Target::Row(3))));
        assert_eq!(row(4), Some(Hit::Queue(Target::Body)));
        assert_eq!(
            l.hit(1500.0 + PANEL.more_x, 61.0 + 181.5),
            Some(Hit::Queue(Target::More(1)))
        );
        assert_eq!(
            l.hit(1500.0 + 251.0, 61.0 + 85.0),
            Some(Hit::Queue(Target::Tool(Tool::Repeat)))
        );
        assert_eq!(l.hit(1000.0, 500.0), Some(Hit::Video));
        // Bookmarks has only minus and plus, and no "..." on its rows.
        let l = open(Tab::Bookmarks);
        assert_eq!(
            l.hit(1500.0 + 251.0, 61.0 + 85.0),
            Some(Hit::Queue(Target::Body))
        );
        assert_eq!(
            l.hit(1500.0 + PANEL.more_x, 61.0 + 137.5),
            Some(Hit::Queue(Target::Row(0)))
        );
    }

    #[test]
    fn drop_gaps_and_scrolling() {
        let l = open(Tab::Queue);
        let g = &l.queue;
        let top = 61.0 + PANEL.list_top + FIRST_ROW;
        assert_eq!(g.gap_at(top - 30.0), 0);
        assert_eq!(g.gap_at(top + ROW * 1.4), 1);
        assert_eq!(g.gap_at(top + ROW * 2.6), 3);
        assert_eq!(g.gap_at(5000.0), 4);
        assert_eq!(g.max_scroll, 0.0);

        let mut d = data();
        d.items = (0..40).map(|_| d.items[0].clone()).collect();
        let mut l = Layout::new(1920.0, 1080.0, true, false);
        let state = State {
            open: true,
            scroll: 10_000.0,
            ..Default::default()
        };
        l.add_queue(&state, &d);
        assert!(l.queue.max_scroll > 0.0);
        let last = l.queue.rows.last().unwrap();
        assert!(last.b <= l.queue.list.b, "scrolling clamps to the end");
    }

    #[test]
    fn texts() {
        assert_eq!(duration_text(1386.0), "23:06");
        assert_eq!(duration_text(3723.0), "1:02:03");
        let mut d = data();
        d.items.truncate(1);
        d.items[0].duration = None;
        assert_eq!(summary(&d), "1 item");
        d.items[0].duration = Some(45.0);
        assert_eq!(summary(&d), "1 item - 45s");
        assert_eq!(plural(0, "chapter"), "");
        assert_eq!(plural(2, "chapter"), "2 chapters");
    }
}

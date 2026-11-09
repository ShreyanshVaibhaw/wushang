//! The player's tabs: several files open in one window, only the one on screen seen and heard,
//! the others playing on silently. With two or more, the title bar's centred title gives way to
//! a strip of them; the new-tab button keeps one place left of the window buttons, so it's there
//! to be found with one tab too.
//!
//! The design has no tabs, so they borrow its parts: the window buttons' height and hover box,
//! the title's type, the popups' tab colours (the one on screen lit, the others dim until
//! hovered) and the seek bar's dark track behind the tab on screen, which turns pale in the
//! light theme.

use std::borrow::Cow;

use super::*;

/// The widest a tab gets; below that they share the room evenly.
const MAX_W: f32 = 200.0;
const GAP: f32 = 4.0;
/// Between the strip and the new-tab button, and the title bar's group on its left.
const MARGIN: f32 = 12.0;
/// Between the new-tab button and the window buttons.
const NEW_GAP: f32 = 20.0;
/// The least room for a tab's title: about three letters and the ellipsis. A tab with less shows
/// its number, which is also its Cmd shortcut, and one that would have less beside a close
/// button has none (Cmd+W still closes it).
const TITLE_MIN: f32 = 32.0;
/// With less room than this for each tab the title bar keeps its title.
const MIN_W: f32 = 24.0;
const RADIUS: f32 = 6.0;
/// The close button's box, inset from the tab's right end.
const CLOSE: f32 = 18.0;
const CLOSE_INSET: f32 = 5.0;
/// Between a tab's title and its close button.
const CLOSE_GAP: f32 = 4.0;
/// The space either side of a tab's title.
const PAD: f32 = 10.0;
/// The playing mark's box, and the space between it and the title.
const MARK: f32 = 12.0;
const MARK_GAP: f32 = 1.0;
/// The logo's right edge: where the title bar's group would start.
const LOGO_R: f32 = 37.0;
/// The design's title runs between these insets from either side of the window.
const TITLE_INSET: f32 = 300.0;

/// Where one tab sits in the title bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Slot {
    pub tab: Rect,
    /// None when the tab is too narrow for one.
    pub close: Option<Rect>,
}

impl Slot {
    /// Whether the tab shows its title rather than its number.
    pub fn titled(&self) -> bool {
        title_room(self.tab.w(), self.close.is_some()) >= TITLE_MIN
    }
}

/// The room a tab `w` wide leaves its title: the tab less its pads, the close button's area when
/// it has one, and the playing mark's. The mark counts whether it's shown or not, so a tab
/// doesn't swap its title for its number as it starts or stops playing.
fn title_room(w: f32, close: bool) -> f32 {
    let close = if close {
        CLOSE_INSET + CLOSE + CLOSE_GAP - PAD
    } else {
        0.0
    };
    w - 2.0 * PAD - (MARK + MARK_GAP) - close
}

impl Layout {
    /// Lays the player's title bar out for `count` tabs: the new-tab button for one or more, and
    /// the strip of tabs for two or more. Runs after `Layout::player`, before anything that
    /// covers the title bar.
    pub fn add_tabs(&mut self, count: usize) {
        self.tab_count = count;
        self.tab_slots.clear();
        self.new_tab = None;
        self.buttons
            .retain(|(h, _)| !matches!(h, Hit::Tab(_) | Hit::CloseTab(_) | Hit::NewTab));
        // Without the bars there's no title bar.
        if self.page != Page::Player || !self.bars || count == 0 {
            return;
        }
        // Clear of the title bar's group and the box Customize Player drops into it, which is
        // the group's area padded by 4 (the first of `toolbar::ZONES`).
        let start = self.tools.zones[0].r + MARGIN - 4.0;
        let end = self.button(Hit::Minimize).l - NEW_GAP;
        let new = Rect::new(end - 28.0, 6.0, end, 34.0);
        if new.l < start {
            return;
        }
        self.new_tab = Some(new);
        if count >= 2 {
            self.tab_slots = strip(count, self.width, start, new.l - MARGIN);
        }
        // Close buttons go first, as `hit` takes the first rect that holds the point.
        for (i, slot) in self.tab_slots.iter().enumerate() {
            if let Some(close) = slot.close {
                self.buttons.push((Hit::CloseTab(i), close));
            }
        }
        for (i, slot) in self.tab_slots.iter().enumerate() {
            self.buttons.push((Hit::Tab(i), slot.tab));
        }
        self.buttons.push((Hit::NewTab, new));
    }

    /// The tab count `add_tabs` laid out for, so a change of count relayouts.
    pub fn tab_count(&self) -> usize {
        self.tab_count
    }

    /// The right end of the title bar's group, or of the logo when the group is empty.
    fn title_end(&self) -> f32 {
        self.tools
            .placed
            .iter()
            .filter(|p| p.zone == toolbar::Zone::Title)
            .map(|p| p.r.r)
            .fold(LOGO_R, f32::max)
    }

    /// Where the centred title goes: the design's, narrowed evenly on the player page so it
    /// stays clear of a title bar group that reaches into it and of the new-tab button.
    pub(super) fn title_area(&self) -> Rect {
        let w = self.width;
        let mut half = w / 2.0 - TITLE_INSET;
        if self.page == Page::Player {
            half = half.min(w / 2.0 - self.title_end());
            if let Some(new) = self.new_tab {
                half = half.min(new.l - MARGIN - w / 2.0);
            }
        }
        let half = half.max(0.0);
        Rect::new(w / 2.0 - half, 0.0, w / 2.0 + half, TITLE_BAR)
    }
}

/// `count` tabs of one width, centred on the window as the title is, within `start..end`. Empty
/// when they don't fit.
fn strip(count: usize, width: f32, start: f32, end: f32) -> Vec<Slot> {
    let n = count as f32;
    let w = ((end - start - GAP * (n - 1.0)) / n).floor().min(MAX_W);
    if w < MIN_W {
        return Vec::new();
    }
    let total = w * n + GAP * (n - 1.0);
    let left = (width / 2.0 - total / 2.0)
        .round()
        .min(end - total)
        .max(start);
    (0..count)
        .map(|i| {
            let l = left + (w + GAP) * i as f32;
            let tab = Rect::new(l, 6.0, l + w, 34.0);
            let close = (title_room(w, true) >= TITLE_MIN).then(|| {
                let r = tab.r - CLOSE_INSET;
                let cy = TITLE_BAR / 2.0;
                Rect::new(r - CLOSE, cy - CLOSE / 2.0, r, cy + CLOSE / 2.0)
            });
            Slot { tab, close }
        })
        .collect()
}

/// A tab's title or number, as `Painter::label` places it.
#[derive(Debug)]
struct Label<'a> {
    text: Cow<'a, str>,
    /// The playing mark's centre, when the tab has one.
    mark: Option<f32>,
    /// The box the text is set in.
    words: Rect,
}

pub(super) struct Resources {
    text: IDWriteTextFormat,
    dim: ID2D1SolidColorBrush,
    /// The box behind the tab on screen.
    on: ID2D1SolidColorBrush,
    /// The hairline between two tabs without a box.
    line: ID2D1SolidColorBrush,
    close: Svg,
    close_lit: Svg,
    new: Svg,
    new_lit: Svg,
    playing: Svg,
}

impl Resources {
    pub(super) fn new(
        ctx: &ID2D1DeviceContext5,
        dwrite: &IDWriteFactory6,
        fonts: &IDWriteFontCollection2,
    ) -> Result<Self> {
        let brush =
            |rgb: u32, alpha: f32| unsafe { ctx.CreateSolidColorBrush(&color(rgb, alpha), None) };
        let tint = |bytes: &[u8], rgb: u32| {
            let text = String::from_utf8_lossy(bytes).replace("#8F8F8F", &format!("#{rgb:06X}"));
            svg(ctx, text.as_bytes())
        };
        let close = include_bytes!("../../assets/icons/close_tab.svg");
        let new = include_bytes!("../../assets/icons/new_tab.svg");
        Ok(Resources {
            text: text_format(
                dwrite,
                fonts,
                13.0,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            dim: brush(FG_DIM, 1.0)?,
            on: brush(BG_B2, 1.0)?,
            line: brush(0x373737, 1.0)?,
            close: tint(close, FG_DIM)?,
            close_lit: svg(ctx, close)?,
            new: tint(new, FG_DIM)?,
            new_lit: svg(ctx, new)?,
            playing: tint(include_bytes!("../../assets/icons/playing_tab.svg"), ACCENT)?,
        })
    }
}

impl Painter {
    /// The strip of tabs in the title's place. False when the layout has none, for one tab or
    /// a title bar with no room for them.
    pub(super) unsafe fn tab_strip(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
    ) -> bool {
        let slots = &s.layout.tab_slots;
        if slots.is_empty() {
            return false;
        }
        let hovered = match s.hover {
            Some(Hit::Tab(i) | Hit::CloseTab(i)) => Some(i),
            _ => None,
        };
        // The tab on screen and the one under the pointer have a box; the rest a hairline
        // between them.
        let boxed = |i: usize| i == s.active_tab || hovered == Some(i);
        unsafe {
            for (i, slot) in slots.iter().enumerate() {
                let t = slot.tab;
                if i == s.active_tab {
                    rt.FillRoundedRectangle(&rounded(t, RADIUS), &self.strip.on);
                } else if hovered == Some(i) {
                    rt.FillRoundedRectangle(&rounded(t, RADIUS), &self.hover);
                }
                if i > 0 && !boxed(i) && !boxed(i - 1) {
                    let x = t.l - GAP / 2.0;
                    let line = Rect::new(x - 0.5, 13.0, x + 0.5, 27.0);
                    rt.FillRectangle(&line.d2d(), &self.strip.line);
                }
                self.tab_label(rt, ctx, s, i, slot, boxed(i));
            }
        }
        true
    }

    /// What a tab's label shows and where: its title, or its number when narrow, after the
    /// playing mark when it has one.
    fn label<'a>(&self, s: &'a Scene, i: usize, slot: &Slot) -> Label<'a> {
        let (r, t) = (&self.strip, slot.tab);
        let info = s.tabs.get(i);
        // The tab on screen shows its state in the control bar; the mark is for the others.
        let playing = i != s.active_tab && info.is_some_and(|t| t.playing);
        let mark = if playing { MARK + MARK_GAP } else { 0.0 };
        let cx = (t.l + t.r) / 2.0;
        let (text, area) = if !slot.titled() {
            let number = (i + 1).to_string();
            let w = self.measure(&number, &r.text);
            let x = (cx - (mark + w) / 2.0).round();
            (
                Cow::Owned(number),
                Rect::new(x, 0.0, x + mark + w + 1.0, TITLE_BAR),
            )
        } else {
            let title = match info.map(|t| t.title.as_str()) {
                Some("") | None => "wushang",
                Some(title) => title,
            };
            // Centred like the title it replaces, unless that would run into the close button;
            // a long title ends in an ellipsis before it.
            let end = slot.close.map_or(t.r - PAD, |c| c.l - CLOSE_GAP);
            let w = self.measure(title, &r.text);
            let x = (cx - (mark + w) / 2.0)
                .min(end - mark - w)
                .max(t.l + PAD)
                .round();
            (Cow::Borrowed(title), Rect::new(x, 0.0, end, TITLE_BAR))
        };
        Label {
            text,
            mark: playing.then_some(area.l + MARK / 2.0),
            words: Rect::new(area.l + mark, area.t, area.r, area.b),
        }
    }

    /// A tab's title (or number, when narrow), its playing mark and its close button.
    unsafe fn tab_label(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        i: usize,
        slot: &Slot,
        lit: bool,
    ) {
        let r = &self.strip;
        let label = self.label(s, i, slot);
        let brush = if lit { &self.fg } else { &r.dim };
        let cy = TITLE_BAR / 2.0;
        unsafe {
            if let Some(x) = label.mark {
                self.glyph(ctx, &r.playing, x, cy, MARK);
            }
            self.put(rt, &label.text, &r.text, label.words, brush);
            if let Some(c) = slot.close.filter(|_| lit) {
                let over = s.hover == Some(Hit::CloseTab(i));
                if over {
                    rt.FillRoundedRectangle(&rounded(c, 4.0), &self.hover);
                }
                let icon = if over { &r.close_lit } else { &r.close };
                self.glyph(ctx, icon, (c.l + c.r) / 2.0, cy, 16.0);
            }
        }
    }

    /// The new-tab button: quiet until the pointer is over it.
    pub(super) unsafe fn new_tab_button(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
    ) {
        let Some(n) = s.layout.new_tab else { return };
        let over = s.hover == Some(Hit::NewTab);
        let icon = if over {
            &self.strip.new_lit
        } else {
            &self.strip.new
        };
        unsafe {
            self.hover_box(rt, s, Hit::NewTab);
            self.glyph(ctx, icon, (n.l + n.r) / 2.0, TITLE_BAR / 2.0, 24.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rect the title bar holds besides the tabs: the window buttons, the title bar's
    /// group and the box Customize Player drops into it.
    fn others(l: &Layout) -> Vec<(String, Rect)> {
        let buttons = l.buttons.iter().filter(|(h, r)| {
            r.b <= TITLE_BAR && !matches!(h, Hit::Tab(_) | Hit::CloseTab(_) | Hit::NewTab)
        });
        let group = l
            .tools
            .placed
            .iter()
            .filter(|p| p.zone == toolbar::Zone::Title);
        buttons
            .map(|(h, r)| (format!("{h:?}"), *r))
            .chain(group.map(|p| (format!("{:?}", p.item), p.r)))
            .chain([("the title group's drop box".to_owned(), l.tools.zones[0])])
            .collect()
    }

    fn toolbars() -> Vec<(&'static str, Toolbar)> {
        let empty = Toolbar {
            title: Vec::new(),
            ..Toolbar::default()
        };
        let mut busy = Toolbar::default();
        busy.title
            .extend([toolbar::Item::Divider, toolbar::Item::FileName]);
        vec![
            ("default", Toolbar::default()),
            ("empty", empty),
            ("busy", busy),
        ]
    }

    #[test]
    fn tabs_fit_between_the_title_group_and_the_window_buttons() {
        for (w, h) in [(800.0, 450.0), (1280.0, 720.0), (2560.0, 1440.0)] {
            for (name, t) in toolbars() {
                let mut last_w = f32::INFINITY;
                for n in 1..=8 {
                    let mut l = Layout::player(w, h, true, false, &t);
                    l.add_tabs(n);
                    let at = format!("{n} tabs, {w}x{h}, {name} title group");
                    let new = l
                        .new_tab
                        .unwrap_or_else(|| panic!("{at}: a new-tab button"));
                    if n == 1 {
                        assert!(l.tab_slots.is_empty(), "{at}: one tab keeps the title");
                    } else if !(name == "busy" && w < 1000.0) {
                        assert_eq!(l.tab_slots.len(), n, "{at}");
                    }
                    let mut mine = vec![(Hit::NewTab, new)];
                    let others = others(&l);
                    for (i, s) in l.tab_slots.iter().enumerate() {
                        mine.push((Hit::Tab(i), s.tab));
                        assert_eq!(s.tab.w(), l.tab_slots[0].tab.w(), "{at}: even widths");
                        if let Some(c) = s.close {
                            assert!(
                                c.l >= s.tab.l
                                    && c.r <= s.tab.r
                                    && c.t >= s.tab.t
                                    && c.b <= s.tab.b,
                                "{at}: tab {i}'s close button is inside it"
                            );
                            let (x, y) = ((c.l + c.r) / 2.0, (c.t + c.b) / 2.0);
                            assert_eq!(l.hit(x, y), Some(Hit::CloseTab(i)), "{at}");
                            // Its left end is the tab, not the close button.
                            assert_eq!(l.hit(s.tab.l + 4.0, y), Some(Hit::Tab(i)), "{at}");
                        }
                    }
                    if let Some(s) = l.tab_slots.first() {
                        assert!(s.tab.w() <= last_w, "{at}: tabs shrink as they're added");
                        last_w = s.tab.w();
                    }
                    for (i, (hit, r)) in mine.iter().enumerate() {
                        assert!(
                            r.l >= 0.0 && r.r <= w && r.t >= 0.0 && r.b <= TITLE_BAR,
                            "{at}: {hit:?} {r:?} is in the title bar"
                        );
                        assert!(r.w() > 0.0 && r.h() > 0.0, "{at}: {hit:?}");
                        for (other, o) in &mine[i + 1..] {
                            assert!(!r.intersects(o), "{at}: {hit:?} {r:?} overlaps {other:?}");
                        }
                        for (other, o) in &others {
                            assert!(
                                !r.intersects(o),
                                "{at}: {hit:?} {r:?} overlaps {other} {o:?}"
                            );
                        }
                        let (x, y) = ((r.l + r.r) / 2.0, (r.t + r.b) / 2.0);
                        assert_eq!(l.hit(x, y), Some(*hit), "{at}");
                        assert!(hit.in_bars(), "{at}");
                    }
                }
            }
        }
    }

    #[test]
    fn eight_tabs_at_the_smallest_window() {
        let mut l = Layout::new(800.0, 450.0, true, false);
        l.add_tabs(8);
        let first = l.tab_slots[0].tab;
        assert!(first.w() >= MIN_W, "{first:?}");
        assert!(l.tab_slots.iter().all(|s| s.close.is_none()), "too narrow");
        // A wide window gives them room for their titles.
        let mut l = Layout::new(1920.0, 1080.0, true, false);
        l.add_tabs(8);
        assert!(l.tab_slots.iter().all(|s| s.close.is_some()));
    }

    #[test]
    fn narrow_tabs_show_their_numbers() {
        // Window width, tab count, and whether the tabs have titles and close buttons. With 5
        // tabs at 800 a playing tab once had room for the ellipsis alone, and with 6 not even
        // that, so its title was cut mid-letter.
        for (w, n, titled, close) in [
            (800.0, 3, true, true),
            (800.0, 4, true, false),
            (800.0, 5, false, false),
            (800.0, 6, false, false),
            (900.0, 5, true, false),
            (900.0, 7, false, false),
            (900.0, 8, false, false),
            (1000.0, 5, true, true),
            (1000.0, 8, false, false),
        ] {
            let mut l = Layout::new(w, 450.0, true, false);
            l.add_tabs(n);
            assert_eq!(l.tab_slots.len(), n);
            for s in &l.tab_slots {
                assert_eq!(
                    (s.titled(), s.close.is_some()),
                    (titled, close),
                    "{n} tabs at {w}: {s:?}"
                );
            }
        }
    }

    /// Every tab, playing or not, shows its title cut to a few letters and an ellipsis or else
    /// its number, whole either way, and the same one whether it's playing or not.
    #[test]
    fn tab_labels_are_readable_and_steady() {
        use super::super::render_check::{TITLES, painter, scene};
        let (_ctx, p) = painter();
        let browse = Browse::new();
        let style = crate::captions::CaptionStyle::default();
        // The width `text` takes set in `w` DIPs, and whether its end gave way to an ellipsis.
        let set = |text: &str, w: f32| unsafe {
            let utf16: Vec<u16> = text.encode_utf16().collect();
            let layout = p
                .dwrite
                .CreateTextLayout(&utf16, &p.strip.text, w, TITLE_BAR)
                .unwrap();
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m).unwrap();
            let (mut lines, mut count) = ([DWRITE_LINE_METRICS::default()], 0);
            layout.GetLineMetrics(Some(&mut lines), &mut count).unwrap();
            (m.width, lines[0].isTrimmed.as_bool())
        };
        for w in [800.0, 900.0, 1000.0, 1100.0, 1280.0, 1920.0] {
            for n in 2..=8 {
                let mut l = Layout::new(w, 450.0, true, false);
                l.add_tabs(n);
                for playing in [false, true] {
                    let tabs: Vec<TabInfo> = TITLES[..n]
                        .iter()
                        .map(|title| TabInfo {
                            title: (*title).to_owned(),
                            playing,
                        })
                        .collect();
                    let mut s = scene(&l, &browse, &style);
                    s.tabs = &tabs;
                    for (i, slot) in l.tab_slots.iter().enumerate() {
                        let at = format!("tab {i} of {n} at {w}, playing {playing}");
                        let (t, label) = (slot.tab, p.label(&s, i, slot));
                        let shown = if slot.titled() {
                            TITLES[i].to_owned()
                        } else {
                            (i + 1).to_string()
                        };
                        assert_eq!(label.text, shown, "{at}");
                        assert_eq!(label.mark.is_some(), playing && i != 0, "{at}");
                        let left = label.mark.map_or(label.words.l, |x| x - MARK / 2.0);
                        assert!(left >= t.l && label.words.r <= t.r, "{at}: {t:?}");
                        let (width, trimmed) = set(&label.text, label.words.w());
                        assert!(width <= label.words.w() + 0.01, "{at}: clipped");
                        if !slot.titled() {
                            assert!(!trimmed, "{at}");
                        } else if trimmed {
                            // A lone ellipsis, or one after a single letter, says nothing.
                            let two: String = TITLES[i].chars().take(2).chain(['…']).collect();
                            assert!(!set(&two, label.words.w()).1, "{at}: {label:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn few_tabs_sit_where_the_title_was() {
        let mut l = Layout::new(1920.0, 1080.0, true, false);
        l.add_tabs(2);
        let (a, b) = (l.tab_slots[0].tab, l.tab_slots[1].tab);
        assert_eq!(a.w(), MAX_W);
        assert_eq!((a.l + b.r) / 2.0, 960.0, "centred");
        assert_eq!(
            l.hit(a.l - 20.0, 20.0),
            Some(Hit::Caption),
            "room left to drag"
        );
        assert_eq!(l.hit(b.r + 20.0, 20.0), Some(Hit::Caption));
    }

    #[test]
    fn one_tab_keeps_the_title_bar_as_it_was() {
        let plain = Layout::new(1280.0, 720.0, true, false);
        let mut l = plain.clone();
        l.add_tabs(1);
        assert_eq!(l.title_area(), plain.title_area());
        assert_eq!(l.title_area(), Rect::new(300.0, 0.0, 980.0, TITLE_BAR));
        assert_eq!(
            l.hit(640.0, 20.0),
            Some(Hit::Caption),
            "the title drags the window"
        );
        let new = l.new_tab.unwrap();
        assert_eq!(l.hit(new.l + 14.0, 20.0), Some(Hit::NewTab));
        assert_eq!(new.r, l.button(Hit::Minimize).l - NEW_GAP);
    }

    #[test]
    fn no_title_bar_no_tabs() {
        for fullscreen in [false, true] {
            let mut l = Layout::new(1280.0, 720.0, false, fullscreen);
            l.add_tabs(3);
            assert_eq!(l.tab_count(), 3);
            assert!(l.tab_slots.is_empty() && l.new_tab.is_none());
            assert!(!matches!(
                l.hit(640.0, 20.0),
                Some(Hit::Tab(_) | Hit::CloseTab(_) | Hit::NewTab)
            ));
        }
        let mut l = Layout::new(1280.0, 720.0, true, true);
        l.add_tabs(3);
        assert_eq!(l.tab_slots.len(), 3, "fullscreen bars have the tabs too");
    }

    #[test]
    fn laying_out_again_replaces_the_tabs() {
        let mut l = Layout::new(1280.0, 720.0, true, false);
        l.add_tabs(5);
        l.add_tabs(2);
        let tabs = l
            .buttons
            .iter()
            .filter(|(h, _)| matches!(h, Hit::Tab(_)))
            .count();
        assert_eq!(tabs, 2);
        assert_eq!(l.tab_count(), 2);
        l.add_tabs(0);
        assert!(l.new_tab.is_none() && l.hit(1100.0, 20.0) == Some(Hit::Caption));
    }

    #[test]
    fn a_title_group_reaching_the_middle_pushes_the_title_aside() {
        let mut t = Toolbar::default();
        t.title.push(toolbar::Item::FileName);
        let mut l = Layout::player(1280.0, 720.0, true, false, &t);
        l.add_tabs(1);
        let area = l.title_area();
        assert_eq!(area.l, l.title_end(), "{area:?}");
        assert_eq!(area.l + area.r, 1280.0, "still centred");
        // Past the middle there's no room left for it.
        let l = Layout::player(900.0, 720.0, true, false, &t);
        assert_eq!(l.title_area().w(), 0.0);
    }

    #[test]
    fn clicks_on_tabs() {
        let mut ui = Ui::new();
        ui.page = Page::Player;
        ui.layout = Layout::new(1280.0, 720.0, true, false);
        ui.layout.add_tabs(3);
        let click = |ui: &mut Ui, r: Rect| {
            let (x, y) = ((r.l + r.r) / 2.0, (r.t + r.b) / 2.0);
            ui.on_down(x, y);
            match ui.on_up(x, y) {
                Some(Action::Click(h)) => Some(h),
                _ => None,
            }
        };
        let slot = ui.layout.tab_slots[1];
        assert_eq!(click(&mut ui, slot.tab), Some(Hit::Tab(1)));
        assert_eq!(click(&mut ui, slot.close.unwrap()), Some(Hit::CloseTab(1)));
        let new = ui.layout.new_tab.unwrap();
        assert_eq!(click(&mut ui, new), Some(Hit::NewTab));
    }
}

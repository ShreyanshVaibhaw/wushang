//! Customize Player, from the design's "Player - customize" frames: a palette of what the bars
//! can hold over the player, whose bars show every item and group in a dashed box. Items drag
//! from the palette into a group, between and across groups, and back into the palette to come
//! out. The bars show the draft as it changes; Save keeps it, Cancel or Esc drops it.
//!
//! Sizes are DIPs from the 1048x653 dialog; it grows a row when the palette needs a fifth, and
//! scales down to fit between the bars.

use windows::Win32::UI::Input::KeyboardAndMouse::*;

use super::toolbar::{Item, Placed, ZONES, Zone};
use super::*;

const W: f32 = 1048.0;
const H: f32 = 653.0;
const HEADER: f32 = 91.0;
const COLUMNS: usize = 9;
const CELL_X: f32 = 77.0;
const CELL_PITCH_X: f32 = 110.0;
const CELL_Y: f32 = 141.0;
const CELL_PITCH_Y: f32 = 118.0;
/// The dashed square around a palette icon.
const ICON_BOX: f32 = 32.0;

const BG_HEADER: u32 = 0x090909;
const BG_BODY: u32 = 0x0E0E0E;
const BG_CANCEL: u32 = 0x1C1C1C;
const FG_TITLE: u32 = 0xB0B0B0;
const FG_HELP: u32 = 0x6E6E6E;
const FG_LABEL: u32 = 0x8A8A8A;
const DASH: u32 = 0x3A3A3A;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Body,
    Close,
    Cancel,
    Save,
    /// The palette outside its items: a drop there takes the item out of the bars.
    Box,
    Palette(Item),
    /// An item in the bars, by zone and index.
    Placed(Zone, usize),
    /// A group's empty part.
    Zone(Zone),
}

/// An item on its way somewhere, and where the pointer is.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub item: Item,
    /// Its place in the bars, or None from the palette.
    pub from: Option<(Zone, usize)>,
    pub at: (f32, f32),
}

/// Where a dragged item would land.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drop {
    /// Back to the palette: out of the bars.
    Remove,
    Into(Zone, usize),
}

pub struct State {
    pub draft: Toolbar,
    pub drag: Option<Drag>,
}

/// The open dialog in dialog units (0..W, 0..h), and the bars' parts in window DIPs.
#[derive(Clone, Default)]
pub struct Geometry {
    origin: (f32, f32),
    scale: f32,
    h: f32,
    cells: Vec<(Item, Rect)>,
    placed: Vec<Placed>,
    zones: [Rect; 4],
}

fn close() -> Rect {
    Rect::around(W - 24.0, 24.0, 32.0)
}

fn cancel(h: f32) -> Rect {
    Rect::new(19.0, h - 51.0, 117.0, h - 21.0)
}

fn save(h: f32) -> Rect {
    Rect::new(W - 119.0, h - 51.0, W - 18.0, h - 21.0)
}

impl Geometry {
    fn local(&self, x: f32, y: f32) -> (f32, f32) {
        (
            (x - self.origin.0) / self.scale,
            (y - self.origin.1) / self.scale,
        )
    }

    fn frame(&self) -> Rect {
        Rect::new(
            self.origin.0,
            self.origin.1,
            self.origin.0 + W * self.scale,
            self.origin.1 + self.h * self.scale,
        )
    }

    pub fn hit(&self, x: f32, y: f32) -> Target {
        if self.frame().contains(x, y) {
            let (x, y) = self.local(x, y);
            if close().contains(x, y) {
                return Target::Close;
            }
            if cancel(self.h).contains(x, y) {
                return Target::Cancel;
            }
            if save(self.h).contains(x, y) {
                return Target::Save;
            }
            return self
                .cells
                .iter()
                .find(|(_, r)| r.contains(x, y))
                .map_or(Target::Box, |(item, _)| Target::Palette(*item));
        }
        if let Some(p) = self.placed.iter().find(|p| p.r.contains(x, y)) {
            return Target::Placed(p.zone, p.index);
        }
        ZONES
            .iter()
            .zip(self.zones)
            .find(|(_, r)| r.contains(x, y))
            .map_or(Target::Body, |(z, _)| Target::Zone(*z))
    }

    /// Where an item dropped at (x, y) goes: the palette takes it out, a group takes it
    /// between the items either side of the pointer.
    pub fn drop_at(&self, x: f32, y: f32) -> Option<Drop> {
        if self.frame().contains(x, y) {
            return Some(Drop::Remove);
        }
        let (zone, _) = ZONES
            .iter()
            .zip(self.zones)
            .find(|(_, r)| r.contains(x, y))?;
        let index = self
            .placed
            .iter()
            .filter(|p| p.zone == *zone && (p.r.l + p.r.r) / 2.0 < x)
            .count();
        Some(Drop::Into(*zone, index))
    }

    /// The x where an item dropped into `zone` at `index` would go, for the insertion line.
    fn insert_x(&self, zone: Zone, index: usize) -> f32 {
        let items: Vec<&Placed> = self.placed.iter().filter(|p| p.zone == zone).collect();
        match (
            index.checked_sub(1).and_then(|i| items.get(i)),
            items.get(index),
        ) {
            (Some(a), Some(b)) => (a.r.r + b.r.l) / 2.0,
            (Some(a), None) => a.r.r + 4.0,
            (None, Some(b)) => b.r.l - 4.0,
            (None, None) => {
                let r = self.zones[ZONES.iter().position(|z| *z == zone).unwrap_or(0)];
                (r.l + r.r) / 2.0
            }
        }
    }
}

impl Layout {
    /// Lays the dialog out over a player layout built from the draft.
    pub fn add_customize(&mut self, st: &State) {
        let palette = st.draft.palette();
        let rows = palette.len().div_ceil(COLUMNS).max(4);
        let h = H + CELL_PITCH_Y * (rows - 4) as f32;
        let cells = palette
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let cx = CELL_X + CELL_PITCH_X * (i % COLUMNS) as f32;
                let cy = CELL_Y + CELL_PITCH_Y * (i / COLUMNS) as f32;
                (*item, Rect::new(cx - 50.0, cy - 24.0, cx + 50.0, cy + 74.0))
            })
            .collect();
        // Between the bars, with a margin; smaller windows scale it down.
        let (ww, wh) = (self.width, self.height);
        let (top, bottom) = (TITLE_BAR + 12.0, wh - self.tools.bar - 12.0);
        let scale = ((ww - 32.0) / W).min((bottom - top) / h).clamp(0.1, 1.0);
        self.customize = Some(Geometry {
            origin: (
                ((ww - W * scale) / 2.0).round(),
                (top + ((bottom - top) - h * scale) / 2.0).round(),
            ),
            scale,
            h,
            cells,
            placed: self.tools.placed.clone(),
            zones: self.tools.zones,
        });
    }
}

impl Ui {
    pub fn open_customize(&mut self) {
        self.close_menu();
        self.panels.show(None, None);
        self.queue.open = false;
        self.overlay = false;
        self.customize = Some(State {
            draft: self.toolbar.clone(),
            drag: None,
        });
        self.relayout = true;
        self.wake();
    }

    pub fn close_customize(&mut self) {
        if self.customize.take().is_some() {
            self.drag = None;
            self.relayout = true;
        }
    }

    /// A press on an item starts a drag.
    pub(super) fn customize_down(&mut self, target: Target, x: f32, y: f32) {
        let Some(st) = self.customize.as_mut() else {
            return;
        };
        let (item, from) = match target {
            Target::Palette(item) => (item, None),
            Target::Placed(z, i) => match st.draft.zone(z).get(i) {
                Some(item) => (*item, Some((z, i))),
                None => return,
            },
            _ => return,
        };
        st.drag = Some(Drag {
            item,
            from,
            at: (x, y),
        });
        self.drag = Some(Hit::Customize(target));
    }

    pub(super) fn customize_drag(&mut self, x: f32, y: f32) {
        if let Some(d) = self.customize.as_mut().and_then(|st| st.drag.as_mut()) {
            d.at = (x, y);
            self.dirty = true;
        }
    }

    /// The drag ends: the item moves where it was dropped.
    pub(super) fn customize_drop(&mut self, x: f32, y: f32) {
        let target = self.layout.customize.as_ref().and_then(|g| g.drop_at(x, y));
        let Some(st) = self.customize.as_mut() else {
            return;
        };
        let Some(d) = st.drag.take() else {
            return;
        };
        match (target, d.from) {
            (Some(Drop::Into(zone, index)), from) => st.draft.place(d.item, from, zone, index),
            (Some(Drop::Remove), Some((zone, index))) => st.draft.remove(zone, index),
            _ => {}
        }
        self.relayout = true;
    }

    pub(super) fn customize_click(&mut self, target: Target) -> Option<Action> {
        match target {
            Target::Close | Target::Cancel => {
                self.close_customize();
                None
            }
            Target::Save => {
                let draft = self.customize.take()?.draft;
                self.relayout = true;
                Some(Action::SaveToolbar(draft))
            }
            _ => None,
        }
    }

    pub fn customize_key(&mut self, key: VIRTUAL_KEY) -> Option<Action> {
        match key {
            VK_ESCAPE => {
                self.close_customize();
                None
            }
            VK_RETURN => self.customize_click(Target::Save),
            _ => None,
        }
    }
}

pub(super) struct Resources {
    veil: ID2D1SolidColorBrush,
    shadow: ID2D1SolidColorBrush,
    header: ID2D1SolidColorBrush,
    body: ID2D1SolidColorBrush,
    cancel: ID2D1SolidColorBrush,
    title_fg: ID2D1SolidColorBrush,
    help_fg: ID2D1SolidColorBrush,
    label_fg: ID2D1SolidColorBrush,
    dash: ID2D1SolidColorBrush,
    dashes: ID2D1StrokeStyle,
    title: IDWriteTextFormat,
    help: IDWriteTextFormat,
    label: IDWriteTextFormat,
    button: IDWriteTextFormat,
}

impl Resources {
    pub(super) fn new(
        ctx: &ID2D1DeviceContext5,
        factory: &ID2D1Factory1,
        dwrite: &IDWriteFactory6,
        fonts: &IDWriteFontCollection2,
    ) -> Result<Self> {
        let brush =
            |rgb: u32, alpha: f32| unsafe { ctx.CreateSolidColorBrush(&color(rgb, alpha), None) };
        let format =
            |size: f32, align| text_format(dwrite, fonts, size, DWRITE_FONT_WEIGHT_REGULAR, align);
        // Labels wrap onto a second line under their icon, as "Playback Speed" does.
        let label = format(13.0, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        unsafe {
            label.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            label.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            label.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 17.0, 13.0)?;
        }
        let dashes = unsafe {
            let props = D2D1_STROKE_STYLE_PROPERTIES1 {
                dashStyle: D2D1_DASH_STYLE_CUSTOM,
                ..Default::default()
            };
            factory.CreateStrokeStyle(&props, Some(&[3.0, 3.0]))?.into()
        };
        Ok(Resources {
            veil: brush(0x000000, 0.35)?,
            shadow: brush(0x000000, 0.12)?,
            header: brush(BG_HEADER, 1.0)?,
            body: brush(BG_BODY, 1.0)?,
            cancel: brush(BG_CANCEL, 1.0)?,
            title_fg: brush(FG_TITLE, 1.0)?,
            help_fg: brush(FG_HELP, 1.0)?,
            label_fg: brush(FG_LABEL, 1.0)?,
            dash: brush(DASH, 1.0)?,
            dashes,
            title: format(16.0, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            help: format(14.0, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            label,
            button: format(14.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
        })
    }
}

impl Painter {
    /// The bars' dashed boxes, the dialog over the video, and the item being dragged.
    pub(super) fn paint_customize(&self, ctx: &ID2D1DeviceContext5, s: &Scene, g: &Geometry) {
        let Some(st) = s.customize else {
            return;
        };
        let rt: &ID2D1RenderTarget = ctx;
        let r = &self.custom;
        let hover = |t: Target| s.hover == Some(Hit::Customize(t));
        unsafe {
            // Every group and every item in the bars gets a dashed edge.
            for zone in g.zones {
                rt.DrawRoundedRectangle(&rounded(zone, 4.0), &r.dash, 1.0, &r.dashes);
            }
            for p in &g.placed {
                rt.DrawRoundedRectangle(&rounded(p.r.inset(-2.0), 4.0), &r.dash, 1.0, &r.dashes);
                let dragged = st.drag.is_some_and(|d| d.from == Some((p.zone, p.index)));
                if dragged {
                    rt.FillRoundedRectangle(&rounded(p.r.inset(-2.0), 4.0), &r.veil);
                } else if hover(Target::Placed(p.zone, p.index)) {
                    rt.FillRoundedRectangle(&rounded(p.r.inset(-2.0), 4.0), &self.hover);
                }
            }

            let base = Matrix3x2 {
                M11: g.scale,
                M12: 0.0,
                M21: 0.0,
                M22: g.scale,
                M31: g.origin.0,
                M32: g.origin.1,
            };
            let frame = Rect::new(0.0, 0.0, W, g.h);
            ctx.SetTransform(&base);
            for spread in [12.0, 8.0, 4.0] {
                rt.FillRoundedRectangle(&rounded(frame.inset(-spread), 8.0 + spread), &r.shadow);
            }
            rt.FillRoundedRectangle(&rounded(frame, 8.0), &r.body);
            let band = Rect::new(0.0, 0.0, W, HEADER);
            rt.PushAxisAlignedClip(&band.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            rt.FillRoundedRectangle(&rounded(frame, 8.0), &r.header);
            rt.PopAxisAlignedClip();
            let title = Rect::new(20.0, 10.0, W - 60.0, 38.0);
            self.put(rt, "Customize Player", &r.title, title, &r.title_fg);
            let help = "Drag items around and out of the box below to re-order them. Drag items \
                        into the box to remove them from the player.";
            self.put(
                rt,
                help,
                &r.help,
                Rect::new(20.0, 56.0, W - 20.0, 80.0),
                &r.help_fg,
            );
            if hover(Target::Close) {
                rt.FillRoundedRectangle(&rounded(close(), 4.0), &self.hover);
            }
            self.prefs_glyph(ctx, &base, &self.icons.close, W - 24.0, 24.0, 24.0);

            for (item, cell) in &g.cells {
                let (cx, cy) = ((cell.l + cell.r) / 2.0, cell.t + 24.0);
                let icon_box = Rect::around(cx, cy, ICON_BOX);
                if hover(Target::Palette(*item)) {
                    rt.FillRoundedRectangle(&rounded(icon_box, 4.0), &self.hover);
                }
                rt.DrawRoundedRectangle(&rounded(icon_box, 4.0), &r.dash, 1.0, &r.dashes);
                self.prefs_glyph(ctx, &base, self.tool_icon(*item), cx, cy, 24.0);
                self.tool_number(rt, *item, cx, cy);
                let label = Rect::new(cell.l, cy + 30.0, cell.r, cy + 72.0);
                let utf16: Vec<u16> = item.label().encode_utf16().collect();
                rt.DrawText(
                    &utf16,
                    &r.label,
                    &label.d2d(),
                    &r.label_fg,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }

            let (cancel, save) = (cancel(g.h), save(g.h));
            rt.FillRoundedRectangle(&rounded(cancel, 4.0), &r.cancel);
            if hover(Target::Cancel) {
                rt.FillRoundedRectangle(&rounded(cancel, 4.0), &self.hover);
            }
            self.put(rt, "Cancel", &r.button, cancel, &r.label_fg);
            rt.FillRoundedRectangle(&rounded(save, 4.0), &self.played);
            if hover(Target::Save) {
                rt.FillRoundedRectangle(&rounded(save, 4.0), &self.hover);
            }
            self.put(rt, "Save", &r.button, save, &self.accent);
            ctx.SetTransform(&Matrix3x2::identity());

            // Where the dragged item would land, then the item under the pointer.
            if let Some(d) = st.drag {
                if let Some(Drop::Into(zone, index)) = g.drop_at(d.at.0, d.at.1) {
                    let x = g.insert_x(zone, index);
                    let z = g.zones[ZONES.iter().position(|z2| *z2 == zone).unwrap_or(0)];
                    let line = Rect::new(x - 1.0, z.t + 4.0, x + 1.0, z.b - 4.0);
                    rt.FillRoundedRectangle(&rounded(line, 1.0), &self.accent);
                }
                let ghost = Rect::around(d.at.0, d.at.1, 40.0);
                rt.FillRoundedRectangle(&rounded(ghost, 6.0), &self.bg);
                rt.DrawRoundedRectangle(&rounded(ghost, 6.0), &self.accent, 1.0, None);
                let (gx, gy) = (ghost.l + 8.0, ghost.t + 8.0);
                self.icon(ctx, self.tool_icon(d.item), gx, gy, false);
                self.tool_number(rt, d.item, gx + 12.0, gy + 12.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Ui {
        let mut ui = Ui::new();
        ui.open_customize();
        let st = ui.customize.as_ref().unwrap();
        let mut layout = Layout::player(1920.0, 1080.0, true, false, &st.draft);
        layout.add_customize(st);
        ui.layout = layout;
        ui
    }

    #[test]
    fn the_palette_lists_what_the_bars_dont_have() {
        let ui = open();
        let g = ui.layout.customize.as_ref().unwrap();
        // The design's 31, less Folder Name, which the title bar starts with.
        assert_eq!(g.cells.len(), 30);
        assert_eq!(g.cells[0].0, Item::Spacer);
        assert_eq!(g.h, H);
        assert!(
            g.cells
                .iter()
                .all(|(i, _)| !matches!(i, Item::Play | Item::Queue))
        );
    }

    #[test]
    fn dragging_from_the_palette_into_a_group_and_back_out() {
        let mut ui = open();
        // Stop goes between Play and Next.
        let (play, next) = {
            let at = |item| {
                ui.layout
                    .tools
                    .placed
                    .iter()
                    .find(|p| p.item == item)
                    .unwrap()
                    .r
            };
            (at(Item::Play), at(Item::Next))
        };
        ui.customize_down(Target::Palette(Item::Stop), 0.0, 0.0);
        let x = (play.r + next.l) / 2.0;
        ui.customize_drop(x, (play.t + play.b) / 2.0);
        let st = ui.customize.as_ref().unwrap();
        assert_eq!(
            st.draft.left,
            [Item::Previous, Item::Play, Item::Stop, Item::Next]
        );
        // Dropped on the dialog, it comes out again.
        let mut layout = Layout::player(1920.0, 1080.0, true, false, &st.draft);
        layout.add_customize(st);
        ui.layout = layout;
        ui.customize_down(Target::Placed(Zone::Left, 2), 0.0, 0.0);
        ui.customize_drop(960.0, 500.0);
        assert_eq!(ui.customize.as_ref().unwrap().draft.left.len(), 3);
        // Save hands the draft over.
        let Some(Action::SaveToolbar(t)) = ui.customize_click(Target::Save) else {
            panic!("save keeps the draft");
        };
        assert_eq!(t, Toolbar::default());
        assert!(ui.customize.is_none());
    }
}

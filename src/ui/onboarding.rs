//! The first-run welcome, from the design's eleven "Onboarding" frames: a welcome with the media
//! folder to start from, the theme and accent colour over a small picture of the player, and the
//! basic keyboard shortcuts. It shows over the library the first time wushang starts (no
//! settings.txt yet). Theme and colour apply as they're picked, like Settings > General.
//!
//! Sizes are DIPs from the 1088x756 frames; on a small window it scales down. The design's text
//! says VLC and lists VLC's keys; this says wushang and lists wushang's.

use super::*;
use crate::prefs::Prefs;

const W: f32 = 1088.0;
const H: f32 = 756.0;

// Sampled from the frames.
const BG_FRAME: u32 = 0x0E0E0E;
const BG_FIELD: u32 = 0x090909;
const BG_BUTTON: u32 = 0x1C1C1C;
const FG_TITLE: u32 = 0xC8C8C8;
const FG_TEXT: u32 = 0x8A8A8A;
const FG_HELP: u32 = 0x6E6E6E;
const FG_ON_ACCENT: u32 = 0x161616;
const TILE_LIGHT: u32 = 0xAFAFAF;
const TILE_DARK: u32 = 0x222222;
const KEY_FACE: u32 = 0x1C1C1C;
const KEY_EDGE: u32 = 0x2E2E2E;
const KEY_TOP: u32 = 0x363636;

/// Next and Finish, and Back beside them.
const NEXT: Rect = Rect::new(926.0, 669.0, 1026.0, 701.0);
const BACK: Rect = Rect::new(821.0, 669.0, 916.0, 701.0);
const BROWSE: Rect = Rect::new(725.0, 532.0, 841.0, 567.0);
const FIELD: Rect = Rect::new(246.0, 532.0, 715.0, 567.0);
/// Step 2's picture of the player, and the size of its bars.
const PREVIEW: Rect = Rect::new(16.0, 16.0, 1072.0, 608.0);
const PREVIEW_BAR: f32 = 26.0;
const TILE: f32 = 102.0;
const TILE_X: [f32; 3] = [389.0, 544.0, 699.0];
const TILE_Y: f32 = 342.0;
const CIRCLE: f32 = 41.0;
const CIRCLE_Y: f32 = 686.0;
/// The theme tiles in the design's order, as `prefs::THEME` indices: Light, Dark, System.
const TILES: [(usize, &str); 3] = [(1, "Light"), (0, "Dark"), (2, "System")];
/// Step 3's rows: label and keys, the left column then the right. Keys joined by "/" are
/// alternatives; the others are pressed together.
const KEYS: [(&str, &[&str], bool); 12] = [
    ("Play / Pause", &["Space"], false),
    ("Seek Backward / Forward", &["\u{2190}", "\u{2192}"], true),
    ("Previous / Next Item", &["P", "N"], true),
    ("Stop", &["S"], false),
    ("Mute", &["M"], false),
    ("Settings", &["\u{2318}", "P"], false),
    ("Toggle Fullscreen", &["F"], false),
    (
        "Increase / Decrease Volume",
        &["\u{2191}", "\u{2193}"],
        true,
    ),
    ("Mini Player", &["\u{2318}", "M"], false),
    ("Hide Controls", &["\u{2318}", "\\"], false),
    ("Media Information", &["\u{2318}", "I"], false),
    ("Quit", &["Q"], false),
];
const KEY_H: f32 = 40.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// The frame outside its controls, or the dimmed window around it.
    Body,
    Browse,
    Back,
    /// Next, or Finish on the last step.
    Next,
    /// A theme tile, as a `prefs::THEME` index.
    Theme(usize),
    /// An accent circle, by index into `theme::ACCENTS`.
    Accent(usize),
}

/// What main.rs does for the welcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnboardingAction {
    /// The folder picker; its folder joins the library's Videos.
    PickFolder,
}

/// The open welcome.
pub struct State {
    pub step: usize,
    pub prefs: Prefs,
    /// The library's first media folder, shown in step 1.
    pub folder: String,
}

impl State {
    pub fn new(prefs: Prefs, folder: String) -> Self {
        State {
            step: 0,
            prefs,
            folder,
        }
    }
}

/// Where the open welcome sits, in frame units (0..W, 0..H).
#[derive(Clone, Default)]
pub struct Geometry {
    /// Window DIPs = origin + frame units * scale.
    origin: (f32, f32),
    scale: f32,
    window: (f32, f32),
    step: usize,
}

impl Geometry {
    pub fn hit(&self, x: f32, y: f32) -> Target {
        let (x, y) = (
            (x - self.origin.0) / self.scale,
            (y - self.origin.1) / self.scale,
        );
        if NEXT.contains(x, y) {
            return Target::Next;
        }
        match self.step {
            0 if BROWSE.contains(x, y) => Target::Browse,
            1 | 2 if BACK.contains(x, y) => Target::Back,
            1 => {
                if let Some(i) = TILE_X
                    .iter()
                    .position(|cx| Rect::around(*cx, TILE_Y, TILE).contains(x, y))
                {
                    return Target::Theme(TILES[i].0);
                }
                (0..theme::ACCENTS.len())
                    .find(|i| circle(*i).contains(x, y))
                    .map_or(Target::Body, Target::Accent)
            }
            _ => Target::Body,
        }
    }
}

/// An accent circle's square.
fn circle(i: usize) -> Rect {
    Rect::around(74.0 + 80.0 * i as f32, CIRCLE_Y, CIRCLE)
}

impl Layout {
    pub fn add_onboarding(&mut self, st: &State) {
        let (ww, wh) = (self.width, self.height);
        let margin = 16.0;
        let scale = ((ww - 2.0 * margin) / W)
            .min((wh - 2.0 * margin) / H)
            .clamp(0.1, 1.0);
        self.onboarding = Some(Geometry {
            origin: (
                ((ww - W * scale) / 2.0).round(),
                ((wh - H * scale) / 2.0).round(),
            ),
            scale,
            window: (ww, wh),
            step: st.step,
        });
    }
}

impl Ui {
    pub fn open_onboarding(&mut self, prefs: Prefs, folder: String) {
        self.close_menu();
        self.onboarding = Some(State::new(prefs, folder));
        self.relayout = true;
        self.wake();
    }

    pub fn close_onboarding(&mut self) {
        if self.onboarding.take().is_some() {
            self.relayout = true;
        }
    }

    /// The folder main.rs added from the picker.
    pub fn onboarding_folder(&mut self, folder: String) {
        if let Some(st) = self.onboarding.as_mut() {
            st.folder = folder;
            self.dirty = true;
        }
    }

    pub(super) fn onboarding_click(&mut self, target: Target) -> Option<Action> {
        let st = self.onboarding.as_mut()?;
        self.relayout = true;
        match target {
            Target::Body => None,
            Target::Browse => Some(Action::Onboarding(OnboardingAction::PickFolder)),
            Target::Back => {
                st.step = st.step.saturating_sub(1);
                None
            }
            Target::Next if st.step < 2 => {
                st.step += 1;
                None
            }
            Target::Next => {
                self.close_onboarding();
                None
            }
            Target::Theme(i) => {
                st.prefs.theme = i;
                Some(Action::Preferences(PrefsAction::Changed(Box::new(
                    st.prefs.clone(),
                ))))
            }
            Target::Accent(i) => {
                st.prefs.accent = theme::ACCENTS[i];
                Some(Action::Preferences(PrefsAction::Changed(Box::new(
                    st.prefs.clone(),
                ))))
            }
        }
    }

    /// Enter goes on, Esc skips the rest.
    pub fn onboarding_key(&mut self, key: VIRTUAL_KEY) -> Option<Action> {
        match key {
            VK_RETURN => self.onboarding_click(Target::Next),
            VK_ESCAPE => {
                self.close_onboarding();
                None
            }
            _ => None,
        }
    }
}

pub(super) struct Resources {
    veil: ID2D1SolidColorBrush,
    shadow: ID2D1SolidColorBrush,
    frame: ID2D1SolidColorBrush,
    field: ID2D1SolidColorBrush,
    button: ID2D1SolidColorBrush,
    title_fg: ID2D1SolidColorBrush,
    text_fg: ID2D1SolidColorBrush,
    help_fg: ID2D1SolidColorBrush,
    on_accent: ID2D1SolidColorBrush,
    /// Step 2's text sits on the preview's black video, so it stays light in either theme.
    over_video: ID2D1SolidColorBrush,
    over_video_dim: ID2D1SolidColorBrush,
    key_face: ID2D1SolidColorBrush,
    key_edge: ID2D1SolidColorBrush,
    key_top: ID2D1SolidColorBrush,
    /// Theme tiles and accent circles show their real colours in either theme.
    swatch: ID2D1SolidColorBrush,
    welcome: IDWriteTextFormat,
    heading: IDWriteTextFormat,
    body: IDWriteTextFormat,
    text: IDWriteTextFormat,
    centred: IDWriteTextFormat,
    small: IDWriteTextFormat,
    key: IDWriteTextFormat,
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
        let (lead, centre) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER);
        // The welcome's paragraph wraps, on the design's 28 DIP lines.
        let body = format(15.0, regular, lead)?;
        unsafe {
            body.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            body.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            body.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 28.0, 21.0)?;
        }
        Ok(Resources {
            veil: brush(0x000000, 0.5)?,
            shadow: brush(0x000000, 0.12)?,
            frame: brush(BG_FRAME, 1.0)?,
            field: brush(BG_FIELD, 1.0)?,
            button: brush(BG_BUTTON, 1.0)?,
            title_fg: brush(FG_TITLE, 1.0)?,
            text_fg: brush(FG_TEXT, 1.0)?,
            help_fg: brush(FG_HELP, 1.0)?,
            // Dark on the accent in either theme, as the design's Next.
            on_accent: brush(theme::fixed(FG_ON_ACCENT), 1.0)?,
            over_video: brush(theme::fixed(FG_TITLE), 1.0)?,
            over_video_dim: brush(theme::fixed(FG_TEXT), 1.0)?,
            key_face: brush(KEY_FACE, 1.0)?,
            key_edge: brush(KEY_EDGE, 1.0)?,
            key_top: brush(KEY_TOP, 1.0)?,
            swatch: brush(theme::fixed(0xFFFFFF), 1.0)?,
            welcome: format(32.0, regular, centre)?,
            heading: format(20.0, regular, centre)?,
            body,
            text: format(14.0, regular, lead)?,
            centred: format(14.0, regular, centre)?,
            small: format(13.0, regular, lead)?,
            key: format(13.0, medium, centre)?,
        })
    }
}

impl Painter {
    /// Dims the window and paints the welcome on top.
    pub(super) fn paint_onboarding(&self, ctx: &ID2D1DeviceContext5, s: &Scene, g: &Geometry) {
        let Some(st) = s.onboarding else {
            return;
        };
        let rt: &ID2D1RenderTarget = ctx;
        let r = &self.onboard;
        let base = Matrix3x2 {
            M11: g.scale,
            M12: 0.0,
            M21: 0.0,
            M22: g.scale,
            M31: g.origin.0,
            M32: g.origin.1,
        };
        let frame = Rect::new(0.0, 0.0, W, H);
        let hover = |t: Target| s.hover == Some(Hit::Onboarding(t));
        unsafe {
            let window = Rect::new(0.0, 0.0, g.window.0, g.window.1);
            rt.FillRectangle(&window.d2d(), &r.veil);
            ctx.SetTransform(&base);
            for spread in [12.0, 8.0, 4.0] {
                rt.FillRoundedRectangle(&rounded(frame.inset(-spread), 8.0 + spread), &r.shadow);
            }
            rt.FillRoundedRectangle(&rounded(frame, 8.0), &r.frame);
            match st.step {
                0 => self.welcome_step(rt, ctx, &base, st, hover(Target::Browse)),
                1 => self.theme_step(rt, ctx, &base, s, st),
                _ => self.keys_step(rt),
            }
            if st.step > 0 {
                rt.FillRoundedRectangle(&rounded(BACK, 4.0), &r.button);
                if hover(Target::Back) {
                    rt.FillRoundedRectangle(&rounded(BACK, 4.0), &self.hover);
                }
                self.put(rt, "Back", &r.centred, BACK, &r.text_fg);
            }
            rt.FillRoundedRectangle(&rounded(NEXT, 4.0), &self.accent);
            if hover(Target::Next) {
                rt.FillRoundedRectangle(&rounded(NEXT, 4.0), &self.hover);
            }
            let next = if st.step == 2 { "Finish" } else { "Next" };
            self.put(rt, next, &r.centred, NEXT, &r.on_accent);
            ctx.SetTransform(&Matrix3x2::identity());
        }
    }

    unsafe fn welcome_step(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        base: &Matrix3x2,
        st: &State,
        browse_hovered: bool,
    ) {
        let r = &self.onboard;
        unsafe {
            self.prefs_glyph(ctx, base, &self.icons.logo, W / 2.0, 186.0, 124.0);
            let title = Rect::new(0.0, 285.0, W, 325.0);
            self.put(rt, "Welcome to wushang!", &r.welcome, title, &r.title_fg);
            let text = "wushang is a free video and music player built on FFmpeg. It plays most \
                        video and audio files, with their subtitles and tracks, and keeps a \
                        library of your media folders.";
            let utf16: Vec<u16> = text.encode_utf16().collect();
            let para = Rect::new(FIELD.l, 362.0, BROWSE.r, 460.0);
            rt.DrawText(
                &utf16,
                &r.body,
                &para.d2d(),
                &r.help_fg,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            let label = Rect::new(FIELD.l, 498.0, BROWSE.r, 522.0);
            self.put(
                rt,
                "Choose a media folder to get started with",
                &r.small,
                label,
                &r.text_fg,
            );
            rt.FillRoundedRectangle(&rounded(FIELD, 4.0), &r.field);
            let path = Rect::new(FIELD.l + 14.0, FIELD.t, FIELD.r - 10.0, FIELD.b);
            self.put(rt, &st.folder, &r.text, path, &r.title_fg);
            rt.FillRoundedRectangle(&rounded(BROWSE, 4.0), &r.button);
            if browse_hovered {
                rt.FillRoundedRectangle(&rounded(BROWSE, 4.0), &self.hover);
            }
            self.put(rt, "Browse", &r.centred, BROWSE, &r.text_fg);
        }
    }

    /// The theme tiles and accent circles over a small picture of the player, which shows the
    /// colours as they're picked.
    unsafe fn theme_step(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        base: &Matrix3x2,
        s: &Scene,
        st: &State,
    ) {
        let r = &self.onboard;
        let p = PREVIEW;
        unsafe {
            rt.FillRoundedRectangle(&rounded(p, 6.0), &self.black);
            let top = Rect::new(p.l, p.t, p.r, p.t + PREVIEW_BAR);
            let bottom = Rect::new(p.l, p.b - PREVIEW_BAR, p.r, p.b);
            for bar in [top, bottom] {
                rt.PushAxisAlignedClip(&bar.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
                rt.FillRoundedRectangle(&rounded(p, 6.0), &self.bg);
                rt.PopAxisAlignedClip();
            }
            // The player's control bar at 0.64: play button, seek bar and its played part.
            let cy = (bottom.t + bottom.b) / 2.0;
            let play = Rect::new(p.l + 37.0, cy - 9.0, p.l + 72.0, cy + 9.0);
            rt.FillRoundedRectangle(&rounded(play, 3.0), &self.accent);
            let track = Rect::new(p.l + 163.0, cy - 6.0, p.r - 274.0, cy + 6.0);
            rt.FillRoundedRectangle(&rounded(track, 3.0), &self.track);
            let played = Rect::new(track.l + 1.0, track.t + 1.0, track.l + 50.0, track.b - 1.0);
            rt.FillRoundedRectangle(&rounded(played, 2.0), &self.played);
            self.prefs_glyph(ctx, base, &self.icons.logo, p.l + 16.0, top.t + 13.0, 15.0);

            let heading = Rect::new(0.0, 178.0, W, 208.0);
            let ask = "Select your preferred theme and color.";
            self.put(rt, ask, &r.heading, heading, &r.over_video);
            for (i, (theme, name)) in TILES.iter().enumerate() {
                let tile = Rect::around(TILE_X[i], TILE_Y, TILE);
                self.theme_tile(rt, tile, *theme);
                if st.prefs.theme == *theme {
                    let ring = rounded(tile.inset(-8.0), 16.0);
                    rt.DrawRoundedRectangle(&ring, &self.accent, 3.0, None);
                } else if s.hover == Some(Hit::Onboarding(Target::Theme(*theme))) {
                    rt.FillRoundedRectangle(&rounded(tile, 10.0), &self.hover);
                }
                let label = Rect::new(tile.l - 20.0, tile.b + 30.0, tile.r + 20.0, tile.b + 54.0);
                self.put(rt, name, &r.centred, label, &r.over_video_dim);
            }
            // The accents, the picked one bright in a ring of itself; the others dimmed.
            for (i, rgb) in theme::ACCENTS.iter().enumerate() {
                let c = circle(i);
                let (cx, cy) = ((c.l + c.r) / 2.0, (c.t + c.b) / 2.0);
                let picked = st.prefs.accent == *rgb;
                let hovered = s.hover == Some(Hit::Onboarding(Target::Accent(i)));
                let alpha = if picked || hovered { 1.0 } else { 0.5 };
                r.swatch.SetColor(&color(theme::fixed(*rgb), alpha));
                let dot = D2D1_ELLIPSE {
                    point: Vector2 { X: cx, Y: cy },
                    radiusX: CIRCLE / 2.0,
                    radiusY: CIRCLE / 2.0,
                };
                rt.FillEllipse(&dot, &r.swatch);
                if picked {
                    r.swatch.SetColor(&color(theme::fixed(*rgb), 0.45));
                    let ring = D2D1_ELLIPSE {
                        radiusX: CIRCLE / 2.0 + 8.5,
                        radiusY: CIRCLE / 2.0 + 8.5,
                        ..dot
                    };
                    rt.DrawEllipse(&ring, &r.swatch, 3.0, None);
                }
            }
        }
    }

    /// A theme's tile: its background grey, or System's two greys split on the diagonal.
    unsafe fn theme_tile(&self, rt: &ID2D1RenderTarget, tile: Rect, theme: usize) {
        let r = &self.onboard;
        let fill = |rgb: u32| unsafe {
            r.swatch.SetColor(&color(theme::fixed(rgb), 1.0));
            rt.FillRoundedRectangle(&rounded(tile, 10.0), &r.swatch);
        };
        match theme {
            1 => fill(TILE_LIGHT),
            0 => fill(TILE_DARK),
            _ => unsafe {
                fill(TILE_DARK);
                // The light half: above the line from a quarter along the top to three quarters
                // along the bottom, clipped to the tile's rounded corners.
                let Ok(path) = self.factory.CreatePathGeometry() else {
                    return;
                };
                let Ok(sink) = path.Open() else {
                    return;
                };
                let (l, t, w, b) = (tile.l, tile.t, tile.w(), tile.b);
                sink.BeginFigure(
                    Vector2 {
                        X: l + w * 0.25,
                        Y: t,
                    },
                    D2D1_FIGURE_BEGIN_FILLED,
                );
                sink.AddLine(Vector2 { X: tile.r, Y: t });
                sink.AddLine(Vector2 { X: tile.r, Y: b });
                sink.AddLine(Vector2 {
                    X: l + w * 0.75,
                    Y: b,
                });
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                if sink.Close().is_err() {
                    return;
                }
                let Ok(clip) = self
                    .factory
                    .CreateRoundedRectangleGeometry(&rounded(tile, 10.0))
                    .and_then(|g| g.cast::<ID2D1Geometry>())
                else {
                    return;
                };
                let params = D2D1_LAYER_PARAMETERS {
                    contentBounds: tile.d2d(),
                    geometricMask: std::mem::ManuallyDrop::new(Some(clip)),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: Matrix3x2::identity(),
                    opacity: 1.0,
                    opacityBrush: std::mem::ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS_NONE,
                };
                rt.PushLayer(&params, None);
                drop(std::mem::ManuallyDrop::into_inner(params.geometricMask));
                r.swatch.SetColor(&color(theme::fixed(TILE_LIGHT), 1.0));
                rt.FillGeometry(&path, &r.swatch, None);
                rt.PopLayer();
            },
        }
    }

    unsafe fn keys_step(&self, rt: &ID2D1RenderTarget) {
        let r = &self.onboard;
        unsafe {
            let heading = Rect::new(0.0, 66.0, W, 96.0);
            self.put(
                rt,
                "Learn the basic keyboard shortcuts.",
                &r.heading,
                heading,
                &r.title_fg,
            );
            for (i, (label, keys, either)) in KEYS.iter().enumerate() {
                let (column, row) = (i / 6, i % 6);
                let cy = 173.0 + 82.2 * row as f32;
                let (label_x, keys_x) = if column == 0 {
                    (61.0, 303.0)
                } else {
                    (603.0, 843.0)
                };
                let lr = Rect::new(label_x, cy - 12.0, keys_x - 16.0, cy + 12.0);
                self.put(rt, label, &r.text, lr, &r.text_fg);
                let mut x = keys_x;
                for (k, key) in keys.iter().enumerate() {
                    if k > 0 {
                        if *either {
                            let slash = Rect::new(x, cy - 12.0, x + 24.0, cy + 12.0);
                            self.put(rt, "/", &r.centred, slash, &r.help_fg);
                            x += 24.0;
                        } else {
                            x += 8.0;
                        }
                    }
                    let w = match *key {
                        "Space" => 94.0,
                        k if k.chars().count() > 1 => 70.0,
                        _ => 47.0,
                    };
                    self.keycap(
                        rt,
                        Rect::new(x, cy - KEY_H / 2.0, x + w, cy + KEY_H / 2.0),
                        key,
                    );
                    x += w;
                }
            }
            let note = Rect::new(61.0, 671.0, 700.0, 695.0);
            let text = "The full list is in the \u{2026} menu, or press F1.";
            self.put(rt, text, &r.text, note, &r.help_fg);
        }
    }

    /// A key: a face over a slightly lighter lower edge, with a highlight along its top.
    unsafe fn keycap(&self, rt: &ID2D1RenderTarget, k: Rect, label: &str) {
        let r = &self.onboard;
        unsafe {
            rt.FillRoundedRectangle(&rounded(k, 6.0), &r.key_edge);
            let face = Rect::new(k.l, k.t, k.r, k.b - 4.0);
            rt.FillRoundedRectangle(&rounded(face, 6.0), &r.key_face);
            let top = Rect::new(k.l + 5.0, k.t, k.r - 5.0, k.t + 1.0);
            rt.FillRectangle(&top.d2d(), &r.key_top);
            self.put(rt, label, &r.key, face, &r.title_fg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(step: usize, x: f32, y: f32) -> Target {
        let g = Geometry {
            origin: (0.0, 0.0),
            scale: 1.0,
            window: (W, H),
            step,
        };
        g.hit(x, y)
    }

    #[test]
    fn each_step_has_its_controls() {
        assert_eq!(at(0, 780.0, 550.0), Target::Browse);
        assert_eq!(
            at(0, 860.0, 685.0),
            Target::Body,
            "no Back on the first step"
        );
        assert_eq!(at(1, 860.0, 685.0), Target::Back);
        assert_eq!(at(1, 389.0, 342.0), Target::Theme(1), "Light comes first");
        assert_eq!(at(1, 544.0, 342.0), Target::Theme(0));
        assert_eq!(at(1, 74.0 + 80.0 * 5.0, CIRCLE_Y), Target::Accent(5));
        assert_eq!(at(2, 976.0, 685.0), Target::Next);
        assert_eq!(at(2, 389.0, 342.0), Target::Body);
    }

    #[test]
    fn next_walks_the_steps_and_finish_closes() {
        let mut ui = Ui::new();
        ui.open_onboarding(Prefs::default(), "C:\\Videos".into());
        ui.onboarding_click(Target::Next);
        assert_eq!(ui.onboarding.as_ref().unwrap().step, 1);
        let Some(Action::Preferences(PrefsAction::Changed(p))) =
            ui.onboarding_click(Target::Accent(5))
        else {
            panic!("picking a colour changes the settings");
        };
        assert_eq!(p.accent, theme::ACCENTS[5]);
        ui.onboarding_click(Target::Back);
        assert_eq!(ui.onboarding.as_ref().unwrap().step, 0);
        for _ in 0..3 {
            ui.onboarding_click(Target::Next);
        }
        assert!(ui.onboarding.is_none());
        assert_eq!(crate::prefs::THEME[TILES[0].0].1, "Light");
    }
}

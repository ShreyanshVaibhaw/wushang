//! The mini player, from the design's "Miniplayer" component: the playing file's video, progress
//! and name on a card, with close, expand and previous / play / next over the video while the
//! pointer is on the card, and the queue above it when its button is on. The library shows the
//! card at its bottom-right while something plays; the mini player window (Ctrl+M) is the card
//! alone, kept above other windows. Sizes are DIPs from the component.

use std::mem::ManuallyDrop;

use super::*;

pub const W: f32 = 391.0;
pub const H: f32 = 338.0;
/// The queue's part of the card when it's open, above the video.
pub const QUEUE_H: f32 = 349.0;
/// Less room than this above the card and the queue stays shut: it would show no rows.
const QUEUE_MIN: f32 = 160.0;

/// Where the card's parts sit.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Card {
    /// The whole card, the queue included.
    pub frame: Rect,
    /// The top of the video's part (the frame's top unless the queue is open).
    top: f32,
    /// Where the design's card would start if it ended at the bottom: the strip under the
    /// video (progress, name, folder) keeps the design's size and sits from here, and a bigger
    /// mini player window gives the extra room to the video.
    foot: f32,
    pub video: Rect,
    pub progress: Rect,
    pub queue: bool,
    /// The card is the mini player window: no shadow, and its body moves the window.
    pub window: bool,
}

impl Card {
    /// A card with its bottom-left corner at (l, b) and `queue` DIPs of queue above it (0 for
    /// none, up to `QUEUE_H`).
    pub fn new(l: f32, b: f32, queue: f32, window: bool) -> Card {
        Card::sized(l, b, W, H, queue, window)
    }

    /// Like `new`, the card `w` by `h` DIPs (at least the design's size) without its queue.
    pub fn sized(l: f32, b: f32, w: f32, h: f32, queue: f32, window: bool) -> Card {
        let (w, h) = (w.max(W), h.max(H));
        let (top, foot) = (b - h, b - H);
        let queue = if queue >= QUEUE_MIN {
            queue.min(QUEUE_H)
        } else {
            0.0
        };
        let t = top - queue;
        Card {
            frame: Rect::new(l, t, l + w, b),
            top,
            foot,
            video: Rect::new(l + 8.0, top + 8.0, l + w - 8.0, foot + 217.0),
            progress: Rect::new(l + 8.0, foot + 223.0, l + w - 8.0, foot + 237.0),
            queue: queue > 0.0,
            window,
        }
    }

    /// The queue's part, when it's open.
    pub fn queue_frame(&self) -> Option<Rect> {
        let f = self.frame;
        self.queue.then(|| Rect::new(f.l, f.t, f.r, self.top))
    }

    pub fn buttons(&self) -> [(Hit, Rect); 6] {
        let v = self.video;
        let (cx, cy) = ((v.l + v.r) / 2.0, (v.t + v.b) / 2.0);
        [
            (Hit::MiniClose, Rect::around(v.l + 26.0, v.t + 26.0, 32.0)),
            (Hit::MiniExpand, Rect::around(v.r - 26.0, v.t + 26.0, 32.0)),
            (Hit::Previous, Rect::around(cx - 76.0, cy, 36.0)),
            (Hit::Play, Rect::around(cx, cy, 40.0)),
            (Hit::Next, Rect::around(cx + 76.0, cy, 36.0)),
            (
                Hit::Playlist,
                Rect::around(self.frame.r - 31.0, self.foot + 310.0, 32.0),
            ),
        ]
    }

    /// What's under (x, y) on the card outside the queue.
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if !self.frame.contains(x, y) {
            return None;
        }
        if let Some((hit, _)) = self.buttons().iter().find(|(_, r)| r.contains(x, y)) {
            return Some(*hit);
        }
        let p = self.progress;
        if Rect::new(p.l, p.t - 4.0, p.r, p.b + 4.0).contains(x, y) {
            return Some(Hit::Seek);
        }
        Some(if self.window { Hit::Caption } else { Hit::Card })
    }

    /// Whether the pointer is on the card, so its controls show.
    fn hovered(&self, hover: Option<Hit>) -> bool {
        match hover {
            Some(Hit::Caption) => self.window,
            Some(
                Hit::Card
                | Hit::MiniClose
                | Hit::MiniExpand
                | Hit::Previous
                | Hit::Play
                | Hit::Next
                | Hit::Seek
                | Hit::Playlist,
            ) => true,
            _ => false,
        }
    }
}

impl Layout {
    /// The mini player window: the card fills its width and `card` DIPs of its height (the size
    /// the user gave it), the queue whatever height is left above that.
    pub fn mini(width: f32, height: f32, card: f32, queue: bool) -> Layout {
        let queue = if queue { height - card } else { 0.0 };
        let card = Card::sized(0.0, height, width, card.min(height), queue, true);
        Layout {
            page: Page::Mini,
            bars: true,
            width,
            height,
            video: card.video,
            track: card.progress,
            card: Some(card),
            ..Default::default()
        }
    }
}

/// The controls drawn over the video, light in either theme.
pub(super) struct Resources {
    name: IDWriteTextFormat,
    label: ID2D1SolidColorBrush,
    shadow: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    close: Svg,
    expand: Svg,
    previous: Svg,
    play: Svg,
    pause: Svg,
    next: Svg,
}

impl Resources {
    pub(super) fn new(
        ctx: &ID2D1DeviceContext5,
        dwrite: &IDWriteFactory6,
        fonts: &IDWriteFontCollection2,
    ) -> Result<Self> {
        let brush =
            |rgb: u32, alpha: f32| unsafe { ctx.CreateSolidColorBrush(&color(rgb, alpha), None) };
        // Sampled from the hover variant: near-white icons on the video dimmed by 60% black.
        let bright = |bytes: &[u8]| {
            let text = String::from_utf8_lossy(bytes)
                .replace("#8F8F8F", "#D0D0D0")
                .replace("#6B6B6B", "#D0D0D0")
                .replace("#161616", "#D0D0D0")
                .replace("#0E0E0E", "#D0D0D0");
            svg_as_is(ctx, text.as_bytes())
        };
        Ok(Resources {
            name: text_format(
                dwrite,
                fonts,
                15.0,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            label: brush(FG_LABEL, 1.0)?,
            shadow: brush(0x000000, 0.18)?,
            dim: brush(theme::fixed(0x000000), 0.6)?,
            close: bright(include_bytes!("../../assets/icons/close.svg"))?,
            expand: bright(include_bytes!("../../assets/icons/open_external.svg"))?,
            previous: bright(include_bytes!("../../assets/icons/previous.svg"))?,
            play: bright(include_bytes!("../../assets/icons/play.svg"))?,
            pause: bright(include_bytes!("../../assets/icons/pause.svg"))?,
            next: bright(include_bytes!("../../assets/icons/next.svg"))?,
        })
    }
}

/// Starts a layer that keeps drawing off the video, whose picture is already in the back buffer.
/// End it with `PopLayer`.
pub(super) unsafe fn push_mask(ctx: &ID2D1DeviceContext5, mask: &ID2D1PathGeometry1) -> Result<()> {
    let params = D2D1_LAYER_PARAMETERS1 {
        contentBounds: Rect::new(f32::MIN, f32::MIN, f32::MAX, f32::MAX).d2d(),
        geometricMask: ManuallyDrop::new(Some(mask.cast()?)),
        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        maskTransform: Matrix3x2::identity(),
        opacity: 1.0,
        opacityBrush: ManuallyDrop::new(None),
        layerOptions: D2D1_LAYER_OPTIONS1_NONE,
    };
    unsafe { ctx.PushLayer(&params, None) };
    drop(ManuallyDrop::into_inner(params.geometricMask));
    Ok(())
}

impl Painter {
    /// The mini player window: the card, its queue, and the controls over the video.
    pub(super) fn paint_mini(&mut self, ctx: &ID2D1DeviceContext5, s: &Scene) -> Result<()> {
        let rt: &ID2D1RenderTarget = ctx;
        let l = s.layout;
        let Some(card) = l.card else {
            return Ok(());
        };
        unsafe {
            if !s.has_frame {
                rt.FillRectangle(&card.video.d2d(), &self.black);
                self.art(rt, ctx, s, card.video)?;
            }
            let mask = self.frame_geometry(l)?;
            push_mask(ctx, &mask)?;
            self.mini_card(rt, ctx, s, &card);
            if let Some(q) = s.queue.filter(|_| card.queue) {
                self.queue_panel(rt, ctx, s, q);
            }
            ctx.PopLayer();
            self.mini_controls(rt, ctx, s, &card);
        }
        Ok(())
    }

    /// The card's body: everything but the video and the controls over it. Runs inside the
    /// layer that keeps the video clear.
    pub(super) unsafe fn mini_card(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        c: &Card,
    ) {
        let r = &self.mini;
        let f = c.frame;
        unsafe {
            if c.window {
                rt.FillRectangle(&f.d2d(), &self.bg);
            } else {
                for spread in [6.0, 4.0, 2.0] {
                    rt.FillRoundedRectangle(&rounded(f.inset(-spread), 8.0 + spread), &r.shadow);
                }
                rt.FillRoundedRectangle(&rounded(f, 8.0), &self.bg);
            }
            rt.FillRoundedRectangle(&rounded(c.video.inset(-1.0), 5.0), &self.track);
            rt.FillRoundedRectangle(&rounded(c.progress, 4.0), &self.track);
            let t = c.foot;
            let name = Rect::new(f.l + 15.0, t + 258.0, f.r - 15.0, t + 284.0);
            self.put(rt, s.file, &r.name, name, &r.label);
            self.icon(ctx, &self.icons.folder, f.l + 13.0, t + 298.0, false);
            let folder = Rect::new(f.l + 46.0, t + 298.0, f.r - 60.0, t + 322.0);
            self.put(rt, s.folder, &r.name, folder, &self.fg);
            let playlist = if c.queue {
                &self.queue.playlist_on
            } else {
                &self.icons.playlist
            };
            if s.hover == Some(Hit::Playlist) {
                let (_, b) = c.buttons()[5];
                rt.FillRoundedRectangle(&rounded(b, 4.0), &self.hover);
            }
            self.icon(ctx, playlist, f.r - 43.0, t + 298.0, false);
        }
    }

    /// The progress bar's fill, and over the video the design's hover controls. Runs after the
    /// video layer, since it draws on the picture.
    pub(super) unsafe fn mini_controls(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        c: &Card,
    ) {
        let r = &self.mini;
        let hovered = c.hovered(s.hover);
        unsafe {
            let p = c.progress.inset(2.0);
            let fraction = if s.duration > 0.0 {
                (s.position / s.duration).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            let x = p.l + p.w() * fraction;
            if s.duration > 0.0 {
                let fill = Rect::new(p.l, p.t, x.max(p.l + 6.0), p.b);
                rt.FillRoundedRectangle(&rounded(fill, 3.0), &self.played);
            }
            if !hovered {
                return;
            }
            // The thumb: an accent bar a little taller than the track.
            let thumb = Rect::new(x - 4.0, c.progress.t - 1.0, x + 4.0, c.progress.b + 1.0);
            rt.FillRoundedRectangle(&rounded(thumb, 2.0), &self.accent);

            rt.FillRoundedRectangle(&rounded(c.video, 4.0), &r.dim);
            for (hit, b) in &c.buttons()[..5] {
                let svg = match hit {
                    Hit::MiniClose => &r.close,
                    Hit::MiniExpand => &r.expand,
                    Hit::Previous => &r.previous,
                    Hit::Play if s.paused => &r.play,
                    Hit::Play => &r.pause,
                    _ => &r.next,
                };
                if s.hover == Some(*hit) {
                    rt.FillRoundedRectangle(&rounded(*b, 6.0), &self.hover);
                }
                let (cx, cy) = ((b.l + b.r) / 2.0, (b.t + b.b) / 2.0);
                // The design's expand arrow points up and right: open_external mirrored.
                let mirrored = *hit == Hit::MiniExpand;
                let size = if *hit == Hit::Play { 32.0 } else { 24.0 };
                let m = size / svg.width;
                let local = Matrix3x2 {
                    M11: if mirrored { -m } else { m },
                    M12: 0.0,
                    M21: 0.0,
                    M22: m,
                    M31: if mirrored {
                        cx + size / 2.0
                    } else {
                        cx - size / 2.0
                    },
                    M32: cy - size / 2.0,
                };
                ctx.SetTransform(&local);
                ctx.DrawSvgDocument(&svg.doc);
                ctx.SetTransform(&Matrix3x2::identity());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_opens_above_the_video() {
        let closed = Card::new(10.0, 700.0, 0.0, false);
        let open = Card::new(10.0, 700.0, QUEUE_H, false);
        assert_eq!(closed.video, open.video, "the video stays put");
        assert_eq!(open.frame.h(), H + QUEUE_H);
        assert_eq!(open.queue_frame().unwrap().b, closed.frame.t);
        assert!(closed.queue_frame().is_none());
        // Short of room, the queue gets shorter, then stays shut.
        assert_eq!(Card::new(0.0, 700.0, 200.0, false).frame.h(), H + 200.0);
        assert!(!Card::new(0.0, 700.0, 100.0, false).queue);
    }

    #[test]
    fn a_bigger_window_gives_the_room_to_the_video() {
        let small = Layout::mini(W, H, H, false).card.unwrap();
        let big = Layout::mini(800.0, 600.0, 600.0, false).card.unwrap();
        assert_eq!(big.frame, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert_eq!(big.video, Rect::new(8.0, 8.0, 792.0, 600.0 - H + 217.0));
        // The strip under the video keeps its size, at the bottom.
        assert_eq!(big.progress.h(), small.progress.h());
        assert_eq!(600.0 - big.progress.t, H - small.progress.t);
        assert_eq!(
            big.hit(800.0 - 31.0, 600.0 - H + 310.0),
            Some(Hit::Playlist)
        );
        // Never smaller than the design.
        assert_eq!(Card::sized(0.0, H, 100.0, 100.0, 0.0, true).frame.w(), W);
    }

    #[test]
    fn buttons_sit_where_the_design_has_them() {
        let c = Card::new(0.0, H, 0.0, true);
        let at = |x, y| c.hit(x, y);
        assert_eq!(at(26.0, 34.0), Some(Hit::MiniClose));
        assert_eq!(at(W - 34.0, 34.0), Some(Hit::MiniExpand));
        let (cx, cy) = (W / 2.0, (8.0 + 217.0) / 2.0);
        assert_eq!(at(cx, cy), Some(Hit::Play));
        assert_eq!(at(cx - 76.0, cy), Some(Hit::Previous));
        assert_eq!(at(cx + 76.0, cy), Some(Hit::Next));
        assert_eq!(at(100.0, 230.0), Some(Hit::Seek));
        assert_eq!(at(W - 31.0, 310.0), Some(Hit::Playlist));
        // The window's body drags it; the library's card opens the player.
        assert_eq!(at(100.0, 270.0), Some(Hit::Caption));
        let lib = Card::new(0.0, H, 0.0, false);
        assert_eq!(lib.hit(100.0, 270.0), Some(Hit::Card));
        assert_eq!(lib.hit(100.0, H + 1.0), None);
    }
}

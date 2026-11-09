//! Paints whole screens through `Painter::paint` into bitmaps, to look at them:
//! `cargo test render_check -- --ignored` writes PNGs to `$WUSHANG_RENDER_DIR`, or the temp
//! directory.

use std::path::PathBuf;

use super::*;
use crate::captions::CaptionStyle;
use crate::library::{Crumb, Entry, MediaKind, Place};

/// Device pixels per DIP: a Retina screen.
const SCALE: f32 = 2.0;

fn out_dir() -> PathBuf {
    std::env::var_os("WUSHANG_RENDER_DIR").map_or_else(std::env::temp_dir, PathBuf::from)
}

fn scene<'a>(layout: &'a Layout, browse: &'a Browse, style: &'a CaptionStyle) -> Scene<'a> {
    Scene {
        layout,
        hover: None,
        position: 754.0,
        duration: 2843.0,
        live: false,
        time_left: false,
        paused: true,
        volume: 0.8,
        muted: false,
        title: "Big Buck Bunny (2008)",
        folder: "Movies",
        file: "Big Buck Bunny (2008).mkv",
        art: None,
        media: None,
        artist: "",
        playlist: "",
        fullscreen: false,
        has_frame: true,
        browse,
        panel: None,
        queue: None,
        dialog: None,
        preferences: None,
        onboarding: None,
        customize: None,
        repeat: crate::queue::Repeat::Off,
        shuffle: false,
        favorite: false,
        recording: false,
        cues: &[],
        sub_marks: &[],
        visual: None,
        caption_style: style,
        stats: None,
        notice: None,
        seek_hover: None,
        tabs: &[],
        active_tab: 0,
    }
}

/// A painter on a fresh device context, as the renderer makes one.
fn painter() -> (ID2D1DeviceContext5, Painter) {
    let ctx = ID2D1DeviceContext5::new();
    let painter = Painter::new(&ctx, ID2D1Factory1::new()).unwrap();
    (ctx, painter)
}

/// Paints `s` and saves it.
fn shoot(ctx: &ID2D1DeviceContext5, painter: &mut Painter, s: &Scene, name: &str) -> Canvas {
    let canvas = paint(ctx, painter, s);
    let path = out_dir().join(format!("wushang-{name}.png"));
    canvas.save_png(&path).unwrap();
    eprintln!("wrote {}", path.display());
    canvas
}

/// Paints `s` over a stand-in video frame (a dark gradient in the video area).
fn paint(ctx: &ID2D1DeviceContext5, painter: &mut Painter, s: &Scene) -> Canvas {
    let (w, h) = (s.layout.width, s.layout.height);
    let size = ((w * SCALE) as u32, (h * SCALE) as u32);
    let canvas = Canvas::new(size.0, size.1).unwrap();
    ctx.begin(&canvas.cg, size, SCALE);
    unsafe {
        ctx.Clear(Some(&D2D1_COLOR_F::default()));
        let v = s.layout.video;
        let steps = 24;
        for i in 0..steps {
            let f = i as f32 / steps as f32;
            let band = Rect::new(
                v.l,
                v.t + v.h() * f,
                v.r,
                v.t + v.h() * (f + 1.0 / steps as f32),
            );
            let c = D2D1_COLOR_F {
                r: 0.10 + 0.15 * f,
                g: 0.16 + 0.10 * f,
                b: 0.22 + 0.05 * f,
                a: 1.0,
            };
            let brush = ctx.CreateSolidColorBrush(&c, None).unwrap();
            ctx.FillRectangle(&band.d2d(), &brush);
        }
    }
    painter.paint(ctx, s).unwrap();
    ctx.end();
    canvas
}

/// A 16:9 test card as BGRA, for thumbnails.
fn card(w: u32, h: u32) -> Vec<u8> {
    (0..h)
        .flat_map(|y| {
            (0..w).flat_map(move |x| {
                let (r, g) = ((x * 255 / w) as u8, (y * 255 / h) as u8);
                [160, g, r, 255]
            })
        })
        .collect()
}

#[test]
#[ignore = "writes PNGs to look at"]
fn render_check_player() {
    let (ctx, mut painter) = painter();
    let browse = Browse::new();
    let style = CaptionStyle::default();
    let layout = Layout::new(1280.0, 720.0, true, false);
    let mut s = scene(&layout, &browse, &style);
    s.hover = Some(Hit::Next);
    shoot(&ctx, &mut painter, &s, "player");
    s.seek_hover = Some(1200.0);
    s.paused = false;
    s.notice = Some("Snapshot saved to Pictures");
    shoot(&ctx, &mut painter, &s, "player-seek");
}

#[test]
#[ignore = "writes PNGs to look at"]
fn render_check_menu() {
    let (ctx, mut painter) = painter();
    let mut ui = Ui::new();
    ui.page = Page::Player;
    ui.layout = Layout::new(1280.0, 720.0, true, false);
    let profiles = ["Default".to_owned(), "Late Night".to_owned()];
    let items = menu::main_menu(&[], &profiles, "Default", &[], menu::Converting::Nothing);
    ui.open_menu(menu::Kind::Main, items, (51.0, 38.0), Hit::More);
    let browse = Browse::new();
    let style = CaptionStyle::default();
    let s = scene(&ui.layout, &browse, &style);
    shoot(&ctx, &mut painter, &s, "menu");
}

#[test]
#[ignore = "writes PNGs to look at"]
fn render_check_settings() {
    let (ctx, mut painter) = painter();
    let mut ui = Ui::new();
    ui.open_preferences(crate::prefs::Prefs::default(), vec![], Category::Playback);
    let mut layout = Layout::new(1280.0, 800.0, true, false);
    layout.add_preferences(ui.preferences.as_ref().unwrap());
    let browse = Browse::new();
    let style = CaptionStyle::default();
    let mut s = scene(&layout, &browse, &style);
    s.preferences = ui.preferences.as_ref();
    shoot(&ctx, &mut painter, &s, "settings");
}

#[test]
#[ignore = "writes PNGs to look at"]
fn render_check_dialog() {
    let (ctx, mut painter) = painter();
    let mut ui = Ui::new();
    ui.open_dialog(Dialog::About);
    let mut layout = Layout::new(1280.0, 720.0, true, false);
    painter.add_dialog(&mut layout, ui.dialog.as_ref().unwrap());
    let browse = Browse::new();
    let style = CaptionStyle::default();
    let mut s = scene(&layout, &browse, &style);
    s.dialog = ui.dialog.as_ref();
    shoot(&ctx, &mut painter, &s, "about");
}

#[test]
#[ignore = "writes PNGs to look at"]
fn render_check_library() {
    let (ctx, mut painter) = painter();
    let mut browse = Browse::new();
    browse.place = Place::Videos;
    browse.crumbs = vec![Crumb {
        label: "Videos".into(),
        place: Place::Videos,
    }];
    let media = |name: &str, progress: Option<f32>| Entry::Media {
        label: name.into(),
        path: PathBuf::from(format!("/Movies/{name}.mkv")),
        kind: MediaKind::Video,
        subtitle: None,
        album: None,
        progress,
        finished: false,
    };
    browse.entries = vec![
        Entry::Folder {
            label: "Holidays".into(),
            place: Place::Folder(Section::Videos, PathBuf::from("/Movies/Holidays")),
        },
        media("Big Buck Bunny (2008)", Some(0.4)),
        media("Sintel", None),
        media("Tears of Steel, the extended cut with a long name", None),
    ];
    let thumb = card(320, 180);
    painter.add_thumbnail(
        &ctx,
        PathBuf::from("/Movies/Big Buck Bunny (2008).mkv"),
        Some((320, 180, &thumb)),
    );
    let layout = painter.library_layout(1280.0, 720.0, &browse, None);
    let style = CaptionStyle::default();
    let s = scene(&layout, &browse, &style);
    shoot(&ctx, &mut painter, &s, "library");
}

/// The pixel at DIP (x, y), as 0xRRGGBB.
#[cfg(test)]
pub(super) fn pixel_at(canvas: &Canvas, x: f32, y: f32) -> u32 {
    let (px, py) = ((x * SCALE) as u32, (y * SCALE) as u32);
    let i = ((py * canvas.width + px) * 4) as usize;
    let p = canvas.pixels();
    u32::from(p[i + 2]) << 16 | u32::from(p[i + 1]) << 8 | u32::from(p[i])
}

#[test]
fn the_bars_paint_in_the_design_colours() {
    let (ctx, mut painter) = painter();
    let browse = Browse::new();
    let style = CaptionStyle::default();
    let layout = Layout::new(1280.0, 720.0, true, false);
    let s = scene(&layout, &browse, &style);
    let canvas = paint(&ctx, &mut painter, &s);
    // The title bar is BG_A3 and the play button the accent.
    assert_eq!(pixel_at(&canvas, 200.0, 4.0), theme::map(BG_A3));
    let play = layout.button(Hit::Play);
    assert_eq!(
        pixel_at(&canvas, play.l + 3.0, play.t + 3.0),
        theme::map(ACCENT)
    );
}

#[test]
#[ignore = "writes PNGs to look at"]
fn render_check_welcome_and_customize() {
    let (ctx, mut painter) = painter();
    let browse = Browse::new();
    let style = CaptionStyle::default();
    let mut ui = Ui::new();
    ui.open_onboarding(crate::prefs::Prefs::default(), "/Users/me/Movies".into());
    let mut layout = Layout::new(1280.0, 720.0, true, false);
    layout.add_onboarding(ui.onboarding.as_ref().unwrap());
    let mut s = scene(&layout, &browse, &style);
    s.onboarding = ui.onboarding.as_ref();
    shoot(&ctx, &mut painter, &s, "welcome");

    let mut ui = Ui::new();
    ui.open_customize();
    let c = ui.customize.as_ref().unwrap();
    let mut layout = Layout::player(1280.0, 720.0, true, false, &c.draft);
    layout.add_customize(c);
    let mut s = scene(&layout, &browse, &style);
    s.customize = Some(c);
    shoot(&ctx, &mut painter, &s, "customize");
}

#[test]
#[ignore = "writes PNGs to look at"]
fn render_check_popup() {
    let (ctx, mut painter) = painter();
    let browse = Browse::new();
    let style = CaptionStyle::default();
    let data = PanelData::default();
    let mut ui = Ui::new();
    let panel = ui.panels.for_button(Hit::Settings);
    ui.panels.show(panel, Some(Hit::Settings));
    let mut layout = Layout::new(1280.0, 720.0, true, false);
    layout.add_panel(&ui.panels, &data);
    let mut s = scene(&layout, &browse, &style);
    s.panel = Some(&data);
    shoot(&ctx, &mut painter, &s, "popup");
}

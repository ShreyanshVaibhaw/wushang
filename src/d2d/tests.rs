use super::*;

const INTER: [&[u8]; 3] = [
    include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
];

fn inter(dwrite: &IDWriteFactory6) -> IDWriteFontCollection2 {
    unsafe {
        let loader = dwrite.CreateInMemoryFontFileLoader().unwrap();
        let builder = dwrite.CreateFontSetBuilder().unwrap();
        let builder1: &IDWriteFontSetBuilder1 = &builder;
        for data in INTER {
            let file = loader
                .CreateInMemoryFontFileReference(
                    dwrite,
                    data.as_ptr().cast(),
                    data.len() as u32,
                    None,
                )
                .unwrap();
            builder1.AddFontFile(&file).unwrap();
        }
        dwrite
            .CreateFontCollectionFromFontSet(
                &builder.CreateFontSet().unwrap(),
                DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC,
            )
            .unwrap()
    }
}

fn format(size: f32, wrap: bool) -> (IDWriteFactory6, IDWriteTextFormat) {
    let dwrite: IDWriteFactory6 =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    let fonts = inter(&dwrite);
    let f = unsafe {
        let base: &IDWriteFactory = &dwrite;
        let f = base
            .CreateTextFormat(
                w!("Inter"),
                &fonts,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )
            .unwrap();
        if !wrap {
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).unwrap();
            let trimming = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                ..Default::default()
            };
            f.SetTrimming(&trimming, &base.CreateEllipsisTrimmingSign(&f).unwrap())
                .unwrap();
        }
        f
    };
    (dwrite, f)
}

fn metrics(
    dwrite: &IDWriteFactory6,
    f: &IDWriteTextFormat,
    text: &str,
    w: f32,
    h: f32,
) -> DWRITE_TEXT_METRICS {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe {
        dwrite
            .CreateTextLayout(&utf16, f, w, h)
            .unwrap()
            .GetMetrics(&mut m)
            .unwrap()
    };
    m
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn lines_are_as_tall_as_directwrite_makes_inter() {
    let (dwrite, f) = format(14.0, true);
    // Inter's typo ascender + descender: (1984 + 494) / 2048 em.
    let line = 14.0 * 2478.0 / 2048.0;
    let m = metrics(&dwrite, &f, "Hello", 1000.0, 100.0);
    assert!(close(m.height, line), "{}", m.height);
    assert_eq!(m.lineCount, 1);
    let two = metrics(&dwrite, &f, "Hello\nworld", 1000.0, 100.0);
    assert!(close(two.height, 2.0 * line) && two.lineCount == 2);
    // A trailing line break opens an empty last line.
    assert_eq!(metrics(&dwrite, &f, "Hello\n", 1000.0, 100.0).lineCount, 2);
    assert!(close(metrics(&dwrite, &f, "", 1000.0, 100.0).height, line));
}

#[test]
fn widths_count_trailing_spaces_only_where_asked() {
    let (dwrite, f) = format(14.0, true);
    let one = metrics(&dwrite, &f, "0", 1000.0, 100.0).width;
    let four = metrics(&dwrite, &f, "0000", 1000.0, 100.0);
    assert!(one > 5.0 && close(four.width, 4.0 * one));
    let spaced = metrics(&dwrite, &f, "0000  ", 1000.0, 100.0);
    assert!(close(spaced.width, four.width));
    assert!(spaced.widthIncludingTrailingWhitespace > four.width + 4.0);
}

#[test]
fn wrapping_breaks_at_words_and_alignment_centres() {
    let (dwrite, f) = format(14.0, true);
    let m = metrics(&dwrite, &f, "one two three four five six", 60.0, 500.0);
    assert!(m.lineCount >= 3 && m.width <= 60.0, "{m:?}");
    unsafe {
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)
            .unwrap();
        f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER).unwrap();
        f.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 17.0, 13.0)
            .unwrap();
    }
    let m = metrics(&dwrite, &f, "Hi", 100.0, 40.0);
    assert!(close(m.height, 17.0) && close(m.top, 11.5));
    assert!(close(m.left, (100.0 - m.width) / 2.0));
}

#[test]
fn long_text_ends_in_an_ellipsis_inside_the_box() {
    let (dwrite, f) = format(15.0, false);
    let text = "Tears of Steel, the extended cut with a long name";
    let full = metrics(&dwrite, &f, text, 10_000.0, 100.0).width;
    let cut = metrics(&dwrite, &f, text, 120.0, 100.0);
    assert!(
        full > 200.0 && cut.width <= 120.0 && cut.width > 100.0,
        "{full} {cut:?}"
    );
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let layout = unsafe { dwrite.CreateTextLayout(&utf16, &f, 120.0, 100.0).unwrap() };
    let mut count = 0;
    let mut lines = [DWRITE_LINE_METRICS::default(); 1];
    unsafe { layout.GetLineMetrics(Some(&mut lines), &mut count).unwrap() };
    assert!(count == 1 && lines[0].isTrimmed.as_bool());
}

#[test]
fn carets_land_between_characters() {
    let (dwrite, f) = format(14.0, false);
    let utf16: Vec<u16> = "0000".encode_utf16().collect();
    let layout = unsafe {
        dwrite
            .CreateTextLayout(&utf16, &f, 10_000.0, 100.0)
            .unwrap()
    };
    let one = metrics(&dwrite, &f, "0", 1000.0, 100.0).width;
    let caret = |i: u32, trailing: bool| {
        let (mut x, mut y) = (0.0, 0.0);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            layout
                .HitTestTextPosition(i, trailing, &mut x, &mut y, &mut m)
                .unwrap()
        };
        x
    };
    assert!(close(caret(0, false), 0.0));
    assert!(close(caret(2, false), 2.0 * one));
    assert!(close(caret(1, true), 2.0 * one));
    assert!(close(caret(4, false), 4.0 * one));
}

/// A 100x100 DIP canvas at `scale`, with `f` drawing into it.
fn draw(scale: f32, f: impl FnOnce(&ID2D1DeviceContext5)) -> Canvas {
    let px = (100.0 * scale) as u32;
    let canvas = Canvas::new(px, px).unwrap();
    let ctx = ID2D1DeviceContext5::new();
    ctx.begin(&canvas.cg, (px, px), scale);
    f(&ctx);
    ctx.end();
    canvas
}

/// The pixel at device (x, y) as premultiplied (b, g, r, a).
fn at(c: &Canvas, x: u32, y: u32) -> [u8; 4] {
    let p = c.pixels();
    let i = ((y * c.width + x) * 4) as usize;
    [p[i], p[i + 1], p[i + 2], p[i + 3]]
}

fn red(ctx: &ID2D1DeviceContext5) -> ID2D1SolidColorBrush {
    let c = D2D1_COLOR_F {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    unsafe { ctx.CreateSolidColorBrush(&c, None).unwrap() }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

const RED: [u8; 4] = [0, 0, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

#[test]
fn rounded_rects_fill_top_down_with_soft_corners() {
    let c = draw(2.0, |ctx| unsafe {
        let rr = D2D1_ROUNDED_RECT {
            rect: rect(10.0, 20.0, 60.0, 50.0),
            radiusX: 8.0,
            radiusY: 8.0,
        };
        ctx.FillRoundedRectangle(&rr, &red(ctx));
    });
    // y grows downwards: DIP (35, 22) is near the top edge, inside.
    assert_eq!(at(&c, 70, 44), RED);
    assert_eq!(at(&c, 70, 99), RED);
    assert_eq!(at(&c, 70, 101), CLEAR);
    assert_eq!(at(&c, 70, 39), CLEAR);
    // The corner pixel is outside the 8 DIP arc; one a little in is partly covered.
    assert_eq!(at(&c, 20, 40), CLEAR);
    let edge = at(&c, 24, 44)[3];
    assert!(edge > 0 && edge < 255, "{edge}");
}

#[test]
fn clips_nest_and_aliased_ones_snap_to_pixels() {
    let c = draw(1.0, |ctx| unsafe {
        let b = red(ctx);
        ctx.PushAxisAlignedClip(&rect(10.3, 10.0, 60.0, 60.0), D2D1_ANTIALIAS_MODE_ALIASED);
        ctx.PushAxisAlignedClip(&rect(30.0, 30.0, 90.0, 90.0), D2D1_ANTIALIAS_MODE_ALIASED);
        ctx.FillRectangle(&rect(0.0, 0.0, 100.0, 100.0), &b);
        ctx.PopAxisAlignedClip();
        ctx.FillRectangle(&rect(0.0, 0.0, 20.0, 20.0), &b);
        ctx.PopAxisAlignedClip();
        ctx.FillRectangle(&rect(80.0, 0.0, 90.0, 10.0), &b);
    });
    assert_eq!(at(&c, 45, 45), RED);
    assert_eq!(at(&c, 65, 45), CLEAR);
    assert_eq!(at(&c, 25, 45), CLEAR);
    // Inside only the outer clip; its left edge at 10.3 snaps to pixel 10, all or nothing.
    assert_eq!(at(&c, 10, 15), RED);
    assert_eq!(at(&c, 9, 15), CLEAR);
    assert_eq!(at(&c, 85, 5), RED);
}

#[test]
fn transforms_layers_and_combined_geometry() {
    let c = draw(1.0, |ctx| unsafe {
        let b = red(ctx);
        ctx.SetTransform(&Matrix3x2::translation(50.0, 0.0));
        ctx.FillRectangle(&rect(0.0, 0.0, 10.0, 10.0), &b);
        ctx.SetTransform(&Matrix3x2::identity());
        // A frame: the square minus a smaller one inside.
        let f = ID2D1Factory1::new();
        let outer = f
            .CreateRectangleGeometry(&rect(0.0, 20.0, 40.0, 60.0))
            .unwrap();
        let inner = f
            .CreateRectangleGeometry(&rect(10.0, 30.0, 30.0, 50.0))
            .unwrap();
        let path = f.CreatePathGeometry().unwrap();
        let sink = path.Open().unwrap();
        outer
            .CombineWithGeometry(&inner, D2D1_COMBINE_MODE_EXCLUDE, None, 0.25, &sink)
            .unwrap();
        sink.Close().unwrap();
        ctx.FillGeometry(&path, &b, None);
        // Half-transparent layer, masked to a triangle.
        let tri = f.CreatePathGeometry().unwrap();
        let sink = tri.Open().unwrap();
        sink.BeginFigure(Vector2::new(60.0, 60.0), D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLine(Vector2::new(100.0, 60.0));
        sink.AddLine(Vector2::new(60.0, 100.0));
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close().unwrap();
        let params = D2D1_LAYER_PARAMETERS1 {
            contentBounds: rect(0.0, 0.0, 100.0, 100.0),
            geometricMask: std::mem::ManuallyDrop::new(Some(tri.cast().unwrap())),
            maskTransform: Matrix3x2::identity(),
            opacity: 0.5,
            ..Default::default()
        };
        ctx.PushLayer(&params, None);
        ctx.FillRectangle(&rect(0.0, 0.0, 100.0, 100.0), &b);
        ctx.PopLayer();
    });
    assert_eq!(at(&c, 55, 5), RED);
    assert_eq!(at(&c, 5, 5), CLEAR);
    assert_eq!(at(&c, 5, 40), RED);
    assert_eq!(at(&c, 20, 40), CLEAR);
    let half = at(&c, 65, 65);
    assert!((125..=130).contains(&half[3]) && half[0] == 0, "{half:?}");
    assert_eq!(at(&c, 95, 95), CLEAR);
}

#[test]
fn text_draws_upright_on_a_snapped_baseline() {
    let (dwrite, f) = format(20.0, false);
    let c = draw(1.0, |ctx| unsafe {
        let text: Vec<u16> = "L".encode_utf16().collect();
        let _ = &dwrite;
        ctx.DrawText(
            &text,
            &f,
            &rect(10.3, 10.4, 90.0, 90.0),
            &red(ctx),
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    });
    // An L: ink along the bottom row, under the stem, ending at the baseline (10.4 + 0.96875 *
    // 20 = 29.8, snapped to 30). Rows below it are clear.
    let ink = |y: u32| (10..30).map(|x| u32::from(at(&c, x, y)[3])).sum::<u32>();
    assert!(ink(29) > ink(20) * 2, "{} {}", ink(29), ink(20));
    assert_eq!(ink(30), 0);
    assert!(ink(16) > 0);
}

#[test]
fn svg_icons_raster_at_the_device_scale() {
    let svg = br##"<svg width="24" height="24" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><rect x="4" y="4" width="16" height="8" fill="#FF0000"/></svg>"##;
    let c = draw(2.0, |ctx| unsafe {
        let doc = ctx
            .CreateSvgDocument(
                svg,
                D2D_SIZE_F {
                    width: 24.0,
                    height: 24.0,
                },
            )
            .unwrap();
        ctx.SetTransform(&Matrix3x2::translation(10.0, 10.0));
        ctx.DrawSvgDocument(&doc);
        // Mirrored, as the title bar draws some icons.
        let m = Matrix3x2 {
            M11: -1.0,
            M31: 74.0,
            M32: 10.0,
            ..Matrix3x2::identity()
        };
        ctx.SetTransform(&m);
        ctx.DrawSvgDocument(&doc);
    });
    // DIP (14..30, 14..22) at 2x: device (28..60, 28..44), crisp edges.
    assert_eq!(at(&c, 28, 28), RED);
    assert_eq!(at(&c, 59, 43), RED);
    assert_eq!(at(&c, 27, 28), CLEAR);
    assert_eq!(at(&c, 60, 28), CLEAR);
    assert_eq!(at(&c, 28, 44), CLEAR);
    // The mirror covers DIP 54..70.
    assert_eq!(at(&c, 108, 28), RED);
    assert_eq!(at(&c, 139, 43), RED);
    assert_eq!(at(&c, 140, 30), CLEAR);
}

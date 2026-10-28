//! Paints subtitle cues over the video with Direct2D: outlined text stacked up from the bottom of
//! the picture, and bitmap subtitles scaled from the canvas they were authored on.

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::{HSTRING, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::subs::{Cue, CueContent, Span};

/// How captions look. Sizes scale with the picture, so a window and fullscreen match.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionStyle {
    pub font_family: String,
    /// Font size as a fraction of the video height.
    pub size_ratio: f32,
    /// Fill colour, 0xRRGGBB.
    pub text: u32,
    pub outline: u32,
    /// Outline thickness in DIPs.
    pub outline_width: f32,
    /// Gap between the bottom of the picture and the last line, as a fraction of its height.
    pub bottom_margin_ratio: f32,
    pub bold: bool,
    /// Of the whole caption, outline included, 0..1.
    pub opacity: f32,
    /// A soft dark copy down and to the right, under the outline.
    pub shadow: bool,
}

impl Default for CaptionStyle {
    /// The design's caption: light grey Inter with a near-black outline.
    fn default() -> Self {
        CaptionStyle {
            font_family: "Inter".into(),
            size_ratio: 0.055,
            text: 0xB8B8B8,
            outline: 0x0C0C0C,
            outline_width: 2.0,
            bottom_margin_ratio: 0.06,
            bold: false,
            opacity: 1.0,
            shadow: false,
        }
    }
}

/// Text wraps at this fraction of the video width.
const WRAP: f32 = 0.85;
/// Space between stacked cues, as a fraction of the font size.
const CUE_GAP: f32 = 0.15;

/// Device-bound caption resources: brushes, plus layouts and bitmaps of the cues on screen, kept
/// until their cue leaves.
pub struct CaptionRenderer {
    dwrite: IDWriteFactory6,
    fonts: Option<IDWriteFontCollection2>,
    fill: ID2D1SolidColorBrush,
    outline: ID2D1SolidColorBrush,
    shadow: ID2D1SolidColorBrush,
    /// The text format for the current family, size and weight.
    format: Option<(String, f32, bool, IDWriteTextFormat)>,
    layouts: Vec<CachedLayout>,
    /// Bitmap cues by start time, with each rect's position on the canvas.
    bitmaps: Vec<(u64, Vec<(ID2D1Bitmap, D2D_RECT_F)>)>,
}

struct CachedLayout {
    lines: Vec<Vec<Span>>,
    max_width: f32,
    layout: IDWriteTextLayout,
    used: bool,
}

impl CaptionRenderer {
    /// `fonts` is the collection to look `font_family` up in (the app's embedded Inter); None
    /// uses the system fonts. Missing glyphs fall back to system fonts either way.
    pub fn new(
        ctx: &ID2D1DeviceContext5,
        dwrite: &IDWriteFactory6,
        fonts: Option<&IDWriteFontCollection2>,
    ) -> Result<Self> {
        let style = CaptionStyle::default();
        unsafe {
            Ok(CaptionRenderer {
                dwrite: dwrite.clone(),
                fonts: fonts.cloned(),
                fill: ctx.CreateSolidColorBrush(&color(style.text), None)?,
                outline: ctx.CreateSolidColorBrush(&color(style.outline), None)?,
                shadow: ctx.CreateSolidColorBrush(&D2D1_COLOR_F { a: 0.6, ..color(0) }, None)?,
                format: None,
                layouts: Vec::new(),
                bitmaps: Vec::new(),
            })
        }
    }

    /// Draws the active cues inside `video` (DIPs, the letterboxed picture rect: left, top,
    /// right, bottom). Call between BeginDraw and EndDraw.
    pub fn draw(
        &mut self,
        ctx: &ID2D1DeviceContext5,
        video: (f32, f32, f32, f32),
        cues: &[Cue],
        style: &CaptionStyle,
    ) -> Result<()> {
        let (left, top, right, bottom) = video;
        let (width, height) = (right - left, bottom - top);
        // Bitmaps of cues that left the screen go now, so a long film doesn't pile them up.
        self.bitmaps.retain(|(start, _)| {
            cues.iter().any(|c| {
                c.start.to_bits() == *start && matches!(c.content, CueContent::Bitmap { .. })
            })
        });
        if width < 1.0 || height < 1.0 {
            return Ok(());
        }

        for cue in cues {
            let CueContent::Bitmap {
                width: cw,
                height: ch,
                rects,
            } = &cue.content
            else {
                continue;
            };
            if *cw == 0 || *ch == 0 {
                continue;
            }
            let key = cue.start.to_bits();
            if !self.bitmaps.iter().any(|(k, _)| *k == key) {
                self.bitmaps.push((key, upload(ctx, rects)?));
            }
            let (sx, sy) = (width / *cw as f32, height / *ch as f32);
            let (_, bitmaps) = self.bitmaps.iter().find(|(k, _)| *k == key).unwrap();
            let dc: &ID2D1DeviceContext = ctx;
            for (bitmap, r) in bitmaps {
                let dest = D2D_RECT_F {
                    left: left + r.left * sx,
                    top: top + r.top * sy,
                    right: left + r.right * sx,
                    bottom: top + r.bottom * sy,
                };
                unsafe {
                    dc.DrawBitmap(
                        bitmap,
                        Some(&dest),
                        1.0,
                        D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                        None,
                        None,
                    )
                };
            }
        }

        let size = (style.size_ratio * height).max(1.0);
        let max_width = WRAP * width;
        self.ensure_format(&style.font_family, size, style.bold)?;
        for l in &mut self.layouts {
            l.used = false;
        }
        unsafe {
            self.fill.SetColor(&color(style.text));
            self.outline.SetColor(&color(style.outline));
        }
        let rt: &ID2D1RenderTarget = ctx;
        let x = left + (width - max_width) / 2.0;
        let mut line_bottom = bottom - style.bottom_margin_ratio * height;
        // The earliest cue sits lowest and later ones stack above it, as libass places them.
        for cue in cues {
            let CueContent::Text(lines) = &cue.content else {
                continue;
            };
            let layout = self.layout(lines, max_width)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            unsafe { layout.GetMetrics(&mut metrics)? };
            let y = line_bottom - metrics.height;
            // Outline: the text in the outline colour at 8 offsets around a circle, then the fill.
            // Rounder than a stroked geometry would need at these widths, with no custom renderer.
            let r = style.outline_width;
            let d = r * std::f32::consts::FRAC_1_SQRT_2;
            let offsets = [
                (r, 0.0),
                (-r, 0.0),
                (0.0, r),
                (0.0, -r),
                (d, d),
                (d, -d),
                (-d, d),
                (-d, -d),
            ];
            // A see-through caption fades as one picture; drawn straight, the outline copies
            // would show through each other and the fill.
            let faded = style.opacity < 1.0;
            unsafe {
                if faded {
                    let params = D2D1_LAYER_PARAMETERS1 {
                        contentBounds: D2D_RECT_F {
                            left,
                            top,
                            right,
                            bottom,
                        },
                        maskTransform: Matrix3x2::identity(),
                        opacity: style.opacity.clamp(0.0, 1.0),
                        ..Default::default()
                    };
                    ctx.PushLayer(&params, None);
                }
                if style.shadow {
                    let off = r + size * 0.05;
                    let at = Vector2 {
                        X: x + off,
                        Y: y + off,
                    };
                    rt.DrawTextLayout(at, &layout, &self.shadow, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                if r > 0.0 {
                    for (dx, dy) in offsets {
                        let at = Vector2 {
                            X: x + dx,
                            Y: y + dy,
                        };
                        rt.DrawTextLayout(at, &layout, &self.outline, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                }
                rt.DrawTextLayout(
                    Vector2 { X: x, Y: y },
                    &layout,
                    &self.fill,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
                if faded {
                    ctx.PopLayer();
                }
            }
            line_bottom = y - CUE_GAP * size;
        }
        self.layouts.retain(|l| l.used);
        Ok(())
    }

    /// Recreates the text format (and drops the layouts built with it) when the family, size or
    /// weight changes; the size changes on a window resize.
    fn ensure_format(&mut self, family: &str, size: f32, bold: bool) -> Result<()> {
        if self
            .format
            .as_ref()
            .is_some_and(|(f, s, b, _)| f == family && *s == size && *b == bold)
        {
            return Ok(());
        }
        // IDWriteFactory6 overloads CreateTextFormat with font axes; the classic one is on IDWriteFactory.
        let dwrite: &IDWriteFactory = &self.dwrite;
        let format = unsafe {
            let format = dwrite.CreateTextFormat(
                &HSTRING::from(family),
                // The embedded collection only has Inter; other families are Windows' own.
                (self.fonts.as_ref())
                    .filter(|_| family == "Inter")
                    .map(|f| -> &IDWriteFontCollection { f }),
                if bold {
                    DWRITE_FONT_WEIGHT_BOLD
                } else {
                    DWRITE_FONT_WEIGHT_MEDIUM
                },
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            format
        };
        self.format = Some((family.to_owned(), size, bold, format));
        self.layouts.clear();
        Ok(())
    }

    /// The layout of `lines`, built once per cue and size.
    fn layout(&mut self, lines: &[Vec<Span>], max_width: f32) -> Result<IDWriteTextLayout> {
        if let Some(l) = self
            .layouts
            .iter_mut()
            .find(|l| l.max_width == max_width && l.lines == lines)
        {
            l.used = true;
            return Ok(l.layout.clone());
        }
        let (_, _, _, format) = self.format.as_ref().unwrap();
        // One string with a line break per line; spans become styled ranges, in UTF-16 units.
        let mut text: Vec<u16> = Vec::new();
        let mut styled = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                text.push(u16::from(b'\n'));
            }
            for span in line {
                let start = text.len() as u32;
                text.extend(span.text.encode_utf16());
                styled.push((span, start, text.len() as u32 - start));
            }
        }
        let layout = unsafe {
            let layout = self
                .dwrite
                .CreateTextLayout(&text, format, max_width, f32::MAX)?;
            for (span, start, length) in styled {
                let range = DWRITE_TEXT_RANGE {
                    startPosition: start,
                    length,
                };
                if span.italic {
                    layout.SetFontStyle(DWRITE_FONT_STYLE_ITALIC, range)?;
                }
                if span.bold {
                    layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, range)?;
                }
            }
            layout
        };
        self.layouts.push(CachedLayout {
            lines: lines.to_vec(),
            max_width,
            layout: layout.clone(),
            used: true,
        });
        Ok(layout)
    }
}

/// Creates a bitmap per rect, with the rect's place on the subtitle canvas.
fn upload(
    ctx: &ID2D1DeviceContext5,
    rects: &[crate::subs::BitmapRect],
) -> Result<Vec<(ID2D1Bitmap, D2D_RECT_F)>> {
    let props = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    let rt: &ID2D1RenderTarget = ctx;
    rects
        .iter()
        .filter(|r| r.w > 0 && r.h > 0 && r.bgra.len() >= (r.w * r.h * 4) as usize)
        .map(|r| {
            let size = D2D_SIZE_U {
                width: r.w,
                height: r.h,
            };
            let bitmap =
                unsafe { rt.CreateBitmap(size, Some(r.bgra.as_ptr().cast()), r.w * 4, &props)? };
            let place = D2D_RECT_F {
                left: r.x as f32,
                top: r.y as f32,
                right: (r.x + r.w as i32) as f32,
                bottom: (r.y + r.h as i32) as f32,
            };
            Ok((bitmap, place))
        })
        .collect()
}

fn color(rgb: u32) -> D2D1_COLOR_F {
    let c = |shift: u32| ((rgb >> shift) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F {
        r: c(16),
        g: c(8),
        b: c(0),
        a: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subs::BitmapRect;
    use std::sync::Arc;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::core::Interface;

    const W: u32 = 1280;
    const H: u32 = 720;
    /// The stand-in for the video frame behind the captions.
    const BACKGROUND: u32 = 0x3A4E6B;

    /// Inter from the app's assets, as ui.rs loads it.
    fn inter(dwrite: &IDWriteFactory6) -> IDWriteFontCollection2 {
        const FONTS: [&[u8]; 3] = [
            include_bytes!("../assets/fonts/Inter-Regular.ttf"),
            include_bytes!("../assets/fonts/Inter-Medium.ttf"),
            include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
        ];
        unsafe {
            let loader = dwrite.CreateInMemoryFontFileLoader().unwrap();
            dwrite.RegisterFontFileLoader(&loader).unwrap();
            let builder = dwrite.CreateFontSetBuilder().unwrap();
            let builder1: &IDWriteFontSetBuilder1 = &builder;
            for data in FONTS {
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

    /// Renders `cues` over a flat frame on a WARP device and returns the BGRA pixels.
    fn render(cues: &[Cue]) -> Vec<u8> {
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .unwrap();
            let dxgi: IDXGIDevice = device.unwrap().cast().unwrap();
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
            let ctx: ID2D1DeviceContext5 = factory
                .CreateDevice(&dxgi)
                .unwrap()
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                .unwrap()
                .cast()
                .unwrap();
            ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let props = |options| D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: options,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let size = D2D_SIZE_U {
                width: W,
                height: H,
            };
            let target = ctx
                .CreateBitmap(size, None, 0, &props(D2D1_BITMAP_OPTIONS_TARGET))
                .unwrap();
            let dwrite: IDWriteFactory6 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap();
            let fonts = inter(&dwrite);
            let mut captions = CaptionRenderer::new(&ctx, &dwrite, Some(&fonts)).unwrap();

            ctx.SetTarget(&target);
            ctx.BeginDraw();
            ctx.Clear(Some(&color(BACKGROUND)));
            let video = (0.0, 0.0, W as f32, H as f32);
            // Twice, so the second pass draws from the caches.
            captions
                .draw(&ctx, video, cues, &CaptionStyle::default())
                .unwrap();
            captions
                .draw(&ctx, video, cues, &CaptionStyle::default())
                .unwrap();
            ctx.EndDraw(None, None).unwrap();
            assert_eq!((captions.layouts.len(), captions.bitmaps.len()), (2, 1));

            let readback = ctx
                .CreateBitmap(
                    size,
                    None,
                    0,
                    &props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW),
                )
                .unwrap();
            readback.CopyFromBitmap(None, &target, None).unwrap();
            let mapped = readback.Map(D2D1_MAP_OPTIONS_READ).unwrap();
            let mut pixels = Vec::with_capacity((W * H * 4) as usize);
            for y in 0..H as usize {
                let row = std::slice::from_raw_parts(
                    mapped.bits.add(y * mapped.pitch as usize),
                    W as usize * 4,
                );
                pixels.extend_from_slice(row);
            }
            readback.Unmap().unwrap();
            pixels
        }
    }

    fn span(text: &str, italic: bool) -> Span {
        Span {
            text: text.into(),
            italic,
            bold: false,
        }
    }

    /// Pixels in the box (x0..x1, y0..y1) within `tolerance` of `rgb`.
    fn count(
        pixels: &[u8],
        (x0, y0, x1, y1): (u32, u32, u32, u32),
        rgb: u32,
        tolerance: i32,
    ) -> usize {
        let want = [rgb & 0xFF, (rgb >> 8) & 0xFF, (rgb >> 16) & 0xFF].map(|c| c as i32);
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| ((y * W + x) * 4) as usize))
            .filter(|&i| (0..3).all(|c| (pixels[i + c] as i32 - want[c]).abs() <= tolerance))
            .count()
    }

    #[test]
    fn draws_outlined_text_and_bitmaps() {
        let text = |start: f64, lines: Vec<Vec<Span>>| Cue {
            start,
            end: start + 2.0,
            content: CueContent::Text(lines),
        };
        // A 120x30 orange box at (60, 40) on a 720x480 DVD canvas.
        let orange: Vec<u8> = (0..120 * 30)
            .flat_map(|_| [0x18, 0x79, 0xD3, 0xFF])
            .collect();
        let picture = Cue {
            start: 0.5,
            end: 3.0,
            content: CueContent::Bitmap {
                width: 720,
                height: 480,
                rects: vec![BitmapRect {
                    x: 60,
                    y: 40,
                    w: 120,
                    h: 30,
                    bgra: Arc::from(orange),
                }],
            },
        };
        let cues = [
            picture,
            text(
                1.0,
                vec![
                    vec![span("Hello, ", false), span("world", true)],
                    vec![span("second line", false)],
                ],
            ),
            text(1.5, vec![vec![span("A later cue stacks above", false)]]),
        ];
        let pixels = render(&cues);
        png(&std::env::temp_dir().join("myvlc-captions.png"), &pixels);

        // Text fill and its outline in the bottom 30%, centred.
        let bottom = (W / 8, H * 7 / 10, W * 7 / 8, H);
        let fill = count(&pixels, bottom, 0xB8B8B8, 2);
        let outline = count(&pixels, bottom, 0x0C0C0C, 2);
        assert!(
            fill > 3000 && outline > 3000,
            "fill {fill}, outline {outline}"
        );
        // Nothing drawn in the middle of the picture, nor below the bottom margin.
        assert_eq!(
            count(&pixels, (0, H / 3, W, H / 2), BACKGROUND, 0),
            (W * (H / 2 - H / 3)) as usize
        );
        assert_eq!(
            count(&pixels, (0, H - 30, W, H), BACKGROUND, 0),
            (W * 30) as usize
        );
        // The bitmap lands at its canvas position scaled to the video: x * 1280/720, y * 720/480.
        let (x, y) = (60.0 * W as f32 / 720.0, 40.0 * H as f32 / 480.0);
        let inside = (x as u32 + 4, y as u32 + 4, x as u32 + 200, y as u32 + 40);
        assert_eq!(count(&pixels, inside, 0xD37918, 1), 196 * 36);
        assert_eq!(
            count(&pixels, (0, 0, x as u32 - 2, H / 3), BACKGROUND, 0),
            ((x as u32 - 2) * (H / 3)) as usize
        );
    }

    /// Writes BGRA pixels as an uncompressed PNG, enough to look at a test render.
    fn png(path: &std::path::Path, bgra: &[u8]) {
        fn crc(data: &[u8]) -> u32 {
            !data.iter().fold(!0u32, |c, &b| {
                (0..8).fold(c ^ u32::from(b), |c, _| {
                    if c & 1 != 0 {
                        0xEDB8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    }
                })
            })
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8], body: &[u8]) {
            out.extend((body.len() as u32).to_be_bytes());
            let tagged = [kind, body].concat();
            out.extend(&tagged);
            out.extend(crc(&tagged).to_be_bytes());
        }
        let mut raw = Vec::new();
        for row in bgra.chunks(W as usize * 4) {
            raw.push(0);
            raw.extend(row.chunks(4).flat_map(|p| [p[2], p[1], p[0], p[3]]));
        }
        // zlib with stored deflate blocks, then the Adler-32 of the raw data.
        let mut zlib = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
        for (i, block) in blocks.iter().enumerate() {
            zlib.push(u8::from(i + 1 == blocks.len()));
            let n = block.len() as u16;
            zlib.extend(n.to_le_bytes());
            zlib.extend((!n).to_le_bytes());
            zlib.extend(*block);
        }
        let (a, b) = raw.iter().fold((1u32, 0u32), |(a, b), &x| {
            let a = (a + u32::from(x)) % 65521;
            (a, (b + a) % 65521)
        });
        zlib.extend((b << 16 | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let ihdr = [&W.to_be_bytes()[..], &H.to_be_bytes(), &[8, 6, 0, 0, 0]].concat();
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &zlib);
        chunk(&mut out, b"IEND", &[]);
        std::fs::write(path, out).unwrap();
    }
}

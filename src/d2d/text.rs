//! DirectWrite on Core Text: font collections from memory, text formats and layouts.
//!
//! Line breaking, shaping and font fallback are Core Text's; the layout around them
//! (line heights, baselines, alignment, trimming, metrics) follows DirectWrite, so text lands
//! where it does on Windows. DirectWrite takes a font's line metrics from OS/2's typo values
//! when USE_TYPO_METRICS is set (Inter sets it) and from its win values otherwise; the baseline
//! sits `ascent` below the top of the line.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2_core_foundation::{
    CFAttributedString, CFBoolean, CFData, CFDictionary, CFIndex, CFMutableAttributedString,
    CFNumber, CFRange, CFRetained, CFString, CFType, CGAffineTransform, CGFloat, CGPoint,
};
use objc2_core_graphics::{CGContext, CGDataProvider, CGFont};
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontSymbolicTraits, CTLine, CTLineTruncationType, CTTypesetter,
    kCTFontAttributeName, kCTFontFamilyNameAttribute, kCTFontSymbolicTrait, kCTFontTraitsAttribute,
    kCTFontWeightTrait, kCTForegroundColorFromContextAttributeName,
};

use super::*;

/// DirectWrite's oblique simulation, for italics a family has no face for.
const OBLIQUE: f64 = 0.333;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct DWRITE_TEXT_RANGE {
    pub startPosition: u32,
    pub length: u32,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct DWRITE_TRIMMING {
    pub granularity: DWRITE_TRIMMING_GRANULARITY,
    pub delimiter: u32,
    pub delimiterCount: u32,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct DWRITE_TEXT_METRICS {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub widthIncludingTrailingWhitespace: f32,
    pub height: f32,
    pub layoutWidth: f32,
    pub layoutHeight: f32,
    pub maxBidiReorderingDepth: u32,
    pub lineCount: u32,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct DWRITE_HIT_TEST_METRICS {
    pub textPosition: u32,
    pub length: u32,
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub bidiLevel: u32,
    pub isText: BOOL,
    pub isTrimmed: BOOL,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct DWRITE_LINE_METRICS {
    pub length: u32,
    pub trailingWhitespaceLength: u32,
    pub newlineLength: u32,
    pub height: f32,
    pub baseline: f32,
    pub isTrimmed: BOOL,
}

/// The DirectWrite factory; it holds nothing.
#[derive(Clone, Default)]
pub struct IDWriteFactory(());

/// `IDWriteFactory6`; its methods are the base factory's.
#[derive(Clone, Default)]
pub struct IDWriteFactory6(IDWriteFactory);

impl Deref for IDWriteFactory6 {
    type Target = IDWriteFactory;
    fn deref(&self) -> &IDWriteFactory {
        &self.0
    }
}

impl Interface for IDWriteFactory {}
impl Interface for IDWriteFactory6 {}

/// What `DWriteCreateFactory` can make.
pub trait DWriteFactory: Sized {
    #[doc(hidden)]
    fn make() -> Self;
}

impl DWriteFactory for IDWriteFactory {
    fn make() -> Self {
        IDWriteFactory(())
    }
}

impl DWriteFactory for IDWriteFactory6 {
    fn make() -> Self {
        IDWriteFactory6(IDWriteFactory(()))
    }
}

pub unsafe fn DWriteCreateFactory<T: DWriteFactory>(
    _factorytype: DWRITE_FACTORY_TYPE,
) -> Result<T> {
    Ok(T::make())
}

/// A font file's bytes.
#[derive(Clone)]
pub struct IDWriteFontFile(CFRetained<CFData>);

#[derive(Clone)]
pub struct IDWriteInMemoryFontFileLoader(());

impl IDWriteInMemoryFontFileLoader {
    /// Copies `size` bytes at `data` into a font file.
    pub unsafe fn CreateInMemoryFontFileReference<F>(
        &self,
        _factory: &F,
        data: *const c_void,
        size: u32,
        _owner: Option<&()>,
    ) -> Result<IDWriteFontFile> {
        if data.is_null() {
            return Err(Error::new(E_INVALIDARG, "no font data"));
        }
        let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) };
        Ok(IDWriteFontFile(CFData::from_bytes(bytes)))
    }
}

/// Collects font files for a font set.
#[derive(Clone, Default)]
pub struct IDWriteFontSetBuilder1(Rc<RefCell<Vec<IDWriteFontFile>>>);
pub type IDWriteFontSetBuilder = IDWriteFontSetBuilder1;

/// `IDWriteFontSetBuilder2`, whose own AddFontFile takes a path.
#[derive(Clone, Default)]
pub struct IDWriteFontSetBuilder2(IDWriteFontSetBuilder1);

impl Deref for IDWriteFontSetBuilder2 {
    type Target = IDWriteFontSetBuilder1;
    fn deref(&self) -> &IDWriteFontSetBuilder1 {
        &self.0
    }
}

impl IDWriteFontSetBuilder1 {
    pub unsafe fn AddFontFile(&self, fontfile: &IDWriteFontFile) -> Result<()> {
        self.0.borrow_mut().push(fontfile.clone());
        Ok(())
    }

    pub unsafe fn CreateFontSet(&self) -> Result<IDWriteFontSet> {
        Ok(IDWriteFontSet(self.0.borrow().clone()))
    }
}

impl IDWriteFontSetBuilder2 {
    pub unsafe fn AddFontFile(&self, filepath: impl StrParam) -> Result<()> {
        let bytes = std::fs::read(filepath.as_str()).map_err(|e| Error::fail(e.to_string()))?;
        self.0
            .0
            .borrow_mut()
            .push(IDWriteFontFile(CFData::from_bytes(&bytes)));
        Ok(())
    }
}

#[derive(Clone)]
pub struct IDWriteFontSet(Vec<IDWriteFontFile>);

/// The ellipsis trimming sign; layouts draw "…" in the trimmed text's font.
#[derive(Clone)]
pub struct IDWriteInlineObject(());

/// One face of a collection, with DirectWrite's line metrics per em.
struct Face {
    family: String,
    weight: i32,
    italic: bool,
    cg: CFRetained<CGFont>,
    ascent: f32,
    descent: f32,
    gap: f32,
}

/// Fonts loaded from memory. `IDWriteFontCollection1` and `2` are this type.
#[derive(Clone)]
pub struct IDWriteFontCollection(Rc<Vec<Face>>);
pub type IDWriteFontCollection1 = IDWriteFontCollection;
pub type IDWriteFontCollection2 = IDWriteFontCollection;

impl Interface for IDWriteFontCollection {}

impl IDWriteFontCollection {
    /// DirectWrite's (and CSS's) weight matching within `family`: the exact weight, then for
    /// 400 and 500 the other of the two, then lighter for light requests and heavier for bold
    /// ones, then the other direction.
    fn best(&self, family: &str, weight: i32, italic: bool) -> Option<&Face> {
        let faces: Vec<&Face> = self
            .0
            .iter()
            .filter(|f| f.family.eq_ignore_ascii_case(family))
            .collect();
        let styled: Vec<&Face> = faces
            .iter()
            .copied()
            .filter(|f| f.italic == italic)
            .collect();
        let pool = if styled.is_empty() { faces } else { styled };
        let rank = |f: &Face| -> (u8, i32) {
            let w = f.weight;
            if w == weight {
                return (0, 0);
            }
            if (400..=500).contains(&weight) {
                if w > weight && w <= 500 {
                    return (1, w - weight);
                }
                if w < weight {
                    return (2, weight - w);
                }
                return (3, w - weight);
            }
            if weight < 400 {
                if w < weight {
                    (1, weight - w)
                } else {
                    (2, w - weight)
                }
            } else if w > weight {
                (1, w - weight)
            } else {
                (2, weight - w)
            }
        };
        pool.into_iter().min_by_key(|f| rank(f))
    }
}

/// A collection argument: `&collection`, or an Option of one.
pub trait CollectionParam {
    #[doc(hidden)]
    fn collection(&self) -> Option<&IDWriteFontCollection>;
}

impl CollectionParam for &IDWriteFontCollection {
    fn collection(&self) -> Option<&IDWriteFontCollection> {
        Some(self)
    }
}

impl CollectionParam for Option<&IDWriteFontCollection> {
    fn collection(&self) -> Option<&IDWriteFontCollection> {
        *self
    }
}

impl IDWriteFactory {
    pub unsafe fn CreateInMemoryFontFileLoader(&self) -> Result<IDWriteInMemoryFontFileLoader> {
        Ok(IDWriteInMemoryFontFileLoader(()))
    }

    pub unsafe fn RegisterFontFileLoader(
        &self,
        _loader: &IDWriteInMemoryFontFileLoader,
    ) -> Result<()> {
        Ok(())
    }

    pub unsafe fn CreateFontSetBuilder(&self) -> Result<IDWriteFontSetBuilder2> {
        Ok(IDWriteFontSetBuilder2::default())
    }

    /// Parses the set's fonts. A file Core Graphics can't read is an error, as a bad font file
    /// is for DirectWrite.
    pub unsafe fn CreateFontCollectionFromFontSet(
        &self,
        fontset: &IDWriteFontSet,
        _model: DWRITE_FONT_FAMILY_MODEL,
    ) -> Result<IDWriteFontCollection2> {
        let mut faces = Vec::new();
        for file in &fontset.0 {
            let bytes = file.0.to_vec();
            let info = parse_font(&bytes).ok_or_else(|| Error::fail("not a font"))?;
            let provider = CGDataProvider::with_cf_data(Some(&file.0))
                .ok_or_else(|| Error::fail("font data"))?;
            let cg =
                CGFont::with_data_provider(&provider).ok_or_else(|| Error::fail("bad font"))?;
            faces.push(Face {
                family: info.family,
                weight: info.weight,
                italic: info.italic,
                cg,
                ascent: info.ascent,
                descent: info.descent,
                gap: info.gap,
            });
        }
        Ok(IDWriteFontCollection(Rc::new(faces)))
    }

    pub unsafe fn CreateTextFormat(
        &self,
        fontfamilyname: impl StrParam,
        fontcollection: impl CollectionParam,
        fontweight: DWRITE_FONT_WEIGHT,
        fontstyle: DWRITE_FONT_STYLE,
        _fontstretch: DWRITE_FONT_STRETCH,
        fontsize: f32,
        _localename: impl StrParam,
    ) -> Result<IDWriteTextFormat> {
        if fontsize.is_nan() || fontsize <= 0.0 {
            return Err(Error::new(E_INVALIDARG, "font size"));
        }
        Ok(IDWriteTextFormat(Rc::new(Format {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            family: fontfamilyname.as_str().to_owned(),
            collection: fontcollection.collection().cloned(),
            weight: fontweight,
            style: fontstyle,
            size: fontsize,
            settings: Cell::new(Settings::default()),
            generation: Cell::new(0),
            fonts: RefCell::new(HashMap::new()),
        })))
    }

    pub unsafe fn CreateEllipsisTrimmingSign(
        &self,
        _textformat: &IDWriteTextFormat,
    ) -> Result<IDWriteInlineObject> {
        Ok(IDWriteInlineObject(()))
    }

    pub unsafe fn CreateTextLayout(
        &self,
        string: &[u16],
        textformat: &IDWriteTextFormat,
        maxwidth: f32,
        maxheight: f32,
    ) -> Result<IDWriteTextLayout> {
        Ok(IDWriteTextLayout::new(
            string, textformat, maxwidth, maxheight,
        ))
    }
}

/// What a font file says about itself.
struct FontInfo {
    family: String,
    weight: i32,
    italic: bool,
    ascent: f32,
    descent: f32,
    gap: f32,
}

/// Reads the name, weight, style and DirectWrite's vertical metrics (per em) from an
/// OpenType file's tables.
fn parse_font(d: &[u8]) -> Option<FontInfo> {
    let u16_at =
        |o: usize| -> Option<u16> { Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?])) };
    let i16_at = |o: usize| u16_at(o).map(|v| v as i16);
    let u32_at = |o: usize| -> Option<u32> {
        Some(u32::from_be_bytes([
            *d.get(o)?,
            *d.get(o + 1)?,
            *d.get(o + 2)?,
            *d.get(o + 3)?,
        ]))
    };
    let tables = u16_at(4)? as usize;
    let mut find = HashMap::new();
    for i in 0..tables {
        let at = 12 + 16 * i;
        let tag = d.get(at..at + 4)?;
        find.insert(
            tag.to_vec(),
            (u32_at(at + 8)? as usize, u32_at(at + 12)? as usize),
        );
    }
    let (head, _) = *find.get(b"head".as_slice())?;
    let upm = f32::from(u16_at(head + 18)?.max(1));
    let mac_style = u16_at(head + 44)?;
    let (hhea, _) = *find.get(b"hhea".as_slice())?;
    let (h_asc, h_desc, h_gap) = (i16_at(hhea + 4)?, i16_at(hhea + 6)?, i16_at(hhea + 8)?);
    let (mut weight, mut italic) = (400, mac_style & 2 != 0);
    let (mut ascent, mut descent, mut gap) =
        (f32::from(h_asc), -f32::from(h_desc), f32::from(h_gap));
    if let Some(&(os2, len)) = find.get(b"OS/2".as_slice())
        && len >= 78
    {
        weight = i32::from(u16_at(os2 + 4)?);
        let selection = u16_at(os2 + 62)?;
        italic = selection & 1 != 0;
        let (t_asc, t_desc, t_gap) = (i16_at(os2 + 68)?, i16_at(os2 + 70)?, i16_at(os2 + 72)?);
        let (w_asc, w_desc) = (u16_at(os2 + 74)?, u16_at(os2 + 76)?);
        if selection & 0x80 != 0 {
            (ascent, descent, gap) = (f32::from(t_asc), -f32::from(t_desc), f32::from(t_gap));
        } else {
            // GDI-compatible metrics, with hhea's spare room as the gap.
            ascent = f32::from(w_asc);
            descent = f32::from(w_desc);
            let hhea_total = f32::from(h_asc) - f32::from(h_desc) + f32::from(h_gap);
            gap = (hhea_total - ascent - descent).max(0.0);
        }
    }
    let family = font_name(d, &find, 16).or_else(|| font_name(d, &find, 1))?;
    Some(FontInfo {
        family,
        weight,
        italic,
        ascent: ascent / upm,
        descent: descent / upm,
        gap: gap / upm,
    })
}

/// A name from the name table, in its Windows (UTF-16) record.
fn font_name(d: &[u8], tables: &HashMap<Vec<u8>, (usize, usize)>, id: u16) -> Option<String> {
    let u16_at =
        |o: usize| -> Option<u16> { Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?])) };
    let &(name, _) = tables.get(b"name".as_slice())?;
    let count = u16_at(name + 2)? as usize;
    let strings = name + u16_at(name + 4)? as usize;
    for i in 0..count {
        let r = name + 6 + 12 * i;
        let (platform, name_id) = (u16_at(r)?, u16_at(r + 6)?);
        if platform == 3 && name_id == id {
            let (len, off) = (u16_at(r + 8)? as usize, u16_at(r + 10)? as usize);
            let raw = d.get(strings + off..strings + off + len)?;
            let units: Vec<u16> = raw
                .chunks(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            return Some(String::from_utf16_lossy(&units));
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Settings {
    align: DWRITE_TEXT_ALIGNMENT,
    paragraph: DWRITE_PARAGRAPH_ALIGNMENT,
    wrap: DWRITE_WORD_WRAPPING,
    trim: bool,
    spacing: (DWRITE_LINE_SPACING_METHOD, f32, f32),
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            align: DWRITE_TEXT_ALIGNMENT_LEADING,
            paragraph: DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
            wrap: DWRITE_WORD_WRAPPING_WRAP,
            trim: false,
            spacing: (DWRITE_LINE_SPACING_METHOD_DEFAULT, 0.0, 0.0),
        }
    }
}

/// A resolved font at one size, with the line metrics DirectWrite would use, in DIPs.
#[derive(Clone)]
struct Font {
    ct: CFRetained<CTFont>,
    ascent: f32,
    descent: f32,
    gap: f32,
}

struct Format {
    /// Identifies the format in DrawText's layout cache.
    id: u64,
    family: String,
    collection: Option<IDWriteFontCollection>,
    weight: DWRITE_FONT_WEIGHT,
    style: DWRITE_FONT_STYLE,
    size: f32,
    settings: Cell<Settings>,
    /// Bumped by every setter, so cached layouts of the old settings aren't reused.
    generation: Cell<u64>,
    fonts: RefCell<HashMap<(i32, i32), Font>>,
}

impl Format {
    fn font(&self, weight: DWRITE_FONT_WEIGHT, style: DWRITE_FONT_STYLE) -> Font {
        let key = (weight.0, style.0);
        if let Some(f) = self.fonts.borrow().get(&key) {
            return f.clone();
        }
        let f = resolve(
            &self.family,
            self.collection.as_ref(),
            weight,
            style,
            self.size,
        );
        self.fonts.borrow_mut().insert(key, f.clone());
        f
    }
}

/// Finds `family` in `collection`, else among the system's fonts.
fn resolve(
    family: &str,
    collection: Option<&IDWriteFontCollection>,
    weight: DWRITE_FONT_WEIGHT,
    style: DWRITE_FONT_STYLE,
    size: f32,
) -> Font {
    let italic = style != DWRITE_FONT_STYLE_NORMAL;
    let skew = CGAffineTransform {
        a: 1.0,
        b: 0.0,
        // Negative: Core Text applies the font matrix in the flipped text space we draw in.
        c: -OBLIQUE,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };
    if let Some(face) = collection.and_then(|c| c.best(family, weight.0, italic)) {
        let matrix: *const CGAffineTransform = if italic && !face.italic {
            &skew
        } else {
            std::ptr::null()
        };
        let ct = unsafe { CTFont::with_graphics_font(&face.cg, size.into(), matrix, None) };
        return Font {
            ct,
            ascent: face.ascent * size,
            descent: face.descent * size,
            gap: face.gap * size,
        };
    }
    let name = CFString::from_str(family);
    let weight_value = CFNumber::new_f64(ct_weight(weight.0));
    let traits_value = CFNumber::new_i32(if italic {
        CTFontSymbolicTraits::TraitItalic.0 as i32
    } else {
        0
    });
    let traits: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[unsafe { kCTFontWeightTrait }, unsafe {
            kCTFontSymbolicTrait
        }],
        &[weight_value.as_ref(), traits_value.as_ref()],
    );
    let attributes: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[unsafe { kCTFontFamilyNameAttribute }, unsafe {
            kCTFontTraitsAttribute
        }],
        &[name.as_ref(), traits.as_ref()],
    );
    let descriptor = unsafe { CTFontDescriptor::with_attributes(attributes.as_opaque()) };
    let mut ct =
        unsafe { CTFont::with_font_descriptor(&descriptor, size.into(), std::ptr::null()) };
    let slanted = unsafe { ct.symbolic_traits() }.contains(CTFontSymbolicTraits::TraitItalic);
    if italic && !slanted {
        ct = unsafe { CTFont::with_font_descriptor(&descriptor, size.into(), &skew) };
    }
    let (ascent, descent, gap) = unsafe { (ct.ascent(), ct.descent(), ct.leading()) };
    Font {
        ct,
        ascent: ascent as f32,
        descent: descent as f32,
        gap: gap as f32,
    }
}

/// An OpenType weight as Core Text's -1..1 weight trait.
fn ct_weight(w: i32) -> f64 {
    const TABLE: [(i32, f64); 9] = [
        (100, -0.8),
        (200, -0.6),
        (300, -0.4),
        (400, 0.0),
        (500, 0.23),
        (600, 0.3),
        (700, 0.4),
        (800, 0.56),
        (900, 0.62),
    ];
    let w = w.clamp(100, 900);
    let i = TABLE.iter().position(|&(t, _)| t >= w).unwrap_or(8);
    if i == 0 || TABLE[i].0 == w {
        return TABLE[i].1;
    }
    let ((w0, v0), (w1, v1)) = (TABLE[i - 1], TABLE[i]);
    v0 + (v1 - v0) * f64::from(w - w0) / f64::from(w1 - w0)
}

/// A text format: font, size and paragraph settings for layouts.
#[derive(Clone)]
pub struct IDWriteTextFormat(Rc<Format>);

impl Interface for IDWriteTextFormat {}

impl IDWriteTextFormat {
    pub(crate) fn id(&self) -> u64 {
        self.0.id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.0.generation.get()
    }

    fn update(&self, f: impl FnOnce(&mut Settings)) -> Result<()> {
        let mut s = self.0.settings.get();
        f(&mut s);
        self.0.settings.set(s);
        self.0.generation.set(self.0.generation.get() + 1);
        Ok(())
    }

    pub unsafe fn SetTextAlignment(&self, textalignment: DWRITE_TEXT_ALIGNMENT) -> Result<()> {
        self.update(|s| s.align = textalignment)
    }

    pub unsafe fn SetParagraphAlignment(
        &self,
        paragraphalignment: DWRITE_PARAGRAPH_ALIGNMENT,
    ) -> Result<()> {
        self.update(|s| s.paragraph = paragraphalignment)
    }

    pub unsafe fn SetWordWrapping(&self, wordwrapping: DWRITE_WORD_WRAPPING) -> Result<()> {
        self.update(|s| s.wrap = wordwrapping)
    }

    /// Character or word trimming with a sign trims with an ellipsis; Core Text cuts at
    /// clusters either way.
    pub unsafe fn SetTrimming(
        &self,
        trimmingoptions: &DWRITE_TRIMMING,
        trimmingsign: &IDWriteInlineObject,
    ) -> Result<()> {
        let _ = trimmingsign;
        let on = trimmingoptions.granularity != DWRITE_TRIMMING_GRANULARITY_NONE;
        self.update(|s| s.trim = on)
    }

    pub unsafe fn SetLineSpacing(
        &self,
        linespacingmethod: DWRITE_LINE_SPACING_METHOD,
        linespacing: f32,
        baseline: f32,
    ) -> Result<()> {
        self.update(|s| s.spacing = (linespacingmethod, linespacing, baseline))
    }

    pub unsafe fn GetFontSize(&self) -> f32 {
        self.0.size
    }

    pub unsafe fn GetFontWeight(&self) -> DWRITE_FONT_WEIGHT {
        self.0.weight
    }

    pub unsafe fn GetTextAlignment(&self) -> DWRITE_TEXT_ALIGNMENT {
        self.0.settings.get().align
    }

    pub unsafe fn GetParagraphAlignment(&self) -> DWRITE_PARAGRAPH_ALIGNMENT {
        self.0.settings.get().paragraph
    }

    pub unsafe fn GetWordWrapping(&self) -> DWRITE_WORD_WRAPPING {
        self.0.settings.get().wrap
    }
}

/// One laid-out line. `top` and `x` are relative to the text block, `baseline` to `top`.
struct Line {
    ct: Option<CFRetained<CTLine>>,
    start: usize,
    len: usize,
    x: f32,
    top: f32,
    baseline: f32,
    height: f32,
    width: f32,
    width_ws: f32,
    /// Height and baseline from the line's fonts, before line spacing.
    natural: (f32, f32),
    trimmed: bool,
}

struct Built {
    lines: Vec<Line>,
    /// The block's top within the layout box, from the paragraph alignment.
    y0: f32,
    metrics: DWRITE_TEXT_METRICS,
}

struct Layout {
    text: Vec<u16>,
    format: IDWriteTextFormat,
    settings: Settings,
    max_w: f32,
    max_h: f32,
    weights: Vec<(DWRITE_TEXT_RANGE, DWRITE_FONT_WEIGHT)>,
    styles: Vec<(DWRITE_TEXT_RANGE, DWRITE_FONT_STYLE)>,
    built: Option<Rc<Built>>,
}

/// A text layout: the string broken into lines and placed in its box, built when first
/// measured or drawn.
#[derive(Clone)]
pub struct IDWriteTextLayout(Rc<RefCell<Layout>>);

impl Interface for IDWriteTextLayout {}

fn is_break(c: u16) -> bool {
    matches!(c, 0x0A | 0x0B | 0x0C | 0x0D | 0x85 | 0x2028 | 0x2029)
}

impl IDWriteTextLayout {
    pub(crate) fn new(text: &[u16], format: &IDWriteTextFormat, max_w: f32, max_h: f32) -> Self {
        IDWriteTextLayout(Rc::new(RefCell::new(Layout {
            text: text.to_vec(),
            format: format.clone(),
            settings: format.0.settings.get(),
            max_w: max_w.max(0.0),
            max_h: max_h.max(0.0),
            weights: Vec::new(),
            styles: Vec::new(),
            built: None,
        })))
    }

    fn change(&self, f: impl FnOnce(&mut Layout)) -> Result<()> {
        let mut l = self.0.borrow_mut();
        f(&mut l);
        l.built = None;
        Ok(())
    }

    pub unsafe fn SetFontWeight(
        &self,
        weight: DWRITE_FONT_WEIGHT,
        range: DWRITE_TEXT_RANGE,
    ) -> Result<()> {
        self.change(|l| l.weights.push((range, weight)))
    }

    pub unsafe fn SetFontStyle(
        &self,
        style: DWRITE_FONT_STYLE,
        range: DWRITE_TEXT_RANGE,
    ) -> Result<()> {
        self.change(|l| l.styles.push((range, style)))
    }

    pub unsafe fn SetTextAlignment(&self, textalignment: DWRITE_TEXT_ALIGNMENT) -> Result<()> {
        self.change(|l| l.settings.align = textalignment)
    }

    pub unsafe fn SetParagraphAlignment(&self, a: DWRITE_PARAGRAPH_ALIGNMENT) -> Result<()> {
        self.change(|l| l.settings.paragraph = a)
    }

    pub unsafe fn SetWordWrapping(&self, wordwrapping: DWRITE_WORD_WRAPPING) -> Result<()> {
        self.change(|l| l.settings.wrap = wordwrapping)
    }

    pub unsafe fn SetMaxWidth(&self, maxwidth: f32) -> Result<()> {
        self.change(|l| l.max_w = maxwidth.max(0.0))
    }

    pub unsafe fn SetMaxHeight(&self, maxheight: f32) -> Result<()> {
        self.change(|l| l.max_h = maxheight.max(0.0))
    }

    pub unsafe fn GetMaxWidth(&self) -> f32 {
        self.0.borrow().max_w
    }

    pub unsafe fn GetMaxHeight(&self) -> f32 {
        self.0.borrow().max_h
    }

    pub unsafe fn GetMetrics(&self, textmetrics: &mut DWRITE_TEXT_METRICS) -> Result<()> {
        *textmetrics = self.built().metrics;
        Ok(())
    }

    pub unsafe fn GetLineMetrics(
        &self,
        linemetrics: Option<&mut [DWRITE_LINE_METRICS]>,
        actuallinecount: &mut u32,
    ) -> Result<()> {
        let b = self.built();
        let text = self.0.borrow().text.clone();
        *actuallinecount = b.lines.len() as u32;
        if let Some(out) = linemetrics {
            for (m, line) in out.iter_mut().zip(&b.lines) {
                let units = &text[line.start..line.start + line.len];
                let newline = units.iter().rev().take_while(|&&c| is_break(c)).count();
                let spaces = units[..units.len() - newline]
                    .iter()
                    .rev()
                    .take_while(|&&c| c == 0x20 || c == 0x09 || c == 0x3000)
                    .count();
                *m = DWRITE_LINE_METRICS {
                    length: line.len as u32,
                    trailingWhitespaceLength: (spaces + newline) as u32,
                    newlineLength: newline as u32,
                    height: line.height,
                    baseline: line.baseline,
                    isTrimmed: BOOL::from(line.trimmed),
                };
            }
        }
        Ok(())
    }

    /// Where the caret goes before (or after, when trailing) the unit at `textposition`.
    pub unsafe fn HitTestTextPosition(
        &self,
        textposition: u32,
        istrailinghit: bool,
        pointx: &mut f32,
        pointy: &mut f32,
        hittestmetrics: &mut DWRITE_HIT_TEST_METRICS,
    ) -> Result<()> {
        let b = self.built();
        let n = self.0.borrow().text.len();
        let pos = (textposition as usize).min(n);
        let line = b
            .lines
            .iter()
            .find(|l| pos >= l.start && pos < l.start + l.len)
            .or(b.lines.last())
            .ok_or_else(|| Error::fail("empty layout"))?;
        let offset = |i: usize| -> f32 {
            line.ct.as_ref().map_or(0.0, |ct| unsafe {
                ct.offset_for_string_index(i as CFIndex, std::ptr::null_mut()) as f32
            })
        };
        let next = (pos + 1).min(line.start + line.len).max(pos);
        let (here, after) = (offset(pos), offset(next));
        let x = line.x + if istrailinghit { after } else { here };
        *pointx = x;
        *pointy = b.y0 + line.top;
        *hittestmetrics = DWRITE_HIT_TEST_METRICS {
            textPosition: pos as u32,
            length: (next - pos) as u32,
            left: line.x + here,
            top: b.y0 + line.top,
            width: after - here,
            height: line.height,
            bidiLevel: 0,
            isText: BOOL::from(pos < n),
            isTrimmed: BOOL::from(line.trimmed),
        };
        Ok(())
    }

    /// The unit under (x, y), and whether the point is on its trailing half.
    pub unsafe fn HitTestPoint(
        &self,
        pointx: f32,
        pointy: f32,
        istrailinghit: &mut BOOL,
        isinside: &mut BOOL,
        hittestmetrics: &mut DWRITE_HIT_TEST_METRICS,
    ) -> Result<()> {
        let b = self.built();
        let y = pointy - b.y0;
        let line = b
            .lines
            .iter()
            .find(|l| y < l.top + l.height)
            .or(b.lines.last())
            .ok_or_else(|| Error::fail("empty layout"))?;
        let local = pointx - line.x;
        let caret = line.ct.as_ref().map_or(line.start, |ct| unsafe {
            ct.string_index_for_position(CGPoint::new(local.into(), 0.0))
                .max(0) as usize
        });
        let offset = |i: usize| -> f32 {
            line.ct.as_ref().map_or(0.0, |ct| unsafe {
                ct.offset_for_string_index(i as CFIndex, std::ptr::null_mut()) as f32
            })
        };
        // A caret after the point's character means the point was on its trailing half.
        let (pos, trailing) = if caret > line.start && offset(caret) > local {
            (caret - 1, false)
        } else if caret > line.start {
            (caret - 1, true)
        } else {
            (caret, false)
        };
        *istrailinghit = BOOL::from(trailing);
        *isinside =
            BOOL::from(y >= 0.0 && y < b.metrics.height && local >= 0.0 && local <= line.width_ws);
        let mut x = 0.0;
        let mut yy = 0.0;
        unsafe { self.HitTestTextPosition(pos as u32, false, &mut x, &mut yy, hittestmetrics) }
    }

    fn built(&self) -> Rc<Built> {
        if let Some(b) = &self.0.borrow().built {
            return b.clone();
        }
        let b = Rc::new(self.0.borrow().build());
        self.0.borrow_mut().built = Some(b.clone());
        b
    }

    /// Draws the lines with their tops at `origin`, in `paint`'s colour. Baselines snap to
    /// whole pixels as Direct2D's do, unless NO_SNAP.
    pub(crate) fn draw(
        &self,
        t: &DcState,
        origin: Vector2,
        paint: &PaintSpec,
        options: D2D1_DRAW_TEXT_OPTIONS,
    ) {
        let PaintSpec::Solid(color, opacity) = paint else {
            // Text with a bitmap brush isn't something the interface draws.
            return;
        };
        let b = self.built();
        let (max_w, max_h) = {
            let l = self.0.borrow();
            (l.max_w, l.max_h)
        };
        let clip = options.0 & D2D1_DRAW_TEXT_OPTIONS_CLIP.0 != 0;
        let m = t.world;
        let snap =
            options.0 & D2D1_DRAW_TEXT_OPTIONS_NO_SNAP.0 == 0 && m.axis_aligned() && m.M22 != 0.0;
        let scale = t.scale;
        t.with_world(|cg| {
            if clip {
                let r = D2D_RECT_F {
                    left: origin.X,
                    top: origin.Y,
                    right: origin.X + max_w,
                    bottom: origin.Y + max_h,
                };
                CGContext::clip_to_rect(Some(cg), r.cg());
            }
            t.set_fill(cg, color, *opacity);
            CGContext::set_text_matrix(
                Some(cg),
                CGAffineTransform {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: -1.0,
                    tx: 0.0,
                    ty: 0.0,
                },
            );
            for line in &b.lines {
                let Some(ct) = &line.ct else { continue };
                let mut y = origin.Y + b.y0 + line.top + line.baseline;
                if snap {
                    let device = (y * m.M22 + m.M32) * scale;
                    y = (device.round() / scale - m.M32) / m.M22;
                }
                let x = origin.X + line.x;
                CGContext::set_text_position(Some(cg), x.into(), y.into());
                unsafe { ct.draw(cg) };
            }
        });
    }
}

impl Layout {
    fn build(&self) -> Built {
        let fmt = &self.format.0;
        let s = self.settings;
        let text = &self.text;
        let n = text.len();
        let base = fmt.font(fmt.weight, fmt.style);

        // Each unit's weight and style, the ranges applied in order.
        let mut weights = vec![fmt.weight; n];
        let mut styles = vec![fmt.style; n];
        let span = |r: &DWRITE_TEXT_RANGE| {
            let start = (r.startPosition as usize).min(n);
            start..(start.saturating_add(r.length as usize)).min(n)
        };
        for (r, w) in &self.weights {
            weights[span(r)].fill(*w);
        }
        for (r, st) in &self.styles {
            styles[span(r)].fill(*st);
        }

        let string = unsafe { CFString::with_characters(None, text.as_ptr(), n as CFIndex) }
            .unwrap_or_else(|| CFString::from_str(""));
        let attributed = CFMutableAttributedString::new(None, 0).expect("attributed string");
        unsafe {
            CFMutableAttributedString::replace_string(
                Some(&attributed),
                CFRange::new(0, 0),
                Some(&string),
            );
        }
        let mut fonts: Vec<Font> = Vec::new();
        let mut i = 0;
        while i < n {
            let mut j = i + 1;
            while j < n && weights[j] == weights[i] && styles[j] == styles[i] {
                j += 1;
            }
            let font = fmt.font(weights[i], styles[i]);
            unsafe {
                CFMutableAttributedString::set_attribute(
                    Some(&attributed),
                    CFRange::new(i as CFIndex, (j - i) as CFIndex),
                    Some(kCTFontAttributeName),
                    Some(font.ct.as_ref()),
                );
            }
            fonts.push(font);
            i = j;
        }
        if n > 0 {
            unsafe {
                CFMutableAttributedString::set_attribute(
                    Some(&attributed),
                    CFRange::new(0, n as CFIndex),
                    Some(kCTForegroundColorFromContextAttributeName),
                    Some(CFBoolean::new(true).as_ref()),
                );
            }
        }
        let attributed: &CFAttributedString = &attributed;
        let typesetter = unsafe { CTTypesetter::with_attributed_string(attributed) };
        let wrap = s.wrap != DWRITE_WORD_WRAPPING_NO_WRAP;
        let width = if wrap { f64::from(self.max_w) } else { 1.0e7 };

        // Lines as unit ranges: paragraphs at hard breaks, then wrapped within them.
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        loop {
            let mut end = start;
            while end < n && !is_break(text[end]) {
                end += 1;
            }
            let brk = match () {
                _ if end >= n => 0,
                _ if text[end] == 0x0D && text.get(end + 1) == Some(&0x0A) => 2,
                _ => 1,
            };
            let stop = end + brk;
            if !wrap || start == end {
                ranges.push((start, stop - start));
            } else {
                let mut at = start;
                while at < stop {
                    let mut count = unsafe { typesetter.suggest_line_break(at as CFIndex, width) };
                    if count <= 0 {
                        count = unsafe { typesetter.suggest_cluster_break(at as CFIndex, width) }
                            .max(1);
                    }
                    let count = (count as usize).min(stop - at);
                    // A break at the paragraph's last character takes the newline along.
                    let count = if at + count == end { stop - at } else { count };
                    ranges.push((at, count));
                    at += count;
                }
            }
            if brk == 0 {
                break;
            }
            start = stop;
            if start >= n {
                // Text ending in a line break has an empty last line.
                ranges.push((n, 0));
                break;
            }
        }

        let line_of = |start: usize, len: usize| -> Option<CFRetained<CTLine>> {
            (len > 0)
                .then(|| unsafe { typesetter.line(CFRange::new(start as CFIndex, len as CFIndex)) })
        };
        let ellipsis = |at: usize| -> CFRetained<CTLine> {
            let font = if at < n {
                fmt.font(weights[at], styles[at])
            } else {
                base.clone()
            };
            ellipsis_line(&font)
        };
        let mut lines: Vec<Line> = ranges
            .iter()
            .map(|&(start, len)| {
                let ct = line_of(start, len);
                let mut line = Line {
                    ct,
                    start,
                    len,
                    x: 0.0,
                    top: 0.0,
                    baseline: 0.0,
                    height: 0.0,
                    width: 0.0,
                    width_ws: 0.0,
                    natural: (0.0, 0.0),
                    trimmed: false,
                };
                measure(&mut line, &base);
                if s.trim && !wrap && line.width > self.max_w + 0.01 {
                    trim(&mut line, self.max_w, &ellipsis(start), &base);
                }
                line
            })
            .collect();

        place_vertically(&mut lines, s.spacing);
        // Wrapped text taller than the box loses its last lines, the last kept one trimmed.
        if s.trim && wrap && lines.len() > 1 {
            let fits = lines
                .iter()
                .take_while(|l| l.top + l.height <= self.max_h + 0.01)
                .count();
            if fits < lines.len() {
                let keep = fits.max(1);
                lines.truncate(keep);
                let last = lines.last_mut().unwrap();
                last.ct = line_of(last.start, n - last.start);
                last.len = n - last.start;
                measure(last, &base);
                trim(last, self.max_w, &ellipsis(last.start), &base);
                place_vertically(&mut lines, s.spacing);
            }
        }

        let total: f32 = lines.iter().map(|l| l.height).sum();
        let y0 = match s.paragraph {
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER => (self.max_h - total) / 2.0,
            DWRITE_PARAGRAPH_ALIGNMENT_FAR => self.max_h - total,
            _ => 0.0,
        };
        for line in &mut lines {
            line.x = match s.align {
                DWRITE_TEXT_ALIGNMENT_CENTER => (self.max_w - line.width) / 2.0,
                DWRITE_TEXT_ALIGNMENT_TRAILING => self.max_w - line.width,
                _ => 0.0,
            };
        }
        let metrics = DWRITE_TEXT_METRICS {
            left: lines
                .iter()
                .map(|l| l.x)
                .fold(f32::MAX, f32::min)
                .min(self.max_w),
            top: y0,
            width: lines.iter().map(|l| l.width).fold(0.0, f32::max),
            widthIncludingTrailingWhitespace: lines.iter().map(|l| l.width_ws).fold(0.0, f32::max),
            height: total,
            layoutWidth: self.max_w,
            layoutHeight: self.max_h,
            maxBidiReorderingDepth: 1,
            lineCount: lines.len() as u32,
        };
        Built { lines, y0, metrics }
    }
}

/// A line's widths and DirectWrite's line height and baseline from its fonts.
fn measure(line: &mut Line, base: &Font) {
    let (mut ascent, mut descent, mut gap) = (base.ascent, base.descent, base.gap);
    if let Some(ct) = &line.ct {
        let (mut a, mut d, mut l): (CGFloat, CGFloat, CGFloat) = (0.0, 0.0, 0.0);
        let w = unsafe { ct.typographic_bounds(&mut a, &mut d, &mut l) };
        let trailing = unsafe { ct.trailing_whitespace_width() };
        line.width_ws = w as f32;
        line.width = (w - trailing).max(0.0) as f32;
        // A fallback font taller than the format's makes its line taller, as in DirectWrite.
        ascent = ascent.max(a as f32);
        descent = descent.max(d as f32);
        gap = gap.max(l as f32);
    } else {
        line.width = 0.0;
        line.width_ws = 0.0;
    }
    line.natural = (ascent + descent + gap, ascent);
}

fn trim(line: &mut Line, max_w: f32, token: &CTLine, base: &Font) {
    let Some(ct) = &line.ct else { return };
    let cut = unsafe { ct.truncated_line(max_w.into(), CTLineTruncationType::End, Some(token)) };
    if let Some(cut) = cut {
        line.ct = Some(cut);
        line.trimmed = true;
        let (height, baseline) = line.natural;
        measure(line, base);
        // Trimming changes the glyphs, not the line box.
        line.natural = (height.max(line.natural.0), baseline.max(line.natural.1));
    }
}

/// Stacks the lines from the top, with the format's line spacing.
fn place_vertically(
    lines: &mut [Line],
    (method, spacing, baseline): (DWRITE_LINE_SPACING_METHOD, f32, f32),
) {
    let mut top = 0.0;
    for line in lines {
        let (height, base) = line.natural;
        (line.height, line.baseline) = match method {
            DWRITE_LINE_SPACING_METHOD_UNIFORM => (spacing, baseline),
            DWRITE_LINE_SPACING_METHOD_PROPORTIONAL => (height * spacing, base * baseline),
            _ => (height, base),
        };
        line.top = top;
        top += line.height;
    }
}

fn ellipsis_line(font: &Font) -> CFRetained<CTLine> {
    let string = CFString::from_str("\u{2026}");
    let attributed = CFMutableAttributedString::new(None, 0).expect("attributed string");
    unsafe {
        CFMutableAttributedString::replace_string(
            Some(&attributed),
            CFRange::new(0, 0),
            Some(&string),
        );
        let all = CFRange::new(0, 1);
        CFMutableAttributedString::set_attribute(
            Some(&attributed),
            all,
            Some(kCTFontAttributeName),
            Some(font.ct.as_ref()),
        );
        CFMutableAttributedString::set_attribute(
            Some(&attributed),
            all,
            Some(kCTForegroundColorFromContextAttributeName),
            Some(CFBoolean::new(true).as_ref()),
        );
        CTLine::with_attributed_string(&attributed)
    }
}

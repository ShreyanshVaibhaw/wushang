//! Direct2D and DirectWrite, as far as the interface uses them, on Core Graphics and Core Text.
//!
//! The types, constants and methods keep windows-rs's names and call shapes (unsafe methods,
//! `Result` returns, out-parameters), so `src/ui/**` and `src/captions.rs` stay diffable against
//! the Windows code. Drawing goes into the CGBitmapContext that [`ID2D1DeviceContext5::begin`]
//! is handed, with Direct2D's top-left origin, y down, in DIPs.
//!
//! What it implements is what the interface needs, with Direct2D's semantics where they show:
//! aliased clips snap to pixels, text baselines snap to pixels, path geometries fill
//! alternate (even-odd), Inter's line height and baseline follow DirectWrite's metrics.

#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]
#![allow(
    clippy::missing_safety_doc,
    clippy::too_many_arguments,
    clippy::upper_case_acronyms
)]
// An API surface like the windows crate's: what the interface doesn't call yet stays.
#![allow(dead_code)]

mod geometry;
mod render;
mod svg;
mod text;

use std::any::Any;
use std::fmt;
use std::ops::Mul;

pub use geometry::*;
pub use render::*;
pub use svg::*;
pub use text::*;

/// An HRESULT, kept for `Error::from(E_FAIL)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HRESULT(pub i32);

pub const E_FAIL: HRESULT = HRESULT(0x8000_4005_u32 as i32);
pub const E_INVALIDARG: HRESULT = HRESULT(0x8007_0057_u32 as i32);

/// Replaces `windows::core::Error`: a code and what went wrong.
#[derive(Clone, PartialEq, Eq)]
pub struct Error {
    code: HRESULT,
    message: String,
}

impl Error {
    pub fn new(code: HRESULT, message: impl Into<String>) -> Self {
        Error {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> HRESULT {
        self.code
    }

    pub fn message(&self) -> String {
        self.message.clone()
    }

    pub(crate) fn fail(message: impl Into<String>) -> Self {
        Error::new(E_FAIL, message)
    }
}

impl From<HRESULT> for Error {
    fn from(code: HRESULT) -> Self {
        Error {
            code,
            message: String::new(),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error({:#010X}, {:?})", self.code.0 as u32, self.message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "{:#010X}", self.code.0 as u32)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// `windows::core::Interface`'s `cast`: between the views of one object (a path geometry as
/// an `ID2D1Geometry`, a device context as its render target).
pub trait Interface: Clone + 'static {
    /// The object seen as `T`, if it is one.
    fn view(&self, want: std::any::TypeId) -> Option<Box<dyn Any>> {
        (want == std::any::TypeId::of::<Self>()).then(|| Box::new(self.clone()) as Box<dyn Any>)
    }

    fn cast<T: Interface>(&self) -> Result<T> {
        self.view(std::any::TypeId::of::<T>())
            .and_then(|b| b.downcast::<T>().ok())
            .map(|b| *b)
            .ok_or_else(|| Error::new(E_FAIL, "no such interface"))
    }
}

/// `windows_numerics::Vector2`.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Vector2 {
    pub X: f32,
    pub Y: f32,
}

impl Vector2 {
    pub const fn new(X: f32, Y: f32) -> Self {
        Vector2 { X, Y }
    }
}

/// `windows_numerics::Matrix3x2`: row vectors, so `a * b` is a then b.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Matrix3x2 {
    pub M11: f32,
    pub M12: f32,
    pub M21: f32,
    pub M22: f32,
    pub M31: f32,
    pub M32: f32,
}

impl Matrix3x2 {
    pub const fn identity() -> Self {
        Matrix3x2 {
            M11: 1.0,
            M12: 0.0,
            M21: 0.0,
            M22: 1.0,
            M31: 0.0,
            M32: 0.0,
        }
    }

    pub const fn translation(x: f32, y: f32) -> Self {
        Matrix3x2 {
            M31: x,
            M32: y,
            ..Self::identity()
        }
    }

    pub fn scale(x: f32, y: f32) -> Self {
        Matrix3x2 {
            M11: x,
            M22: y,
            ..Self::identity()
        }
    }

    pub fn transform(&self, x: f32, y: f32) -> (f32, f32) {
        (
            x * self.M11 + y * self.M21 + self.M31,
            x * self.M12 + y * self.M22 + self.M32,
        )
    }

    pub fn is_invertible(&self) -> bool {
        (self.M11 * self.M22 - self.M12 * self.M21).abs() > f32::EPSILON
    }

    /// No rotation or skew: rectangles stay axis-aligned.
    pub(crate) fn axis_aligned(&self) -> bool {
        self.M12 == 0.0 && self.M21 == 0.0
    }

    pub(crate) fn cg(&self) -> objc2_core_foundation::CGAffineTransform {
        objc2_core_foundation::CGAffineTransform {
            a: self.M11.into(),
            b: self.M12.into(),
            c: self.M21.into(),
            d: self.M22.into(),
            tx: self.M31.into(),
            ty: self.M32.into(),
        }
    }
}

impl Mul for Matrix3x2 {
    type Output = Matrix3x2;

    fn mul(self, b: Matrix3x2) -> Matrix3x2 {
        let a = self;
        Matrix3x2 {
            M11: a.M11 * b.M11 + a.M12 * b.M21,
            M12: a.M11 * b.M12 + a.M12 * b.M22,
            M21: a.M21 * b.M11 + a.M22 * b.M21,
            M22: a.M21 * b.M12 + a.M22 * b.M22,
            M31: a.M31 * b.M11 + a.M32 * b.M21 + b.M31,
            M32: a.M31 * b.M12 + a.M32 * b.M22 + b.M32,
        }
    }
}

pub type D2D_MATRIX_3X2_F = Matrix3x2;
pub type D2D_POINT_2F = Vector2;

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D_RECT_F {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl D2D_RECT_F {
    /// Left <= right and top <= bottom, as Direct2D treats a flipped rectangle.
    pub(crate) fn normalized(&self) -> D2D_RECT_F {
        D2D_RECT_F {
            left: self.left.min(self.right),
            top: self.top.min(self.bottom),
            right: self.left.max(self.right),
            bottom: self.top.max(self.bottom),
        }
    }

    /// Finite, so a `f32::MIN..f32::MAX` "everything" rectangle survives scaling.
    pub(crate) fn cg(&self) -> objc2_core_foundation::CGRect {
        let r = self.normalized();
        let c = |v: f32| f64::from(v).clamp(-1.0e7, 1.0e7);
        let (l, t, rr, b) = (c(r.left), c(r.top), c(r.right), c(r.bottom));
        objc2_core_foundation::CGRect::new(
            objc2_core_foundation::CGPoint::new(l, t),
            objc2_core_foundation::CGSize::new(rr - l, b - t),
        )
    }
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D_SIZE_F {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct D2D_SIZE_U {
    pub width: u32,
    pub height: u32,
}

/// Straight (not premultiplied) RGBA, 0..1.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D1_COLOR_F {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D1_ROUNDED_RECT {
    pub rect: D2D_RECT_F,
    pub radiusX: f32,
    pub radiusY: f32,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct D2D1_ELLIPSE {
    pub point: Vector2,
    pub radiusX: f32,
    pub radiusY: f32,
}

/// The windows-rs shape of a C enum: a newtype with named constants.
macro_rules! enums {
    ($($ty:ident { $($name:ident = $v:expr),* $(,)? })*) => {
        $(
            #[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
            pub struct $ty(pub i32);
            $(pub const $name: $ty = $ty($v);)*
        )*
    };
}

enums! {
    D2D1_ANTIALIAS_MODE { D2D1_ANTIALIAS_MODE_PER_PRIMITIVE = 0, D2D1_ANTIALIAS_MODE_ALIASED = 1 }
    D2D1_TEXT_ANTIALIAS_MODE {
        D2D1_TEXT_ANTIALIAS_MODE_DEFAULT = 0,
        D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE = 1,
        D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE = 2,
        D2D1_TEXT_ANTIALIAS_MODE_ALIASED = 3,
    }
    D2D1_DRAW_TEXT_OPTIONS {
        D2D1_DRAW_TEXT_OPTIONS_NONE = 0,
        D2D1_DRAW_TEXT_OPTIONS_NO_SNAP = 1,
        D2D1_DRAW_TEXT_OPTIONS_CLIP = 2,
        D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT = 4,
    }
    DWRITE_MEASURING_MODE {
        DWRITE_MEASURING_MODE_NATURAL = 0,
        DWRITE_MEASURING_MODE_GDI_CLASSIC = 1,
        DWRITE_MEASURING_MODE_GDI_NATURAL = 2,
    }
    D2D1_INTERPOLATION_MODE {
        D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR = 0,
        D2D1_INTERPOLATION_MODE_LINEAR = 1,
        D2D1_INTERPOLATION_MODE_CUBIC = 2,
        D2D1_INTERPOLATION_MODE_MULTI_SAMPLE_LINEAR = 3,
        D2D1_INTERPOLATION_MODE_ANISOTROPIC = 4,
        D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC = 5,
    }
    D2D1_BITMAP_INTERPOLATION_MODE {
        D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR = 0,
        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR = 1,
    }
    D2D1_ALPHA_MODE {
        D2D1_ALPHA_MODE_UNKNOWN = 0,
        D2D1_ALPHA_MODE_PREMULTIPLIED = 1,
        D2D1_ALPHA_MODE_STRAIGHT = 2,
        D2D1_ALPHA_MODE_IGNORE = 3,
    }
    DXGI_FORMAT { DXGI_FORMAT_UNKNOWN = 0, DXGI_FORMAT_R8G8B8A8_UNORM = 28, DXGI_FORMAT_B8G8R8A8_UNORM = 87 }
    D2D1_LAYER_OPTIONS { D2D1_LAYER_OPTIONS_NONE = 0, D2D1_LAYER_OPTIONS_INITIALIZE_FOR_CLEARTYPE = 1 }
    D2D1_LAYER_OPTIONS1 {
        D2D1_LAYER_OPTIONS1_NONE = 0,
        D2D1_LAYER_OPTIONS1_INITIALIZE_FROM_BACKGROUND = 1,
        D2D1_LAYER_OPTIONS1_IGNORE_ALPHA = 2,
    }
    D2D1_FIGURE_BEGIN { D2D1_FIGURE_BEGIN_FILLED = 0, D2D1_FIGURE_BEGIN_HOLLOW = 1 }
    D2D1_FIGURE_END { D2D1_FIGURE_END_OPEN = 0, D2D1_FIGURE_END_CLOSED = 1 }
    D2D1_FILL_MODE { D2D1_FILL_MODE_ALTERNATE = 0, D2D1_FILL_MODE_WINDING = 1 }
    D2D1_COMBINE_MODE {
        D2D1_COMBINE_MODE_UNION = 0,
        D2D1_COMBINE_MODE_INTERSECT = 1,
        D2D1_COMBINE_MODE_XOR = 2,
        D2D1_COMBINE_MODE_EXCLUDE = 3,
    }
    D2D1_CAP_STYLE {
        D2D1_CAP_STYLE_FLAT = 0,
        D2D1_CAP_STYLE_SQUARE = 1,
        D2D1_CAP_STYLE_ROUND = 2,
        D2D1_CAP_STYLE_TRIANGLE = 3,
    }
    D2D1_LINE_JOIN {
        D2D1_LINE_JOIN_MITER = 0,
        D2D1_LINE_JOIN_BEVEL = 1,
        D2D1_LINE_JOIN_ROUND = 2,
        D2D1_LINE_JOIN_MITER_OR_BEVEL = 3,
    }
    D2D1_DASH_STYLE {
        D2D1_DASH_STYLE_SOLID = 0,
        D2D1_DASH_STYLE_DASH = 1,
        D2D1_DASH_STYLE_DOT = 2,
        D2D1_DASH_STYLE_DASH_DOT = 3,
        D2D1_DASH_STYLE_DASH_DOT_DOT = 4,
        D2D1_DASH_STYLE_CUSTOM = 5,
    }
    D2D1_STROKE_TRANSFORM_TYPE {
        D2D1_STROKE_TRANSFORM_TYPE_NORMAL = 0,
        D2D1_STROKE_TRANSFORM_TYPE_FIXED = 1,
        D2D1_STROKE_TRANSFORM_TYPE_HAIRLINE = 2,
    }
    D2D1_FACTORY_TYPE { D2D1_FACTORY_TYPE_SINGLE_THREADED = 0, D2D1_FACTORY_TYPE_MULTI_THREADED = 1 }
    D2D1_EXTEND_MODE { D2D1_EXTEND_MODE_CLAMP = 0, D2D1_EXTEND_MODE_WRAP = 1, D2D1_EXTEND_MODE_MIRROR = 2 }
    DWRITE_FACTORY_TYPE { DWRITE_FACTORY_TYPE_SHARED = 0, DWRITE_FACTORY_TYPE_ISOLATED = 1 }
    DWRITE_FONT_WEIGHT {
        DWRITE_FONT_WEIGHT_THIN = 100,
        DWRITE_FONT_WEIGHT_EXTRA_LIGHT = 200,
        DWRITE_FONT_WEIGHT_LIGHT = 300,
        DWRITE_FONT_WEIGHT_SEMI_LIGHT = 350,
        DWRITE_FONT_WEIGHT_NORMAL = 400,
        DWRITE_FONT_WEIGHT_REGULAR = 400,
        DWRITE_FONT_WEIGHT_MEDIUM = 500,
        DWRITE_FONT_WEIGHT_SEMI_BOLD = 600,
        DWRITE_FONT_WEIGHT_BOLD = 700,
        DWRITE_FONT_WEIGHT_EXTRA_BOLD = 800,
        DWRITE_FONT_WEIGHT_BLACK = 900,
    }
    DWRITE_FONT_STYLE { DWRITE_FONT_STYLE_NORMAL = 0, DWRITE_FONT_STYLE_OBLIQUE = 1, DWRITE_FONT_STYLE_ITALIC = 2 }
    DWRITE_FONT_STRETCH { DWRITE_FONT_STRETCH_UNDEFINED = 0, DWRITE_FONT_STRETCH_NORMAL = 5 }
    DWRITE_FONT_FAMILY_MODEL { DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC = 0, DWRITE_FONT_FAMILY_MODEL_WEIGHT_STRETCH_STYLE = 1 }
    DWRITE_TEXT_ALIGNMENT {
        DWRITE_TEXT_ALIGNMENT_LEADING = 0,
        DWRITE_TEXT_ALIGNMENT_TRAILING = 1,
        DWRITE_TEXT_ALIGNMENT_CENTER = 2,
        DWRITE_TEXT_ALIGNMENT_JUSTIFIED = 3,
    }
    DWRITE_PARAGRAPH_ALIGNMENT {
        DWRITE_PARAGRAPH_ALIGNMENT_NEAR = 0,
        DWRITE_PARAGRAPH_ALIGNMENT_FAR = 1,
        DWRITE_PARAGRAPH_ALIGNMENT_CENTER = 2,
    }
    DWRITE_WORD_WRAPPING {
        DWRITE_WORD_WRAPPING_WRAP = 0,
        DWRITE_WORD_WRAPPING_NO_WRAP = 1,
        DWRITE_WORD_WRAPPING_EMERGENCY_BREAK = 2,
        DWRITE_WORD_WRAPPING_WHOLE_WORD = 3,
        DWRITE_WORD_WRAPPING_CHARACTER = 4,
    }
    DWRITE_TRIMMING_GRANULARITY {
        DWRITE_TRIMMING_GRANULARITY_NONE = 0,
        DWRITE_TRIMMING_GRANULARITY_CHARACTER = 1,
        DWRITE_TRIMMING_GRANULARITY_WORD = 2,
    }
    DWRITE_LINE_SPACING_METHOD {
        DWRITE_LINE_SPACING_METHOD_DEFAULT = 0,
        DWRITE_LINE_SPACING_METHOD_UNIFORM = 1,
        DWRITE_LINE_SPACING_METHOD_PROPORTIONAL = 2,
    }
}

/// `windows::core::w!`: a wide string literal; here the literal itself.
macro_rules! w {
    ($s:literal) => {
        $crate::d2d::PCWSTR($s)
    };
}
pub(crate) use w;

/// A string constant from `w!`, and the cursor ids `LoadCursorW` takes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PCWSTR(pub &'static str);

pub const IDC_ARROW: PCWSTR = PCWSTR("IDC_ARROW");
pub const IDC_IBEAM: PCWSTR = PCWSTR("IDC_IBEAM");
pub const IDC_HAND: PCWSTR = PCWSTR("IDC_HAND");

/// `windows::core::HSTRING`: an owned string handed to a call.
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug)]
pub struct HSTRING(pub String);

impl From<&str> for HSTRING {
    fn from(s: &str) -> Self {
        HSTRING(s.to_owned())
    }
}

impl From<&String> for HSTRING {
    fn from(s: &String) -> Self {
        HSTRING(s.clone())
    }
}

impl From<String> for HSTRING {
    fn from(s: String) -> Self {
        HSTRING(s)
    }
}

impl From<&std::path::Path> for HSTRING {
    fn from(p: &std::path::Path) -> Self {
        HSTRING(p.to_string_lossy().into_owned())
    }
}

/// A string argument: `w!("...")` or `&HSTRING`.
pub trait StrParam {
    fn as_str(&self) -> &str;
}

impl StrParam for PCWSTR {
    fn as_str(&self) -> &str {
        self.0
    }
}

impl StrParam for &HSTRING {
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// A Windows virtual-key code. The shell translates macOS key codes into these.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub struct VIRTUAL_KEY(pub u16);

macro_rules! keys {
    ($($name:ident = $v:expr),* $(,)?) => {
        $(pub const $name: VIRTUAL_KEY = VIRTUAL_KEY($v);)*
    };
}

keys! {
    VK_BACK = 0x08, VK_TAB = 0x09, VK_RETURN = 0x0D, VK_SHIFT = 0x10, VK_CONTROL = 0x11,
    VK_MENU = 0x12, VK_ESCAPE = 0x1B, VK_SPACE = 0x20, VK_PRIOR = 0x21, VK_NEXT = 0x22,
    VK_END = 0x23, VK_HOME = 0x24, VK_LEFT = 0x25, VK_UP = 0x26, VK_RIGHT = 0x27,
    VK_DOWN = 0x28, VK_INSERT = 0x2D, VK_DELETE = 0x2E,
    VK_0 = 0x30, VK_1 = 0x31, VK_2 = 0x32, VK_3 = 0x33, VK_4 = 0x34,
    VK_5 = 0x35, VK_6 = 0x36, VK_7 = 0x37, VK_8 = 0x38, VK_9 = 0x39,
    VK_A = 0x41, VK_B = 0x42, VK_C = 0x43, VK_D = 0x44, VK_E = 0x45, VK_F = 0x46, VK_G = 0x47,
    VK_H = 0x48, VK_I = 0x49, VK_J = 0x4A, VK_K = 0x4B, VK_L = 0x4C, VK_M = 0x4D, VK_N = 0x4E,
    VK_O = 0x4F, VK_P = 0x50, VK_Q = 0x51, VK_R = 0x52, VK_S = 0x53, VK_T = 0x54, VK_U = 0x55,
    VK_V = 0x56, VK_W = 0x57, VK_X = 0x58, VK_Y = 0x59, VK_Z = 0x5A,
    VK_F1 = 0x70, VK_F2 = 0x71, VK_F3 = 0x72, VK_F4 = 0x73, VK_F5 = 0x74, VK_F6 = 0x75,
    VK_F7 = 0x76, VK_F8 = 0x77, VK_F9 = 0x78, VK_F10 = 0x79, VK_F11 = 0x7A, VK_F12 = 0x7B,
    VK_OEM_1 = 0xBA, VK_OEM_PLUS = 0xBB, VK_OEM_COMMA = 0xBC, VK_OEM_MINUS = 0xBD,
    VK_OEM_PERIOD = 0xBE, VK_OEM_2 = 0xBF, VK_OEM_3 = 0xC0, VK_OEM_4 = 0xDB, VK_OEM_5 = 0xDC,
    VK_OEM_6 = 0xDD, VK_OEM_7 = 0xDE,
}

pub const D2D1_DEFAULT_FLATTENING_TOLERANCE: f32 = 0.25;

#[cfg(test)]
mod tests;

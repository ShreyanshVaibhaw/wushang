//! Geometries: rectangles, rounded rectangles and path geometries built with a sink.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGMutablePath, CGPath};

use super::*;

struct Geometry {
    /// The figures that fill: path geometries leave out hollow ones.
    fill: RefCell<Option<CFRetained<CGPath>>>,
    /// Every figure, for strokes.
    stroke: RefCell<Option<CFRetained<CGPath>>>,
    /// Path geometries fill alternate (even-odd) unless the sink says otherwise; the shapes'
    /// rule makes no difference, so they say winding.
    mode: Cell<D2D1_FILL_MODE>,
}

/// Any geometry. Direct2D's geometry interfaces are views of one object here, so
/// `ID2D1PathGeometry1`, `ID2D1RoundedRectangleGeometry` and the rest are this type.
#[derive(Clone)]
pub struct ID2D1Geometry(Rc<Geometry>);
pub type ID2D1PathGeometry1 = ID2D1Geometry;
pub type ID2D1RectangleGeometry = ID2D1Geometry;
pub type ID2D1RoundedRectangleGeometry = ID2D1Geometry;

impl Interface for ID2D1Geometry {}

impl ID2D1Geometry {
    fn shape(path: CFRetained<CGPath>) -> Self {
        ID2D1Geometry(Rc::new(Geometry {
            fill: RefCell::new(Some(path.clone())),
            stroke: RefCell::new(Some(path)),
            mode: Cell::new(D2D1_FILL_MODE_WINDING),
        }))
    }

    pub(crate) fn fill_path(&self) -> Option<(CFRetained<CGPath>, bool)> {
        let path = self.0.fill.borrow().clone()?;
        Some((path, self.0.mode.get() == D2D1_FILL_MODE_ALTERNATE))
    }

    pub(crate) fn stroke_path(&self) -> Option<CFRetained<CGPath>> {
        self.0.stroke.borrow().clone()
    }

    /// Starts (re)defining a path geometry; the figures land when the sink closes.
    pub unsafe fn Open(&self) -> Result<ID2D1GeometrySink> {
        Ok(ID2D1GeometrySink(Rc::new(Sink {
            target: self.clone(),
            fill: CGMutablePath::new(),
            stroke: CGMutablePath::new(),
            filled: Cell::new(true),
        })))
    }

    /// Combines this geometry's area with `input`'s (moved by `transform`) into `sink`.
    pub unsafe fn CombineWithGeometry(
        &self,
        input: &ID2D1Geometry,
        mode: D2D1_COMBINE_MODE,
        transform: Option<&Matrix3x2>,
        _flatteningtolerance: f32,
        sink: &ID2D1GeometrySink,
    ) -> Result<()> {
        let (a, even_odd) = self
            .fill_path()
            .ok_or_else(|| Error::fail("empty geometry"))?;
        let (b, _) = input
            .fill_path()
            .ok_or_else(|| Error::fail("empty geometry"))?;
        let b = match transform {
            Some(m) => unsafe { CGPath::new_copy_by_transforming_path(Some(&b), &m.cg()) }
                .ok_or_else(|| Error::fail("transform"))?,
            None => b,
        };
        let out = match mode {
            D2D1_COMBINE_MODE_UNION => {
                CGPath::new_copy_by_unioning_path(Some(&a), Some(&b), even_odd)
            }
            D2D1_COMBINE_MODE_INTERSECT => {
                CGPath::new_copy_by_intersecting_path(Some(&a), Some(&b), even_odd)
            }
            D2D1_COMBINE_MODE_XOR => {
                CGPath::new_copy_by_symmetric_difference_of_path(Some(&a), Some(&b), even_odd)
            }
            _ => CGPath::new_copy_by_subtracting_path(Some(&a), Some(&b), even_odd),
        }
        .ok_or_else(|| Error::fail("combine"))?;
        sink.add_path(&out);
        Ok(())
    }
}

impl ID2D1Factory1 {
    pub unsafe fn CreatePathGeometry(&self) -> Result<ID2D1PathGeometry1> {
        Ok(ID2D1Geometry(Rc::new(Geometry {
            fill: RefCell::new(None),
            stroke: RefCell::new(None),
            mode: Cell::new(D2D1_FILL_MODE_ALTERNATE),
        })))
    }

    pub unsafe fn CreateRectangleGeometry(
        &self,
        rect: &D2D_RECT_F,
    ) -> Result<ID2D1RectangleGeometry> {
        let path = unsafe { CGPath::with_rect(rect.cg(), std::ptr::null()) };
        Ok(ID2D1Geometry::shape(path))
    }

    pub unsafe fn CreateRoundedRectangleGeometry(
        &self,
        roundedrectangle: &D2D1_ROUNDED_RECT,
    ) -> Result<ID2D1RoundedRectangleGeometry> {
        let path = rounded_path(roundedrectangle).unwrap_or_else(|| unsafe {
            CGPath::with_rect(roundedrectangle.rect.cg(), std::ptr::null())
        });
        Ok(ID2D1Geometry::shape(path))
    }
}

/// The Win32 BOOL.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct BOOL(pub i32);

#[cfg(test)]
impl BOOL {
    pub fn as_bool(self) -> bool {
        self.0 != 0
    }
}

impl From<bool> for BOOL {
    fn from(b: bool) -> Self {
        BOOL(i32::from(b))
    }
}

struct Sink {
    target: ID2D1Geometry,
    fill: CFRetained<CGMutablePath>,
    stroke: CFRetained<CGMutablePath>,
    /// The open figure fills.
    filled: Cell<bool>,
}

/// Collects a path geometry's figures until `Close`.
#[derive(Clone)]
pub struct ID2D1GeometrySink(Rc<Sink>);

impl ID2D1GeometrySink {
    /// Runs `f` on the stroke path, and on the fill path for a filled figure.
    fn each(&self, f: impl Fn(&CGMutablePath)) {
        f(&self.0.stroke);
        if self.0.filled.get() {
            f(&self.0.fill);
        }
    }

    fn add_path(&self, path: &CGPath) {
        unsafe {
            CGMutablePath::add_path(Some(&self.0.fill), std::ptr::null(), Some(path));
            CGMutablePath::add_path(Some(&self.0.stroke), std::ptr::null(), Some(path));
        }
    }

    pub unsafe fn BeginFigure(&self, startpoint: Vector2, figurebegin: D2D1_FIGURE_BEGIN) {
        self.0.filled.set(figurebegin == D2D1_FIGURE_BEGIN_FILLED);
        self.each(|p| unsafe {
            CGMutablePath::move_to_point(
                Some(p),
                std::ptr::null(),
                startpoint.X.into(),
                startpoint.Y.into(),
            )
        });
    }

    pub unsafe fn AddLine(&self, point: Vector2) {
        self.each(|p| unsafe {
            CGMutablePath::add_line_to_point(
                Some(p),
                std::ptr::null(),
                point.X.into(),
                point.Y.into(),
            )
        });
    }

    pub unsafe fn EndFigure(&self, figureend: D2D1_FIGURE_END) {
        if figureend == D2D1_FIGURE_END_CLOSED {
            self.each(|p| CGMutablePath::close_subpath(Some(p)));
        }
        self.0.filled.set(true);
    }

    /// Hands the figures to the geometry.
    pub unsafe fn Close(&self) -> Result<()> {
        let g = &self.0.target.0;
        let copy = |p: &CGMutablePath| CGPath::new_copy(Some(p));
        *g.fill.borrow_mut() = copy(&self.0.fill);
        *g.stroke.borrow_mut() = copy(&self.0.stroke);
        g.mode.set(D2D1_FILL_MODE_ALTERNATE);
        Ok(())
    }
}

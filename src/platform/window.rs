//! The one window: titled for AppKit's resizing, rounded corners, shadow, fullscreen and
//! Mission Control, with its title bar and buttons hidden because the design draws its own.

use std::cell::{Cell, OnceCell};

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSColor, NSFloatingWindowLevel, NSNormalWindowLevel,
    NSScreen, NSWindow, NSWindowButton, NSWindowCollectionBehavior, NSWindowStyleMask,
    NSWindowTabbingMode, NSWindowTitleVisibility,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSUserDefaults};
use objc2_quartz_core::CAMetalLayer;

use super::app::{self, mtm};
use super::view::View;

/// Where the window is in AppKit's native fullscreen, which animates in and out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fullscreen {
    Off,
    Entering,
    On,
    Leaving,
}

/// A rectangle in points with Windows' orientation: from the main screen's top-left corner,
/// y growing down. AppKit's own run from the bottom left.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
}

thread_local! {
    static WINDOW: OnceCell<Retained<NSWindow>> = const { OnceCell::new() };
    static VIEW: OnceCell<Retained<View>> = const { OnceCell::new() };
    static FULLSCREEN: Cell<Fullscreen> = const { Cell::new(Fullscreen::Off) };
    /// The frame last seen while the window was neither zoomed nor fullscreen: what it
    /// restores to, which Windows keeps as the placement's normal rect.
    static NORMAL: Cell<Option<Rect>> = const { Cell::new(None) };
}

/// The main screen's height, which flips AppKit's y into ours and back.
fn flip_height() -> f64 {
    let screens = NSScreen::screens(mtm());
    screens.firstObject().map_or(0.0, |s| s.frame().size.height)
}

fn from_ns(r: NSRect) -> Rect {
    Rect {
        x: r.origin.x,
        y: flip_height() - (r.origin.y + r.size.height),
        w: r.size.width,
        h: r.size.height,
    }
}

fn to_ns(r: Rect) -> NSRect {
    NSRect::new(
        NSPoint::new(r.x, flip_height() - r.y - r.h),
        NSSize::new(r.w, r.h),
    )
}

/// The main screen's size in points, and its pixels per point.
pub fn main_screen() -> (f64, f64, f64) {
    NSScreen::mainScreen(mtm()).map_or((1440.0, 900.0, 2.0), |s| {
        let f = s.frame();
        (f.size.width, f.size.height, s.backingScaleFactor())
    })
}

/// Creates the window, hidden, with its content `size` and `min` in points; `show` puts it on
/// screen once the renderer is ready.
pub fn create_window(title: &str, size: (f64, f64), min: (f64, f64)) -> Window {
    let mtm = mtm();
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView;
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(size.0, size.1));
    // SAFETY: the window is kept below, so it isn't released on close.
    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window
    };
    window.setTitle(&NSString::from_str(title));
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    for button in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(b) = window.standardWindowButton(button) {
            b.setHidden(true);
        }
    }
    window.setBackgroundColor(Some(&NSColor::blackColor()));
    window.setContentMinSize(NSSize::new(min.0, min.1));
    window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
    window.setTabbingMode(NSWindowTabbingMode::Disallowed);
    // wushang puts the window back itself, from its settings.
    window.setRestorable(false);
    window.setAcceptsMouseMovedEvents(true);
    window.setDelegate(Some(&app::window_delegate()));
    let view = View::new(mtm, rect);
    window.setContentView(Some(&view));
    window.makeFirstResponder(Some(&view));
    window.center();
    view.metal_layer()
        .setContentsScale(window.backingScaleFactor());
    WINDOW.with(|w| _ = w.set(window));
    VIEW.with(|v| _ = v.set(view));
    note_frame();
    Window
}

/// For the debug builds' scripted input.
#[cfg(debug_assertions)]
pub(super) fn ns() -> Retained<NSWindow> {
    window()
}

fn window() -> Retained<NSWindow> {
    WINDOW
        .with(|w| w.get().cloned())
        .expect("the window exists")
}

pub(super) fn set_fullscreen(state: Fullscreen) {
    FULLSCREEN.set(state);
}

/// Keeps the restored frame up to date as the window moves and resizes.
pub(super) fn note_frame() {
    let Some(w) = WINDOW.with(|w| w.get().cloned()) else {
        return;
    };
    if FULLSCREEN.get() == Fullscreen::Off && !w.isZoomed() && !w.isMiniaturized() {
        NORMAL.set(Some(from_ns(w.frame())));
    }
}

/// The screen's scale changed (the window moved to another screen): the layer follows.
pub(super) fn backing_changed() {
    let Some(w) = WINDOW.with(|w| w.get().cloned()) else {
        return;
    };
    if let Some(v) = VIEW.with(|v| v.get().cloned()) {
        v.metal_layer().setContentsScale(w.backingScaleFactor());
    }
}

/// Gives the keyboard back to the window, once a file picker is gone.
pub(super) fn refocus() {
    if let Some(w) = WINDOW.with(|w| w.get().cloned())
        && w.isVisible()
        && !w.isMiniaturized()
    {
        w.makeKeyWindow();
    }
}

pub(super) fn deminiaturize() {
    if let Some(w) = WINDOW.with(|w| w.get().cloned()) {
        if w.isMiniaturized() {
            w.deminiaturize(None);
        }
        w.makeKeyAndOrderFront(None);
    }
}

/// The window, for `main.rs`; like an HWND it is a plain handle to copy around.
#[derive(Clone, Copy, Debug)]
pub struct Window;

impl Window {
    pub fn ns_window(self) -> Retained<NSWindow> {
        window()
    }

    /// The layer the renderer draws into.
    pub fn layer(self) -> Retained<CAMetalLayer> {
        VIEW.with(|v| v.get().expect("the window exists").metal_layer())
    }

    /// Pixels per point.
    pub fn scale(self) -> f64 {
        window().backingScaleFactor()
    }

    /// The view's size in points (the layout's DIPs).
    pub fn size(self) -> (f64, f64) {
        let b = VIEW.with(|v| v.get().expect("the window exists").bounds());
        (b.size.width, b.size.height)
    }

    /// The view's size in pixels.
    pub fn size_px(self) -> (u32, u32) {
        let (w, h) = self.size();
        let s = self.scale();
        ((w * s).round() as u32, (h * s).round() as u32)
    }

    pub fn set_title(self, title: &str) {
        window().setTitle(&NSString::from_str(title));
    }

    /// Puts the window on screen and in front, or (launched hidden) only in the window list.
    pub fn show(self, activate: bool) {
        let w = window();
        let app = NSApplication::sharedApplication(mtm());
        if activate {
            w.makeKeyAndOrderFront(None);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        } else {
            w.orderBack(None);
        }
    }

    pub fn minimize(self) {
        window().miniaturize(None);
    }

    pub fn is_zoomed(self) -> bool {
        window().isZoomed()
    }

    /// Maximize and restore: AppKit's zoom, the green button's other half.
    pub fn zoom(self) {
        window().zoom(None);
    }

    /// A double-click on the title bar does what System Settings say: zoom, minimize, or
    /// nothing.
    pub fn title_double_click(self) {
        let action = NSUserDefaults::standardUserDefaults()
            .stringForKey(&NSString::from_str("AppleActionOnDoubleClick"))
            .map(|s| s.to_string());
        match action.as_deref() {
            Some("Minimize") => self.minimize(),
            Some("None") => {}
            _ => self.zoom(),
        }
    }

    pub fn fullscreen(self) -> Fullscreen {
        FULLSCREEN.get()
    }

    /// In fullscreen or on the way there.
    pub fn is_fullscreen(self) -> bool {
        matches!(FULLSCREEN.get(), Fullscreen::On | Fullscreen::Entering)
    }

    /// Native fullscreen in or out; asked again mid-animation, it waits for the end.
    pub fn toggle_fullscreen(self) {
        if matches!(FULLSCREEN.get(), Fullscreen::On | Fullscreen::Off) {
            window().toggleFullScreen(None);
        }
    }

    /// Above other windows ("Keep window above other windows", the mini player), or not.
    pub fn set_floating(self, on: bool) {
        let level = if on {
            NSFloatingWindowLevel
        } else {
            NSNormalWindowLevel
        };
        window().setLevel(level);
    }

    /// The window's frame.
    pub fn frame(self) -> Rect {
        from_ns(window().frame())
    }

    pub fn set_frame(self, r: Rect) {
        window().setFrame_display(to_ns(r), true);
    }

    /// The frame to restore to, and whether the window is zoomed: Windows' placement.
    pub fn placement(self) -> (Rect, bool) {
        let zoomed = self.is_zoomed();
        (NORMAL.get().unwrap_or_else(|| self.frame()), zoomed)
    }

    /// The window's screen less the menu bar and the Dock: Windows' work area.
    pub fn work_area(self) -> Rect {
        let w = window();
        let screen = w.screen().or_else(|| NSScreen::mainScreen(mtm()));
        screen.map_or_else(|| self.frame(), |s| from_ns(s.visibleFrame()))
    }

    /// Whether any part of `r` is on a screen.
    pub fn on_a_screen(r: Rect) -> bool {
        NSScreen::screens(mtm())
            .iter()
            .any(|s| from_ns(s.frame()).intersects(&r))
    }

    /// The smallest the window may be made, in points.
    pub fn set_min_size(self, w: f64, h: f64) {
        window().setContentMinSize(NSSize::new(w, h));
    }

    /// Moves the window with the pointer, from the mouse down being handled.
    pub fn drag(self) {
        let app = NSApplication::sharedApplication(mtm());
        if let Some(event) = app.currentEvent() {
            window().performWindowDragWithEvent(&event);
        }
    }

    /// Closes the window for good; the pump then reports it gone.
    pub fn close(self) {
        app::CLOSED.set(true);
        window().orderOut(None);
    }
}

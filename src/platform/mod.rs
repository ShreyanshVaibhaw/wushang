//! The macOS side of the window: AppKit's application, the window and its view, the menu bar,
//! and the system services the rest of wushang asks for (Finder, file pickers, the clipboard,
//! keeping the screen awake).
//!
//! Everything here runs on the main thread. AppKit calls back into the view and the delegates,
//! which turn what happened into an [`Event`] for the handler `main.rs` registers, in the
//! shape of the window procedure it had on Windows.

mod app;
pub mod keys;
#[cfg(debug_assertions)]
mod remote;
mod services;
mod view;
mod window;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

pub use app::{
    Command, autoreleasepool, end_pump, finish, init, launched_hidden, pump_messages,
    raise_thread_priority, set_menu_enabled, set_modal_tick, wait_for_event,
};
pub use keys::VIRTUAL_KEY;
pub use services::*;
pub use window::{Fullscreen, Rect, Window, create_window, main_screen};

/// What the window and the application tell `main.rs`. Positions are in DIPs (AppKit's points)
/// from the view's top-left corner.
#[derive(Debug)]
pub enum Event {
    MouseMove {
        x: f32,
        y: f32,
    },
    /// The pointer left the view (WM_MOUSELEAVE); while a button is held it waits for the
    /// release, as Windows' mouse capture does.
    MouseLeave,
    /// The left button went down; `clicks` counts a double-click as 2.
    MouseDown {
        x: f32,
        y: f32,
        clicks: isize,
    },
    MouseUp {
        x: f32,
        y: f32,
    },
    /// A right-click (or a Control-click) let go.
    RightUp {
        x: f32,
        y: f32,
    },
    /// The wheel or a trackpad: `notches` is a Windows wheel's 120 units, fractional for smooth
    /// scrolling, in the scrolling direction the system set. `steps` are whole notches for
    /// things that move by steps (a list row) and `physical` whole notches in the direction the
    /// fingers or wheel went, for volume and seeking. Both are 0 until enough has built up.
    Wheel {
        x: f32,
        y: f32,
        notches: f32,
        steps: f32,
        physical: f32,
    },
    /// A key went down (WM_KEYDOWN), with Command or Control as `ctrl`.
    Key {
        key: VIRTUAL_KEY,
        shift: bool,
        ctrl: bool,
        alt: bool,
    },
    /// A typed character (WM_CHAR): text, and Backspace as 8, Enter as 13, Esc as 27.
    Char(u32),
    Resized,
    /// The window changed screens, or a screen changed (resolution, HDR, arrangement).
    DisplayChanged,
    /// The system switched between light and dark.
    AppearanceChanged,
    /// Files dropped on the window.
    Drop(Vec<PathBuf>),
    /// Files or addresses opened from Finder, "Open With" or the Dock.
    Open(Vec<PathBuf>),
    Menu(Command),
    /// Quit was asked for from outside the window (the Dock, the system).
    Close,
}

thread_local! {
    static HANDLER: Cell<Option<fn(Event)>> = const { Cell::new(None) };
    /// Files opened before the window existed, for `init` to hand back.
    static EARLY_OPEN: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    /// Quit was asked for before the window existed; the event pump passes it on.
    static EARLY_CLOSE: Cell<bool> = const { Cell::new(false) };
}

/// Where events go from now on.
pub fn set_handler(handler: fn(Event)) {
    HANDLER.set(Some(handler));
}

fn dispatch(event: Event) {
    match (HANDLER.get(), event) {
        (Some(handler), event) => handler(event),
        (None, Event::Open(paths)) => EARLY_OPEN.with_borrow_mut(|o| o.extend(paths)),
        (None, Event::Close) => EARLY_CLOSE.set(true),
        (None, _) => {}
    }
}

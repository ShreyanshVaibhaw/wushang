//! Debug builds only: input from a test script, for driving the app without the real mouse and
//! keyboard. With `WUSHANG_REMOTE=<fifo>` set, each line written to the FIFO becomes an NSEvent
//! that goes through AppKit's own dispatch, as a real one would:
//!
//! - `key <code> [cmd,shift,alt,ctrl] [chars]`: a key down and up, by macOS key code
//! - `move <x> <y>`, `down <x> <y> [clicks]`, `up <x> <y> [clicks]`, `click <x> <y> [clicks]`,
//!   `rclick <x> <y>`, `drag <x0> <y0> <x1> <y1>`: the left or right button, in the view's
//!   points from its top left
//! - `scroll <x> <y> <dy> [px]`: wheel lines, or a trackpad's pixels
//! - `frame <x> <y> <w> <h>`: moves and sizes the window (top-left points)
//! - `menu <n>`: the menu bar's command `n` (1 = About ...), `open <path>`, `drop <path>`
//! - `cancel`: closes the open file picker; `terminate`: quits as the Dock does
//! - `menukey <code> <chars>`: a Command shortcut given to the menu bar alone
//! - `track <secs>`: holds a menu open (AppKit's own loop); `appearance dark|light|system`

use std::collections::VecDeque;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Mutex;

use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
use objc2_core_graphics::{CGEvent, CGScrollEventUnit};
use objc2_foundation::{NSPoint, NSString};

use super::app::mtm;
use super::window::{self, Rect, Window};
use super::{Command, Event, dispatch};

static LINES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Starts reading the FIFO named by WUSHANG_REMOTE, if set.
pub(super) fn start() {
    let Some(path) = std::env::var_os("WUSHANG_REMOTE") else {
        return;
    };
    std::thread::spawn(move || {
        loop {
            // Opening blocks until a writer comes; its end of file lets the next one in.
            let Ok(file) = std::fs::File::open(&path) else {
                return;
            };
            for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
                LINES.lock().unwrap().push_back(line);
            }
        }
    });
}

/// Plays the lines that came in, from the event pump.
pub(super) fn run() {
    loop {
        let Some(line) = LINES.lock().unwrap().pop_front() else {
            return;
        };
        if let Err(e) = command(&line) {
            eprintln!("remote: {line:?}: {e}");
        }
    }
}

fn flags(s: &str) -> NSEventModifierFlags {
    let mut f = NSEventModifierFlags::empty();
    for part in s.split(',') {
        f |= match part {
            "cmd" => NSEventModifierFlags::Command,
            "shift" => NSEventModifierFlags::Shift,
            "alt" => NSEventModifierFlags::Option,
            "ctrl" => NSEventModifierFlags::Control,
            _ => NSEventModifierFlags::empty(),
        };
    }
    f
}

fn command(line: &str) -> Result<(), String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let num = |i: usize| -> Result<f64, String> {
        words
            .get(i)
            .ok_or("missing number")?
            .parse::<f64>()
            .map_err(|e| e.to_string())
    };
    let app = NSApplication::sharedApplication(mtm());
    let send = |e: Option<objc2::rc::Retained<NSEvent>>| match e {
        Some(e) => {
            app.sendEvent(&e);
            Ok(())
        }
        None => Err("AppKit made no event".to_owned()),
    };
    match words.first().copied().unwrap_or("") {
        "key" => {
            let code = num(1)? as u16;
            let mods = words
                .get(2)
                .map_or(NSEventModifierFlags::empty(), |s| flags(s));
            // What a real key event carries for keys that type no letter.
            let chars = match (words.get(3).copied(), code) {
                (Some(c), _) => c,
                (None, 51) => "\u{7f}",
                (None, 36) => "\r",
                (None, 53) => "\u{1b}",
                (None, 48) => "\t",
                (None, 49) => " ",
                (None, _) => "",
            };
            for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
                send(key_event(kind, mods, chars, code))?;
            }
            Ok(())
        }
        // A Command shortcut offered to the menu bar alone, as when it gets the key first.
        "menukey" => {
            let chars = words.get(2).copied().unwrap_or("");
            let event = key_event(
                NSEventType::KeyDown,
                NSEventModifierFlags::Command,
                chars,
                num(1)? as u16,
            )
            .ok_or("AppKit made no event")?;
            let menu = app.mainMenu().ok_or("no menu bar")?;
            let took = menu.performKeyEquivalent(&event);
            eprintln!("remote: menu bar took {chars:?}: {took}");
            Ok(())
        }
        "move" => send(mouse(NSEventType::MouseMoved, num(1)?, num(2)?, 0)),
        "down" => send(mouse(
            NSEventType::LeftMouseDown,
            num(1)?,
            num(2)?,
            num(3).unwrap_or(1.0) as isize,
        )),
        "up" => send(mouse(
            NSEventType::LeftMouseUp,
            num(1)?,
            num(2)?,
            num(3).unwrap_or(1.0) as isize,
        )),
        "click" => {
            let (x, y) = (num(1)?, num(2)?);
            for n in 1..=num(3).unwrap_or(1.0) as isize {
                send(mouse(NSEventType::LeftMouseDown, x, y, n))?;
                send(mouse(NSEventType::LeftMouseUp, x, y, n))?;
            }
            Ok(())
        }
        "rclick" => {
            let (x, y) = (num(1)?, num(2)?);
            send(mouse(NSEventType::RightMouseDown, x, y, 1))?;
            send(mouse(NSEventType::RightMouseUp, x, y, 1))
        }
        "drag" => {
            let (x0, y0, x1, y1) = (num(1)?, num(2)?, num(3)?, num(4)?);
            send(mouse(NSEventType::LeftMouseDown, x0, y0, 1))?;
            for i in 1..=20 {
                let t = f64::from(i) / 20.0;
                let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
                send(mouse(NSEventType::LeftMouseDragged, x, y, 1))?;
            }
            send(mouse(NSEventType::LeftMouseUp, x1, y1, 1))
        }
        "scroll" => {
            let (x, y, dy) = (num(1)?, num(2)?, num(3)? as i32);
            let unit = if words.get(4) == Some(&"px") {
                CGScrollEventUnit::Pixel
            } else {
                CGScrollEventUnit::Line
            };
            let cg = CGEvent::new_scroll_wheel_event2(None, unit, 1, dy, 0, 0)
                .ok_or("no scroll event")?;
            let f = Window.frame();
            CGEvent::set_location(
                Some(&cg),
                objc2_core_foundation::CGPoint::new(f.x + x, f.y + y),
            );
            send(NSEvent::eventWithCGEvent(&cg))
        }
        "frame" => {
            Window.set_frame(Rect {
                x: num(1)?,
                y: num(2)?,
                w: num(3)?,
                h: num(4)?,
            });
            Ok(())
        }
        "menu" => {
            let n = num(1)? as isize;
            let c = Command::ALL.into_iter().find(|c| *c as isize == n);
            dispatch(Event::Menu(c.ok_or("no such command")?));
            Ok(())
        }
        "open" => {
            dispatch(Event::Open(vec![PathBuf::from(&line[5..])]));
            Ok(())
        }
        "drop" => {
            dispatch(Event::Drop(vec![PathBuf::from(&line[5..])]));
            Ok(())
        }
        // Closes a file picker as Cancel would; its window lives in another process.
        "cancel" => {
            app.abortModal();
            Ok(())
        }
        // Quit as the Dock's menu asks it.
        "terminate" => {
            app.terminate(None);
            Ok(())
        }
        // Holds an open menu for `secs`: AppKit's tracking loop, as a live resize runs one.
        "track" => {
            let secs = num(1)?;
            let menu = objc2_app_kit::NSMenu::new(mtm());
            // SAFETY: an item with no action.
            unsafe {
                menu.addItemWithTitle_action_keyEquivalent(
                    &NSString::from_str("Tracking"),
                    None,
                    &NSString::new(),
                )
            };
            let closing = menu.clone();
            let block =
                block2::RcBlock::new(move |_: std::ptr::NonNull<objc2_foundation::NSTimer>| {
                    closing.cancelTracking();
                });
            // SAFETY: the block is 'static; the mode is AppKit's own.
            unsafe {
                let timer = objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(
                    secs, false, &block,
                );
                objc2_foundation::NSRunLoop::mainRunLoop()
                    .addTimer_forMode(&timer, objc2_app_kit::NSEventTrackingRunLoopMode);
            }
            let view = window::ns().contentView().ok_or("no view")?;
            menu.popUpMenuPositioningItem_atLocation_inView(
                None,
                NSPoint::new(20.0, 20.0),
                Some(&view),
            );
            Ok(())
        }
        // The app's appearance, which "System" theme follows: `appearance dark|light|system`.
        "appearance" => {
            // SAFETY: static AppKit names.
            let name = match words.get(1).copied() {
                Some("dark") => Some(unsafe { objc2_app_kit::NSAppearanceNameDarkAqua }),
                Some("light") => Some(unsafe { objc2_app_kit::NSAppearanceNameAqua }),
                _ => None,
            };
            let appearance = name.and_then(objc2_app_kit::NSAppearance::appearanceNamed);
            app.setAppearance(appearance.as_deref());
            Ok(())
        }
        "" => Ok(()),
        other => Err(format!("unknown command {other}")),
    }
}

fn key_event(
    kind: NSEventType,
    mods: NSEventModifierFlags,
    chars: &str,
    code: u16,
) -> Option<objc2::rc::Retained<NSEvent>> {
    // To the key window: a file picker, while one is open.
    let app = NSApplication::sharedApplication(mtm());
    let w = app.keyWindow().unwrap_or_else(window::ns);
    let chars = NSString::from_str(chars);
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        kind,
        NSPoint::new(0.0, 0.0),
        mods,
        0.0,
        w.windowNumber(),
        None,
        &chars,
        &chars,
        false,
        code,
    )
}

/// A mouse event at (x, y) in the view's top-left points.
fn mouse(kind: NSEventType, x: f64, y: f64, clicks: isize) -> Option<objc2::rc::Retained<NSEvent>> {
    let w = window::ns();
    let (_, h) = Window.size();
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind,
        NSPoint::new(x, h - y),
        NSEventModifierFlags::empty(),
        0.0,
        w.windowNumber(),
        None,
        0,
        clicks,
        1.0,
    )
}

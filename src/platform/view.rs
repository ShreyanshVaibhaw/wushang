//! The window's one view: layer-backed with the CAMetalLayer the renderer draws into, and the
//! window procedure's input side: the mouse, the wheel, keys, typed text and dropped files.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSDragOperation, NSDraggingInfo, NSEvent, NSEventModifierFlags, NSEventPhase,
    NSPasteboardTypeFileURL, NSResponder, NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions,
    NSView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSPoint, NSRange,
    NSRangePointer, NSRect, NSString, NSUInteger, NSURL,
};
use objc2_quartz_core::{CALayer, CAMetalLayer};

use super::{Event, dispatch, keys};

#[derive(Default)]
pub(super) struct Ivars {
    /// The input method's text being composed (dead keys, CJK input), which isn't the
    /// interface's until it is committed.
    marked: RefCell<String>,
    /// What this key down typed, sent once its Event::Key has gone, as WM_CHAR follows
    /// WM_KEYDOWN.
    typed: RefCell<Vec<u32>>,
    /// The input method took this key into its composition.
    composed: Cell<bool>,
    /// A Control-click: it is a right-click, which acts on release.
    control_click: Cell<bool>,
    /// The pointer left during a drag; the leave comes once the button is let go.
    left_while_down: Cell<bool>,
    /// Fractions of a step left over from smooth scrolling, in both directions' terms.
    wheel_steps: Cell<f32>,
    wheel_physical: Cell<f32>,
}

define_class!(
    // SAFETY: NSView may be subclassed, and View has no Drop.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WushangView"]
    #[ivars = Ivars]
    pub(super) struct View;

    impl View {
        /// Top-left origin, as the layout's coordinates are.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        /// A click on the window while another app is active acts too, as on Windows.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _: Option<&NSEvent>) -> bool {
            true
        }

        /// Only the design's title bar moves the window, through performWindowDragWithEvent.
        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool {
            false
        }

        #[unsafe(method_id(makeBackingLayer))]
        fn make_backing_layer(&self) -> Retained<CALayer> {
            let layer = CAMetalLayer::new();
            layer.setNeedsDisplayOnBoundsChange(false);
            Retained::into_super(layer)
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        /// The renderer fills the layer; AppKit has nothing to draw.
        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {}

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.moved(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.moved(event);
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            self.moved(event);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let flags = event.modifierFlags();
            let control = flags.contains(NSEventModifierFlags::Control)
                && !flags.contains(NSEventModifierFlags::Command);
            self.ivars().control_click.set(control);
            if control {
                return;
            }
            let (x, y) = self.point(event);
            dispatch(Event::MouseDown {
                x,
                y,
                clicks: event.clickCount(),
            });
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            if self.ivars().control_click.replace(false) {
                dispatch(Event::RightUp { x, y });
            } else {
                dispatch(Event::MouseUp { x, y });
            }
            self.leave_if_outside(event);
        }

        /// Swallowed, so AppKit opens no context menu of its own; the release acts.
        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _: &NSEvent) {}

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            dispatch(Event::RightUp { x, y });
            self.leave_if_outside(event);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _: &NSEvent) {
            if NSEvent::pressedMouseButtons() != 0 {
                self.ivars().left_while_down.set(true);
            } else {
                dispatch(Event::MouseLeave);
            }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _: &NSEvent) {
            self.ivars().left_while_down.set(false);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let iv = self.ivars();
            let (x, y) = self.point(event);
            let dy = event.scrollingDeltaY() as f32;
            // A trackpad or Magic Mouse reports points, a wheel lines (one per notch).
            let precise = event.hasPreciseScrollingDeltas();
            let notches = if precise { dy / 120.0 } else { dy };
            // A step every 40 points of finger travel: a trackpad's swipe moves a few.
            let unit = if precise { 40.0 } else { 1.0 };
            if event.phase().contains(NSEventPhase::Began) {
                iv.wheel_steps.set(0.0);
                iv.wheel_physical.set(0.0);
            }
            let steps = take_whole(&iv.wheel_steps, dy / unit);
            // Volume and seeking follow the fingers, not the content, and ignore the glide
            // after them, which would overshoot.
            let physical_dy = if event.isDirectionInvertedFromDevice() { -dy } else { dy };
            let physical = if event.momentumPhase() == NSEventPhase::None {
                take_whole(&iv.wheel_physical, physical_dy / unit)
            } else {
                0.0
            };
            dispatch(Event::Wheel {
                x,
                y,
                notches,
                steps,
                physical,
            });
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.key(event);
        }

        /// Command shortcuts reach the window before the menu bar. The ones the interface
        /// knows go through the same path as other keys, so an open dialog still takes them
        /// first, as it takes Ctrl shortcuts on Windows; the rest (Quit, Hide, Settings) are
        /// the menu bar's.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let flags = event.modifierFlags();
            let shortcut =
                flags.intersects(NSEventModifierFlags::Command | NSEventModifierFlags::Control);
            use crate::d2d::*;
            let ours = shortcut && matches!(
                keys::from_event(event),
                Some(
                    VK_O | VK_R | VK_D | VK_N | VK_F | VK_P | VK_M | VK_I | VK_OEM_5 | VK_A
                        | VK_C | VK_V | VK_X | VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_BACK
                        | VK_DELETE | VK_HOME | VK_END | VK_INSERT | VK_RETURN
                )
            );
            if ours {
                self.key(event);
            }
            ours
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn did_change_appearance(&self) {
            dispatch(Event::AppearanceChanged);
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            drag_operation(info)
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            drag_operation(info)
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let paths = dropped_files(info);
            let any = !paths.is_empty();
            if any {
                dispatch(Event::Drop(paths));
            }
            any
        }
    }

    unsafe impl NSObjectProtocol for View {}

    // Typed text comes through here from interpretKeyEvents:, after the input method (dead
    // keys, Chinese and Japanese input) has done its part.
    unsafe impl NSTextInputClient for View {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text(&self, string: &AnyObject, _replacement: NSRange) {
            let iv = self.ivars();
            iv.marked.borrow_mut().clear();
            let text = text_of(string);
            // Function keys arrive as characters in a private range.
            let chars = text.chars().filter(|c| !('\u{F700}'..='\u{F8FF}').contains(c));
            iv.typed.borrow_mut().extend(chars.map(u32::from));
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command(&self, selector: Sel) {
            // What WM_CHAR sends for these keys.
            let c = if selector == sel!(deleteBackward:) {
                8
            } else if selector == sel!(insertNewline:) {
                13
            } else if selector == sel!(cancelOperation:) {
                27
            } else if selector == sel!(insertTab:) {
                9
            } else {
                return;
            };
            self.ivars().typed.borrow_mut().push(c);
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, string: &AnyObject, _selected: NSRange, _replacement: NSRange) {
            *self.ivars().marked.borrow_mut() = text_of(string);
            self.ivars().composed.set(true);
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            self.ivars().marked.borrow_mut().clear();
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(NSNotFound as NSUInteger, 0)
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let len = self.ivars().marked.borrow().encode_utf16().count();
            if len == 0 {
                NSRange::new(NSNotFound as NSUInteger, 0)
            } else {
                NSRange::new(0, len)
            }
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            !self.ivars().marked.borrow().is_empty()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(
            &self,
            _range: NSRange,
            _actual: NSRangePointer,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        /// Where the input method's candidate window goes: under the middle of the window's
        /// top, near where the dialogs' fields are.
        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect(&self, _range: NSRange, _actual: NSRangePointer) -> NSRect {
            let Some(window) = self.window() else {
                return NSRect::ZERO;
            };
            let bounds = self.bounds();
            let at = NSPoint::new(bounds.size.width / 2.0, bounds.size.height / 3.0);
            let in_window = self.convertPoint_toView(at, None);
            let mut rect = NSRect::new(in_window, objc2_foundation::NSSize::new(1.0, 20.0));
            rect = window.convertRectToScreen(rect);
            rect
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index(&self, _point: NSPoint) -> NSUInteger {
            NSNotFound as NSUInteger
        }
    }
);

impl View {
    pub(super) fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars::default());
        // SAFETY: NSView's designated initializer.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setWantsLayer(true);
        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the view owns the area and outlives it; no user info.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(&view),
                None,
            )
        };
        view.addTrackingArea(&area);
        // SAFETY: a static AppKit pasteboard type.
        let types = NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]);
        view.registerForDraggedTypes(&types);
        view
    }

    pub(super) fn metal_layer(&self) -> Retained<CAMetalLayer> {
        let layer = self.layer().expect("the view is layer-backed");
        layer
            .downcast::<CAMetalLayer>()
            .expect("makeBackingLayer made a CAMetalLayer")
    }

    fn moved(&self, event: &NSEvent) {
        let (x, y) = self.point(event);
        dispatch(Event::MouseMove { x, y });
    }

    /// The event's position in the view's points, from the top left.
    fn point(&self, event: &NSEvent) -> (f32, f32) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        (p.x as f32, p.y as f32)
    }

    /// A button let go outside the view, which it was dragged out of: the leave held back.
    fn leave_if_outside(&self, event: &NSEvent) {
        if self.ivars().left_while_down.replace(false) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let b = self.bounds();
            let inside = p.x >= 0.0 && p.y >= 0.0 && p.x < b.size.width && p.y < b.size.height;
            if !inside {
                dispatch(Event::MouseLeave);
            }
        }
    }

    /// A key down: the input method sees it first, then the interface gets the key (unless
    /// it went into a composition), then what it typed.
    fn key(&self, event: &NSEvent) {
        let iv = self.ivars();
        let flags = event.modifierFlags();
        let command = flags.contains(NSEventModifierFlags::Command);
        let control = flags.contains(NSEventModifierFlags::Control);
        let composing = !iv.marked.borrow().is_empty();
        iv.composed.set(false);
        iv.typed.borrow_mut().clear();
        // Shortcuts type nothing, as Ctrl+letters type only control characters on Windows.
        if !command && !control {
            self.interpretKeyEvents(&NSArray::from_slice(&[event]));
        }
        let composed = composing || iv.composed.get() || !iv.marked.borrow().is_empty();
        if !composed && let Some(key) = keys::from_event(event) {
            dispatch(Event::Key {
                key,
                shift: flags.contains(NSEventModifierFlags::Shift),
                ctrl: command || control,
                alt: flags.contains(NSEventModifierFlags::Option),
            });
        }
        for c in iv.typed.take() {
            dispatch(Event::Char(c));
        }
    }
}

/// Adds `add` to the fractions in `acc` and takes out the whole steps.
fn take_whole(acc: &Cell<f32>, add: f32) -> f32 {
    let v = acc.get() + add;
    let whole = v.trunc();
    acc.set(v - whole);
    whole
}

/// An NSString or NSAttributedString's text.
fn text_of(string: &AnyObject) -> String {
    if let Some(s) = string.downcast_ref::<NSString>() {
        s.to_string()
    } else if let Some(s) = string.downcast_ref::<NSAttributedString>() {
        s.string().to_string()
    } else {
        String::new()
    }
}

fn drag_operation(info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
    // SAFETY: a static AppKit pasteboard type.
    let file = unsafe { NSPasteboardTypeFileURL };
    let has_files = info
        .draggingPasteboard()
        .types()
        .is_some_and(|types| types.containsObject(file));
    if has_files {
        NSDragOperation::Copy
    } else {
        NSDragOperation::None
    }
}

/// The dropped files' paths, in order. Finder hands over file reference URLs, which name the
/// file by id; they resolve to paths here.
fn dropped_files(info: &ProtocolObject<dyn NSDraggingInfo>) -> Vec<PathBuf> {
    // SAFETY: a static AppKit pasteboard type.
    let file = unsafe { NSPasteboardTypeFileURL };
    let items = info.draggingPasteboard().pasteboardItems();
    items.map_or_else(Vec::new, |items| {
        items
            .iter()
            .filter_map(|item| {
                let s = item.stringForType(file)?;
                let url = NSURL::URLWithString(&s)?;
                let url = url.filePathURL().unwrap_or(url);
                Some(PathBuf::from(url.path()?.to_string()))
            })
            .collect()
    })
}

//! What wushang asks of the system beyond its window: Finder, the default apps, the file
//! pickers, the clipboard, the pointer, keeping the screen on, and the light or dark setting.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class,
    msg_send, sel,
};
use objc2_app_kit::{
    NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSCursor, NSModalResponseOK,
    NSOpenPanel, NSPasteboard, NSPasteboardTypeString, NSPopUpButton, NSSavePanel, NSSound,
    NSTextAlignment, NSTextField, NSView, NSWorkspace,
};
use objc2_core_foundation::CFString;
use objc2_foundation::{NSArray, NSData, NSObject, NSPoint, NSRect, NSSize, NSString, NSURL};
use objc2_uniform_type_identifiers::UTType;

use super::app::mtm;
use super::window::refocus;

/// Shows `path` selected in a Finder window.
pub fn reveal_in_finder(path: &Path) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_slice(&[&*url]));
}

/// Opens a file in the app macOS has for it, or a web address in the browser.
pub fn open_with_default_app(path_or_url: &Path) {
    let s = path_or_url.to_string_lossy();
    let url = if s.contains("://") {
        NSURL::URLWithString(&NSString::from_str(&s))
    } else {
        Some(NSURL::fileURLWithPath(&NSString::from_str(&s)))
    };
    match url {
        Some(url) if NSWorkspace::sharedWorkspace().openURL(&url) => {}
        _ => eprintln!("wushang: nothing opens {s}"),
    }
}

/// Opens a text file in the user's text editor (TextEdit unless they chose another).
pub fn edit_text_file(path: &Path) {
    if let Err(e) = std::process::Command::new("/usr/bin/open")
        .arg("-t")
        .arg(path)
        .spawn()
    {
        eprintln!("wushang: cannot open {} for editing: {e}", path.display());
    }
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: &CFString,
        level: u32,
        name: &CFString,
        id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

thread_local! {
    static AWAKE: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Keeps the display (and so the system) from sleeping while `on`, as Windows'
/// ES_DISPLAY_REQUIRED does while a video plays.
pub fn keep_awake(on: bool) {
    match (on, AWAKE.get()) {
        (true, None) => {
            let kind = CFString::from_static_str("PreventUserIdleDisplaySleep");
            let reason = CFString::from_static_str("wushang is playing a video");
            let mut id = 0;
            // 255 is kIOPMAssertionLevelOn.
            let done = unsafe { IOPMAssertionCreateWithName(&kind, 255, &reason, &mut id) };
            if done == 0 {
                AWAKE.set(Some(id));
            }
        }
        (false, Some(id)) => {
            unsafe { IOPMAssertionRelease(id) };
            AWAKE.set(None);
        }
        _ => {}
    }
}

/// Whether the system appearance is dark. Off the main thread it answers light.
pub fn system_dark_mode() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let appearance = NSApplication::sharedApplication(mtm).effectiveAppearance();
    // SAFETY: static AppKit names.
    let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    appearance
        .bestMatchFromAppearancesWithNames(&NSArray::from_slice(&[aqua, dark]))
        .is_some_and(|name| &*name == dark)
}

/// The clipboard's text, if it holds any.
pub fn clipboard_text() -> Option<String> {
    // SAFETY: a static AppKit pasteboard type.
    let kind = unsafe { NSPasteboardTypeString };
    let text = NSPasteboard::generalPasteboard().stringForType(kind)?;
    Some(text.to_string())
}

/// Puts `text` on the clipboard.
pub fn set_clipboard_text(text: &str) -> bool {
    // SAFETY: a static AppKit pasteboard type.
    let kind = unsafe { NSPasteboardTypeString };
    let board = NSPasteboard::generalPasteboard();
    board.clearContents();
    board.setString_forType(&NSString::from_str(text), kind)
}

/// The pointer's shapes the interface asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    Arrow,
    /// Over a text field.
    IBeam,
    /// Over a link.
    Hand,
}

thread_local! {
    static CURSOR: Cell<Cursor> = const { Cell::new(Cursor::Arrow) };
    static SOUND: RefCell<Option<Retained<NSSound>>> = const { RefCell::new(None) };
}

/// Sets the pointer's shape, when it changes.
pub fn set_cursor(cursor: Cursor) {
    if CURSOR.replace(cursor) == cursor {
        return;
    }
    let c = match cursor {
        Cursor::Arrow => NSCursor::arrowCursor(),
        Cursor::IBeam => NSCursor::IBeamCursor(),
        Cursor::Hand => NSCursor::pointingHandCursor(),
    };
    c.set();
}

/// Hides the pointer until the mouse moves, as over a playing video once the bars hide.
pub fn hide_cursor_until_moved() {
    NSCursor::setHiddenUntilMouseMoves(true);
}

/// Plays a WAV file held in memory, stopping one still playing.
pub fn play_sound(wav: Vec<u8>) {
    SOUND.with_borrow_mut(|sound| {
        if let Some(old) = sound.take() {
            old.stop();
        }
        let data = NSData::from_vec(wav);
        if let Some(s) = NSSound::initWithData(NSSound::alloc(), &data) {
            s.play();
            *sound = Some(s);
        }
    });
}

/// A file type filter as Windows' pickers had them: a name and "*.a;*.b", or "*.*" for all.
type Filter<'a> = (&'a str, String);

/// The extensions in a "*.a;*.b" spec; None for every file.
fn extensions(spec: &str) -> Option<Vec<String>> {
    let exts: Vec<String> = spec
        .split(';')
        .map(|p| {
            p.trim()
                .trim_start_matches('*')
                .trim_start_matches('.')
                .to_owned()
        })
        .collect();
    if exts.iter().any(|e| e.is_empty() || e == "*") {
        None
    } else {
        Some(exts)
    }
}

/// The content types a filter allows; empty allows everything.
fn content_types(spec: &str) -> Retained<NSArray<UTType>> {
    let types: Vec<Retained<UTType>> = extensions(spec)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| UTType::typeWithFilenameExtension(&NSString::from_str(e)))
        .collect();
    NSArray::from_retained_slice(&types)
}

/// The panel shows only files of `types`, or every file when it's empty. Sent by name, since
/// objc2-app-kit's binding needs a feature this crate doesn't turn on.
fn allow(panel: &NSSavePanel, types: &NSArray<UTType>) {
    // SAFETY: setAllowedContentTypes: takes an array of UTTypes (macOS 11).
    let () = unsafe { msg_send![panel, setAllowedContentTypes: types] };
}

struct FilterIvars {
    panel: Retained<NSSavePanel>,
    types: Vec<Retained<NSArray<UTType>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and FilterChoice has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WushangFilterChoice"]
    #[ivars = FilterIvars]
    struct FilterChoice;

    unsafe impl NSObjectProtocol for FilterChoice {}

    impl FilterChoice {
        /// The file type list under the picker changed: it shows those files.
        #[unsafe(method(filterChanged:))]
        fn filter_changed(&self, sender: &NSPopUpButton) {
            let iv = self.ivars();
            if let Some(types) = usize::try_from(sender.indexOfSelectedItem())
                .ok()
                .and_then(|i| iv.types.get(i))
            {
                allow(&iv.panel, types);
            }
        }
    }
);

/// Gives `panel` the first of `filters`, and a list to switch between them when there are
/// more, as Windows' pickers have under the file name. Returns what must outlive the panel.
fn set_filters(
    mtm: MainThreadMarker,
    panel: &NSSavePanel,
    filters: &[Filter<'_>],
) -> Option<Retained<FilterChoice>> {
    let types: Vec<_> = filters
        .iter()
        .map(|(_, spec)| content_types(spec))
        .collect();
    if let Some(first) = types.first() {
        allow(panel, first);
    }
    if filters.len() < 2 {
        return None;
    }
    let choice = FilterChoice::alloc(mtm).set_ivars(FilterIvars {
        panel: panel.retain(),
        types,
    });
    // SAFETY: NSObject's init.
    let choice: Retained<FilterChoice> = unsafe { msg_send![super(choice), init] };

    let view = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 44.0)),
    );
    let label = NSTextField::labelWithString(&NSString::from_str("Show:"), mtm);
    label.setAlignment(NSTextAlignment::Right);
    label.setFrame(NSRect::new(
        NSPoint::new(20.0, 13.0),
        NSSize::new(70.0, 18.0),
    ));
    let popup = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::new(96.0, 8.0), NSSize::new(270.0, 26.0)),
        false,
    );
    for (name, _) in filters {
        popup.addItemWithTitle(&NSString::from_str(name));
    }
    popup.selectItemAtIndex(0);
    // SAFETY: the target is kept alive until the panel closes, and answers filterChanged:.
    unsafe {
        popup.setTarget(Some(&choice));
        popup.setAction(Some(sel!(filterChanged:)));
    }
    view.addSubview(&label);
    view.addSubview(&popup);
    panel.setAccessoryView(Some(&view));
    Some(choice)
}

fn path_of(url: &NSURL) -> Option<PathBuf> {
    url.path().map(|p| PathBuf::from(p.to_string()))
}

/// The system file picker with (name, "*.a;*.b") filters; empty when cancelled. Modal.
pub fn pick_files(filters: &[(&str, String)], multiple: bool) -> Vec<PathBuf> {
    let mtm = mtm();
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(multiple);
    let _choice = set_filters(mtm, &panel, filters);
    if filters.len() > 1 {
        panel.setAccessoryViewDisclosed(true);
    }
    let answer = panel.runModal();
    refocus();
    if answer != NSModalResponseOK {
        return Vec::new();
    }
    panel.URLs().iter().filter_map(|u| path_of(&u)).collect()
}

/// The system folder picker. Modal.
pub fn pick_folder() -> Option<PathBuf> {
    let panel = NSOpenPanel::openPanel(mtm());
    panel.setCanChooseFiles(false);
    panel.setCanChooseDirectories(true);
    panel.setCanCreateDirectories(true);
    panel.setAllowsMultipleSelection(false);
    let answer = panel.runModal();
    refocus();
    if answer != NSModalResponseOK {
        return None;
    }
    panel.URLs().firstObject().and_then(|u| path_of(&u))
}

/// The system Save dialog, starting at the file name `name`, for files of `filter`. It asks
/// before replacing a file.
pub fn pick_save_file(name: &str, filter: (&str, &str)) -> Option<PathBuf> {
    let mtm = mtm();
    let panel = NSSavePanel::savePanel(mtm);
    let _choice = set_filters(mtm, &panel, &[(filter.0, filter.1.to_owned())]);
    panel.setCanCreateDirectories(true);
    panel.setNameFieldStringValue(&NSString::from_str(name));
    let answer = panel.runModal();
    refocus();
    if answer != NSModalResponseOK {
        return None;
    }
    panel.URL().and_then(|u| path_of(&u))
}

#[cfg(test)]
mod tests {
    use super::extensions;

    #[test]
    fn specs() {
        assert_eq!(
            extensions("*.srt;*.ass"),
            Some(vec!["srt".into(), "ass".into()])
        );
        assert_eq!(extensions("*.*"), None);
        assert_eq!(extensions("*.iso"), Some(vec!["iso".into()]));
    }
}

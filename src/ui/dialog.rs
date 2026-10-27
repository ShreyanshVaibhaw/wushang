//! Modal dialogs: New Playlist and Rename Playlist (the design's "new playlist" frame), and About,
//! Keyboard Shortcuts, Properties and New Profile, which have no frame and borrow its title strip,
//! padding, fonts and colours. A dialog dims the window and takes every click and key while it is open;
//! what it wants done goes out as a [`DialogAction`] for main.rs.
//!
//! Sizes are DIPs from the 713x589 frame. A dialog taller or wider than the window is scaled down
//! as a whole, so all of it stays reachable on small windows.

use std::ops::Range;
use std::path::{Path, PathBuf};

use super::*;
use crate::net::NetKind;
use windows::Win32::Foundation::{GENERIC_READ, GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{IDC_HAND, IDC_IBEAM};
use windows::core::{HSTRING, Interface, PCWSTR};
use windows_numerics::Vector2;

/// What main.rs can open.
#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    /// Starts with `files` in its list, for "New playlist from selection".
    NewPlaylist {
        files: Vec<PathBuf>,
    },
    /// The playlist's current name.
    RenamePlaylist(String),
    About,
    Shortcuts,
    Properties(PathBuf),
    /// Asks before deleting the named playlist.
    DeletePlaylist(String),
    /// Names a new settings profile.
    NewProfile,
    /// Asks for a stream to play, or a podcast feed to add.
    Address(NetKind),
}

/// What the open dialog asks main.rs to do.
#[derive(Clone, Debug, PartialEq)]
pub enum DialogAction {
    /// Show the media file picker and pass the files to [`Ui::dialog_add_files`].
    PickFiles,
    /// Show the image picker and pass the image to [`Ui::dialog_set_thumbnail`].
    PickThumbnail,
    /// Create the playlist; on success close the dialog, else report with [`Ui::dialog_error`].
    CreatePlaylist {
        name: String,
        thumbnail: Option<PathBuf>,
        files: Vec<PathBuf>,
    },
    RenamePlaylist {
        from: String,
        to: String,
        thumbnail: Option<PathBuf>,
    },
    OpenLink(&'static str),
    /// The user confirmed deleting the playlist.
    DeletePlaylist(String),
    /// Add the profile and switch to it; on success close the dialog, else report with
    /// [`Ui::dialog_error`].
    CreateProfile(String),
    /// Play the stream or add the feed at the typed address; on success close the dialog, else
    /// report with [`Ui::dialog_error`].
    OpenAddress(NetKind, String),
}

/// What a click lands on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// The dialog outside its controls, or the dimmed window around it.
    Body,
    Close,
    Field,
    Thumbnail,
    /// The drop box and its "+": pick files.
    Files,
    RemoveFile(usize),
    Cancel,
    /// Save, or Close in the dialogs that only show things.
    Primary,
    CopyPath,
    Link(usize),
}

const W: f32 = 713.0;
const TITLE_H: f32 = 50.0;
const PAD: f32 = 20.0;
const RADIUS: f32 = 8.0;
const BUTTON_W: f32 = 98.0;
const BUTTON_H: f32 = 32.0;
/// From the content's bottom to the footer buttons' top.
const FOOTER_GAP: f32 = 43.0;
const THUMB: f32 = 108.0;
const FILE_ROW: f32 = 32.0;
/// File rows shown under the drop box; more scroll.
const FILE_ROWS: usize = 4;
const TABLE_ROW: f32 = 30.0;
/// Longest playlist name, in characters.
const MAX_NAME: usize = 120;
/// The caret's blink half-period.
const BLINK_MS: u128 = 530;
/// Largest side of a decoded thumbnail picture, in pixels.
const MAX_IMAGE: u32 = 512;
const FIGMA_URL: &str = "https://www.figma.com/community/file/1691465999134370980/vlc-redesign";
const CC_BY_URL: &str = "https://creativecommons.org/licenses/by/4.0/";
const CF_UNICODETEXT: u32 = 13;

// Sampled from the "new playlist" frame.
const BG_DIALOG: u32 = 0x191919;
const BG_WELL: u32 = BG_B2;
const BG_PLUS: u32 = 0x181818;
const BG_BUTTON: u32 = BG_A3;
const BG_BUTTON_OFF: u32 = 0x1E1E1E;
const LINE_DIALOG: u32 = 0x1E1E1E;
const DASH: u32 = 0x383838;
const FG_HEADING: u32 = 0xD9D9D9;
const FG_HELP: u32 = 0x777777;
const FG_PLACEHOLDER: u32 = 0x444444;
const FG_DROP: u32 = 0x555555;
const FG_BUTTON_OFF: u32 = 0x606060;

/// The keys main.rs handles, for the Keyboard Shortcuts dialog.
// ponytail: a copy of on_key and the menus' accelerators; one table both read would keep them
// in step.
const SHORTCUTS: [(&str, &str); 22] = [
    ("Space", "Play or pause"),
    ("Left / Right", "Skip back or forward (Settings > Controls)"),
    (
        "Alt / Shift / Ctrl + arrows",
        "Very short, medium or long skip",
    ),
    ("Up / Down", "Volume up or down"),
    ("Mouse wheel", "Volume in the player, scroll in the library"),
    ("M", "Mute"),
    ("F, double-click the video", "Fullscreen"),
    (
        "Esc",
        "Close a panel, leave fullscreen, or go to the library",
    ),
    ("Backspace", "Back, in the library"),
    ("S / P / N", "Stop, previous file, next file"),
    ("Shift + S", "Snapshot, saved in Pictures\\myvlc"),
    ("Ctrl + O", "Open files"),
    ("Ctrl + F", "Open a folder"),
    ("Ctrl + N", "Open a network stream"),
    ("Ctrl + P", "Settings"),
    ("Ctrl + M", "Mini player, and back"),
    ("Ctrl + I", "Properties of the file playing or pointed at"),
    ("Ctrl + \\", "Hide the controls until the mouse moves"),
    ("Right-click", "Player and library menus"),
    ("F1", "Keyboard shortcuts"),
    ("Shift + F1", "About myvlc"),
    ("Q, Ctrl + Q", "Quit"),
];

/// A single-line text field's text, caret and selection. Positions are byte offsets on char
/// boundaries; the selection runs between `anchor` and `caret`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextField {
    pub text: String,
    caret: usize,
    anchor: usize,
}

impl TextField {
    /// `text` with all of it selected, so typing replaces it.
    pub fn new(text: &str) -> Self {
        TextField {
            text: text.to_owned(),
            caret: text.len(),
            anchor: 0,
        }
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn selection(&self) -> Range<usize> {
        self.caret.min(self.anchor)..self.caret.max(self.anchor)
    }

    pub fn selected(&self) -> &str {
        &self.text[self.selection()]
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i]
            .char_indices()
            .next_back()
            .map_or(0, |(j, _)| j)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..]
            .chars()
            .next()
            .map_or(i, |c| i + c.len_utf8())
    }

    /// The start of the word before `i`, skipping spaces first, like Ctrl+Left.
    fn word_left(&self, mut i: usize) -> usize {
        while i > 0 && self.text[..i].ends_with(char::is_whitespace) {
            i = self.prev(i);
        }
        while i > 0 && !self.text[..i].ends_with(char::is_whitespace) {
            i = self.prev(i);
        }
        i
    }

    /// The start of the next word after `i`, like Ctrl+Right.
    fn word_right(&self, mut i: usize) -> usize {
        let at = |i: usize| self.text[i..].chars().next();
        while at(i).is_some_and(|c| !c.is_whitespace()) {
            i = self.next(i);
        }
        while at(i).is_some_and(char::is_whitespace) {
            i = self.next(i);
        }
        i
    }

    /// Replaces the selection with `s`. Line breaks become spaces and other control characters
    /// go, since the field holds one line; the text stops at `MAX_NAME` characters.
    pub fn insert(&mut self, s: &str) {
        let sel = self.selection();
        let room = MAX_NAME
            .saturating_sub(self.text.chars().count() - self.text[sel.clone()].chars().count());
        let clean: String = s
            .chars()
            .map(|c| {
                if c == '\n' || c == '\r' || c == '\t' {
                    ' '
                } else {
                    c
                }
            })
            .filter(|c| !c.is_control())
            .take(room)
            .collect();
        self.text.replace_range(sel.clone(), &clean);
        self.caret = sel.start + clean.len();
        self.anchor = self.caret;
    }

    /// Deletes the selection, else the character (or word) before the caret.
    pub fn backspace(&mut self, word: bool) {
        if self.anchor == self.caret {
            self.anchor = if word {
                self.word_left(self.caret)
            } else {
                self.prev(self.caret)
            };
        }
        self.insert("");
    }

    /// Deletes the selection, else the character (or word) after the caret.
    pub fn delete(&mut self, word: bool) {
        if self.anchor == self.caret {
            self.anchor = if word {
                self.word_right(self.caret)
            } else {
                self.next(self.caret)
            };
        }
        self.insert("");
    }

    /// Puts the caret at `i` (clamped to a char boundary), extending the selection or not.
    pub fn move_to(&mut self, i: usize, extend: bool) {
        let mut i = i.min(self.text.len());
        while !self.text.is_char_boundary(i) {
            i -= 1;
        }
        self.caret = i;
        if !extend {
            self.anchor = i;
        }
    }

    /// Left arrow: a selection collapses to its start, else the caret moves.
    pub fn left(&mut self, extend: bool, word: bool) {
        let to = match (extend, word) {
            (false, false) if self.anchor != self.caret => self.selection().start,
            (_, true) => self.word_left(self.caret),
            _ => self.prev(self.caret),
        };
        self.move_to(to, extend);
    }

    pub fn right(&mut self, extend: bool, word: bool) {
        let to = match (extend, word) {
            (false, false) if self.anchor != self.caret => self.selection().end,
            (_, true) => self.word_right(self.caret),
            _ => self.next(self.caret),
        };
        self.move_to(to, extend);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Removes and returns the selection, if any.
    pub fn cut(&mut self) -> Option<String> {
        let s = self.selected().to_owned();
        if s.is_empty() {
            return None;
        }
        self.insert("");
        Some(s)
    }
}

/// The open dialog.
pub struct State {
    pub dialog: Dialog,
    pub field: TextField,
    pub files: Vec<PathBuf>,
    pub thumbnail: Option<PathBuf>,
    /// Shown under the name field: why Save is off, or what main.rs reported.
    pub error: Option<String>,
    /// Properties: (label, value) rows.
    rows: Vec<(String, String)>,
    /// The first file row shown.
    files_scroll: usize,
    copied: bool,
    /// When the caret last moved; it shows solid for a moment after.
    blink: Instant,
    blink_on: bool,
    /// A key went down since the dialog opened. The WM_CHAR of the key that opened it arrives
    /// after it, and must not land in the field.
    typing: bool,
    /// The user picked the thumbnail here, rather than it being the playlist's current one.
    thumbnail_picked: bool,
    /// The last press in the field, to spot a double-click.
    last_press: Option<Instant>,
}

impl State {
    pub fn new(dialog: Dialog) -> Self {
        let mut state = State {
            field: TextField::default(),
            files: Vec::new(),
            thumbnail: None,
            error: None,
            rows: Vec::new(),
            files_scroll: 0,
            copied: false,
            blink: Instant::now(),
            blink_on: true,
            typing: false,
            thumbnail_picked: false,
            last_press: None,
            dialog: dialog.clone(),
        };
        match dialog {
            Dialog::NewPlaylist { files } => state.add_files(files),
            Dialog::RenamePlaylist(name) => state.field = TextField::new(&name),
            Dialog::Properties(path) => state.rows = properties(&path),
            Dialog::About
            | Dialog::Shortcuts
            | Dialog::DeletePlaylist(_)
            | Dialog::NewProfile
            | Dialog::Address(_) => {}
        }
        state
    }

    fn title(&self) -> &'static str {
        match self.dialog {
            Dialog::NewPlaylist { .. } => "New Playlist",
            Dialog::RenamePlaylist(_) => "Rename Playlist",
            Dialog::About => "About",
            Dialog::Shortcuts => "Keyboard Shortcuts",
            Dialog::Properties(_) => "Properties",
            Dialog::DeletePlaylist(_) => "Delete Playlist",
            Dialog::NewProfile => "New Profile",
            Dialog::Address(NetKind::Podcast) => "Add Podcast",
            Dialog::Address(_) => "Open Network Stream",
        }
    }

    pub fn has_field(&self) -> bool {
        matches!(
            self.dialog,
            Dialog::NewPlaylist { .. }
                | Dialog::RenamePlaylist(_)
                | Dialog::NewProfile
                | Dialog::Address(_)
        )
    }

    /// The trimmed name, when it can be saved. An address is checked by main.rs on Save, since
    /// one half typed always looks wrong.
    fn name(&self) -> Option<&str> {
        let name = self.field.text.trim();
        let address = matches!(self.dialog, Dialog::Address(_));
        (!name.is_empty() && (address || crate::library::check_name(name).is_ok())).then_some(name)
    }

    /// The live complaint about the typed name, before main.rs ever sees it.
    fn name_error(&self) -> Option<&'static str> {
        let name = self.field.text.trim();
        let address = matches!(self.dialog, Dialog::Address(_));
        (!name.is_empty() && !address && crate::library::check_name(name).is_err())
            .then_some("A name can't use < > : \" / \\ | ? *, end with a dot, or be a device name.")
    }

    /// Adds media files to the list, skipping ones already in it.
    pub fn add_files(&mut self, files: Vec<PathBuf>) {
        for f in files {
            if crate::library::media_kind(&f).is_some() && !self.files.contains(&f) {
                self.files.push(f);
            }
        }
        // Show the newest files.
        self.files_scroll = self.files.len().saturating_sub(FILE_ROWS);
    }

    fn caret_visible(&self) -> bool {
        (self.blink.elapsed().as_millis() / BLINK_MS).is_multiple_of(2)
    }

    /// A key for the name field; true when it changed the text or caret.
    fn field_key(&mut self, key: VIRTUAL_KEY, shift: bool, ctrl: bool) -> bool {
        let f = &mut self.field;
        match key {
            VK_LEFT => f.left(shift, ctrl),
            VK_RIGHT => f.right(shift, ctrl),
            VK_HOME => f.move_to(0, shift),
            VK_END => f.move_to(usize::MAX, shift),
            VK_BACK => f.backspace(ctrl),
            VK_DELETE if shift => {
                if let Some(s) = f.cut() {
                    set_clipboard_text(&s);
                }
            }
            VK_DELETE => f.delete(ctrl),
            VK_A if ctrl => f.select_all(),
            VK_C | VK_INSERT if ctrl => {
                if !f.selected().is_empty() {
                    set_clipboard_text(f.selected());
                }
                return false;
            }
            VK_X if ctrl => {
                if let Some(s) = f.cut() {
                    set_clipboard_text(&s);
                }
            }
            VK_V if ctrl => f.insert(&clipboard_text().unwrap_or_default()),
            VK_INSERT if shift => f.insert(&clipboard_text().unwrap_or_default()),
            _ => return false,
        }
        self.edited();
        true
    }

    /// After any edit or caret move: the caret shows, and an old error goes.
    fn edited(&mut self) {
        self.blink = Instant::now();
        self.error = self.name_error().map(str::to_owned);
    }

    /// The action Save stands for, when Save is on.
    fn save(&self) -> Option<DialogAction> {
        let name = self.name()?.to_owned();
        Some(match &self.dialog {
            Dialog::NewPlaylist { .. } => DialogAction::CreatePlaylist {
                name,
                thumbnail: self.thumbnail.clone(),
                files: self.files.clone(),
            },
            Dialog::RenamePlaylist(from) => DialogAction::RenamePlaylist {
                from: from.clone(),
                to: name,
                thumbnail: self.thumbnail.clone().filter(|_| self.thumbnail_picked),
            },
            Dialog::NewProfile => DialogAction::CreateProfile(name),
            Dialog::Address(kind) => DialogAction::OpenAddress(*kind, name),
            _ => return None,
        })
    }
}

/// Name, folder, size, date, tags and streams of a file, as Properties rows.
// ponytail: probes on the UI thread when the dialog opens, a few milliseconds for a local file;
// a slow network share stalls the window until it answers. Move it to the prober thread then.
fn properties(path: &Path) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut row = |label: &str, value: String| {
        if !value.is_empty() {
            rows.push((label.to_owned(), value));
        }
    };
    let lossy = |p: Option<&std::ffi::OsStr>| {
        p.map_or_else(String::new, |s| s.to_string_lossy().into_owned())
    };
    row("Name", lossy(path.file_name()));
    row(
        "Folder",
        path.parent()
            .map_or_else(String::new, |p| p.display().to_string()),
    );
    match std::fs::metadata(path) {
        Ok(m) => {
            use std::os::windows::fs::MetadataExt;
            row("Size", size_text(m.len()));
            row(
                "Modified",
                local_time(m.last_write_time()).unwrap_or_default(),
            );
        }
        Err(e) => row("Size", format!("Can't read the file ({e})")),
    }
    if let Some(info) = crate::thumbs::probe(path) {
        for (label, tag) in [
            ("Title", info.title),
            ("Artist", info.artist),
            ("Album", info.album),
            ("Genre", info.genre),
        ] {
            row(label, tag.unwrap_or_default());
        }
    }
    if let Some(d) = crate::probe::details(path) {
        if d.duration > 0.0 {
            row("Duration", duration_text(d.duration));
        }
        row("Container", d.container);
        if d.bit_rate > 0 {
            row(
                "Bit rate",
                format!("{:.0} kb/s", d.bit_rate as f64 / 1000.0),
            );
        }
        // A file with dozens of tracks would run off the window; the rest are counted.
        const MAX_STREAMS: usize = 10;
        let more = d.streams.len().saturating_sub(MAX_STREAMS);
        for (kind, text) in d.streams.into_iter().take(MAX_STREAMS) {
            row(kind, text);
        }
        if more > 0 {
            row("", format!("and {more} more streams"));
            rows.last_mut().unwrap().0 = "Streams".into();
        }
    }
    rows
}

/// "12.3 MB (12,345,678 bytes)".
fn size_text(bytes: u64) -> String {
    let units = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < units.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    let digits = bytes.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    if unit == 0 {
        format!("{grouped} bytes")
    } else {
        format!("{v:.1} {} ({grouped} bytes)", units[unit])
    }
}

/// "1:02:03" or "2:05".
fn duration_text(seconds: f64) -> String {
    let s = seconds.round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// A FILETIME as local "2026-10-25 21:20".
fn local_time(filetime: u64) -> Option<String> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ft = FILETIME {
        dwLowDateTime: filetime as u32,
        dwHighDateTime: (filetime >> 32) as u32,
    };
    let (mut utc, mut t) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut t).ok()?;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute
    ))
}

/// The clipboard's text, if it holds any.
fn clipboard_text() -> Option<String> {
    unsafe {
        OpenClipboard(Some(GetActiveWindow())).ok()?;
        let text = (|| {
            let h = HGLOBAL(GetClipboardData(CF_UNICODETEXT).ok()?.0);
            let p = GlobalLock(h) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            _ = GlobalUnlock(h);
            Some(s)
        })();
        _ = CloseClipboard();
        text
    }
}

/// Puts `text` on the clipboard. The window must own the clipboard for SetClipboardData to
/// work, so this runs on the UI thread, where the window is active.
fn set_clipboard_text(text: &str) -> bool {
    let utf16: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        if OpenClipboard(Some(GetActiveWindow())).is_err() {
            return false;
        }
        let done = (|| -> Result<()> {
            EmptyClipboard()?;
            let h = GlobalAlloc(GMEM_MOVEABLE, utf16.len() * 2)?;
            let p = GlobalLock(h) as *mut u16;
            if p.is_null() {
                _ = GlobalFree(Some(h));
                return Err(Error::from(E_FAIL));
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr(), p, utf16.len());
            _ = GlobalUnlock(h);
            // The clipboard owns the memory from here on, unless it refuses it.
            if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0))) {
                _ = GlobalFree(Some(h));
                return Err(e);
            }
            Ok(())
        })();
        _ = CloseClipboard();
        done.is_ok()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Style {
    Heading,
    Help,
    Value,
    Key,
    Name,
    Link,
    Error,
    DropText,
}

/// One thing a dialog draws, in dialog units.
#[derive(Clone, Debug, PartialEq)]
enum Part {
    Text(String, Style, Rect),
    Field(Rect),
    Thumb {
        r: Rect,
        plus: Rect,
    },
    Line(Rect),
    Drop {
        r: Rect,
        plus: Rect,
    },
    File {
        i: usize,
        r: Rect,
        remove: Rect,
    },
    Button {
        target: Target,
        label: &'static str,
        r: Rect,
        primary: bool,
        enabled: bool,
    },
    Logo(Rect),
}

/// Where the open dialog sits and what it draws.
#[derive(Clone, Default)]
pub struct Geometry {
    /// Window DIPs = origin + dialog units * scale.
    origin: (f32, f32),
    scale: f32,
    size: (f32, f32),
    window: (f32, f32),
    title: &'static str,
    parts: Vec<Part>,
    targets: Vec<(Target, Rect)>,
    /// The name field's text area, and the x of every caret position (byte offset, x) from its
    /// left edge before scrolling.
    text_area: Rect,
    stops: Vec<(usize, f32)>,
    /// How far the text is scrolled left to keep the caret in view.
    text_scroll: f32,
}

impl Geometry {
    /// Window DIPs to dialog units.
    fn local(&self, x: f32, y: f32) -> (f32, f32) {
        (
            (x - self.origin.0) / self.scale,
            (y - self.origin.1) / self.scale,
        )
    }

    pub fn hit(&self, x: f32, y: f32) -> Target {
        let (x, y) = self.local(x, y);
        // Later targets sit on top: a file's x over its row, the "+" over its box.
        self.targets
            .iter()
            .rev()
            .find(|(_, r)| r.contains(x, y))
            .map_or(Target::Body, |(t, _)| *t)
    }

    /// The caret position nearest to window x.
    fn caret_at(&self, x: f32) -> usize {
        let (x, _) = self.local(x, 0.0);
        let x = x - self.text_area.l + self.text_scroll;
        self.stops
            .iter()
            .min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs()))
            .map_or(0, |s| s.0)
    }

    fn stop(&self, i: usize) -> f32 {
        self.stops.iter().find(|s| s.0 == i).map_or(0.0, |s| s.1)
    }
}

/// Builds a dialog's parts top to bottom.
struct Builder {
    w: f32,
    parts: Vec<Part>,
    targets: Vec<(Target, Rect)>,
}

impl Builder {
    fn text(&mut self, text: impl Into<String>, style: Style, l: f32, cy: f32, r: f32) {
        self.parts.push(Part::Text(
            text.into(),
            style,
            Rect::new(l, cy - 12.0, r, cy + 12.0),
        ));
    }

    fn target(&mut self, target: Target, r: Rect) {
        self.targets.push((target, r));
    }

    fn line(&mut self, y: f32) {
        self.parts
            .push(Part::Line(Rect::new(PAD, y, self.w - PAD, y + 1.0)));
    }

    /// The footer's buttons at `top`: an optional one on the left and the primary on the right.
    fn footer(
        &mut self,
        top: f32,
        left: Option<(Target, &'static str)>,
        primary: &'static str,
        enabled: bool,
    ) -> f32 {
        let button = |l: f32| Rect::new(l, top, l + BUTTON_W, top + BUTTON_H);
        if let Some((target, label)) = left {
            let r = button(PAD);
            self.parts.push(Part::Button {
                target,
                label,
                r,
                primary: false,
                enabled: true,
            });
            self.target(target, r);
        }
        let r = button(self.w - PAD - BUTTON_W);
        self.parts.push(Part::Button {
            target: Target::Primary,
            label: primary,
            r,
            primary: true,
            enabled,
        });
        if enabled {
            self.target(Target::Primary, r);
        }
        top + BUTTON_H + PAD
    }
}

impl Painter {
    /// Lays out the open dialog over `layout`. The name field needs DirectWrite for the caret
    /// positions, which is why this lives on the painter.
    pub fn add_dialog(&self, layout: &mut Layout, d: &State) {
        let w = match d.dialog {
            Dialog::NewPlaylist { .. } | Dialog::RenamePlaylist(_) => W,
            Dialog::Properties(_) => 640.0,
            Dialog::About | Dialog::Shortcuts => 600.0,
            Dialog::DeletePlaylist(_) | Dialog::NewProfile => 520.0,
            Dialog::Address(_) => 600.0,
        };
        let mut b = Builder {
            w,
            parts: Vec::new(),
            targets: vec![(Target::Close, Rect::around(w - 24.0, TITLE_H / 2.0, 32.0))],
        };
        let mut g = Geometry {
            title: d.title(),
            ..Default::default()
        };
        let h = match &d.dialog {
            Dialog::NewPlaylist { .. } | Dialog::RenamePlaylist(_) => {
                self.playlist_parts(&mut b, &mut g, d)
            }
            Dialog::NewProfile => self.profile_parts(&mut b, &mut g, d),
            Dialog::Address(kind) => self.address_parts(&mut b, &mut g, d, *kind),
            Dialog::About => about_parts(&mut b),
            Dialog::Shortcuts => {
                let mut y = TITLE_H + 14.0;
                for (key, action) in SHORTCUTS {
                    let cy = y + TABLE_ROW / 2.0;
                    b.text(key, Style::Key, PAD, cy, 230.0);
                    b.text(action, Style::Value, 240.0, cy, w - PAD);
                    y += TABLE_ROW;
                    b.line(y);
                }
                b.footer(y + 24.0, None, "Close", true)
            }
            Dialog::Properties(_) => {
                let mut y = TITLE_H + 14.0;
                for (label, value) in &d.rows {
                    let cy = y + TABLE_ROW / 2.0;
                    b.text(label.as_str(), Style::Help, PAD, cy, 140.0);
                    b.text(value.as_str(), Style::Value, 150.0, cy, w - PAD);
                    y += TABLE_ROW;
                    b.line(y);
                }
                let copy = if d.copied { "Copied" } else { "Copy path" };
                b.footer(y + 24.0, Some((Target::CopyPath, copy)), "Close", true)
            }
            Dialog::DeletePlaylist(name) => {
                b.text(
                    format!("Delete \"{name}\"?"),
                    Style::Heading,
                    PAD,
                    87.0,
                    w - PAD,
                );
                b.text(
                    "The playlist goes away; the files in it stay where they are.",
                    Style::Help,
                    PAD,
                    117.0,
                    w - PAD,
                );
                b.footer(157.0, Some((Target::Cancel, "Cancel")), "Delete", true)
            }
        };
        // Clicks anywhere on the dialog stop at it; the dimmed window around it is Body too.
        b.targets
            .insert(0, (Target::Body, Rect::new(0.0, 0.0, w, h)));
        let (ww, wh) = (layout.width, layout.height);
        let margin = 16.0;
        let scale = ((ww - 2.0 * margin) / w)
            .min((wh - 2.0 * margin) / h)
            .clamp(0.1, 1.0);
        g.origin = (
            ((ww - w * scale) / 2.0).round(),
            ((wh - h * scale) / 2.0).round(),
        );
        g.scale = scale;
        g.size = (w, h);
        g.window = (ww, wh);
        g.parts = b.parts;
        g.targets = b.targets;
        layout.dialog = Some(g);
    }

    /// New Playlist and Rename Playlist; returns the height.
    fn playlist_parts(&self, b: &mut Builder, g: &mut Geometry, d: &State) -> f32 {
        let w = b.w;
        let thumb = Rect::new(w - PAD - THUMB, 81.0, w - PAD, 81.0 + THUMB);
        b.text(
            "Playlist Details",
            Style::Heading,
            PAD,
            87.0,
            thumb.l - 24.0,
        );
        b.text(
            "The playlist will be saved and displayed with this title and thumbnail.",
            Style::Help,
            PAD,
            117.0,
            thumb.l - 12.0,
        );
        let field = Rect::new(PAD, 154.0, thumb.l - 24.0, 188.0);
        b.parts.push(Part::Field(field));
        b.target(Target::Field, field);
        b.parts.push(Part::Thumb {
            r: thumb,
            plus: Rect::around((thumb.l + thumb.r) / 2.0, (thumb.t + thumb.b) / 2.0, 30.0),
        });
        b.target(Target::Thumbnail, thumb);
        if let Some(e) = &d.error {
            b.text(e.as_str(), Style::Error, PAD, 201.0, thumb.l - 12.0);
        }
        self.field_stops(g, d, field);

        let bottom = if let Dialog::NewPlaylist { .. } = d.dialog {
            b.line(211.0);
            b.text("Select Files", Style::Heading, PAD, 247.5, w - PAD);
            b.text(
                "These files will be added to the playlist.",
                Style::Help,
                PAD,
                277.5,
                w - PAD,
            );
            let drop = Rect::new(PAD, 310.0, w - PAD, 494.0);
            let plus = Rect::new(w / 2.0 - 42.0, 406.0, w / 2.0 + 42.0, 436.0);
            b.parts.push(Part::Drop { r: drop, plus });
            b.text(
                "Drag files into the box or click here to select files.",
                Style::DropText,
                drop.l,
                377.5,
                drop.r,
            );
            b.target(Target::Files, drop);
            let mut y = drop.b;
            if !d.files.is_empty() {
                y += 10.0;
                let shown = d.files_scroll..(d.files_scroll + FILE_ROWS).min(d.files.len());
                for i in shown {
                    let r = Rect::new(PAD, y, w - PAD, y + FILE_ROW);
                    let remove = Rect::around(r.r - 18.0, (r.t + r.b) / 2.0, 26.0);
                    b.parts.push(Part::File { i, r, remove });
                    b.target(Target::RemoveFile(i), remove);
                    y += FILE_ROW;
                }
            }
            y
        } else {
            thumb.b
        };
        b.footer(
            bottom + FOOTER_GAP,
            Some((Target::Cancel, "Cancel")),
            "Save",
            d.name().is_some(),
        )
    }

    /// New Profile: the name field; returns the height.
    fn profile_parts(&self, b: &mut Builder, g: &mut Geometry, d: &State) -> f32 {
        let w = b.w;
        b.text("Profile Name", Style::Heading, PAD, 87.0, w - PAD);
        b.text(
            "The profile starts with the settings in force now.",
            Style::Help,
            PAD,
            117.0,
            w - PAD,
        );
        let field = Rect::new(PAD, 154.0, w - PAD, 188.0);
        b.parts.push(Part::Field(field));
        b.target(Target::Field, field);
        if let Some(e) = &d.error {
            b.text(e.as_str(), Style::Error, PAD, 201.0, w - PAD);
        }
        self.field_stops(g, d, field);
        b.footer(
            field.b + FOOTER_GAP,
            Some((Target::Cancel, "Cancel")),
            "Save",
            d.name().is_some(),
        )
    }

    /// Open Network Stream and Add Podcast: the address field; returns the height.
    fn address_parts(&self, b: &mut Builder, g: &mut Geometry, d: &State, kind: NetKind) -> f32 {
        let w = b.w;
        let (heading, help, button) = match kind {
            NetKind::Podcast => ("Feed Address", "The podcast's RSS or Atom feed.", "Add"),
            _ => (
                "Network Address",
                "http, https, HLS (.m3u8), RTSP, UDP or RTP. It's kept on the Network page.",
                "Play",
            ),
        };
        b.text(heading, Style::Heading, PAD, 87.0, w - PAD);
        b.text(help, Style::Help, PAD, 117.0, w - PAD);
        let field = Rect::new(PAD, 154.0, w - PAD, 188.0);
        b.parts.push(Part::Field(field));
        b.target(Target::Field, field);
        if let Some(e) = &d.error {
            b.text(e.as_str(), Style::Error, PAD, 201.0, w - PAD);
        }
        self.field_stops(g, d, field);
        b.footer(
            field.b + FOOTER_GAP,
            Some((Target::Cancel, "Cancel")),
            button,
            d.name().is_some(),
        )
    }

    /// Measures where the caret can go in the name field.
    fn field_stops(&self, g: &mut Geometry, d: &State, field: Rect) {
        let text = &d.field.text;
        let area = Rect::new(field.l + 14.0, field.t, field.r - 14.0, field.b);
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let layout = unsafe {
            self.dwrite
                .CreateTextLayout(&utf16, &self.dialog.field_text, 10_000.0, 100.0)
                .ok()
        };
        let mut stops = Vec::new();
        let mut units = 0u32;
        let boundaries = text.char_indices().map(|(i, _)| i).chain([text.len()]);
        for (n, i) in boundaries.enumerate() {
            if n > 0 {
                units += text[..i]
                    .chars()
                    .next_back()
                    .map_or(0, |c| c.len_utf16() as u32);
            }
            let x = layout.as_ref().map_or(0.0, |l| {
                let (mut x, mut y) = (0.0, 0.0);
                let mut m = DWRITE_HIT_TEST_METRICS::default();
                unsafe { l.HitTestTextPosition(units, false, &mut x, &mut y, &mut m) }
                    .map_or(0.0, |()| x)
            });
            stops.push((i, x));
        }
        g.stops = stops;
        g.text_area = area;
        let caret = g.stop(d.field.caret());
        g.text_scroll = (caret - (area.w() - 2.0)).max(0.0);
    }
}

/// About: name, version, what it is, and the credits README.md lists. Returns the height.
fn about_parts(b: &mut Builder) -> f32 {
    let w = b.w;
    b.parts
        .push(Part::Logo(Rect::new(PAD, 70.0, PAD + 48.0, 118.0)));
    b.text("myvlc", Style::Name, 84.0, 84.0, w - PAD);
    b.text(
        concat!("Version ", env!("CARGO_PKG_VERSION")),
        Style::Help,
        84.0,
        106.0,
        w - PAD,
    );
    b.text(
        "A small Windows video player in Rust: FFmpeg, Direct3D 11 and WASAPI.",
        Style::Value,
        PAD,
        146.0,
        w - PAD,
    );
    b.line(172.0);
    b.text("Credits", Style::Heading, PAD, 206.0, w - PAD);
    let mut cy = 236.0;
    let mut line = |b: &mut Builder, text: &str, style: Style| {
        b.text(text, style, PAD, cy, w - PAD);
        cy += 24.0;
        cy - 24.0
    };
    line(
        b,
        "Interface: VLC Redesign by Juxtopposed, under CC BY 4.0",
        Style::Value,
    );
    for (i, label) in [
        "figma.com/community/file/1691465999134370980/vlc-redesign",
        "creativecommons.org/licenses/by/4.0",
    ]
    .into_iter()
    .enumerate()
    {
        let y = line(b, label, Style::Link);
        b.target(Target::Link(i), Rect::new(PAD, y - 11.0, w - PAD, y + 11.0));
    }
    line(
        b,
        "Video and audio decoding: FFmpeg, under the LGPL",
        Style::Value,
    );
    line(
        b,
        "Font: Inter, under the SIL Open Font License 1.1",
        Style::Value,
    );
    line(
        b,
        "Not affiliated with VideoLAN or Juxtopposed.",
        Style::Help,
    );
    b.footer(cy + 18.0, None, "Close", true)
}

impl Ui {
    /// Opens a dialog over everything; popups and the sidebar panel close.
    pub fn open_dialog(&mut self, dialog: Dialog) {
        self.panels.show(None, None);
        self.drag = None;
        self.pressed = None;
        self.dialog = Some(State::new(dialog));
        self.relayout = true;
        self.wake();
    }

    pub fn close_dialog(&mut self) {
        if self.dialog.take().is_some() {
            self.drag = None;
            self.relayout = true;
        }
    }

    /// Files from the picker or dropped on the window, for New Playlist.
    pub fn dialog_add_files(&mut self, files: Vec<PathBuf>) {
        if let Some(d) = self.dialog.as_mut() {
            d.add_files(files);
            self.relayout = true;
        }
    }

    /// The image the user picked for the thumbnail.
    pub fn dialog_set_thumbnail(&mut self, image: Option<PathBuf>) {
        if let Some(d) = self.dialog.as_mut() {
            d.thumbnail = image;
            d.thumbnail_picked = true;
            self.relayout = true;
        }
    }

    /// The playlist's current thumbnail, shown when Rename Playlist opens.
    pub fn dialog_show_thumbnail(&mut self, image: Option<PathBuf>) {
        if let Some(d) = self.dialog.as_mut() {
            d.thumbnail = image;
            self.relayout = true;
        }
    }

    /// Shows why main.rs couldn't do what the dialog asked; the dialog stays open.
    pub fn dialog_error(&mut self, message: String) {
        if let Some(d) = self.dialog.as_mut() {
            d.error = Some(message);
            self.relayout = true;
        }
    }

    /// Whether the open dialog takes dropped files.
    pub fn dialog_takes_files(&self) -> bool {
        self.dialog
            .as_ref()
            .is_some_and(|d| matches!(d.dialog, Dialog::NewPlaylist { .. }))
    }

    /// The cursor over the open dialog: an I-beam on the name field, a hand on links.
    pub fn dialog_cursor(&self) -> Option<PCWSTR> {
        match self.hover? {
            Hit::Dialog(Target::Field) => Some(IDC_IBEAM),
            Hit::Dialog(Target::Link(_)) => Some(IDC_HAND),
            _ => None,
        }
    }

    /// A typed character for the open dialog. False when no dialog is open.
    pub fn dialog_char(&mut self, c: u32) -> bool {
        let Some(d) = self.dialog.as_mut() else {
            return false;
        };
        // Backspace, Enter, Esc and Ctrl+letters arrive here too; WM_KEYDOWN handles those.
        if let Some(c) = char::from_u32(c).filter(|c| !c.is_control())
            && d.has_field()
            && d.typing
        {
            d.field.insert(c.encode_utf8(&mut [0; 4]));
            d.edited();
            self.relayout = true;
        }
        true
    }

    /// A key for the open dialog: Esc closes, Enter is the primary button, the rest edit the
    /// name field.
    pub fn dialog_key(&mut self, key: VIRTUAL_KEY, shift: bool, ctrl: bool) -> Option<Action> {
        let d = self.dialog.as_mut()?;
        d.typing = true;
        match key {
            VK_ESCAPE => {
                self.close_dialog();
                None
            }
            VK_RETURN => self.dialog_click(Target::Primary),
            _ => {
                if d.has_field() && d.field_key(key, shift, ctrl) {
                    self.relayout = true;
                }
                None
            }
        }
    }

    /// The mouse wheel over a dialog scrolls its file list.
    pub fn scroll_dialog(&mut self, notches: f32) {
        if let Some(d) = self.dialog.as_mut() {
            let max = d.files.len().saturating_sub(FILE_ROWS);
            let step = -notches.round() as isize;
            d.files_scroll = d.files_scroll.saturating_add_signed(step).min(max);
            self.relayout = true;
        }
    }

    /// True when the caret's blink turned since the last call, so the window repaints.
    pub fn dialog_tick(&mut self) -> bool {
        let Some(d) = self.dialog.as_mut().filter(|d| d.has_field()) else {
            return false;
        };
        let on = d.caret_visible();
        std::mem::replace(&mut d.blink_on, on) != on
    }

    /// A press on the dialog: in the name field it places the caret and starts a selection.
    pub(super) fn dialog_down(&mut self, target: Target, x: f32) -> Option<Action> {
        if target != Target::Field {
            return None;
        }
        let shift = unsafe { GetKeyState(i32::from(VK_SHIFT.0)) } < 0;
        let g = self.layout.dialog.as_ref()?;
        let i = g.caret_at(x);
        let d = self.dialog.as_mut()?;
        let double = d.last_press.is_some_and(|at| at.elapsed() < DOUBLE_CLICK);
        d.last_press = Some(Instant::now());
        if double {
            d.field.select_all();
            d.last_press = None;
        } else {
            d.field.move_to(i, shift);
            self.drag = Some(Hit::Dialog(Target::Field));
        }
        d.blink = Instant::now();
        self.relayout = true;
        None
    }

    /// Dragging in the name field extends the selection.
    pub(super) fn dialog_drag(&mut self, x: f32) -> Option<Action> {
        let i = self.layout.dialog.as_ref()?.caret_at(x);
        let d = self.dialog.as_mut()?;
        if d.field.caret() != i {
            d.field.move_to(i, true);
            d.blink = Instant::now();
            self.relayout = true;
        }
        None
    }

    /// A click on the dialog: closing and list edits happen here, the rest goes to main.rs.
    pub(super) fn dialog_click(&mut self, target: Target) -> Option<Action> {
        let d = self.dialog.as_mut()?;
        self.relayout = true;
        let action = match target {
            Target::Body | Target::Field => return None,
            Target::Close | Target::Cancel => {
                self.close_dialog();
                return None;
            }
            Target::Primary if d.has_field() => d.save()?,
            Target::Primary if matches!(d.dialog, Dialog::DeletePlaylist(_)) => {
                let Dialog::DeletePlaylist(name) = d.dialog.clone() else {
                    unreachable!()
                };
                self.close_dialog();
                DialogAction::DeletePlaylist(name)
            }
            Target::Primary => {
                self.close_dialog();
                return None;
            }
            Target::Thumbnail => DialogAction::PickThumbnail,
            Target::Files => DialogAction::PickFiles,
            Target::RemoveFile(i) => {
                if i < d.files.len() {
                    d.files.remove(i);
                    d.files_scroll = d.files_scroll.min(d.files.len().saturating_sub(FILE_ROWS));
                }
                return None;
            }
            Target::CopyPath => {
                if let Dialog::Properties(path) = &d.dialog {
                    d.copied = set_clipboard_text(&path.display().to_string());
                }
                return None;
            }
            Target::Link(i) => DialogAction::OpenLink([FIGMA_URL, CC_BY_URL][i.min(1)]),
        };
        Some(Action::Dialog(action))
    }
}

/// Brushes, fonts and decoded pictures the dialogs use.
pub(super) struct Resources {
    veil: ID2D1SolidColorBrush,
    shadow: ID2D1SolidColorBrush,
    body: ID2D1SolidColorBrush,
    strip: ID2D1SolidColorBrush,
    well: ID2D1SolidColorBrush,
    plus_bg: ID2D1SolidColorBrush,
    button_bg: ID2D1SolidColorBrush,
    button_off: ID2D1SolidColorBrush,
    primary_bg: ID2D1SolidColorBrush,
    line: ID2D1SolidColorBrush,
    dash: ID2D1SolidColorBrush,
    hover: ID2D1SolidColorBrush,
    selection: ID2D1SolidColorBrush,
    title_fg: ID2D1SolidColorBrush,
    heading_fg: ID2D1SolidColorBrush,
    help_fg: ID2D1SolidColorBrush,
    placeholder_fg: ID2D1SolidColorBrush,
    drop_fg: ID2D1SolidColorBrush,
    label_fg: ID2D1SolidColorBrush,
    off_fg: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    dashes: ID2D1StrokeStyle,
    title: IDWriteTextFormat,
    heading: IDWriteTextFormat,
    help: IDWriteTextFormat,
    centred: IDWriteTextFormat,
    pub(super) field_text: IDWriteTextFormat,
    button: IDWriteTextFormat,
    key: IDWriteTextFormat,
    name: IDWriteTextFormat,
    small: IDWriteTextFormat,
    plus: Svg,
    wic: Option<IWICImagingFactory>,
    /// Pictures by file; None means it didn't decode.
    images: HashMap<PathBuf, Option<ID2D1Bitmap1>>,
}

impl Resources {
    pub(super) fn new(
        ctx: &ID2D1DeviceContext5,
        factory: &ID2D1Factory1,
        dwrite: &IDWriteFactory6,
        fonts: &IDWriteFontCollection2,
    ) -> Result<Self> {
        let brush =
            |rgb: u32, alpha: f32| unsafe { ctx.CreateSolidColorBrush(&color(rgb, alpha), None) };
        let format = |size: f32, weight, align| text_format(dwrite, fonts, size, weight, align);
        let (regular, medium) = (DWRITE_FONT_WEIGHT_REGULAR, DWRITE_FONT_WEIGHT_MEDIUM);
        let (lead, centre) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER);
        // The drop box's dotted edge: 2 on, 2 off, in stroke widths.
        let dashes = unsafe {
            let props = D2D1_STROKE_STYLE_PROPERTIES1 {
                dashStyle: D2D1_DASH_STYLE_CUSTOM,
                ..Default::default()
            };
            factory.CreateStrokeStyle(&props, Some(&[2.0, 2.0]))?.into()
        };
        Ok(Resources {
            veil: brush(0x000000, 0.5)?,
            shadow: brush(0x000000, 0.12)?,
            body: brush(BG_DIALOG, 1.0)?,
            strip: brush(BG_B2, 1.0)?,
            well: brush(BG_WELL, 1.0)?,
            plus_bg: brush(BG_PLUS, 1.0)?,
            button_bg: brush(BG_BUTTON, 1.0)?,
            button_off: brush(BG_BUTTON_OFF, 1.0)?,
            primary_bg: brush(BG_ACCENT_BRIGHT, 1.0)?,
            line: brush(LINE_DIALOG, 1.0)?,
            dash: brush(DASH, 1.0)?,
            hover: brush(0xFFFFFF, 0.04)?,
            selection: brush(ACCENT, 0.35)?,
            title_fg: brush(FG_LABEL, 1.0)?,
            heading_fg: brush(FG_HEADING, 1.0)?,
            help_fg: brush(FG_HELP, 1.0)?,
            placeholder_fg: brush(FG_PLACEHOLDER, 1.0)?,
            drop_fg: brush(FG_DROP, 1.0)?,
            label_fg: brush(FG_LABEL, 1.0)?,
            off_fg: brush(FG_BUTTON_OFF, 1.0)?,
            accent: brush(ACCENT, 1.0)?,
            dashes,
            title: format(14.0, regular, lead)?,
            heading: format(16.0, regular, lead)?,
            help: format(14.0, regular, lead)?,
            centred: format(14.0, regular, centre)?,
            field_text: format(14.0, regular, lead)?,
            button: format(14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, centre)?,
            key: format(13.0, medium, lead)?,
            name: format(22.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, lead)?,
            small: format(12.0, regular, lead)?,
            plus: svg(ctx, include_bytes!("../../assets/icons/nav_add.svg"))?,
            wic: None,
            images: HashMap::new(),
        })
    }

    /// The picture at `path`, decoded once with WIC and kept.
    fn image(&mut self, ctx: &ID2D1DeviceContext5, path: &Path) -> Option<ID2D1Bitmap1> {
        if let Some(known) = self.images.get(path) {
            return known.clone();
        }
        if self.wic.is_none() {
            self.wic =
                unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                    .ok();
        }
        let bitmap = self
            .wic
            .as_ref()
            .and_then(|wic| decode(ctx, wic, path).ok());
        self.images.insert(path.to_owned(), bitmap.clone());
        bitmap
    }
}

/// Decodes a PNG, JPEG or any format WIC knows into a bitmap no larger than `MAX_IMAGE`.
fn decode(
    ctx: &ID2D1DeviceContext5,
    wic: &IWICImagingFactory,
    path: &Path,
) -> Result<ID2D1Bitmap1> {
    unsafe {
        let decoder = wic.CreateDecoderFromFilename(
            &HSTRING::from(path),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )?;
        let frame = decoder.GetFrame(0)?;
        let (mut w, mut h) = (0, 0);
        frame.GetSize(&mut w, &mut h)?;
        let mut source: IWICBitmapSource = frame.cast()?;
        if w.max(h) > MAX_IMAGE {
            let s = MAX_IMAGE as f32 / w.max(h) as f32;
            let scaler = wic.CreateBitmapScaler()?;
            let size = |v: u32| ((v as f32 * s).round() as u32).max(1);
            scaler.Initialize(&source, size(w), size(h), WICBitmapInterpolationModeFant)?;
            source = scaler.cast()?;
        }
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;
        ctx.CreateBitmapFromWicBitmap(&converter, None)
    }
}

/// a then b, for D2D's row-vector transforms.
fn then(a: &Matrix3x2, b: &Matrix3x2) -> Matrix3x2 {
    Matrix3x2 {
        M11: a.M11 * b.M11 + a.M12 * b.M21,
        M12: a.M11 * b.M12 + a.M12 * b.M22,
        M21: a.M21 * b.M11 + a.M22 * b.M21,
        M22: a.M21 * b.M12 + a.M22 * b.M22,
        M31: a.M31 * b.M11 + a.M32 * b.M21 + b.M31,
        M32: a.M31 * b.M12 + a.M32 * b.M22 + b.M32,
    }
}

fn scale_at(s: f32, x: f32, y: f32) -> Matrix3x2 {
    Matrix3x2 {
        M11: s,
        M12: 0.0,
        M21: 0.0,
        M22: s,
        M31: x,
        M32: y,
    }
}

fn centre(r: Rect) -> (f32, f32) {
    ((r.l + r.r) / 2.0, (r.t + r.b) / 2.0)
}

impl Painter {
    /// Dims the window and paints the dialog on top. Runs after everything else.
    pub(super) fn paint_dialog(
        &mut self,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        g: &Geometry,
        d: &State,
    ) -> Result<()> {
        let picture = d
            .thumbnail
            .as_deref()
            .and_then(|p| self.dialog.image(ctx, p));
        let rt: &ID2D1RenderTarget = ctx;
        let r = &self.dialog;
        let base = scale_at(g.scale, g.origin.0, g.origin.1);
        let (w, h) = g.size;
        let frame = Rect::new(0.0, 0.0, w, h);
        let hover = |t: Target| s.hover == Some(Hit::Dialog(t));
        unsafe {
            let window = Rect::new(0.0, 0.0, g.window.0, g.window.1);
            rt.FillRectangle(&window.d2d(), &r.veil);
            ctx.SetTransform(&base);
            for spread in [12.0, 8.0, 4.0] {
                rt.FillRoundedRectangle(&rounded(frame.inset(-spread), RADIUS + spread), &r.shadow);
            }
            rt.FillRoundedRectangle(&rounded(frame, RADIUS), &r.body);
            rt.PushAxisAlignedClip(
                &Rect::new(0.0, 0.0, w, TITLE_H).d2d(),
                D2D1_ANTIALIAS_MODE_ALIASED,
            );
            rt.FillRoundedRectangle(&rounded(frame, RADIUS), &r.strip);
            rt.PopAxisAlignedClip();
            self.say(
                rt,
                g.title,
                &r.title,
                Rect::new(PAD, 0.0, w - 60.0, TITLE_H),
                &r.title_fg,
            );
            // The X: 11 DIPs across, thinner than the title bar's.
            let (cx, cy) = (w - 24.0, TITLE_H / 2.0);
            if hover(Target::Close) {
                rt.FillRoundedRectangle(&rounded(Rect::around(cx, cy, 30.0), 4.0), &r.hover);
            }
            let p = |x: f32, y: f32| Vector2 { X: x, Y: y };
            rt.DrawLine(
                p(cx - 5.5, cy - 5.5),
                p(cx + 5.5, cy + 5.5),
                &r.label_fg,
                1.0,
                None,
            );
            rt.DrawLine(
                p(cx + 5.5, cy - 5.5),
                p(cx - 5.5, cy + 5.5),
                &r.label_fg,
                1.0,
                None,
            );

            let plus = |at: Rect, svg_base: &Matrix3x2| {
                let (cx, cy) = centre(at);
                ctx.SetTransform(&then(&translate(cx - 12.0, cy - 12.0), svg_base));
                ctx.DrawSvgDocument(&r.plus.doc);
                ctx.SetTransform(svg_base);
            };
            for part in &g.parts {
                match part {
                    Part::Text(text, style, rect) => {
                        let (format, brush) = match style {
                            Style::Heading => (&r.heading, &r.heading_fg),
                            Style::Help => (&r.help, &r.help_fg),
                            Style::Value => (&r.help, &r.label_fg),
                            Style::Key => (&r.key, &r.heading_fg),
                            Style::Name => (&r.name, &r.heading_fg),
                            Style::Link => (&r.help, &r.accent),
                            Style::Error => (&r.small, &r.accent),
                            Style::DropText => (&r.centred, &r.drop_fg),
                        };
                        self.say(rt, text, format, *rect, brush);
                    }
                    Part::Field(f) => {
                        rt.FillRoundedRectangle(&rounded(*f, 4.0), &r.well);
                        self.paint_field(rt, g, d);
                    }
                    Part::Thumb { r: t, plus: at } => {
                        rt.FillRoundedRectangle(&rounded(*t, 4.0), &r.well);
                        match &picture {
                            Some(bitmap) => {
                                let size = bitmap.GetSize();
                                let brush = ctx.CreateBitmapBrush(bitmap, None, None)?;
                                let sc = (t.w() / size.width).max(t.h() / size.height);
                                brush.SetTransform(&scale_at(
                                    sc,
                                    t.l + (t.w() - size.width * sc) / 2.0,
                                    t.t + (t.h() - size.height * sc) / 2.0,
                                ));
                                rt.FillRoundedRectangle(&rounded(*t, 4.0), &brush);
                            }
                            None => {
                                rt.FillRoundedRectangle(&rounded(*at, 4.0), &r.plus_bg);
                                plus(*at, &base);
                            }
                        }
                        if hover(Target::Thumbnail) {
                            rt.FillRoundedRectangle(&rounded(*t, 4.0), &r.hover);
                        }
                    }
                    Part::Line(l) => rt.FillRectangle(&l.d2d(), &r.line),
                    Part::Drop { r: b, plus: at } => {
                        rt.FillRoundedRectangle(&rounded(*b, 2.0), &r.well);
                        if hover(Target::Files) {
                            rt.FillRoundedRectangle(&rounded(*b, 2.0), &r.hover);
                        }
                        rt.DrawRoundedRectangle(
                            &rounded(b.inset(0.6), 2.0),
                            &r.dash,
                            1.25,
                            &r.dashes,
                        );
                        rt.FillRoundedRectangle(&rounded(*at, 4.0), &r.plus_bg);
                        plus(*at, &base);
                    }
                    Part::File { i, r: row, remove } => {
                        let Some(file) = d.files.get(*i) else {
                            continue;
                        };
                        let name = file.file_name().map_or_else(
                            || file.display().to_string(),
                            |n| n.to_string_lossy().into_owned(),
                        );
                        let text = Rect::new(row.l + 12.0, row.t, remove.l - 8.0, row.b);
                        self.say(rt, &name, &r.help, text, &r.label_fg);
                        let (cx, cy) = centre(*remove);
                        if hover(Target::RemoveFile(*i)) {
                            rt.FillRoundedRectangle(&rounded(*remove, 4.0), &r.hover);
                        }
                        rt.DrawLine(
                            p(cx - 5.0, cy - 5.0),
                            p(cx + 5.0, cy + 5.0),
                            &r.label_fg,
                            1.25,
                            None,
                        );
                        rt.DrawLine(
                            p(cx + 5.0, cy - 5.0),
                            p(cx - 5.0, cy + 5.0),
                            &r.label_fg,
                            1.25,
                            None,
                        );
                        if *i + 1 < d.files.len() {
                            let line = Rect::new(row.l, row.b - 1.0, row.r, row.b);
                            rt.FillRectangle(&line.d2d(), &r.line);
                        }
                    }
                    Part::Button {
                        target,
                        label,
                        r: b,
                        primary,
                        enabled,
                    } => {
                        let (bg, fg) = match (primary, enabled) {
                            (true, true) => (&r.primary_bg, &r.accent),
                            (true, false) => (&r.button_off, &r.off_fg),
                            (false, _) => (&r.button_bg, &r.label_fg),
                        };
                        rt.FillRoundedRectangle(&rounded(*b, 6.0), bg);
                        if *enabled && hover(*target) {
                            rt.FillRoundedRectangle(&rounded(*b, 6.0), &r.hover);
                        }
                        self.say(rt, label, &r.button, *b, fg);
                    }
                    Part::Logo(at) => {
                        let s = at.w() / self.icons.logo.width;
                        ctx.SetTransform(&then(&scale_at(s, at.l, at.t), &base));
                        ctx.DrawSvgDocument(&self.icons.logo.doc);
                        ctx.SetTransform(&base);
                    }
                }
            }
            ctx.SetTransform(&Matrix3x2::identity());
        }
        Ok(())
    }

    unsafe fn say(
        &self,
        rt: &ID2D1RenderTarget,
        text: &str,
        format: &IDWriteTextFormat,
        r: Rect,
        brush: &ID2D1SolidColorBrush,
    ) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            rt.DrawText(
                &utf16,
                format,
                &r.d2d(),
                brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        };
    }

    /// The name field's text or placeholder, selection and caret, in dialog units.
    unsafe fn paint_field(&self, rt: &ID2D1RenderTarget, g: &Geometry, d: &State) {
        let r = &self.dialog;
        let area = g.text_area;
        let f = &d.field;
        unsafe {
            rt.PushAxisAlignedClip(&area.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            if f.text.is_empty() {
                let example = match d.dialog {
                    Dialog::NewProfile => "e.g., Late Night",
                    Dialog::Address(NetKind::Podcast) => "e.g., https://example.com/feed.xml",
                    Dialog::Address(_) => "e.g., https://example.com/live.m3u8",
                    _ => "e.g., Favorite Videos",
                };
                self.say(rt, example, &r.field_text, area, &r.placeholder_fg);
            } else {
                let x0 = area.l - g.text_scroll;
                let sel = f.selection();
                let cy = (area.t + area.b) / 2.0;
                if !sel.is_empty() {
                    let band = Rect::new(
                        x0 + g.stop(sel.start),
                        cy - 10.0,
                        x0 + g.stop(sel.end),
                        cy + 10.0,
                    );
                    rt.FillRectangle(&band.d2d(), &r.selection);
                }
                // Wide enough that the format's ellipsis never cuts the text; the clip does.
                let text = Rect::new(x0, area.t, x0 + 10_000.0, area.b);
                self.say(rt, &f.text, &r.field_text, text, &r.heading_fg);
            }
            if d.caret_visible() {
                let x = area.l - g.text_scroll + g.stop(f.caret()).round();
                let cy = (area.t + area.b) / 2.0;
                rt.FillRectangle(
                    &Rect::new(x, cy - 9.0, x + 1.0, cy + 9.0).d2d(),
                    &r.heading_fg,
                );
            }
            rt.PopAxisAlignedClip();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(text: &str, caret: usize, anchor: usize) -> TextField {
        TextField {
            text: text.into(),
            caret,
            anchor,
        }
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut f = TextField::new("Old name");
        assert_eq!(f.selected(), "Old name");
        f.insert("N");
        f.insert("ew");
        assert_eq!((f.text.as_str(), f.caret()), ("New", 3));
        f.move_to(1, false);
        f.insert("éé");
        assert_eq!((f.text.as_str(), f.caret()), ("Nééew", 5));
        // Line breaks from a paste become spaces; other control characters go.
        f.move_to(usize::MAX, false);
        f.insert("\r\nx\u{7}y");
        assert_eq!(f.text, "Nééew  xy");
    }

    #[test]
    fn deleting_by_character_and_word() {
        let mut f = field("Road Trip 2", 11, 11);
        f.backspace(false);
        assert_eq!(f.text, "Road Trip ");
        f.backspace(true);
        assert_eq!((f.text.as_str(), f.caret()), ("Road ", 5));
        f.move_to(0, false);
        f.delete(true);
        assert_eq!(f.text, "");
        f.backspace(false);
        f.delete(false);
        assert_eq!((f.text.as_str(), f.caret()), ("", 0));

        let mut f = field("añb", 1, 1);
        f.delete(false);
        assert_eq!(f.text, "ab");
        let mut f = field("añb", 3, 3);
        f.backspace(false);
        assert_eq!((f.text.as_str(), f.caret()), ("ab", 1));
    }

    #[test]
    fn caret_moves_and_selects() {
        let mut f = field("one two three", 0, 0);
        f.right(false, true);
        assert_eq!(f.caret(), 4);
        f.right(true, true);
        assert_eq!(f.selected(), "two ");
        f.left(true, false);
        assert_eq!(f.selected(), "two");
        // An arrow without Shift collapses the selection to its side.
        f.left(false, false);
        assert_eq!((f.caret(), f.selected()), (4, ""));
        f.move_to(usize::MAX, true);
        assert_eq!(f.selected(), "two three");
        f.right(false, false);
        assert_eq!(f.caret(), 13);
        f.left(false, true);
        assert_eq!(f.caret(), 8);
        f.move_to(0, true);
        assert_eq!(f.selection(), 0..8);
        f.select_all();
        assert_eq!(f.selected(), "one two three");
        // Positions inside a character snap back to its start.
        let mut f = field("ñ", 0, 0);
        f.move_to(1, false);
        assert_eq!(f.caret(), 0);
        f.right(false, false);
        assert_eq!(f.caret(), 2);
    }

    #[test]
    fn cut_and_paste() {
        let mut f = field("Favorite Videos", 9, 15);
        assert_eq!(f.cut().as_deref(), Some("Videos"));
        assert_eq!(f.cut(), None);
        f.insert("Songs");
        assert_eq!(f.text, "Favorite Songs");
        f.select_all();
        f.insert(&"x".repeat(500));
        assert_eq!(f.text.chars().count(), MAX_NAME);
        f.insert("more");
        assert_eq!(
            f.text.chars().count(),
            MAX_NAME,
            "a full field takes no more"
        );
    }

    #[test]
    fn save_needs_a_valid_name() {
        let mut d = State::new(Dialog::NewPlaylist {
            files: vec![
                "a.mp4".into(),
                "notes.txt".into(),
                "a.mp4".into(),
                "b.mp3".into(),
            ],
        });
        assert_eq!(d.files, [PathBuf::from("a.mp4"), "b.mp3".into()]);
        assert_eq!(d.save(), None);
        d.field.insert("  Mix  ");
        assert_eq!(
            d.save(),
            Some(DialogAction::CreatePlaylist {
                name: "Mix".into(),
                thumbnail: None,
                files: d.files.clone(),
            })
        );
        d.field.insert("/");
        d.edited();
        assert_eq!(d.save(), None);
        assert!(d.error.is_some());

        let mut r = State::new(Dialog::RenamePlaylist("Old".into()));
        r.field.insert("New");
        assert_eq!(
            r.save(),
            Some(DialogAction::RenamePlaylist {
                from: "Old".into(),
                to: "New".into(),
                thumbnail: None
            })
        );
        assert_eq!(State::new(Dialog::About).save(), None);
        let mut p = State::new(Dialog::NewProfile);
        assert_eq!(p.save(), None, "no name yet");
        p.field.insert(" Late Night ");
        assert_eq!(
            p.save(),
            Some(DialogAction::CreateProfile("Late Night".into()))
        );
    }

    #[test]
    fn number_formats() {
        assert_eq!(size_text(512), "512 bytes");
        assert_eq!(size_text(12_345_678), "11.8 MB (12,345,678 bytes)");
        assert_eq!(duration_text(125.4), "2:05");
        assert_eq!(duration_text(3723.0), "1:02:03");
        let a = scale_at(2.0, 10.0, 20.0);
        let m = then(&translate(1.0, 1.0), &a);
        assert_eq!((m.M11, m.M31, m.M32), (2.0, 12.0, 22.0));
    }
}

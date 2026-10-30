//! The library page: sidebar, header with history, breadcrumb, sort, view and search, the media
//! grid or list, and the mini player card. Sizes are DIPs from the design's 1920x1080 library
//! frames ("Videos > Shows > The Wand > S01"), colours sampled from them.

use windows_numerics::Vector2;

use super::*;
use crate::library::{Crumb, Entry, Library, MediaKind, Place};

pub const SECTIONS: [Section; 6] = [
    Section::Home,
    Section::Videos,
    Section::Music,
    Section::Network,
    Section::Playlists,
    Section::Recent,
];
const SIDEBAR_R: f32 = 208.0;
const NAV_TOP: f32 = 76.0;
const NAV_PITCH: f32 = 46.0;
const CONTENT_L: f32 = 238.0;
const CONTENT_RIGHT_GAP: f32 = 22.0;
const HEADER_CY: f32 = 76.0;
const DIVIDER: f32 = 110.0;
const FIRST_ROW: f32 = 131.0;
const TILE_W: f32 = 316.0;
const GAP: f32 = 20.0;
/// Thumbnail bottom to the next row's thumbnail top: the label and the gap.
const BELOW_THUMB: f32 = 79.0;
const LIST_COLUMN: f32 = 415.0;
const LIST_ROW: f32 = 42.0;
const LIST_THUMB: f32 = 32.0;
/// Folders, songs and actions get square tiles, as on the design's Home page.
const SQUARE: f32 = 200.0;
/// The second line under a song: its artist.
const SUBTITLE: f32 = 29.0;
// The player's overlay panel, from the sidebar_states frames: a rail of section icons and a
// list, as tall as the video area. Offsets are from the panel's top-left corner.
const RAIL_W: f32 = 50.0;
const OVERLAY_W: f32 = 427.0;
const OVERLAY_HEADER: f32 = 31.0;
const OVERLAY_TOOLS: f32 = 76.0;
const OVERLAY_DIVIDER: f32 = 105.0;
const OVERLAY_FIRST_ROW: f32 = 119.0;
const SEARCH_W: f32 = 323.0;
/// Room for "..." and its separator in front of a shortened breadcrumb.
const ELIDED: f32 = 40.0;

fn count_text(count: usize) -> String {
    format!("{count} {}", if count == 1 { "item" } else { "items" })
}

/// The design's "Sorting Options", in its order. Title A-Z is the library's own order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortMode {
    UnwatchedFirst,
    WatchedFirst,
    NewestFirst,
    OldestFirst,
    #[default]
    TitleAz,
    TitleZa,
    ArtistAz,
    ArtistZa,
    AlbumAz,
    AlbumZa,
}

impl SortMode {
    pub const ALL: [SortMode; 10] = [
        SortMode::UnwatchedFirst,
        SortMode::WatchedFirst,
        SortMode::NewestFirst,
        SortMode::OldestFirst,
        SortMode::TitleAz,
        SortMode::TitleZa,
        SortMode::ArtistAz,
        SortMode::ArtistZa,
        SortMode::AlbumAz,
        SortMode::AlbumZa,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortMode::UnwatchedFirst => "Unwatched First",
            SortMode::WatchedFirst => "Watched First",
            SortMode::NewestFirst => "Newest First",
            SortMode::OldestFirst => "Oldest First",
            SortMode::TitleAz => "Title A-Z",
            SortMode::TitleZa => "Title Z-A",
            SortMode::ArtistAz => "Artist A-Z",
            SortMode::ArtistZa => "Artist Z-A",
            SortMode::AlbumAz => "Album A-Z",
            SortMode::AlbumZa => "Album Z-A",
        }
    }

    fn uses_time(self) -> bool {
        matches!(self, SortMode::NewestFirst | SortMode::OldestFirst)
    }
}

/// Where an entry's modification time comes from: the file, or the folder on disk.
fn entry_path(e: &Entry) -> Option<&std::path::Path> {
    match e {
        Entry::Media { path, .. } => Some(path),
        Entry::Folder {
            place: Place::Folder(_, dir),
            ..
        } => Some(dir),
        _ => None,
    }
}

/// Reorders `entries`, which come in the library's natural order. Sorts are stable, so ties
/// keep that order; folders stay in front of files except when sorting by time.
fn sort_entries(
    entries: &mut [Entry],
    mode: SortMode,
    time: impl Fn(&Entry) -> Option<std::time::SystemTime>,
) {
    use std::cmp::Reverse;
    // 0 unwatched, 1 started, 2 finished; folders before all of them.
    let watched = |e: &Entry| match e {
        Entry::Media { finished: true, .. } => 2,
        Entry::Media {
            progress: Some(p), ..
        } if *p > 0.0 => 1,
        Entry::Media { .. } => 0,
        _ => -1,
    };
    let artist = |e: &Entry| match e {
        Entry::Media {
            subtitle: Some(a), ..
        } => Some(a.to_lowercase()),
        _ => None,
    };
    let album = |e: &Entry| match e {
        Entry::Media { album: Some(a), .. } => Some(a.to_lowercase()),
        _ => None,
    };
    let folder_first = |e: &Entry| !matches!(e, Entry::Folder { .. });
    match mode {
        SortMode::TitleAz => {}
        SortMode::TitleZa => entries.reverse(),
        SortMode::UnwatchedFirst => entries.sort_by_key(watched),
        SortMode::WatchedFirst => {
            entries.sort_by_key(|e| match watched(e) {
                -1 => -1,
                w => 2 - w,
            });
        }
        // Entries without a time (playlists, music views) go last either way.
        SortMode::NewestFirst => entries.sort_by_key(|e| {
            let t = time(e);
            (t.is_none(), Reverse(t))
        }),
        SortMode::OldestFirst => entries.sort_by_key(|e| {
            let t = time(e);
            (t.is_none(), t)
        }),
        // Files without an artist go last either way.
        SortMode::ArtistAz => entries.sort_by_key(|e| {
            let a = artist(e);
            (folder_first(e), a.is_none(), a)
        }),
        SortMode::ArtistZa => entries.sort_by_key(|e| {
            let a = artist(e);
            (folder_first(e), a.is_none(), Reverse(a))
        }),
        // Likewise files without an album; an album's tracks keep their title order.
        SortMode::AlbumAz => entries.sort_by_key(|e| {
            let a = album(e);
            (folder_first(e), a.is_none(), a)
        }),
        SortMode::AlbumZa => entries.sort_by_key(|e| {
            let a = album(e);
            (folder_first(e), a.is_none(), Reverse(a))
        }),
    }
}

/// Where the library page is looking and how it shows it.
pub struct Browse {
    pub place: Place,
    back: Vec<Place>,
    forward: Vec<Place>,
    all: Vec<Entry>,
    /// What the page shows: `all` filtered by the search and sorted.
    pub entries: Vec<Entry>,
    pub crumbs: Vec<Crumb>,
    pub scroll: f32,
    pub grid: bool,
    pub sort: SortMode,
    pub search: String,
    pub searching: bool,
    /// The place or the library changed: reload the entries.
    pub stale: bool,
    /// The entry whose context menu is open; it stays highlighted under the menu.
    pub context: Option<usize>,
    /// Modification times for the time sorts, read once per refresh.
    times: HashMap<PathBuf, Option<std::time::SystemTime>>,
}

impl Browse {
    pub fn new() -> Self {
        Browse {
            place: Place::Home,
            back: Vec::new(),
            forward: Vec::new(),
            all: Vec::new(),
            entries: Vec::new(),
            crumbs: Vec::new(),
            scroll: 0.0,
            grid: true,
            sort: SortMode::default(),
            search: String::new(),
            searching: false,
            stale: true,
            context: None,
            times: HashMap::new(),
        }
    }

    pub fn go(&mut self, place: Place) {
        if place != self.place {
            self.back.push(std::mem::replace(&mut self.place, place));
            self.forward.clear();
            self.arrived();
        }
    }

    pub fn back(&mut self) {
        if let Some(place) = self.back.pop() {
            self.forward.push(std::mem::replace(&mut self.place, place));
            self.arrived();
        }
    }

    pub fn forward(&mut self) {
        if let Some(place) = self.forward.pop() {
            self.back.push(std::mem::replace(&mut self.place, place));
            self.arrived();
        }
    }

    fn arrived(&mut self) {
        self.scroll = 0.0;
        self.search.clear();
        self.searching = false;
        self.stale = true;
    }

    pub fn refresh(&mut self, library: &Library) {
        self.all = library.entries(&self.place);
        self.crumbs = library.crumbs(&self.place);
        self.stale = false;
        self.context = None;
        self.times.clear();
        self.apply();
    }

    /// Re-filters and re-sorts after the search text or sort order changed.
    pub fn apply(&mut self) {
        let needle = self.search.to_lowercase();
        let searching = !needle.is_empty();
        let (actions, mut rest): (Vec<Entry>, Vec<Entry>) = self
            .all
            .iter()
            .filter(|e| match e {
                // Home's section titles mean nothing once the sections are filtered.
                Entry::Header { .. } => !searching,
                Entry::Action { .. } => !searching,
                e => e.label().to_lowercase().contains(&needle),
            })
            .cloned()
            .partition(|e| matches!(e, Entry::Action { .. }));
        // Home is ordered by section; elsewhere "Add" stays first and the rest is sorted.
        if self.place != Place::Home {
            if self.sort.uses_time() {
                for path in rest.iter().filter_map(entry_path) {
                    if !self.times.contains_key(path) {
                        let t = std::fs::metadata(path).and_then(|m| m.modified()).ok();
                        self.times.insert(path.to_owned(), t);
                    }
                }
            }
            let times = &self.times;
            sort_entries(&mut rest, self.sort, |e| {
                entry_path(e).and_then(|p| times.get(p).copied().flatten())
            });
        }
        self.entries = actions;
        self.entries.append(&mut rest);
    }

    /// Folders and files on the page, as the header counts them.
    fn count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, Entry::Folder { .. } | Entry::Media { .. }))
            .count()
    }

    pub fn set_sort(&mut self, mode: SortMode) {
        self.sort = mode;
        self.apply();
    }

    /// Whether any file on the page has an artist, for the artist sorts.
    pub fn has_artists(&self) -> bool {
        self.all.iter().any(|e| {
            matches!(
                e,
                Entry::Media {
                    subtitle: Some(_),
                    ..
                }
            )
        })
    }

    /// Whether any file on the page has an album, for the album sorts.
    pub fn has_albums(&self) -> bool {
        (self.all.iter()).any(|e| matches!(e, Entry::Media { album: Some(_), .. }))
    }

    /// Whether the header shows "+": adding a folder or creating a playlist.
    pub fn can_add(&self) -> bool {
        matches!(
            self.place,
            Place::Videos | Place::Music | Place::Playlists | Place::Playlist(_)
        )
    }
}

#[derive(Clone, Default)]
pub struct Geometry {
    /// The scrolling area under the header.
    pub content: Rect,
    pub tiles: Vec<Tile>,
    pub max_scroll: f32,
    /// Breadcrumb items shown, by index into `Browse::crumbs`; leading ones that don't fit go.
    crumbs: Vec<(usize, Rect)>,
    elided: bool,
    count: Rect,
    search: Rect,
    /// Thumbnail grid, or the list.
    grid: bool,
    /// Centre line of the row holding the breadcrumb.
    header_cy: f32,
    /// The player's overlay panel, when open.
    pub overlay: Option<Rect>,
}

#[derive(Clone, Copy)]
pub struct Tile {
    pub index: usize,
    pub thumb: Rect,
    pub label: Rect,
    /// The artist line under a song in the grid.
    sub: Option<Rect>,
}

impl Tile {
    fn area(&self) -> Rect {
        let bottom = self.sub.unwrap_or(self.label).b;
        Rect::new(self.thumb.l, self.thumb.t, self.label.r, bottom)
    }

    fn header(index: usize, r: Rect) -> Tile {
        Tile {
            index,
            thumb: Rect::new(r.l, r.t, r.l, r.t),
            label: r,
            sub: None,
        }
    }
}

fn nav_row(i: usize) -> Rect {
    let cy = NAV_TOP + NAV_PITCH * i as f32;
    Rect::new(16.0, cy - 19.0, SIDEBAR_R - 8.0, cy + 19.0)
}

/// The library page; `card` is Some(queue open) while something plays.
pub(super) fn layout(
    p: &Painter,
    width: f32,
    height: f32,
    b: &Browse,
    card: Option<bool>,
) -> Layout {
    let w = width;
    let mut buttons: Vec<(Hit, Rect)> = Layout::window_buttons(w).into();
    for (i, section) in SECTIONS.iter().enumerate() {
        buttons.push((Hit::Nav(*section), nav_row(i)));
    }
    buttons.push((Hit::AddItem, nav_row(SECTIONS.len())));
    buttons.push((Hit::Back, Rect::around(248.0, HEADER_CY, 32.0)));
    buttons.push((Hit::Forward, Rect::around(284.0, HEADER_CY, 32.0)));

    // The right-hand cluster, from the window edge in.
    let add = b.can_add();
    buttons.push((Hit::HeaderMore, Rect::around(w - 32.0, HEADER_CY, 32.0)));
    if add {
        buttons.push((Hit::AddHere, Rect::around(w - 68.0, HEADER_CY, 32.0)));
    }
    let search_r = w - if add { 101.0 } else { 65.0 };
    // The design's 323 DIPs from 1615 wide; narrower windows give the breadcrumb the room.
    let search_w = (w * 0.2).clamp(160.0, SEARCH_W);
    let search = Rect::new(search_r - search_w, 60.0, search_r, 95.0);
    buttons.push((Hit::Search, search));
    buttons.push((
        Hit::ViewMode,
        Rect::around(search.l - 34.0, HEADER_CY, 32.0),
    ));
    buttons.push((Hit::Sort, Rect::around(search.l - 78.0, HEADER_CY, 32.0)));
    let count_r = search.l - 109.0;
    let count = Rect::new(count_r - 120.0, 60.0, count_r, 92.0);

    let count_w = p.measure(&count_text(b.count()), &p.lib.count);
    let room = count_r - count_w - 24.0 - 307.0;
    let (crumbs, elided) = fit_crumbs(p, b, 307.0, HEADER_CY, room, &mut buttons);

    let content = Rect::new(
        CONTENT_L,
        DIVIDER + 1.0,
        w - CONTENT_RIGHT_GAP,
        height - 8.0,
    );
    let (tiles, bottom) = if b.grid {
        grid(&b.entries, content, b.scroll)
    } else {
        list(&b.entries, content, FIRST_ROW - b.scroll)
    };
    let max_scroll = (bottom + b.scroll - content.b).max(0.0);

    // The card's queue gets the room above it, short of the title bar.
    let card = card.map(|queue| {
        let room = height - 6.0 - mini::H - TITLE_BAR - 6.0;
        let queue = if queue { room } else { 0.0 };
        mini::Card::new(w - 6.0 - mini::W, height - 6.0, queue, false)
    });
    Layout {
        page: Page::Library,
        bars: true,
        width,
        height,
        video: card.map_or_else(Rect::default, |c| c.video),
        track: card.map_or_else(Rect::default, |c| c.progress),
        card,
        buttons,
        browse: Geometry {
            content,
            tiles,
            max_scroll,
            crumbs,
            elided,
            count,
            search,
            grid: b.grid,
            header_cy: HEADER_CY,
            overlay: None,
        },
        ..Default::default()
    }
}

/// Adds the overlay panel to a player layout: section rail, history, breadcrumb and the list.
pub(super) fn overlay(p: &Painter, l: &mut Layout, b: &Browse) {
    let c = l.content;
    let panel = Rect::new(c.l, c.t, c.l + OVERLAY_W, c.b);
    let (x0, t) = (panel.l + RAIL_W, panel.t);
    for (i, section) in SECTIONS.iter().enumerate() {
        l.buttons.push((Hit::Nav(*section), rail_icon(panel, i)));
    }
    l.buttons
        .push((Hit::AddItem, rail_icon(panel, SECTIONS.len())));
    let cy = t + OVERLAY_HEADER;
    l.buttons
        .push((Hit::Back, Rect::around(x0 + 32.0, cy, 32.0)));
    l.buttons
        .push((Hit::Forward, Rect::around(x0 + 68.0, cy, 32.0)));
    l.buttons
        .push((Hit::HeaderMore, Rect::around(x0 + 308.0, cy, 32.0)));
    l.buttons
        .push((Hit::OpenLibrary, Rect::around(x0 + 344.0, cy, 32.0)));
    let tools = t + OVERLAY_TOOLS;
    l.buttons
        .push((Hit::Sort, Rect::around(x0 + 273.0, tools, 32.0)));
    l.buttons
        .push((Hit::ViewMode, Rect::around(x0 + 308.0, tools, 32.0)));
    l.buttons
        .push((Hit::Search, Rect::around(x0 + 344.0, tools, 32.0)));
    let room = x0 + 290.0 - (x0 + 90.0);
    let (crumbs, elided) = fit_crumbs(p, b, x0 + 90.0, cy, room, &mut l.buttons);
    let content = Rect::new(
        x0 + 20.0,
        t + OVERLAY_DIVIDER + 1.0,
        panel.r - 10.0,
        panel.b,
    );
    let (tiles, bottom) = list(&b.entries, content, t + OVERLAY_FIRST_ROW - b.scroll);
    l.browse = Geometry {
        content,
        tiles,
        max_scroll: (bottom + b.scroll - content.b).max(0.0),
        crumbs,
        elided,
        count: Rect::new(x0 + 20.0, tools - 14.0, x0 + 200.0, tools + 14.0),
        search: Rect::default(),
        grid: false,
        header_cy: cy,
        overlay: Some(panel),
    };
}

fn rail_icon(panel: Rect, i: usize) -> Rect {
    Rect::around(
        panel.l + RAIL_W / 2.0,
        panel.t + 24.0 + NAV_PITCH * i as f32,
        36.0,
    )
}

/// Places the breadcrumb from `x`, dropping leading crumbs behind "..." until the rest fits `room`.
fn fit_crumbs(
    p: &Painter,
    b: &Browse,
    x: f32,
    cy: f32,
    room: f32,
    buttons: &mut Vec<(Hit, Rect)>,
) -> (Vec<(usize, Rect)>, bool) {
    let widths: Vec<f32> = b
        .crumbs
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let last = i + 1 == b.crumbs.len();
            let format = if last {
                &p.lib.crumb_last
            } else {
                &p.lib.crumb
            };
            p.measure(&c.label, format)
        })
        .collect();
    let step = 21.0; // gap, separator, gap
    let mut first = 0;
    let total = |from: usize| widths[from..].iter().map(|w| w + step).sum::<f32>();
    while first + 1 < widths.len() && total(first) + if first > 0 { ELIDED } else { 0.0 } > room {
        first += 1;
    }
    let mut x = x + if first > 0 { ELIDED } else { 0.0 };
    let mut crumbs = Vec::new();
    for (i, width) in widths.iter().enumerate().skip(first) {
        let r = Rect::new(x, cy - 16.0, x + width.min(room), cy + 16.0);
        crumbs.push((i, r));
        buttons.push((Hit::Crumb(i), r));
        x = r.r + step;
    }
    (crumbs, first > 0)
}

/// Thumbnail tiles: videos 16:9, stretched to fill the row like the design's five columns at
/// 1920; folders, songs and actions square. A change between the two starts a new row.
fn grid(entries: &[Entry], content: Rect, scroll: f32) -> (Vec<Tile>, f32) {
    let columns = ((content.w() + GAP) / (TILE_W + GAP)).floor().max(1.0);
    let wide_w = (content.w() - GAP * (columns - 1.0)) / columns;
    let wide_h = (wide_w * 9.0 / 16.0).round();
    let mut tiles = Vec::new();
    let mut y = FIRST_ROW - scroll;
    // The open row: where its next tile goes, its pitch (0 when none is open), its tile shape.
    let (mut x, mut row_h, mut row_square) = (content.l, 0.0, false);
    for (index, entry) in entries.iter().enumerate() {
        if let Entry::Header { .. } = entry {
            y += row_h;
            (x, row_h) = (content.l, 0.0);
            if index > 0 {
                y += 20.0;
            }
            tiles.push(Tile::header(
                index,
                Rect::new(content.l, y + 11.0, content.r, y + 47.0),
            ));
            y += 61.0;
            continue;
        }
        let square = !matches!(
            entry,
            Entry::Media {
                kind: MediaKind::Video,
                ..
            }
        );
        let (w, h) = if square {
            (SQUARE, SQUARE)
        } else {
            (wide_w, wide_h)
        };
        let sub = matches!(
            entry,
            Entry::Media {
                subtitle: Some(_),
                ..
            }
        );
        if row_h > 0.0 && (square != row_square || x + w > content.r + 0.5) {
            y += row_h;
            (x, row_h) = (content.l, 0.0);
        }
        let thumb = Rect::new(x, y, x + w, y + h);
        let label = Rect::new(x, thumb.b + 15.0, x + w, thumb.b + 43.0);
        tiles.push(Tile {
            index,
            thumb,
            label,
            sub: sub.then(|| Rect::new(x, label.b, x + w, label.b + SUBTITLE)),
        });
        x += w + GAP;
        row_h = f32::max(row_h, h + BELOW_THUMB + if sub { SUBTITLE } else { 0.0 });
        row_square = square;
    }
    let bottom = y + row_h;
    (tiles, bottom)
}

/// The list view: small square thumbnails and names in columns, filled row by row.
fn list(entries: &[Entry], content: Rect, top: f32) -> (Vec<Tile>, f32) {
    let columns = (content.w() / LIST_COLUMN).floor().max(1.0);
    let column_w = content.w() / columns;
    let mut tiles = Vec::new();
    let (mut y, mut column) = (top, 0.0);
    for (index, entry) in entries.iter().enumerate() {
        if let Entry::Header { .. } = entry {
            if column > 0.0 {
                y += LIST_ROW;
                column = 0.0;
            }
            if index > 0 {
                y += 23.0;
            }
            tiles.push(Tile::header(
                index,
                Rect::new(content.l, y, content.r, y + 36.0),
            ));
            y += 44.0;
            continue;
        }
        let x = content.l + column * column_w;
        let top = y + (LIST_ROW - LIST_THUMB) / 2.0;
        tiles.push(Tile {
            index,
            thumb: Rect::new(x, top, x + LIST_THUMB, top + LIST_THUMB),
            label: Rect::new(x + 41.0, y, x + column_w - 16.0, y + LIST_ROW),
            sub: None,
        });
        column += 1.0;
        if column >= columns {
            column = 0.0;
            y += LIST_ROW;
        }
    }
    let bottom = if column > 0.0 { y + LIST_ROW } else { y };
    (tiles, bottom)
}

impl Layout {
    /// Hits inside the player's overlay panel; anything else on it is just the panel.
    pub(super) fn overlay_hit(&self, x: f32, y: f32) -> Option<Option<Hit>> {
        let g = &self.browse;
        g.overlay.filter(|p| p.contains(x, y))?;
        if let Some((hit, _)) = self.buttons.iter().find(|(_, r)| r.contains(x, y)) {
            return Some(Some(*hit));
        }
        if g.content.contains(x, y)
            && let Some(t) = g.tiles.iter().find(|t| t.area().contains(x, y))
        {
            return Some(Some(Hit::Entry(t.index)));
        }
        Some(Some(Hit::Panel))
    }

    pub(super) fn browse_hit(&self, x: f32, y: f32) -> Option<Hit> {
        let g = &self.browse;
        if let Some(target) = self.queue.hit(x, y) {
            return Some(Hit::Queue(target));
        }
        if let Some(hit) = self.card.and_then(|c| c.hit(x, y)) {
            return Some(hit);
        }
        if let Some((hit, _)) = self.buttons.iter().find(|(_, r)| r.contains(x, y)) {
            return Some(*hit);
        }
        if g.content.contains(x, y)
            && let Some(t) = g.tiles.iter().find(|t| t.area().contains(x, y))
        {
            return Some(Hit::Entry(t.index));
        }
        (y < TITLE_BAR).then_some(Hit::Caption)
    }
}

/// Brushes, fonts and icons only the library page uses.
pub(super) struct Resources {
    body: ID2D1SolidColorBrush,
    pub(super) panel: ID2D1SolidColorBrush,
    /// The overlay panel's list, a shade lighter than the page.
    list_bg: ID2D1SolidColorBrush,
    search_bg: ID2D1SolidColorBrush,
    line: ID2D1SolidColorBrush,
    label: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    faint: ID2D1SolidColorBrush,
    veil: ID2D1SolidColorBrush,
    nav: IDWriteTextFormat,
    pub(super) crumb: IDWriteTextFormat,
    pub(super) crumb_last: IDWriteTextFormat,
    count: IDWriteTextFormat,
    placeholder: IDWriteTextFormat,
    name: IDWriteTextFormat,
    header: IDWriteTextFormat,
    /// Home's section titles on the full page.
    section: IDWriteTextFormat,
    active: ActiveIcons,
    open_external: Svg,
    back_off: Svg,
    forward_off: Svg,
    /// The sort icon without its "changed" dot.
    sort_plain: Svg,
    /// Breadcrumb separator, a small right-pointing triangle with its tip at the origin.
    separator: ID2D1PathGeometry1,
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
        let recolor = |bytes: &[u8], from: &str, to: &str| {
            svg(
                ctx,
                String::from_utf8_lossy(bytes).replace(from, to).as_bytes(),
            )
        };
        let sort = String::from_utf8_lossy(include_bytes!("../../assets/icons/sort.svg"))
            .lines()
            .filter(|l| !l.contains("<circle"))
            .collect::<Vec<_>>()
            .join("\n");
        let separator = unsafe {
            let path = factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            sink.BeginFigure(Vector2 { X: -4.0, Y: -3.0 }, D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(Vector2 { X: 0.0, Y: 0.0 });
            sink.AddLine(Vector2 { X: -4.0, Y: 3.0 });
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            path
        };
        let off = FG_DISABLED;
        let off = format!("#{off:06X}");
        Ok(Resources {
            body: brush(BG_BODY, 1.0)?,
            panel: brush(BG_PANEL, 1.0)?,
            list_bg: brush(0x0F0F0F, 1.0)?,
            search_bg: brush(BG_SEARCH, 1.0)?,
            line: brush(LINE, 1.0)?,
            label: brush(FG_LABEL, 1.0)?,
            dim: brush(FG_DIM, 1.0)?,
            faint: brush(FG_FAINT, 1.0)?,
            veil: brush(0x000000, 0.45)?,
            nav: format(
                15.0,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            crumb: format(
                16.0,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            crumb_last: format(
                16.0,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            count: format(
                14.0,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_TEXT_ALIGNMENT_TRAILING,
            )?,
            placeholder: format(
                14.0,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            name: format(
                15.0,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            header: format(
                17.0,
                DWRITE_FONT_WEIGHT_MEDIUM,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            section: format(
                20.0,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            active: ActiveIcons::load(ctx)?,
            open_external: svg(ctx, include_bytes!("../../assets/icons/open_external.svg"))?,
            back_off: recolor(
                include_bytes!("../../assets/icons/arrow_left.svg"),
                "#8F8F8F",
                &off,
            )?,
            forward_off: recolor(
                include_bytes!("../../assets/icons/arrow_right.svg"),
                "#8F8F8F",
                &off,
            )?,
            sort_plain: svg(ctx, sort.as_bytes())?,
            separator,
        })
    }
}

/// Scales a `bw` x `bh` bitmap to cover `r`, centred, as a brush transform.
pub(super) fn cover(r: Rect, bw: f32, bh: f32) -> Matrix3x2 {
    let s = (r.w() / bw).max(r.h() / bh);
    Matrix3x2 {
        M11: s,
        M12: 0.0,
        M21: 0.0,
        M22: s,
        M31: r.l + (r.w() - bw * s) / 2.0,
        M32: r.t + (r.h() - bh * s) / 2.0,
    }
}

impl Painter {
    pub(super) fn paint_library(&mut self, ctx: &ID2D1DeviceContext5, s: &Scene) -> Result<()> {
        let rt: &ID2D1RenderTarget = ctx;
        let l = s.layout;
        unsafe {
            // The card's video is already in the back buffer; a layer keeps everything off it.
            let mask = match l.card {
                Some(card) => {
                    if !s.has_frame {
                        rt.FillRectangle(&card.video.d2d(), &self.black);
                        self.art(rt, ctx, s, card.video)?;
                    }
                    Some(self.frame_geometry(l)?)
                }
                None => None,
            };
            if let Some(mask) = &mask {
                mini::push_mask(ctx, mask)?;
            }
            rt.FillRectangle(&Rect::new(0.0, 0.0, l.width, TITLE_BAR).d2d(), &self.bg);
            rt.FillRectangle(
                &Rect::new(0.0, TITLE_BAR, l.width, l.height).d2d(),
                &self.lib.body,
            );
            self.title_bar(rt, ctx, s);
            self.sidebar(rt, ctx, s);
            self.header(rt, ctx, s);
            self.entries(rt, ctx, s)?;
            if let Some(card) = &l.card {
                self.mini_card(rt, ctx, s, card);
                if let Some(q) = s.queue.filter(|_| card.queue) {
                    self.queue_panel(rt, ctx, s, q);
                }
            }
            if mask.is_some() {
                ctx.PopLayer();
            }
            if let Some(card) = &l.card {
                self.mini_controls(rt, ctx, s, card);
            }
        }
        Ok(())
    }

    unsafe fn text_in(
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

    unsafe fn icon_scaled(&self, ctx: &ID2D1DeviceContext5, svg: &Svg, cx: f32, cy: f32, s: f32) {
        let m = Matrix3x2 {
            M11: s,
            M12: 0.0,
            M21: 0.0,
            M22: s,
            M31: cx - svg.width * s / 2.0,
            M32: cy - svg.width * s / 2.0,
        };
        unsafe {
            ctx.SetTransform(&m);
            ctx.DrawSvgDocument(&svg.doc);
            ctx.SetTransform(&Matrix3x2::identity());
        }
    }

    unsafe fn sidebar(&self, rt: &ID2D1RenderTarget, ctx: &ID2D1DeviceContext5, s: &Scene) {
        let (i, r) = (&self.icons, &self.lib);
        let panel = Rect::new(8.0, 48.0, SIDEBAR_R, s.layout.height - 8.0);
        let current = s.browse.place.section();
        unsafe {
            rt.FillRoundedRectangle(&rounded(panel, 6.0), &r.panel);
            for (n, section) in SECTIONS.iter().enumerate() {
                let active = *section == current;
                let svg = self.section_icon(*section, active);
                self.nav_item(rt, ctx, s, Hit::Nav(*section), n, svg, section.label());
            }
            self.nav_item(rt, ctx, s, Hit::AddItem, SECTIONS.len(), &i.add, "Add item");
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn nav_item(
        &self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        hit: Hit,
        n: usize,
        svg: &Svg,
        label: &str,
    ) {
        let row = nav_row(n);
        let cy = (row.t + row.b) / 2.0;
        unsafe {
            if s.hover == Some(hit) {
                rt.FillRoundedRectangle(&rounded(row, 6.0), &self.hover);
            }
            self.icon(ctx, svg, 26.0, cy - 12.0, false);
            self.text_in(
                rt,
                label,
                &self.lib.nav,
                Rect::new(62.0, cy - 12.0, row.r, cy + 12.0),
                &self.fg,
            );
        }
    }

    unsafe fn header(&self, rt: &ID2D1RenderTarget, ctx: &ID2D1DeviceContext5, s: &Scene) {
        let (i, r, l, b) = (&self.icons, &self.lib, s.layout, s.browse);
        let g = &l.browse;
        let w = l.width;
        unsafe {
            for hit in [
                Hit::Back,
                Hit::Forward,
                Hit::Sort,
                Hit::ViewMode,
                Hit::AddHere,
                Hit::HeaderMore,
            ] {
                self.hover_box(rt, s, hit);
            }
            let back = if b.back.is_empty() {
                &r.back_off
            } else {
                &i.back
            };
            let forward = if b.forward.is_empty() {
                &r.forward_off
            } else {
                &i.forward
            };
            self.icon(ctx, back, 236.0, HEADER_CY - 12.0, false);
            self.icon(ctx, forward, 272.0, HEADER_CY - 12.0, false);

            self.crumbs(rt, s);
            self.text_in(rt, &count_text(b.count()), &r.count, g.count, &r.dim);
            let sort = l.button(Hit::Sort);
            let sort_icon = if b.sort != SortMode::default() {
                &i.sort
            } else {
                &r.sort_plain
            };
            self.icon(ctx, sort_icon, sort.l + 3.0, sort.t + 3.5, false);
            let view = l.button(Hit::ViewMode);
            let view_icon = if b.grid { &i.view_grid } else { &i.view_list };
            self.icon(ctx, view_icon, view.l + 4.0, view.t + 4.0, false);

            let field = g.search;
            rt.FillRoundedRectangle(&rounded(field, 6.0), &r.search_bg);
            self.icon(ctx, &i.search, field.l + 5.0, HEADER_CY - 12.0, false);
            let text_area = Rect::new(field.l + 40.0, field.t, field.r - 10.0, field.b);
            if b.search.is_empty() && !b.searching {
                self.text_in(rt, "Search", &r.placeholder, text_area, &r.faint);
            } else {
                self.text_in(rt, &b.search, &r.name, text_area, &r.label);
                if b.searching {
                    let x = text_area.l + self.measure(&b.search, &r.name) + 1.0;
                    let caret = Rect::new(x, HEADER_CY - 9.0, x + 1.0, HEADER_CY + 9.0);
                    rt.FillRectangle(&caret.d2d(), &r.label);
                }
            }
            if b.can_add() {
                self.icon(ctx, &i.add, w - 80.0, HEADER_CY - 12.0, false);
            }
            self.icon(ctx, &i.more_panel, w - 41.0, HEADER_CY - 9.0, false);
            let divider = Rect::new(CONTENT_L, DIVIDER, w - CONTENT_RIGHT_GAP, DIVIDER + 1.0);
            rt.FillRectangle(&divider.d2d(), &r.line);
        }
    }

    /// Cover art for an audio-only file, fitted inside `area`, or a large music glyph.
    pub(super) unsafe fn art(
        &mut self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        area: Rect,
    ) -> Result<()> {
        let Some(path) = s.art else { return Ok(()) };
        let (cx, cy) = ((area.l + area.r) / 2.0, (area.t + area.b) / 2.0);
        unsafe {
            match self.thumbs.get(path) {
                Some(Some(bitmap)) => {
                    let size = bitmap.GetSize();
                    // A square cover, 60% of the area's height.
                    let side = area.h() * 0.6;
                    let r = Rect::around(cx, cy, side);
                    let brush = ctx.CreateBitmapBrush(bitmap, None, None)?;
                    brush.SetTransform(&cover(r, size.width, size.height));
                    rt.FillRoundedRectangle(&rounded(r, 6.0), &brush);
                }
                known => {
                    if known.is_none() {
                        self.wanted.push(path.to_owned());
                    }
                    let scale = (area.h() / 24.0 * 0.25).max(1.0);
                    self.icon_scaled(ctx, &self.icons.music, cx, cy, scale);
                }
            }
        }
        Ok(())
    }

    /// The breadcrumb, with "..." in front when it was shortened.
    unsafe fn crumbs(&self, rt: &ID2D1RenderTarget, s: &Scene) {
        let (r, g, b) = (&self.lib, &s.layout.browse, s.browse);
        let cy = g.header_cy;
        unsafe {
            if g.elided {
                let x = g.crumbs.first().map_or(0.0, |(_, c)| c.l) - ELIDED;
                let dots = Rect::new(x, cy - 16.0, x + 20.0, cy + 16.0);
                self.text_in(rt, "...", &r.crumb, dots, &self.fg);
                self.separator(rt, dots.r + 13.0, cy);
            }
            for (n, (index, rect)) in g.crumbs.iter().enumerate() {
                let last = n + 1 == g.crumbs.len();
                let (format, brush) = if last {
                    (&r.crumb_last, &r.label)
                } else {
                    (&r.crumb, &self.fg)
                };
                let brush = if s.hover == Some(Hit::Crumb(*index)) && !last {
                    &r.label
                } else {
                    brush
                };
                self.text_in(rt, &b.crumbs[*index].label, format, *rect, brush);
                if !last {
                    self.separator(rt, rect.r + 12.0, cy);
                }
            }
        }
    }

    /// The player's overlay panel: the section rail, then a compact library list.
    pub(super) unsafe fn overlay(
        &mut self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
        panel: Rect,
    ) -> Result<()> {
        let (b, w) = (s.browse, panel.l + RAIL_W);
        unsafe {
            // The panel's left corners follow the video's rounded corners.
            rt.FillRoundedRectangle(&rounded(panel, VIDEO_RADIUS), &self.lib.list_bg);
            let rail = Rect::new(panel.l, panel.t, w, panel.b);
            rt.PushAxisAlignedClip(&rail.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            rt.FillRoundedRectangle(&rounded(panel, VIDEO_RADIUS), &self.lib.panel);
            rt.PopAxisAlignedClip();

            let i = &self.icons;
            let current = b.place.section();
            for (n, section) in SECTIONS.iter().enumerate() {
                let svg = self.section_icon(*section, *section == current);
                let r = rail_icon(panel, n);
                self.hover_box(rt, s, Hit::Nav(*section));
                self.icon(ctx, svg, r.l + 6.0, r.t + 6.0, false);
            }
            let r = rail_icon(panel, SECTIONS.len());
            self.hover_box(rt, s, Hit::AddItem);
            self.icon(ctx, &i.add, r.l + 6.0, r.t + 6.0, false);

            for hit in [
                Hit::Back,
                Hit::Forward,
                Hit::HeaderMore,
                Hit::OpenLibrary,
                Hit::Sort,
                Hit::ViewMode,
                Hit::Search,
            ] {
                self.hover_box(rt, s, hit);
            }
            let l = s.layout;
            let at = |hit| l.button(hit);
            let back = if b.back.is_empty() {
                &self.lib.back_off
            } else {
                &i.back
            };
            let forward = if b.forward.is_empty() {
                &self.lib.forward_off
            } else {
                &i.forward
            };
            self.icon(
                ctx,
                back,
                at(Hit::Back).l + 4.0,
                at(Hit::Back).t + 4.0,
                false,
            );
            self.icon(
                ctx,
                forward,
                at(Hit::Forward).l + 4.0,
                at(Hit::Forward).t + 4.0,
                false,
            );
            self.crumbs(rt, s);
            let more = at(Hit::HeaderMore);
            self.icon(ctx, &i.more_panel, more.l + 7.0, more.t + 7.0, false);
            let open = at(Hit::OpenLibrary);
            // The design mirrors it so the arrow points up and right.
            self.icon(
                ctx,
                &self.lib.open_external,
                open.l + 4.0,
                open.t + 4.0,
                true,
            );

            let g = &l.browse;
            let count = count_text(b.count());
            self.text_in(rt, &count, &self.lib.placeholder, g.count, &self.lib.dim);
            let sort = at(Hit::Sort);
            let sort_icon = if b.sort != SortMode::default() {
                &i.sort
            } else {
                &self.lib.sort_plain
            };
            self.icon(ctx, sort_icon, sort.l + 3.0, sort.t + 3.5, false);
            let view = at(Hit::ViewMode);
            self.icon(ctx, &i.view_list, view.l + 4.0, view.t + 4.0, false);
            let search = at(Hit::Search);
            self.icon(ctx, &i.search, search.l + 4.0, search.t + 4.0, false);
            let divider = Rect::new(
                g.content.l,
                panel.t + OVERLAY_DIVIDER,
                g.content.r,
                panel.t + OVERLAY_DIVIDER + 1.0,
            );
            rt.FillRectangle(&divider.d2d(), &self.lib.line);
            self.entries(rt, ctx, s)?;
        }
        Ok(())
    }

    fn section_icon(&self, section: Section, active: bool) -> &Svg {
        let (i, a) = (&self.icons, &self.lib.active);
        match (section, active) {
            (Section::Home, false) => &i.home,
            (Section::Home, true) => &a.home,
            (Section::Videos, false) => &i.videos,
            (Section::Videos, true) => &a.videos,
            (Section::Music, false) => &i.music,
            (Section::Music, true) => &a.music,
            (Section::Network, false) => &i.network,
            (Section::Network, true) => &a.network,
            (Section::Playlists, false) => &i.playlists,
            (Section::Playlists, true) => &a.playlists,
            (Section::Recent, false) => &i.recent,
            (Section::Recent, true) => &a.recent,
        }
    }

    unsafe fn separator(&self, rt: &ID2D1RenderTarget, x: f32, cy: f32) {
        unsafe {
            rt.SetTransform(&translate(x, cy));
            rt.FillGeometry(&self.lib.separator, &self.lib.dim, None);
            rt.SetTransform(&Matrix3x2::identity());
        }
    }

    unsafe fn entries(
        &mut self,
        rt: &ID2D1RenderTarget,
        ctx: &ID2D1DeviceContext5,
        s: &Scene,
    ) -> Result<()> {
        let g = &s.layout.browse;
        let grid = g.grid;
        unsafe {
            rt.PushAxisAlignedClip(&g.content.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            for tile in g.tiles.iter().filter(|t| t.area().intersects(&g.content)) {
                let hovered =
                    s.hover == Some(Hit::Entry(tile.index)) || s.browse.context == Some(tile.index);
                let radius = if grid { 8.0 } else { 4.0 };
                // The layout is rebuilt whenever the entries change; skip rather than trust it.
                let Some(entry) = s.browse.entries.get(tile.index) else {
                    continue;
                };
                let label_brush = match entry {
                    Entry::Header { label, more } => {
                        let r = tile.label;
                        let brush = if hovered && more.is_some() {
                            &self.lib.label
                        } else {
                            &self.fg
                        };
                        // The full page puts the chevron after a larger title; the list at its end.
                        let format = if grid {
                            &self.lib.section
                        } else {
                            &self.lib.header
                        };
                        self.text_in(rt, label, format, r, brush);
                        if more.is_some() {
                            let x = if grid {
                                r.l + self.measure(label, format) + 17.0
                            } else {
                                r.r - 8.0
                            };
                            let cy = (r.t + r.b) / 2.0;
                            let p = |x: f32, y: f32| Vector2 { X: x, Y: y };
                            rt.DrawLine(p(x - 5.0, cy - 5.0), p(x, cy), brush, 1.5, None);
                            rt.DrawLine(p(x, cy), p(x - 5.0, cy + 5.0), brush, 1.5, None);
                        }
                        continue;
                    }
                    Entry::Media {
                        path,
                        kind,
                        progress,
                        finished,
                        ..
                    } => {
                        match self.thumbs.get(path) {
                            Some(Some(bitmap)) => {
                                let size = bitmap.GetSize();
                                let brush = ctx.CreateBitmapBrush(bitmap, None, None)?;
                                brush.SetTransform(&cover(tile.thumb, size.width, size.height));
                                rt.FillRoundedRectangle(&rounded(tile.thumb, radius), &brush);
                            }
                            known => {
                                if known.is_none() {
                                    self.wanted.push(path.clone());
                                }
                                // Streams never get a picture; they show the Network glyph.
                                let stream = crate::net::is_address(path);
                                let audio = *kind == MediaKind::Audio;
                                let fill = if audio || stream {
                                    &self.lib.panel
                                } else {
                                    &self.black
                                };
                                rt.FillRoundedRectangle(&rounded(tile.thumb, radius), fill);
                                if audio || stream {
                                    let (cx, cy) = (
                                        (tile.thumb.l + tile.thumb.r) / 2.0,
                                        (tile.thumb.t + tile.thumb.b) / 2.0,
                                    );
                                    let scale = if grid { 2.0 } else { 1.0 };
                                    let icon = if stream {
                                        &self.icons.network
                                    } else {
                                        &self.icons.music
                                    };
                                    self.icon_scaled(ctx, icon, cx, cy, scale);
                                }
                            }
                        }
                        // Watched part: along the thumbnail's bottom edge, under it in the list.
                        if let Some(p) = progress.filter(|p| *p > 0.0) {
                            let t = tile.thumb;
                            let bar = if grid {
                                Rect::new(t.l + 2.0, t.b - 4.0, t.l + 2.0 + (t.w() - 4.0) * p, t.b)
                            } else {
                                Rect::new(t.l, t.b, t.l + t.w() * p, t.b + 2.5)
                            };
                            rt.FillRoundedRectangle(&rounded(bar, 1.25), &self.accent);
                        }
                        if hovered {
                            if grid {
                                rt.FillRoundedRectangle(&rounded(tile.thumb, radius), &self.hover);
                            } else {
                                rt.FillRoundedRectangle(
                                    &rounded(tile.thumb, radius),
                                    &self.lib.veil,
                                );
                                let (cx, cy) = (
                                    (tile.thumb.l + tile.thumb.r) / 2.0,
                                    (tile.thumb.t + tile.thumb.b) / 2.0,
                                );
                                self.icon_scaled(ctx, &self.icons.play, cx, cy, 0.67);
                            }
                        }
                        if hovered {
                            &self.accent
                        } else if *finished {
                            &self.lib.dim
                        } else {
                            &self.lib.label
                        }
                    }
                    entry => {
                        // Folders and actions ("Add", "Create playlist") get their glyph on a panel.
                        let svg = match entry {
                            Entry::Folder {
                                place: Place::Playlist(_),
                                ..
                            } => &self.icons.playlists,
                            Entry::Folder { .. } => &self.icons.folder,
                            _ => &self.icons.add,
                        };
                        let t = tile.thumb;
                        let (cx, cy) = ((t.l + t.r) / 2.0, (t.t + t.b) / 2.0);
                        if grid {
                            rt.FillRoundedRectangle(&rounded(t, radius), &self.lib.panel);
                            if hovered {
                                rt.FillRoundedRectangle(&rounded(t, radius), &self.hover);
                            }
                            self.icon_scaled(ctx, svg, cx, cy, 2.0);
                        } else {
                            self.icon_scaled(ctx, svg, cx, cy, 1.0);
                        }
                        if hovered {
                            &self.accent
                        } else {
                            &self.lib.label
                        }
                    }
                };
                // Songs show their artist: on a second line in the grid, "title - artist" in lists.
                let artist = match entry {
                    Entry::Media { subtitle, .. } => subtitle.as_deref(),
                    _ => None,
                };
                match (artist, tile.sub) {
                    (Some(artist), Some(sub)) => {
                        self.text_in(rt, entry.label(), &self.lib.name, tile.label, label_brush);
                        self.text_in(rt, artist, &self.lib.name, sub, &self.lib.dim);
                    }
                    (Some(artist), None) => {
                        let label = format!("{} - {artist}", entry.label());
                        self.text_in(rt, &label, &self.lib.name, tile.label, label_brush);
                    }
                    _ => self.text_in(rt, entry.label(), &self.lib.name, tile.label, label_brush),
                }
            }
            rt.PopAxisAlignedClip();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    fn file(name: &str, progress: Option<f32>, artist: Option<&str>) -> Entry {
        Entry::Media {
            label: name.into(),
            path: PathBuf::from(name),
            kind: MediaKind::Video,
            subtitle: artist.map(Into::into),
            // Each artist made one album, named after them backwards.
            album: artist.map(|a| a.chars().rev().collect()),
            progress,
            finished: progress.is_some_and(|p| p >= 0.95),
        }
    }

    fn dir(name: &str) -> Entry {
        Entry::Folder {
            label: name.into(),
            place: Place::Folder(Section::Videos, PathBuf::from(name)),
        }
    }

    fn labels(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(Entry::label).collect()
    }

    fn sorted(mode: SortMode) -> Vec<String> {
        let mut entries = vec![
            dir("d"),
            file("a", Some(1.0), Some("Zed")),
            file("b", None, None),
            file("c", Some(0.5), Some("Abba")),
        ];
        // Modification times: c newest, then d, a, b; nothing for anything else.
        let epoch = SystemTime::UNIX_EPOCH;
        let age = |e: &Entry| {
            let s = match e.label() {
                "c" => 40,
                "d" => 30,
                "a" => 20,
                "b" => 10,
                _ => return None,
            };
            Some(epoch + Duration::from_secs(s))
        };
        sort_entries(&mut entries, mode, age);
        labels(&entries).into_iter().map(String::from).collect()
    }

    #[test]
    fn sort_modes_order_entries() {
        assert_eq!(sorted(SortMode::TitleAz), ["d", "a", "b", "c"]);
        assert_eq!(sorted(SortMode::TitleZa), ["c", "b", "a", "d"]);
        // Folders stay first; then unwatched, started, finished (or the reverse).
        assert_eq!(sorted(SortMode::UnwatchedFirst), ["d", "b", "c", "a"]);
        assert_eq!(sorted(SortMode::WatchedFirst), ["d", "a", "c", "b"]);
        assert_eq!(sorted(SortMode::NewestFirst), ["c", "d", "a", "b"]);
        assert_eq!(sorted(SortMode::OldestFirst), ["b", "a", "d", "c"]);
        // Files without an artist go last both ways.
        assert_eq!(sorted(SortMode::ArtistAz), ["d", "c", "a", "b"]);
        assert_eq!(sorted(SortMode::ArtistZa), ["d", "a", "c", "b"]);
        // "abbA" before "deZ", and files without an album last both ways.
        assert_eq!(sorted(SortMode::AlbumAz), ["d", "c", "a", "b"]);
        assert_eq!(sorted(SortMode::AlbumZa), ["d", "a", "c", "b"]);
    }

    #[test]
    fn apply_keeps_actions_first_and_home_unsorted() {
        let mut b = Browse::new();
        b.place = Place::Videos;
        b.all = vec![
            Entry::Action {
                label: "Add".into(),
                action: crate::library::Action::AddFolder(Section::Videos),
            },
            dir("x"),
            dir("y"),
        ];
        b.set_sort(SortMode::TitleZa);
        assert_eq!(labels(&b.entries), ["Add", "y", "x"]);
        b.place = Place::Home;
        b.apply();
        assert_eq!(labels(&b.entries), ["Add", "x", "y"]);
        assert!(!b.has_artists());
        assert!(!b.has_albums());
    }
}

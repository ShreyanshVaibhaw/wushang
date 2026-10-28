//! What the player's bars hold, from the design's "Player - customize" frames: the title bar's
//! group after the cone, and the control bar's left, centre and right groups. Customize Player
//! (customize.rs) edits it; it's kept in `toolbar.txt` next to the library state.
//!
//! The control bar is one row while its centre group is empty, the seek bar between the left
//! and right groups, as the design's player frames have it. With something in the centre it
//! takes two rows, the seek bar across the top, as the customize frames show.

use std::fs;
use std::io;
use std::path::Path;

use super::*;

const FILE: &str = "toolbar.txt";
/// The two-row control bar, from the customize frames.
pub const TALL_BAR: f32 = 100.0;
/// The title bar group starts after the cone.
const TITLE_X: f32 = 51.0;
const EDGE: f32 = 20.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Item {
    // In the bars to begin with.
    More,
    Sidebar,
    Folder,
    Previous,
    Play,
    Next,
    Volume,
    Audio,
    Subtitles,
    Settings,
    Queue,
    // The palette's.
    Spacer,
    Divider,
    Speed,
    Forward10,
    Back10,
    Forward15,
    Back15,
    Stop,
    Repeat,
    Shuffle,
    Record,
    Snapshot,
    MiniPlayer,
    Hdr,
    Zoom,
    RightSidebar,
    Quit,
    Preferences,
    Home,
    Thumbnail,
    FileName,
    ArtistName,
    PlaylistName,
    Properties,
    Replay,
    Chapters,
    Favorite,
    Bookmark,
    Bookmarks,
    Playlists,
}

/// The palette in the design's order, then the items the bars start with.
pub const PALETTE: [Item; 41] = [
    Item::Spacer,
    Item::Divider,
    Item::Speed,
    Item::Forward10,
    Item::Back10,
    Item::Forward15,
    Item::Back15,
    Item::Stop,
    Item::Repeat,
    Item::Shuffle,
    Item::Record,
    Item::Snapshot,
    Item::MiniPlayer,
    Item::Hdr,
    Item::Zoom,
    Item::RightSidebar,
    Item::Quit,
    Item::Preferences,
    Item::Home,
    Item::Thumbnail,
    Item::FileName,
    Item::ArtistName,
    Item::Folder,
    Item::PlaylistName,
    Item::Properties,
    Item::Replay,
    Item::Chapters,
    Item::Favorite,
    Item::Bookmark,
    Item::Bookmarks,
    Item::Playlists,
    Item::More,
    Item::Sidebar,
    Item::Previous,
    Item::Play,
    Item::Next,
    Item::Volume,
    Item::Audio,
    Item::Subtitles,
    Item::Settings,
    Item::Queue,
];

impl Item {
    /// The name in toolbar.txt.
    pub fn key(self) -> &'static str {
        match self {
            Item::More => "more",
            Item::Sidebar => "sidebar",
            Item::Folder => "folder",
            Item::Previous => "previous",
            Item::Play => "play",
            Item::Next => "next",
            Item::Volume => "volume",
            Item::Audio => "audio",
            Item::Subtitles => "subtitles",
            Item::Settings => "settings",
            Item::Queue => "queue",
            Item::Spacer => "spacer",
            Item::Divider => "divider",
            Item::Speed => "speed",
            Item::Forward10 => "forward10",
            Item::Back10 => "back10",
            Item::Forward15 => "forward15",
            Item::Back15 => "back15",
            Item::Stop => "stop",
            Item::Repeat => "repeat",
            Item::Shuffle => "shuffle",
            Item::Record => "record",
            Item::Snapshot => "snapshot",
            Item::MiniPlayer => "mini",
            Item::Hdr => "hdr",
            Item::Zoom => "zoom",
            Item::RightSidebar => "right_sidebar",
            Item::Quit => "quit",
            Item::Preferences => "preferences",
            Item::Home => "home",
            Item::Thumbnail => "thumbnail",
            Item::FileName => "file_name",
            Item::ArtistName => "artist_name",
            Item::PlaylistName => "playlist_name",
            Item::Properties => "properties",
            Item::Replay => "replay",
            Item::Chapters => "chapters",
            Item::Favorite => "favorite",
            Item::Bookmark => "bookmark",
            Item::Bookmarks => "bookmarks",
            Item::Playlists => "playlists",
        }
    }

    fn from_key(key: &str) -> Option<Item> {
        PALETTE.iter().copied().find(|i| i.key() == key)
    }

    /// The palette's label.
    pub fn label(self) -> &'static str {
        match self {
            Item::More => "Menu",
            Item::Sidebar => "Library Panel",
            Item::Folder => "Folder Name",
            Item::Previous => "Previous",
            Item::Play => "Play / Pause",
            Item::Next => "Next",
            Item::Volume => "Volume",
            Item::Audio => "Audio Tracks",
            Item::Subtitles => "Subtitles",
            Item::Settings => "Playback Settings",
            Item::Queue => "Queue",
            Item::Spacer => "Spacer",
            Item::Divider => "Divider",
            Item::Speed => "Playback Speed",
            Item::Forward10 => "Forward 10s",
            Item::Back10 => "Backward 10s",
            Item::Forward15 => "Forward 15s",
            Item::Back15 => "Backward 15s",
            Item::Stop => "Stop",
            Item::Repeat => "Repeat",
            Item::Shuffle => "Shuffle",
            Item::Record => "Record",
            Item::Snapshot => "Snapshot",
            Item::MiniPlayer => "Mini Player",
            Item::Hdr => "HDR",
            Item::Zoom => "Zoom",
            Item::RightSidebar => "Right Sidebar",
            Item::Quit => "Quit myvlc",
            Item::Preferences => "Settings",
            Item::Home => "Go Home",
            Item::Thumbnail => "Thumbnail",
            Item::FileName => "File Name",
            Item::ArtistName => "Artist Name",
            Item::PlaylistName => "Playlist Name",
            Item::Properties => "Properties",
            Item::Replay => "Replay",
            Item::Chapters => "Chapters",
            Item::Favorite => "Favorite",
            Item::Bookmark => "Bookmark",
            Item::Bookmarks => "Bookmarks",
            Item::Playlists => "Playlists",
        }
    }

    /// Spacers and dividers can go in as often as wanted; the palette always offers them.
    pub fn repeats(self) -> bool {
        matches!(self, Item::Spacer | Item::Divider)
    }

    /// Width in a bar, and the gap it wants on each side; between two items the larger wins.
    /// These reproduce the design's player frame for the bars myvlc starts with.
    fn size(self) -> (f32, f32) {
        match self {
            Item::More => (28.0, 10.0),
            Item::Sidebar => (28.0, 0.0),
            Item::Divider => (24.0, 0.0),
            Item::Folder => (159.0, 0.0),
            Item::Previous | Item::Next => (30.0, 8.0),
            Item::Play => (54.0, 8.0),
            Item::Volume => (120.0, 20.0),
            Item::Queue => (30.0, 20.0),
            Item::FileName => (180.0, 12.0),
            Item::ArtistName | Item::PlaylistName => (160.0, 12.0),
            Item::Thumbnail => (56.0, 12.0),
            Item::Spacer => (24.0, 0.0),
            _ => (30.0, 12.0),
        }
    }

    /// What a click on it is. The items the bars always had keep their own hits.
    pub fn hit(self) -> Option<Hit> {
        Some(match self {
            Item::More => Hit::More,
            Item::Sidebar => Hit::Sidebar,
            Item::Folder => Hit::Folder,
            Item::Previous => Hit::Previous,
            Item::Play => Hit::Play,
            Item::Next => Hit::Next,
            Item::Volume => Hit::Speaker,
            Item::Audio => Hit::Audio,
            Item::Subtitles => Hit::Subtitles,
            Item::Settings => Hit::Settings,
            Item::Queue => Hit::Playlist,
            Item::Spacer | Item::Divider | Item::FileName => return None,
            item => Hit::Tool(item),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    Title,
    Left,
    Center,
    Right,
}

pub const ZONES: [Zone; 4] = [Zone::Title, Zone::Left, Zone::Center, Zone::Right];

#[derive(Clone, Debug, PartialEq)]
pub struct Toolbar {
    pub title: Vec<Item>,
    pub left: Vec<Item>,
    pub center: Vec<Item>,
    pub right: Vec<Item>,
}

impl Default for Toolbar {
    fn default() -> Self {
        Toolbar {
            title: vec![Item::More, Item::Sidebar, Item::Divider, Item::Folder],
            left: vec![Item::Previous, Item::Play, Item::Next],
            center: Vec::new(),
            right: vec![
                Item::Volume,
                Item::Audio,
                Item::Subtitles,
                Item::Settings,
                Item::Queue,
            ],
        }
    }
}

impl Toolbar {
    pub fn zone(&self, z: Zone) -> &Vec<Item> {
        match z {
            Zone::Title => &self.title,
            Zone::Left => &self.left,
            Zone::Center => &self.center,
            Zone::Right => &self.right,
        }
    }

    pub fn zone_mut(&mut self, z: Zone) -> &mut Vec<Item> {
        match z {
            Zone::Title => &mut self.title,
            Zone::Left => &mut self.left,
            Zone::Center => &mut self.center,
            Zone::Right => &mut self.right,
        }
    }

    pub fn contains(&self, item: Item) -> bool {
        ZONES.iter().any(|z| self.zone(*z).contains(&item))
    }

    /// The palette: everything not in the bars, and spacers and dividers always.
    pub fn palette(&self) -> Vec<Item> {
        PALETTE
            .iter()
            .copied()
            .filter(|i| i.repeats() || !self.contains(*i))
            .collect()
    }

    /// Moves `item` from `from` (None: the palette) to `index` of `zone`.
    pub fn place(&mut self, item: Item, from: Option<(Zone, usize)>, zone: Zone, index: usize) {
        let mut index = index;
        if let Some((z, i)) = from {
            self.zone_mut(z).remove(i);
            if z == zone && i < index {
                index -= 1;
            }
        }
        let list = self.zone_mut(zone);
        list.insert(index.min(list.len()), item);
    }

    /// Takes the item at `index` of `zone` out of the bars, back to the palette.
    pub fn remove(&mut self, zone: Zone, index: usize) {
        let list = self.zone_mut(zone);
        if index < list.len() {
            list.remove(index);
        }
    }

    /// Reads `dir`\toolbar.txt; a missing or unreadable file keeps the design's bars.
    pub fn load(dir: &Path) -> Toolbar {
        let Ok(text) = fs::read_to_string(dir.join(FILE)) else {
            return Toolbar::default();
        };
        let mut t = Toolbar {
            title: Vec::new(),
            left: Vec::new(),
            center: Vec::new(),
            right: Vec::new(),
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let zone = match k.trim() {
                "title" => Zone::Title,
                "left" => Zone::Left,
                "center" => Zone::Center,
                "right" => Zone::Right,
                _ => continue,
            };
            for item in v.split_whitespace().filter_map(Item::from_key) {
                if item.repeats() || !t.contains(item) {
                    t.zone_mut(zone).push(item);
                }
            }
        }
        t
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        let line = |name: &str, items: &[Item]| {
            let keys: Vec<&str> = items.iter().map(|i| i.key()).collect();
            format!("{name} = {}\n", keys.join(" "))
        };
        let text = [
            "# myvlc toolbar v1\n".to_owned(),
            line("title", &self.title),
            line("left", &self.left),
            line("center", &self.center),
            line("right", &self.right),
        ]
        .concat();
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.tmp"));
        fs::write(&tmp, text)?;
        fs::rename(tmp, dir.join(FILE))
    }
}

/// An item where the layout put it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub item: Item,
    pub zone: Zone,
    /// Its index in the zone.
    pub index: usize,
    pub r: Rect,
}

/// Where the bars' parts sit for one window size.
#[derive(Clone, Debug, Default)]
pub struct Bars {
    pub placed: Vec<Placed>,
    /// The control bar's height.
    pub bar: f32,
    pub current: Rect,
    pub track: Rect,
    pub duration: Rect,
    /// Each zone's area, where a dragged item drops; by `ZONES` order.
    pub zones: [Rect; 4],
}

/// Lays `items` out left to right from `x`, centred on `cy`; returns their rects.
fn row(items: &[Item], x: f32, cy: f32, title: bool) -> Vec<Rect> {
    let mut x = x;
    let mut prev: Option<f32> = None;
    items
        .iter()
        .map(|item| {
            let (w, pad) = item.size();
            if let Some(p) = prev {
                x += p.max(pad);
            }
            prev = Some(pad);
            let h = match (title, item) {
                (true, _) => 14.0,
                (false, Item::Play) => 14.0,
                (false, Item::Volume) => 14.0,
                _ => 16.0,
            };
            let r = Rect::new(x, cy - h, x + w, cy + h);
            x += w;
            r
        })
        .collect()
}

fn width(items: &[Item]) -> f32 {
    row(items, 0.0, 0.0, false).last().map_or(0.0, |r| r.r)
}

pub fn layout(t: &Toolbar, w: f32, h: f32) -> Bars {
    let tall = !t.center.is_empty();
    let bar = if tall { TALL_BAR } else { CONTROL_BAR };
    let cy = h - CONTROL_BAR / 2.0;
    let mut placed = Vec::new();
    let mut put = |zone: Zone, items: &[Item], rects: Vec<Rect>| {
        for (index, (item, r)) in items.iter().zip(rects).enumerate() {
            placed.push(Placed {
                item: *item,
                zone,
                index,
                r,
            });
        }
    };

    let title = row(&t.title, TITLE_X, TITLE_BAR / 2.0, true);
    let title_end = title.last().map_or(TITLE_X, |r| r.r);
    put(Zone::Title, &t.title, title);
    let left = row(&t.left, EDGE, cy, false);
    let left_end = left.last().map_or(0.0, |r| r.r);
    put(Zone::Left, &t.left, left);
    let right_w = width(&t.right);
    let right = row(&t.right, w - EDGE - right_w, cy, false);
    let right_start = right.first().map_or(w, |r| r.l);
    put(Zone::Right, &t.right, right);
    let center_w = width(&t.center);
    let center = row(&t.center, ((w - center_w) / 2.0).round(), cy, false);
    let center_span = (
        center.first().map_or(w / 2.0, |r| r.l),
        center.last().map_or(w / 2.0, |r| r.r),
    );
    put(Zone::Center, &t.center, center);

    // The seek bar: between the groups on one row, or across the top row of two.
    let (current, duration) = if tall {
        let sy = h - bar + 27.0;
        (
            Rect::new(EDGE, sy - 14.0, EDGE + 76.0, sy + 14.0),
            Rect::new(w - EDGE - 76.0, sy - 14.0, w - EDGE, sy + 14.0),
        )
    } else {
        (
            Rect::new(left_end + 20.0, cy - 14.0, left_end + 96.0, cy + 14.0),
            Rect::new(right_start - 96.0, cy - 14.0, right_start - 20.0, cy + 14.0),
        )
    };
    let track = Rect::new(current.r + 8.0, current.t, duration.l - 8.0, current.b);

    let row_r = |l: f32, r: f32| Rect::new(l, cy - 20.0, r, cy + 20.0);
    let zones = [
        Rect::new(
            TITLE_X - 4.0,
            2.0,
            title_end.max(TITLE_X + 60.0) + 4.0,
            TITLE_BAR - 2.0,
        ),
        row_r(EDGE - 4.0, (left_end + 4.0).max(EDGE + 60.0)),
        if tall {
            row_r(center_span.0 - 4.0, center_span.1 + 4.0)
        } else {
            // On one row the seek bar takes the centre's place: dropping there makes two.
            Rect::new(current.l, cy - 20.0, duration.r, cy + 20.0)
        },
        row_r((right_start - 4.0).min(w - EDGE - 60.0), w - EDGE + 4.0),
    ];
    Bars {
        placed,
        bar,
        current,
        track,
        duration,
        zones,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_bars_match_the_player_frame() {
        let b = layout(&Toolbar::default(), 1920.0, 1080.0);
        let at = |item| b.placed.iter().find(|p| p.item == item).unwrap().r;
        assert_eq!(at(Item::Previous), Rect::new(20.0, 1037.0, 50.0, 1069.0));
        assert_eq!(at(Item::Play), Rect::new(58.0, 1039.0, 112.0, 1067.0));
        assert_eq!(at(Item::Next), Rect::new(120.0, 1037.0, 150.0, 1069.0));
        assert_eq!(b.current, Rect::new(170.0, 1039.0, 246.0, 1067.0));
        assert_eq!(b.track, Rect::new(254.0, 1039.0, 1492.0, 1067.0));
        assert_eq!(b.duration, Rect::new(1500.0, 1039.0, 1576.0, 1067.0));
        assert_eq!(at(Item::Volume).l, 1596.0);
        assert_eq!(at(Item::Audio).l, 1736.0);
        assert_eq!(at(Item::Subtitles).l, 1778.0);
        assert_eq!(at(Item::Settings).l, 1820.0);
        assert_eq!(at(Item::Queue), Rect::new(1870.0, 1037.0, 1900.0, 1069.0));
        assert_eq!(at(Item::More), Rect::new(51.0, 6.0, 79.0, 34.0));
        assert_eq!(at(Item::Sidebar).l, 89.0);
        assert_eq!(at(Item::Folder).l, 141.0);
        assert_eq!(b.bar, CONTROL_BAR);
    }

    #[test]
    fn a_centre_group_makes_two_rows() {
        let mut t = Toolbar::default();
        let play = t.left.iter().position(|i| *i == Item::Play).unwrap();
        t.place(Item::Play, Some((Zone::Left, play)), Zone::Center, 0);
        let b = layout(&t, 1920.0, 1080.0);
        assert_eq!(b.bar, TALL_BAR);
        let p = b.placed.iter().find(|p| p.item == Item::Play).unwrap();
        assert_eq!((p.r.l + p.r.r) / 2.0, 960.0, "centred");
        assert!(b.track.b < p.r.t, "the seek bar is above the buttons");
        assert_eq!(b.track.l, 104.0);
    }

    #[test]
    fn moving_and_removing_items() {
        let mut t = Toolbar::default();
        assert!(!t.palette().contains(&Item::Play));
        assert!(t.palette().contains(&Item::Divider), "dividers repeat");
        // Moving right within a zone counts the gap it leaves.
        t.place(Item::Previous, Some((Zone::Left, 0)), Zone::Left, 3);
        assert_eq!(t.left, [Item::Play, Item::Next, Item::Previous]);
        t.place(Item::Stop, None, Zone::Right, 0);
        assert_eq!(t.right[0], Item::Stop);
        t.remove(Zone::Right, 0);
        assert!(t.palette().contains(&Item::Stop));
    }

    #[test]
    fn saves_and_loads() {
        let mut t = Toolbar::default();
        t.place(Item::Shuffle, None, Zone::Center, 0);
        t.place(Item::Divider, None, Zone::Right, 1);
        let dir = std::env::temp_dir().join(format!("myvlc-toolbar-{}", std::process::id()));
        t.save(&dir).unwrap();
        assert_eq!(Toolbar::load(&dir), t);
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(Toolbar::load(Path::new("Z:\\nowhere")), Toolbar::default());
    }
}

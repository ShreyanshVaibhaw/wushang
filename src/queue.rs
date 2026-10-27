//! The play queue: the files previous and next step through, with VLC's repeat and shuffle.
//! Pure data, no I/O; main.rs opens whatever it hands back.
//!
//! Duplicates are allowed, as in VLC, so positions are indices into `items`, not paths.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Repeat {
    #[default]
    Off,
    /// Wraps from the last item to the first.
    All,
    /// Replays the current item when it ends; the buttons still move on.
    One,
}

#[derive(Clone, Debug)]
pub struct Queue {
    items: Vec<PathBuf>,
    current: Option<usize>,
    repeat: Repeat,
    shuffle: bool,
    /// Play order as indices into `items` while shuffled; empty otherwise.
    order: Vec<usize>,
    /// Where the current item was in the play order after it was removed, so next and previous
    /// carry on from there.
    gap: Option<usize>,
    /// xorshift64 state.
    rng: u64,
}

impl Default for Queue {
    fn default() -> Self {
        Queue::new(Vec::new(), None)
    }
}

impl Queue {
    /// A queue of `items` playing `current`, which joins the end if it isn't among them.
    pub fn new(mut items: Vec<PathBuf>, current: Option<&Path>) -> Self {
        let current = current.map(|c| match items.iter().position(|p| p == c) {
            Some(i) => i,
            None => {
                items.push(c.to_owned());
                items.len() - 1
            }
        });
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Queue {
            items,
            current,
            repeat: Repeat::Off,
            shuffle: false,
            order: Vec::new(),
            gap: None,
            rng: seed | 1,
        }
    }

    pub fn items(&self) -> &[PathBuf] {
        &self.items
    }

    /// The index of the item playing.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    #[cfg(test)]
    pub fn current_path(&self) -> Option<&Path> {
        self.current.map(|i| self.items[i].as_path())
    }

    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Makes the first item at `path` the current one. False when the queue doesn't have it.
    #[cfg(test)]
    pub fn set_current(&mut self, path: &Path) -> bool {
        match self.items.iter().position(|p| p == path) {
            Some(i) => {
                self.play_at(i);
                true
            }
            None => false,
        }
    }

    /// Makes item `i` current and returns its path. While shuffled it moves up to play right
    /// after the item that was playing, so the rest of the shuffled order still comes after it.
    pub fn play_at(&mut self, i: usize) -> Option<PathBuf> {
        let path = self.items.get(i)?.clone();
        if self.shuffle {
            let from = self.order.iter().position(|&o| o == i)?;
            self.order.remove(from);
            let at = match self.current.and_then(|c| self.pos(c)) {
                Some(p) => p + 1,
                None => self.gap.unwrap_or(0).min(self.order.len()),
            };
            self.order.insert(at, i);
        }
        self.current = Some(i);
        self.gap = None;
        Some(path)
    }

    /// The item to play when the current one ends: the same one under Repeat One, else `next`.
    pub fn on_end(&mut self) -> Option<PathBuf> {
        match (self.repeat, self.current) {
            (Repeat::One, Some(i)) => Some(self.items[i].clone()),
            _ => self.next(),
        }
    }

    /// Moves to the next item in play order. At the end it wraps under Repeat All or One (a
    /// button press always moves on) and stops under Off.
    pub fn next(&mut self) -> Option<PathBuf> {
        let n = self.items.len();
        if n == 0 {
            return None;
        }
        let at = match (self.current.and_then(|c| self.pos(c)), self.gap) {
            (Some(p), _) => p + 1,
            (None, Some(g)) => g,
            (None, None) => 0,
        };
        let at = if at < n {
            at
        } else if self.repeat == Repeat::Off {
            return None;
        } else {
            0
        };
        self.jump(at)
    }

    /// Moves to the previous item in play order, wrapping like `next`.
    pub fn previous(&mut self) -> Option<PathBuf> {
        let n = self.items.len();
        if n == 0 {
            return None;
        }
        let at = match (self.current.and_then(|c| self.pos(c)), self.gap) {
            (Some(p), _) | (None, Some(p)) => p.checked_sub(1),
            (None, None) => None,
        };
        let at = match at {
            Some(at) => at.min(n - 1),
            None if self.repeat == Repeat::Off => return None,
            None => n - 1,
        };
        self.jump(at)
    }

    /// Appends `paths`. While shuffled they land at random places after the current item.
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            self.items.push(path);
            if self.shuffle {
                let from = self.current.and_then(|c| self.pos(c)).map_or(0, |p| p + 1);
                let span = (self.order.len() - from + 1) as u64;
                let at = from + (self.random() % span) as usize;
                self.order.insert(at, self.items.len() - 1);
            }
        }
    }

    /// Inserts `paths` right after the current item (at the start when nothing plays), in the
    /// list and in play order, so they play next.
    #[cfg(test)]
    pub fn insert_next(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let paths: Vec<PathBuf> = paths.into_iter().collect();
        let k = paths.len();
        if k == 0 {
            return;
        }
        let at = self.current.map_or(self.gap.unwrap_or(0), |c| c + 1);
        let at = at.min(self.items.len());
        self.remap(|i| if i >= at { i + k } else { i });
        self.items.splice(at..at, paths);
        if self.shuffle {
            let pos = match self.current.and_then(|c| self.pos(c)) {
                Some(p) => p + 1,
                None => self.gap.unwrap_or(0).min(self.order.len()),
            };
            self.order.splice(pos..pos, at..at + k);
        }
    }

    /// Removes item `i`. Removing the current item leaves nothing current, and next and previous
    /// carry on from where it was.
    pub fn remove(&mut self, i: usize) -> Option<PathBuf> {
        if i >= self.items.len() {
            return None;
        }
        if self.current == Some(i) {
            self.gap = self.pos(i);
            self.current = None;
        }
        if self.shuffle
            && let Some(p) = self.order.iter().position(|&o| o == i)
        {
            self.order.remove(p);
        }
        let path = self.items.remove(i);
        if let Some(c) = &mut self.current
            && *c > i
        {
            *c -= 1;
        }
        for o in &mut self.order {
            if *o > i {
                *o -= 1;
            }
        }
        Some(path)
    }

    /// Moves item `from` so it ends up at index `to`. The play order follows the item.
    pub fn move_item(&mut self, from: usize, to: usize) {
        let n = self.items.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.remap(|i| {
            if i == from {
                to
            } else if from < to && (from + 1..=to).contains(&i) {
                i - 1
            } else if to < from && (to..from).contains(&i) {
                i + 1
            } else {
                i
            }
        });
    }

    /// Moves item `i` to play right after the current one: next in the list, and while shuffled
    /// next in play order too.
    pub fn play_next(&mut self, i: usize) {
        let Some(c) = self.current.filter(|&c| c != i && i < self.items.len()) else {
            return;
        };
        let to = if i < c { c } else { c + 1 };
        self.move_item(i, to);
        if self.shuffle
            && let Some(from) = self.order.iter().position(|&o| o == to)
        {
            self.order.remove(from);
            let at = self.current.and_then(|c| self.pos(c)).map_or(0, |p| p + 1);
            self.order.insert(at, to);
        }
    }

    /// Sorts by file name in natural order ("E2" before "E10"); the current item stays current.
    pub fn sort_by_name(&mut self) {
        let name = |p: &PathBuf| {
            p.file_name()
                .unwrap_or(p.as_os_str())
                .to_string_lossy()
                .to_lowercase()
        };
        let mut idx: Vec<usize> = (0..self.items.len()).collect();
        idx.sort_by(|&a, &b| natural_cmp(&name(&self.items[a]), &name(&self.items[b])));
        // idx[new] = old, so invert it for the old -> new mapping.
        let mut new_of = vec![0; idx.len()];
        for (new, &old) in idx.iter().enumerate() {
            new_of[old] = new;
        }
        self.items = idx.iter().map(|&i| self.items[i].clone()).collect();
        self.remap(|i| new_of[i]);
    }

    #[cfg(test)]
    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.current = None;
        self.gap = None;
    }

    pub fn set_shuffle(&mut self, on: bool) {
        self.shuffle = on;
        self.order.clear();
        self.gap = None;
        if !on {
            return;
        }
        // The current item comes first, so the whole queue plays once before Repeat All wraps.
        let mut rest: Vec<usize> = (0..self.items.len())
            .filter(|&i| Some(i) != self.current)
            .collect();
        for i in (1..rest.len()).rev() {
            let j = (self.random() % (i as u64 + 1)) as usize;
            rest.swap(i, j);
        }
        self.order.extend(self.current);
        self.order.extend(rest);
    }

    pub fn toggle_shuffle(&mut self) {
        self.set_shuffle(!self.shuffle);
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    /// Off, All, One, Off: the order of VLC's repeat button.
    pub fn cycle_repeat(&mut self) {
        self.repeat = match self.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }

    /// Where item `i` is in play order.
    fn pos(&self, i: usize) -> Option<usize> {
        if self.shuffle {
            self.order.iter().position(|&o| o == i)
        } else {
            Some(i)
        }
    }

    /// Makes the item at play-order position `at` current.
    fn jump(&mut self, at: usize) -> Option<PathBuf> {
        let i = if self.shuffle {
            *self.order.get(at)?
        } else {
            at
        };
        self.current = Some(i);
        self.gap = None;
        self.items.get(i).cloned()
    }

    /// Renumbers the current item and the play order after the items moved.
    fn remap(&mut self, f: impl Fn(usize) -> usize) {
        if let Some(c) = &mut self.current {
            *c = f(*c);
        }
        for o in &mut self.order {
            *o = f(*o);
        }
    }

    fn random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

/// Compares with runs of digits as numbers, so "e2" sorts before "e10".
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let number = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = it.peek().filter(|c| c.is_ascii_digit()) {
                        digits.push(*c);
                        it.next();
                    }
                    let trimmed = digits.trim_start_matches('0').to_owned();
                    (trimmed.len(), trimmed)
                };
                let (na, nb) = (number(&mut a), number(&mut b));
                if na != nb {
                    return na.cmp(&nb);
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn queue(current: &str) -> Queue {
        Queue::new(paths(&["a", "b", "c", "d"]), Some(Path::new(current)))
    }

    fn name(p: Option<PathBuf>) -> Option<String> {
        p.map(|p| p.to_string_lossy().into_owned())
    }

    fn names(q: &Queue) -> Vec<String> {
        q.items()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn new_finds_or_appends_the_current_file() {
        let q = queue("c");
        assert_eq!(q.current(), Some(2));
        let q = Queue::new(paths(&["a", "b"]), Some(Path::new("z")));
        assert_eq!(names(&q), ["a", "b", "z"]);
        assert_eq!(q.current(), Some(2));
        let q = Queue::new(paths(&["a"]), None);
        assert_eq!(q.current(), None);
        assert_eq!(q.repeat(), Repeat::Off);
        assert!(!q.shuffle());
    }

    #[test]
    fn next_and_previous_stop_at_the_ends_without_repeat() {
        let mut q = queue("c");
        assert_eq!(name(q.next()).as_deref(), Some("d"));
        assert_eq!(q.next(), None);
        assert_eq!(q.on_end(), None);
        assert_eq!(q.current(), Some(3), "a failed next keeps the current item");
        let mut q = queue("b");
        assert_eq!(name(q.previous()).as_deref(), Some("a"));
        assert_eq!(q.previous(), None);
        assert_eq!(q.current(), Some(0));
    }

    #[test]
    fn repeat_all_wraps_both_ways() {
        let mut q = queue("d");
        q.set_repeat(Repeat::All);
        assert_eq!(name(q.on_end()).as_deref(), Some("a"));
        assert_eq!(name(q.previous()).as_deref(), Some("d"));
        assert_eq!(q.current(), Some(3));
    }

    #[test]
    fn repeat_one_replays_on_end_but_buttons_move_on() {
        let mut q = queue("b");
        q.set_repeat(Repeat::One);
        assert_eq!(name(q.on_end()).as_deref(), Some("b"));
        assert_eq!(name(q.on_end()).as_deref(), Some("b"));
        assert_eq!(name(q.next()).as_deref(), Some("c"));
        let mut q = queue("d");
        q.set_repeat(Repeat::One);
        assert_eq!(name(q.next()).as_deref(), Some("a"), "wraps like All");
        assert_eq!(name(q.previous()).as_deref(), Some("d"));
    }

    #[test]
    fn cycle_repeat_goes_off_all_one() {
        let mut q = queue("a");
        q.cycle_repeat();
        assert_eq!(q.repeat(), Repeat::All);
        q.cycle_repeat();
        assert_eq!(q.repeat(), Repeat::One);
        q.cycle_repeat();
        assert_eq!(q.repeat(), Repeat::Off);
    }

    /// Plays forward from the current item until `next` stops, collecting what played.
    fn play_through(q: &mut Queue) -> Vec<String> {
        let mut played = vec![name(q.current_path().map(Path::to_owned)).unwrap()];
        while let Some(p) = q.next() {
            played.push(name(Some(p)).unwrap());
            assert!(played.len() <= q.items().len(), "looped without repeat");
        }
        played
    }

    #[test]
    fn shuffle_keeps_the_current_item_and_plays_each_once() {
        let items: Vec<PathBuf> = (0..20).map(|i| PathBuf::from(format!("{i:02}"))).collect();
        let mut q = Queue::new(items, Some(Path::new("07")));
        q.toggle_shuffle();
        assert!(q.shuffle());
        assert_eq!(q.current(), Some(7));
        let played = play_through(&mut q);
        assert_eq!(played[0], "07");
        let mut ascending = played[1..].to_vec();
        ascending.sort();
        assert_ne!(played[1..], ascending, "not shuffled");
        let mut sorted = played.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 20, "an item was skipped or played twice");
        // Previous walks the same order back; at the start it stops, or wraps under All.
        let last = q.current();
        q.previous();
        assert_ne!(q.current(), last);
        q.set_repeat(Repeat::All);
        let at_end = q.current();
        q.next();
        let wrapped = q.next();
        assert!(wrapped.is_some() && q.current() != at_end);
        q.toggle_shuffle();
        assert!(!q.shuffle() && q.order.is_empty());
    }

    #[test]
    fn shuffle_at_the_start_stops_or_wraps() {
        let mut q = queue("a");
        q.toggle_shuffle();
        assert_eq!(q.previous(), None);
        q.set_repeat(Repeat::All);
        let last = *q.order.last().unwrap();
        q.previous();
        assert_eq!(q.current(), Some(last));
        q.set_repeat(Repeat::One);
        assert_eq!(
            q.on_end().as_deref(),
            q.items().get(last).map(PathBuf::as_path)
        );
    }

    #[test]
    fn play_at_while_shuffled_keeps_the_rest_of_the_order() {
        let mut q = queue("a");
        q.toggle_shuffle();
        let clicked = q.order[3];
        q.play_at(clicked);
        assert_eq!(q.order[1], clicked);
        assert_eq!(q.current(), Some(clicked));
        let mut sorted = q.order.clone();
        sorted.sort();
        assert_eq!(sorted, [0, 1, 2, 3]);
        assert!(q.set_current(Path::new("a")));
        assert!(!q.set_current(Path::new("nope")));
    }

    #[test]
    fn play_next_comes_right_after_the_current_item() {
        let mut q = queue("b");
        q.play_next(3);
        assert_eq!(names(&q), ["a", "b", "d", "c"]);
        q.play_next(0);
        assert_eq!(names(&q), ["b", "a", "d", "c"]);
        assert_eq!(q.current(), Some(0));
        // Shuffled, it is next in play order wherever the list puts it.
        let items: Vec<PathBuf> = (0..20).map(|i| PathBuf::from(format!("{i:02}"))).collect();
        let mut q = Queue::new(items, Some(Path::new("07")));
        q.toggle_shuffle();
        let last = *q.order.last().unwrap();
        let path = q.items()[last].clone();
        q.play_next(last);
        assert_eq!(q.next(), Some(path));
        let mut sorted = q.order.clone();
        sorted.sort();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn add_appends_and_allows_duplicates() {
        let mut q = queue("b");
        q.add(paths(&["e", "a"]));
        assert_eq!(names(&q), ["a", "b", "c", "d", "e", "a"]);
        assert_eq!(q.current(), Some(1));
        q.toggle_shuffle();
        q.add(paths(&["f"]));
        let mut sorted = q.order.clone();
        sorted.sort();
        assert_eq!(sorted, (0..7).collect::<Vec<_>>());
        assert_eq!(q.order[0], 1, "the current item stays first");
    }

    #[test]
    fn insert_next_plays_next() {
        let mut q = queue("b");
        q.insert_next(paths(&["x", "y"]));
        assert_eq!(names(&q), ["a", "b", "x", "y", "c", "d"]);
        assert_eq!(q.current(), Some(1));
        assert_eq!(name(q.next()).as_deref(), Some("x"));

        let mut q = queue("b");
        q.toggle_shuffle();
        q.insert_next(paths(&["x"]));
        assert_eq!(name(q.next()).as_deref(), Some("x"));
        assert_eq!(q.order.len(), 5);

        let mut q = Queue::new(paths(&["a"]), None);
        q.insert_next(paths(&["x"]));
        assert_eq!(names(&q), ["x", "a"]);
    }

    #[test]
    fn remove_renumbers_and_carries_on_from_a_removed_current_item() {
        let mut q = queue("c");
        assert_eq!(name(q.remove(0)).as_deref(), Some("a"));
        assert_eq!(q.current(), Some(1));
        assert_eq!(q.remove(9), None);
        // Removing the playing item: next is the one after it, previous the one before.
        q.remove(1);
        assert_eq!(names(&q), ["b", "d"]);
        assert_eq!(q.current(), None);
        assert_eq!(name(q.clone().next()).as_deref(), Some("d"));
        assert_eq!(name(q.previous()).as_deref(), Some("b"));

        let mut q = queue("b");
        q.toggle_shuffle();
        q.remove(1);
        assert_eq!(q.order.len(), 3);
        assert!(q.order.iter().all(|&o| o < 3));
        assert!(
            q.next().is_some(),
            "plays what was second in the shuffled order"
        );
    }

    #[test]
    fn move_item_keeps_the_current_item() {
        let mut q = queue("b");
        q.move_item(1, 3);
        assert_eq!(names(&q), ["a", "c", "d", "b"]);
        assert_eq!(q.current(), Some(3));
        q.move_item(0, 3);
        assert_eq!(names(&q), ["c", "d", "b", "a"]);
        assert_eq!(q.current(), Some(2));
        q.move_item(3, 0);
        assert_eq!(names(&q), ["a", "c", "d", "b"]);
        assert_eq!(q.current(), Some(3));
        q.move_item(0, 9);
        assert_eq!(names(&q), ["a", "c", "d", "b"]);

        let mut q = queue("a");
        q.toggle_shuffle();
        let order: Vec<String> = q.order.iter().map(|&i| names(&q)[i].clone()).collect();
        q.move_item(0, 2);
        let after: Vec<String> = q.order.iter().map(|&i| names(&q)[i].clone()).collect();
        assert_eq!(order, after, "the play order follows the items");
    }

    #[test]
    fn sort_by_name_is_natural_and_keeps_the_current_item() {
        let mut q = Queue::new(
            paths(&[r"x\E10.mkv", r"y\e2.mkv", r"x\E1.mkv"]),
            Some(Path::new(r"y\e2.mkv")),
        );
        q.sort_by_name();
        assert_eq!(names(&q), [r"x\E1.mkv", r"y\e2.mkv", r"x\E10.mkv"]);
        assert_eq!(q.current(), Some(1));
        assert_eq!(name(q.next()).as_deref(), Some(r"x\E10.mkv"));
    }

    #[test]
    fn clear_empties_everything() {
        let mut q = queue("b");
        q.toggle_shuffle();
        q.clear();
        assert!(q.is_empty());
        assert_eq!(q.current(), None);
        assert_eq!(q.next(), None);
        assert_eq!(q.previous(), None);
        assert_eq!(q.on_end(), None);
    }
}

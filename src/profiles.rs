//! Profiles: named sets of settings, picked from the "..." menu's Profile submenu (the design's
//! "Profiles" frame). settings.txt always holds the settings in force and `profile.txt` names the
//! profile they belong to. Each profile keeps its own copy in `profiles\<name>\settings.txt`,
//! written when wushang switches to it or away from it.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use crate::prefs::Prefs;

const FILE: &str = "profile.txt";
const DIR: &str = "profiles";
/// The design's profiles, there from the start.
pub const DEFAULTS: [&str; 4] = ["Movies", "Shows", "Music", "Work"];

pub struct Profiles {
    dir: PathBuf,
    /// The defaults, then the ones the user made, by name.
    names: Vec<String>,
    current: String,
}

impl Profiles {
    /// Reads which profile is in force and which ones exist under `dir`.
    pub fn load(dir: &Path) -> Profiles {
        let current = fs::read_to_string(dir.join(FILE))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| crate::library::check_name(s).is_ok())
            .unwrap_or_else(|| DEFAULTS[0].to_owned());
        let mut made: Vec<String> = fs::read_dir(dir.join(DIR))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        made.sort_by_key(|n| n.to_lowercase());
        let mut names: Vec<String> = Vec::new();
        let all = DEFAULTS.iter().map(|s| s.to_string()).chain(made);
        for name in all.chain([current.clone()]) {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                names.push(name);
            }
        }
        Profiles {
            dir: dir.to_owned(),
            names,
            current,
        }
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn current(&self) -> &str {
        &self.current
    }

    /// Switches to `name`: `live` is kept as the current profile's settings, and the new
    /// profile's come back, a copy of `live` the first time it is used.
    pub fn switch(&mut self, name: &str, live: &Prefs) -> io::Result<Prefs> {
        let name = (self.names.iter())
            .find(|n| n.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or(ErrorKind::NotFound)?;
        let folder = self.dir.join(DIR).join(&name);
        let mut new = if folder.join("settings.txt").exists() {
            Prefs::load(&folder)
        } else {
            live.clone()
        };
        // Where the window sits and how loud it plays belong to the machine, not the profile.
        new.window = live.window;
        new.volume = live.volume;
        live.save(&self.dir.join(DIR).join(&self.current))?;
        new.save(&folder)?;
        fs::write(self.dir.join(FILE), &name)?;
        self.current = name;
        Ok(new)
    }

    /// Adds a profile that starts as a copy of `live`, and switches to it.
    pub fn create(&mut self, name: &str, live: &Prefs) -> io::Result<Prefs> {
        let name = name.trim();
        crate::library::check_name(name)?;
        if self.names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "There's already a profile with that name.",
            ));
        }
        self.names.push(name.to_owned());
        let switched = self.switch(name, live);
        if switched.is_err() {
            self.names.pop();
        }
        switched
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_profile_keeps_its_own_settings() {
        let dir = std::env::temp_dir().join(format!("wushang-profiles-{}", std::process::id()));
        _ = fs::remove_dir_all(&dir);
        let mut p = Profiles::load(&dir);
        assert_eq!(p.names(), DEFAULTS);
        assert_eq!(p.current(), "Movies");

        let movies = Prefs {
            speed: 1.5,
            volume: 0.4,
            ..Prefs::default()
        };
        // A profile's first use copies the settings in force.
        let mut music = p.switch("music", &movies).unwrap();
        assert_eq!((p.current(), music.speed), ("Music", 1.5));
        music.speed = 1.0;
        music.theme = 1;
        music.volume = 0.8;
        // Back to Movies: its speed returns, the volume stays where Music left it.
        let back = p.switch("Movies", &music).unwrap();
        assert_eq!((back.speed, back.theme, back.volume), (1.5, 0, 0.8));

        let work = p.create(" Late Night ", &back).unwrap();
        assert_eq!(work, back);
        let again = p.create("late night", &back).unwrap_err();
        assert_eq!(again.kind(), ErrorKind::AlreadyExists);
        assert!(p.create("a/b", &back).is_err());

        // A fresh start reads the same list and current profile, and Music its own settings.
        let mut p = Profiles::load(&dir);
        assert_eq!(p.names().last().unwrap(), "Late Night");
        assert_eq!(p.current(), "Late Night");
        assert_eq!(p.switch("Music", &work).unwrap().theme, 1);
        fs::remove_dir_all(dir).unwrap();
    }
}

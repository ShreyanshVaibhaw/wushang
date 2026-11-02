//! Tells macOS that wushang opens video and audio files, so it is listed in Finder's "Open With"
//! and in Get Info's "Open with". The types themselves are the bundle's `CFBundleDocumentTypes`
//! (assets/macos/Info.plist; a test checks it lists every one of `VIDEO_EXTS` and `AUDIO_EXTS`),
//! ranked Alternate so wushang never takes a type over unasked; Launch Services reads them when
//! the app is copied in or first opened. Registering the running bundle as well makes the copy that ran last
//! the one Finder knows about, as the Windows registry entries did, with no admin rights.

use std::path::{Path, PathBuf};

use objc2_core_foundation::CFURL;

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    /// Launch Services: adds the bundle at `url` to its database. `update` false leaves a bundle
    /// it already has, unchanged since, alone, so this costs next to nothing on later starts.
    fn LSRegisterURL(url: &CFURL, update: u8) -> i32;
}

/// Registers the running app bundle, unless it already is. A bare exe (`cargo run`) has no
/// bundle and no document types, so it registers nothing.
pub fn register() {
    let Some(bundle) = std::env::current_exe().ok().as_deref().and_then(bundle_of) else {
        return;
    };
    let Some(url) = CFURL::from_directory_path(&bundle) else {
        return;
    };
    let status = unsafe { LSRegisterURL(&url, 0) };
    if status != 0 {
        eprintln!("assoc: cannot register {} ({status})", bundle.display());
    }
}

/// The `.app` folder an exe at `<name>.app/Contents/MacOS/<exe>` belongs to.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_app = app
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"));
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && is_app)
        .then(|| app.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_bundle_an_exe_runs_from() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/wushang.app/Contents/MacOS/wushang"
            )),
            Some(PathBuf::from("/Applications/wushang.app"))
        );
        assert_eq!(bundle_of(Path::new("/Users/me/target/debug/wushang")), None);
        assert_eq!(bundle_of(Path::new("/wushang")), None);
    }

    /// Info.plist claims every extension the library plays, each once, and nothing else.
    #[test]
    fn the_bundle_lists_every_media_type() {
        use crate::library::{AUDIO_EXTS, VIDEO_EXTS};
        let plist = include_str!("../assets/macos/Info.plist");
        let mut listed: Vec<&str> = plist
            .split("<string>")
            .filter_map(|s| s.split_once("</string>"))
            .map(|(ext, _)| ext)
            .filter(|ext| plist_ext(plist, ext))
            .collect();
        listed.sort();
        let mut wanted: Vec<&str> = VIDEO_EXTS.iter().chain(AUDIO_EXTS).copied().collect();
        wanted.sort();
        assert_eq!(listed, wanted);
    }

    /// Whether `ext` sits in a CFBundleTypeExtensions array of `plist`.
    fn plist_ext(plist: &str, ext: &str) -> bool {
        plist
            .split("<key>CFBundleTypeExtensions</key>")
            .skip(1)
            .any(|types| {
                let array = types.split("</array>").next().unwrap_or("");
                array.contains(&format!("<string>{ext}</string>"))
            })
    }
}

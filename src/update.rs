//! Updates from GitHub Releases. At start (unless Settings > General turns it off) wushang asks for
//! the latest release and offers it when its version is newer than this build.
//!
//! wushang never replaces its own files: a running app that rewrites itself is what malware does,
//! and on macOS it would also break the bundle's signature. The download, the release's disk
//! image (or its zip when it has no image), goes to Downloads and is shown in Finder, for the
//! user to drag into Applications over the old copy.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;
use std::{fs, thread};

use crate::{net, snapshot};

const LATEST: &str = "https://api.github.com/repos/ShreyanshVaibhaw/wushang/releases/latest";
/// The release assets: `wushang-<version>-macos-arm64.dmg` and `.zip`, from this repository's
/// releases and nowhere else.
const ASSET_PREFIX: &str = "https://github.com/ShreyanshVaibhaw/wushang/releases/download/";
const DMG_SUFFIX: &str = "-macos-arm64.dmg";
const ZIP_SUFFIX: &str = "-macos-arm64.zip";
/// Largest download accepted; a release is about 40 MB.
const MAX_DOWNLOAD: usize = 200 << 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Without the tag's "v", like "1.0.1".
    pub version: String,
    /// The file to download: the disk image, or the zip when the release has no image.
    pub zip: String,
    /// The Windows build's installer; never set on macOS, kept so callers read the same.
    pub msi: Option<String>,
}

/// What an update download saved.
pub enum Saved {
    /// An installer to run when wushang exits. Never made on macOS; kept so main reads the same
    /// on both builds.
    #[allow(dead_code)]
    Installer(PathBuf),
    /// The download, in Downloads and selected in Finder, for the user to copy over.
    Zip(PathBuf),
}

/// Asks GitHub for the latest release on a thread of its own. The receiver gets it only when it
/// is newer than this build; offline or rate-limited, nothing comes.
pub fn check() -> Receiver<Release> {
    let (send, recv) = channel();
    thread::spawn(
        move || match net::fetch_text(LATEST, Duration::from_secs(10)) {
            Ok(json) => {
                if let Some(release) = newer(&json, env!("CARGO_PKG_VERSION")) {
                    _ = send.send(release);
                }
            }
            Err(e) => eprintln!("update: {e}"),
        },
    );
    recv
}

/// The release in GitHub's JSON, when it is newer than `current` and has a macOS download: its
/// disk image, else its zip.
fn newer(json: &str, current: &str) -> Option<Release> {
    let tag = json_strings(json, "tag_name").into_iter().next()?;
    let version = tag.trim_start_matches('v').to_owned();
    let urls = json_strings(json, "browser_download_url");
    let asset = |suffix: &str| {
        urls.iter()
            .find(|u| u.starts_with(ASSET_PREFIX) && u.ends_with(suffix) && !u.contains(".."))
            .cloned()
    };
    let zip = asset(DMG_SUFFIX).or_else(|| asset(ZIP_SUFFIX))?;
    (version_parts(&version)? > version_parts(current)?).then_some(Release {
        version,
        zip,
        msi: None,
    })
}

/// "1.2.3" as [1, 2, 3], padded to three parts. A pre-release tag like "1.2.0-beta" is None, so
/// it is never offered.
fn version_parts(v: &str) -> Option<[u32; 3]> {
    let mut parts = [0; 3];
    for (i, p) in v.split('.').enumerate() {
        *parts.get_mut(i)? = p.parse().ok()?;
    }
    Some(parts)
}

/// Every string value of `key` in `json`, unescaped.
// ponytail: a scan for "key": "value", not a JSON parser; GitHub's release JSON needs no more.
fn json_strings(json: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find(&needle) {
        rest = &rest[i + needle.len()..];
        let Some(value) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        let Some(value) = value.trim_start().strip_prefix('"') else {
            continue;
        };
        let mut s = String::new();
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some(c) => s.push(c),
                    None => break,
                },
                c => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

/// Downloads `release` on a thread into Downloads and selects it in Finder. The receiver gets
/// what was saved, or why it failed.
pub fn download(release: &Release) -> Receiver<Result<Saved, String>> {
    let (send, recv) = channel();
    let release = release.clone();
    thread::spawn(move || _ = send.send(save(&release)));
    recv
}

fn save(release: &Release) -> Result<Saved, String> {
    let suffix = if release.zip.ends_with(DMG_SUFFIX) {
        DMG_SUFFIX
    } else {
        ZIP_SUFFIX
    };
    let dir = snapshot::known_folder(snapshot::Folder::Downloads)
        .map_err(|e| format!("Can't find your Downloads folder: {e}"))?;
    let bytes = net::fetch(&release.zip, Duration::from_secs(60), MAX_DOWNLOAD)
        .map_err(|e| format!("Can't download the update: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("Can't save the update: {e}"))?;
    // Named from the version, which is only digits and dots, never from the address.
    let path = dir.join(format!("wushang-{}{suffix}", release.version));
    fs::write(&path, bytes).map_err(|e| format!("Can't save the update: {e}"))?;
    // Finder opens on Downloads with the file selected, as Explorer did on Windows.
    if let Err(e) = Command::new("/usr/bin/open").arg("-R").arg(&path).spawn() {
        eprintln!("update: can't show the download in Finder: {e}");
    }
    Ok(Saved::Zip(path))
}

/// Would run a downloaded installer once wushang exits; macOS updates have none.
pub fn install_on_exit(_installer: PathBuf) {}

/// The last thing main does. Nothing is left to run on macOS.
pub fn finish() {}

/// Run at start, for the update leftovers the Windows build cleans up. A macOS update leaves
/// nothing behind but the download the user asked for.
pub fn clean_up() {}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"url":"x","tag_name": "v1.2.0","assets":[
        {"name":"a.txt","browser_download_url":"https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/a.txt"},
        {"name":"w","browser_download_url":"https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/wushang-1.2.0-windows-x64.zip"},
        {"name":"z","browser_download_url" : "https:\/\/github.com\/ShreyanshVaibhaw\/wushang\/releases\/download\/v1.2.0\/wushang-1.2.0-macos-arm64.zip"},
        {"name":"d","browser_download_url":"https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/wushang-1.2.0-macos-arm64.dmg"}]}"#;

    #[test]
    fn a_newer_release_is_offered_with_its_disk_image() {
        let r = newer(JSON, "1.0.0").unwrap();
        assert_eq!(r.version, "1.2.0");
        assert_eq!(
            r.zip,
            "https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/wushang-1.2.0-macos-arm64.dmg"
        );
        assert_eq!(r.msi, None);
        // Without an image, the zip; with neither (a Windows-only release), nothing.
        let no_dmg = JSON.replace("arm64.dmg", "arm64.dmg.sig");
        assert_eq!(
            newer(&no_dmg, "1.0.0").unwrap().zip,
            "https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/wushang-1.2.0-macos-arm64.zip"
        );
        assert_eq!(newer(&no_dmg.replace("macos", "linux"), "1.0.0"), None);
        assert_eq!(newer(JSON, "1.2.0"), None, "same version");
        assert_eq!(
            newer(JSON, "1.10.0"),
            None,
            "older, compared by number not text"
        );
        assert_eq!(
            newer(&JSON.replace("v1.2.0\"", "v1.2.0-beta\""), "1.0.0"),
            None
        );
        // Only this repository's release downloads are offered.
        let elsewhere = JSON
            .replace("ShreyanshVaibhaw\\/wushang", "evil\\/wushang")
            .replace("ShreyanshVaibhaw/wushang", "evil/wushang");
        assert_eq!(newer(&elsewhere, "1.0.0"), None);
        // An image whose address climbs out of the releases is passed over for the zip.
        let climbing = JSON.replace("download/v1.2.0/wushang", "download/../x/wushang");
        assert!(newer(&climbing, "1.0.0").unwrap().zip.ends_with(ZIP_SUFFIX));
    }
}

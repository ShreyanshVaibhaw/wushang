//! Updates from GitHub Releases. At start (unless Settings > General turns it off) wushang asks for
//! the latest release and offers it when its version is newer than this build.
//!
//! wushang never replaces its own files: a running exe that renames itself, writes a downloaded
//! exe in its place and starts it is what droppers do, and antivirus (Bitdefender's Advanced
//! Threat Defense) took 1.0.2's updater for one and deleted the whole install. Instead:
//! - A copy the MSI installed downloads the next MSI, and once wushang has exited, Windows' own
//!   msiexec upgrades it and starts it again (installer/wushang.wxs).
//! - A copy someone unzipped saves the next zip to Downloads for them to copy over.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;
use std::{env, fs, thread};

use windows::Win32::UI::Shell::FOLDERID_Downloads;

use crate::{assoc, net, snapshot};

const LATEST: &str = "https://api.github.com/repos/ShreyanshVaibhaw/wushang/releases/latest";
/// The release assets: `wushang-<version>-windows-x64.zip` and `.msi`, from this repository's
/// releases and nowhere else.
const ASSET_PREFIX: &str = "https://github.com/ShreyanshVaibhaw/wushang/releases/download/";
const ZIP_SUFFIX: &str = "-windows-x64.zip";
const MSI_SUFFIX: &str = "-windows-x64.msi";
/// Largest download accepted; a release is about 15 MB.
const MAX_DOWNLOAD: usize = 200 << 20;

/// The installer msiexec runs once this process has saved and exited.
static INSTALL: Mutex<Option<PathBuf>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Without the tag's "v", like "1.0.1".
    pub version: String,
    /// The zip to download.
    pub zip: String,
    /// The installer to run instead, when an MSI installed this copy and the release has one.
    pub msi: Option<String>,
}

/// What an update download saved.
pub enum Saved {
    /// The installer, run when wushang exits.
    Installer(PathBuf),
    /// The zip, in Downloads for the user to copy over.
    Zip(PathBuf),
}

/// Asks GitHub for the latest release on a thread of its own. The receiver gets it only when it
/// is newer than this build; offline or rate-limited, nothing comes.
pub fn check() -> Receiver<Release> {
    let (send, recv) = channel();
    thread::spawn(
        move || match net::fetch_text(LATEST, Duration::from_secs(10)) {
            Ok(json) => {
                if let Some(release) = newer(&json, env!("CARGO_PKG_VERSION"), installed()) {
                    _ = send.send(release);
                }
            }
            Err(e) => eprintln!("update: {e}"),
        },
    );
    recv
}

/// The release in GitHub's JSON, when it is newer than `current` and has a Windows zip. Its
/// installer comes along only for a copy the MSI `installed`.
fn newer(json: &str, current: &str, installed: bool) -> Option<Release> {
    let tag = json_strings(json, "tag_name").into_iter().next()?;
    let version = tag.trim_start_matches('v').to_owned();
    let urls = json_strings(json, "browser_download_url");
    let asset = |suffix: &str| {
        urls.iter()
            .find(|u| u.starts_with(ASSET_PREFIX) && u.ends_with(suffix) && !u.contains(".."))
            .cloned()
    };
    let zip = asset(ZIP_SUFFIX)?;
    let msi = asset(MSI_SUFFIX).filter(|_| installed);
    (version_parts(&version)? > version_parts(current)?).then_some(Release { version, zip, msi })
}

/// Whether the MSI installed this copy: it records its folder, which is this exe's.
fn installed() -> bool {
    let Some(dir) = assoc::read(r"Software\wushang", "InstallDir") else {
        return false;
    };
    let exe = env::current_exe().ok();
    let here = exe.as_deref().and_then(Path::parent);
    here.is_some_and(|h| {
        h.to_string_lossy()
            .eq_ignore_ascii_case(dir.trim_end_matches('\\'))
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

/// Downloads `release` on a thread: its installer into a temporary folder, or else its zip into
/// Downloads. The receiver gets what was saved, or why it failed.
pub fn download(release: &Release) -> Receiver<Result<Saved, String>> {
    let (send, recv) = channel();
    let release = release.clone();
    thread::spawn(move || _ = send.send(save(&release)));
    recv
}

fn save(release: &Release) -> Result<Saved, String> {
    let (url, dir, suffix) = match &release.msi {
        Some(msi) => (msi, update_dir(), MSI_SUFFIX),
        None => (
            &release.zip,
            snapshot::known_folder(&FOLDERID_Downloads)
                .map_err(|e| format!("Can't find your Downloads folder: {e}"))?,
            ZIP_SUFFIX,
        ),
    };
    let bytes = net::fetch(url, Duration::from_secs(60), MAX_DOWNLOAD)
        .map_err(|e| format!("Can't download the update: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("Can't save the update: {e}"))?;
    // Named from the version, which is only digits and dots, never from the address.
    let path = dir.join(format!("wushang-{}{suffix}", release.version));
    fs::write(&path, bytes).map_err(|e| format!("Can't save the update: {e}"))?;
    Ok(match release.msi {
        Some(_) => Saved::Installer(path),
        None => Saved::Zip(path),
    })
}

/// Where a downloaded installer waits; the next start deletes it.
fn update_dir() -> PathBuf {
    env::temp_dir().join("wushang-update")
}

/// Runs `msi` once this process has finished.
pub fn install_on_exit(msi: PathBuf) {
    *INSTALL.lock().unwrap() = Some(msi);
}

/// The last thing main does: hands a downloaded installer to msiexec, which closes nothing (this
/// process is gone by then), shows its progress bar, and starts the new wushang.
pub fn finish() {
    let Some(msi) = INSTALL.lock().unwrap().take() else {
        return;
    };
    // By full path, so an msiexec.exe beside wushang or in the current folder can't stand in.
    let msiexec = PathBuf::from(env::var_os("SystemRoot").unwrap_or("C:\\Windows".into()))
        .join("System32\\msiexec.exe");
    if let Err(e) = Command::new(msiexec)
        .arg("/i")
        .arg(&msi)
        .arg("/passive")
        .spawn()
    {
        eprintln!("update: can't start the installer: {e}");
    }
}

/// What 1.0.2's updater renamed a replaced file to, for the next start to delete. The suffix is
/// wushang's own because wushang may sit in a folder of other people's files (unzipped into
/// Downloads), and only its leftovers may go.
const ASIDE: &str = ".wushang-old";

/// The files 1.0.0 and 1.0.1 shipped, which their updater set aside as `<name>.old`.
const OLD_ASIDE: [&str; 18] = [
    "wushang.exe",
    "avcodec-63.dll",
    "avdevice-63.dll",
    "avformat-63.dll",
    "avutil-61.dll",
    "swresample-7.dll",
    "swscale-10.dll",
    "libass-9.dll",
    "Inter-OFL.txt",
    "LICENSE",
    "THIRD_PARTY_NOTICES.md",
    "Apache-2.0.txt",
    "FreeType-FTL.txt",
    "HarfBuzz.txt",
    "LGPL-2.1.txt",
    "dav1d.txt",
    "libass.txt",
    "zlib.txt",
];

/// Whether `name` is a file an update set aside, which the next start may delete.
fn is_aside(name: &str) -> bool {
    name.ends_with(ASIDE)
        || name
            .strip_suffix(".old")
            .is_some_and(|n| OLD_ASIDE.iter().any(|o| o.eq_ignore_ascii_case(n)))
}

/// Deletes a downloaded installer that has run, and the files an older version's update set
/// aside beside the exe.
pub fn clean_up() {
    _ = fs::remove_dir_all(update_dir());
    // ponytail: the rest only for updates made by 1.0.0 to 1.0.2; drop once nobody runs them.
    let Some(dir) = env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_owned))
    else {
        return;
    };
    for d in [dir.clone(), dir.join("licenses")] {
        let Ok(entries) = fs::read_dir(d) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_aside)
            {
                _ = fs::remove_file(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"url":"x","tag_name": "v1.2.0","assets":[
        {"name":"a.txt","browser_download_url":"https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/a.txt"},
        {"name":"z","browser_download_url" : "https:\/\/github.com\/ShreyanshVaibhaw\/wushang\/releases\/download\/v1.2.0\/wushang-1.2.0-windows-x64.zip"},
        {"name":"m","browser_download_url":"https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/wushang-1.2.0-windows-x64.msi"}]}"#;

    #[test]
    fn a_newer_release_is_offered_with_its_zip() {
        let r = newer(JSON, "1.0.0", false).unwrap();
        assert_eq!(r.version, "1.2.0");
        assert_eq!(
            r.zip,
            "https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/wushang-1.2.0-windows-x64.zip"
        );
        assert_eq!(r.msi, None, "an unzipped copy gets the zip");
        assert_eq!(
            newer(JSON, "1.0.0", true).unwrap().msi.as_deref(),
            Some(
                "https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/wushang-1.2.0-windows-x64.msi"
            )
        );
        assert_eq!(newer(JSON, "1.2.0", false), None, "same version");
        assert_eq!(
            newer(JSON, "1.10.0", false),
            None,
            "older, compared by number not text"
        );
        assert_eq!(
            newer(&JSON.replace("v1.2.0\"", "v1.2.0-beta\""), "1.0.0", false),
            None
        );
        // Only this repository's release downloads install.
        let elsewhere = JSON
            .replace("ShreyanshVaibhaw\\/wushang", "evil\\/wushang")
            .replace("ShreyanshVaibhaw/wushang", "evil/wushang");
        assert_eq!(newer(&elsewhere, "1.0.0", true), None);
    }

    #[test]
    fn only_update_leftovers_are_cleaned_up() {
        assert!(is_aside("wushang.exe.wushang-old"));
        assert!(is_aside("avcodec-63.dll.old"));
        assert!(is_aside("LICENSE.old"));
        assert!(!is_aside("report.docx.old"));
        assert!(!is_aside("notes.old"));
        assert!(!is_aside("wushang.exe"));
    }
}

//! Updates from GitHub Releases. At start (unless Settings > General turns it off) wushang asks for
//! the latest release and offers it when its version is newer than this build.
//!
//! Installing downloads the release's zip, unpacks it with Windows' own tar.exe, and swaps the
//! files in beside the running exe. Windows lets a running exe and its loaded DLLs be renamed, so
//! each old file steps aside as `<name>.wushang-old` and the new one takes its place; the next
//! start deletes those. Then wushang quits as usual (saving everything) and starts again.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;
use std::{env, fs, thread};

use crate::net;

const LATEST: &str = "https://api.github.com/repos/ShreyanshVaibhaw/wushang/releases/latest";
/// The release asset to install: `wushang-<version>-windows-x64.zip`, from this repository's
/// releases and nowhere else.
const ASSET_PREFIX: &str = "https://github.com/ShreyanshVaibhaw/wushang/releases/download/";
const ASSET_SUFFIX: &str = "-windows-x64.zip";
/// Largest download accepted; a release is about 15 MB.
const MAX_DOWNLOAD: usize = 200 << 20;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The exe to start once this process has saved and exited, after an update.
static RESTART: Mutex<Option<PathBuf>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Without the tag's "v", like "1.0.1".
    pub version: String,
    /// The zip to download.
    pub url: String,
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

/// The release in GitHub's JSON, when it is newer than `current` and has a Windows zip.
fn newer(json: &str, current: &str) -> Option<Release> {
    let tag = json_strings(json, "tag_name").into_iter().next()?;
    let version = tag.trim_start_matches('v').to_owned();
    let url = json_strings(json, "browser_download_url")
        .into_iter()
        .find(|u| u.starts_with(ASSET_PREFIX) && u.ends_with(ASSET_SUFFIX) && !u.contains(".."))?;
    (version_parts(&version)? > version_parts(current)?).then_some(Release { version, url })
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

/// Downloads and puts `release` in place on a thread. The receiver gets the exe to start once
/// wushang has exited, or why it failed (nothing is changed then).
pub fn install(release: &Release) -> Receiver<Result<PathBuf, String>> {
    let (send, recv) = channel();
    let release = release.clone();
    thread::spawn(move || _ = send.send(download_and_swap(&release)));
    recv
}

fn download_and_swap(release: &Release) -> Result<PathBuf, String> {
    // Taken before the swap renames the running exe.
    let exe = env::current_exe().map_err(|e| format!("Can't find wushang.exe: {e}"))?;
    let dir = exe.parent().ok_or("Can't find wushang's folder.")?;
    let temp = env::temp_dir().join(format!("wushang-update-{}", release.version));
    _ = fs::remove_dir_all(&temp);
    let files = temp.join("files");
    fs::create_dir_all(&files).map_err(|e| format!("Can't make a folder for the update: {e}"))?;

    let zip = temp.join("update.zip");
    let bytes = net::fetch(&release.url, Duration::from_secs(60), MAX_DOWNLOAD)
        .map_err(|e| format!("Can't download the update: {e}"))?;
    fs::write(&zip, bytes).map_err(|e| format!("Can't save the update: {e}"))?;
    // Windows' tar (bsdtar) reads zip; a Git or MSYS tar earlier on PATH wouldn't.
    let tar = PathBuf::from(env::var_os("SystemRoot").unwrap_or("C:\\Windows".into()))
        .join("System32\\tar.exe");
    let unpacked = Command::new(tar)
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&files)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .is_ok_and(|s| s.success());
    if !unpacked || !files.join("wushang.exe").is_file() {
        return Err("The update didn't unpack. Try again later.".into());
    }
    swap(&files, dir).map_err(|e| format!("Can't replace wushang's files: {e}"))?;
    _ = fs::remove_dir_all(&temp);
    Ok(exe)
}

/// Every file under `dir`, relative to it.
fn files_under(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        for entry in fs::read_dir(dir.join(&rel))? {
            let entry = entry?;
            let rel = rel.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                stack.push(rel);
            } else {
                out.push(rel);
            }
        }
    }
    Ok(out)
}

/// What a replaced file is renamed to until the next start deletes it. The suffix is wushang's
/// own because wushang may sit in a folder of other people's files (unzipped into Downloads),
/// and only its leftovers may go.
const ASIDE: &str = ".wushang-old";

/// The files 1.0.0 and 1.0.1 shipped, which their updater set aside as `<name>.old`.
// ponytail: only for updates made by those versions; drop once nobody runs them.
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

fn aside(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(ASIDE);
    path.with_file_name(name)
}

/// Whether `name` is a file an update set aside, which the next start may delete.
fn is_aside(name: &str) -> bool {
    name.ends_with(ASIDE)
        || name
            .strip_suffix(".old")
            .is_some_and(|n| OLD_ASIDE.iter().any(|o| o.eq_ignore_ascii_case(n)))
}

/// Puts every file from `from` in `to`, moving the ones it replaces aside. On any failure the
/// files already placed are taken back out and the old ones restored.
fn swap(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut placed: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let result = files_under(from).and_then(|files| {
        for rel in files {
            let target = to.join(&rel);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let old = target.exists().then(|| aside(&target));
            if let Some(old) = &old {
                _ = fs::remove_file(old);
                fs::rename(&target, old)?;
            }
            placed.push((target.clone(), old));
            fs::copy(from.join(&rel), &target)?;
        }
        Ok(())
    });
    if result.is_err() {
        for (target, old) in placed.into_iter().rev() {
            _ = fs::remove_file(&target);
            if let Some(old) = old {
                _ = fs::rename(old, &target);
            }
        }
    }
    result
}

/// Deletes the files a previous update set aside beside the exe. One still held by the exiting
/// old process stays until next time.
pub fn clean_up() {
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

/// Starts `exe` once this process has finished, after an installed update.
pub fn restart_with(exe: PathBuf) {
    *RESTART.lock().unwrap() = Some(exe);
}

/// The last thing main does: starts the updated wushang, if an update asked for it.
pub fn finish() {
    if let Some(exe) = RESTART.lock().unwrap().take()
        && let Err(e) = Command::new(&exe).spawn()
    {
        eprintln!("update: can't start {}: {e}", exe.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"url":"x","tag_name": "v1.2.0","assets":[
        {"name":"a.txt","browser_download_url":"https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/a.txt"},
        {"name":"z","browser_download_url" : "https:\/\/github.com\/ShreyanshVaibhaw\/wushang\/releases\/download\/v1.2.0\/wushang-1.2.0-windows-x64.zip"}]}"#;

    #[test]
    fn a_newer_release_is_offered_with_its_zip() {
        let r = newer(JSON, "1.0.0").unwrap();
        assert_eq!(r.version, "1.2.0");
        assert_eq!(
            r.url,
            "https://github.com/ShreyanshVaibhaw/wushang/releases/download/v1.2.0/wushang-1.2.0-windows-x64.zip"
        );
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
        // Only this repository's release downloads install.
        let elsewhere = JSON.replace("ShreyanshVaibhaw\\/wushang", "evil\\/wushang");
        assert_eq!(newer(&elsewhere, "1.0.0"), None);
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

    #[test]
    fn swapping_moves_old_files_aside_and_adds_new_ones() {
        let root = env::temp_dir().join(format!("wushang-swap-{}", std::process::id()));
        _ = fs::remove_dir_all(&root);
        let (new, app) = (root.join("new"), root.join("app"));
        fs::create_dir_all(new.join("licenses")).unwrap();
        fs::create_dir_all(&app).unwrap();
        fs::write(new.join("wushang.exe"), "new exe").unwrap();
        fs::write(new.join("licenses/a.txt"), "licence").unwrap();
        fs::write(app.join("wushang.exe"), "old exe").unwrap();
        swap(&new, &app).unwrap();
        assert_eq!(
            fs::read_to_string(app.join("wushang.exe")).unwrap(),
            "new exe"
        );
        assert_eq!(
            fs::read_to_string(app.join("wushang.exe.wushang-old")).unwrap(),
            "old exe"
        );
        assert_eq!(
            fs::read_to_string(app.join("licenses/a.txt")).unwrap(),
            "licence"
        );
        _ = fs::remove_dir_all(&root);
    }
}

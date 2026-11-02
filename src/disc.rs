//! Discs, VLC's "Open Disc": DVD and Blu-ray folders (a disc's files copied off, or the disc in
//! its drive) and Blu-ray images. A disc plays from an address of its own, like a stream:
//! `dvd://<folder>` or `bluray://<folder or .iso>`.
//!
//! wushang reads the discs' own index files and plays the main title's files back to back
//! through FFmpeg's `concat` protocol. For DVD, each title set's IFO says how long its main
//! program chain runs and where its chapters start (libdvdnav would make the FFmpeg build GPL).
//! For Blu-ray, each playlist (BDMV/PLAYLIST/*.mpls) lists its clips and chapter marks, and the
//! longest is the film, the title libbluray would pick; Homebrew's FFmpeg has no libbluray. An
//! .iso is mounted read-only with hdiutil, out of Finder's sight, and read like a folder. No
//! menus either way, and encrypted discs (CSS, AACS) don't play.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

const DVD: &str = "dvd://";
const BLURAY: &str = "bluray://";

pub fn is_disc_path(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|p| p.starts_with(DVD) || p.starts_with(BLURAY))
}

/// The disc address for `path`: a folder holding VIDEO_TS or BDMV (or one of those folders
/// itself), or a Blu-ray .iso. None when it is no disc.
pub fn address(path: &Path) -> Option<PathBuf> {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_ascii_uppercase())
    };
    let iso = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("iso"));
    if iso && path.is_file() {
        return Some(PathBuf::from(format!("{BLURAY}{}", path.display())));
    }
    let root = match name(path).as_deref() {
        Some("VIDEO_TS" | "BDMV") => path.parent().unwrap_or(path),
        _ => path,
    };
    if root.join("BDMV").join("index.bdmv").is_file() {
        return Some(PathBuf::from(format!("{BLURAY}{}", root.display())));
    }
    if dvd(root).is_some() {
        return Some(PathBuf::from(format!("{DVD}{}", root.display())));
    }
    None
}

/// What the disc is called: its volume label for a drive, else its folder's or image's name.
pub fn title(path: &Path) -> String {
    let inner = Path::new(strip(path));
    let label = volume_label(inner);
    label
        .or_else(|| inner.file_stem().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "Disc".to_owned())
}

/// The folder or image an address points at.
fn strip(path: &Path) -> &str {
    let p = path.to_str().unwrap_or_default();
    p.strip_prefix(DVD)
        .or_else(|| p.strip_prefix(BLURAY))
        .unwrap_or(p)
}

/// The URL FFmpeg opens for a disc address.
pub fn url(path: &Path) -> Result<String, String> {
    let title = main_title(path)?;
    let parts: Vec<String> = title.vobs.iter().map(|v| v.display().to_string()).collect();
    Ok(format!("concat:{}", parts.join("|")))
}

/// The main title of the DVD or Blu-ray at a disc address, with the length and chapters its
/// files don't carry themselves.
pub fn dvd_title(path: &Path) -> Option<Title> {
    main_title(path).ok()
}

fn main_title(path: &Path) -> Result<Title, String> {
    let p = path.to_str().unwrap_or_default();
    if let Some(root) = p.strip_prefix(BLURAY) {
        let root = Path::new(root);
        if root.is_file() {
            // Most images are Blu-rays, but a DVD's image mounts and plays just as well.
            let at = mount(root)?;
            return bluray(&at)
                .or_else(|| dvd(&at))
                .ok_or_else(|| "no DVD or Blu-ray video in that image".to_owned());
        }
        return bluray(root).ok_or_else(|| "no Blu-ray video on that disc".to_owned());
    }
    let root = p.strip_prefix(DVD).ok_or("not a disc address")?;
    dvd(Path::new(root)).ok_or_else(|| "no DVD video in that folder".to_owned())
}

/// A disc's main title: its files in order (a DVD's VOBs, a Blu-ray's clips), its length, and
/// where its chapters start, in seconds.
#[derive(Debug, PartialEq)]
pub struct Title {
    pub vobs: Vec<PathBuf>,
    pub length: f64,
    pub chapters: Vec<f64>,
}

/// The longest title of the DVD whose VIDEO_TS is in `root`.
pub fn dvd(root: &Path) -> Option<Title> {
    let dir = root.join("VIDEO_TS");
    let mut best: Option<Title> = None;
    // Title sets VTS_01 to VTS_99, each with an IFO and VOBs _1 to _9 (_0 holds its menus).
    for set in 1..=99 {
        let ifo = dir.join(format!("VTS_{set:02}_0.IFO"));
        let Ok(bytes) = std::fs::read(&ifo) else {
            if set > 1 && best.is_some() {
                break;
            }
            continue;
        };
        let Some((length, chapters)) = main_chain(&bytes) else {
            continue;
        };
        let vobs: Vec<PathBuf> = (1..=9)
            .map(|n| dir.join(format!("VTS_{set:02}_{n}.VOB")))
            .take_while(|v| v.is_file())
            .collect();
        if !vobs.is_empty() && best.as_ref().is_none_or(|b| length > b.length) {
            best = Some(Title {
                vobs,
                length,
                chapters,
            });
        }
    }
    best
}

/// A title set IFO's longest program chain: its length, and its chapters' start times (each
/// program is a chapter; its cells' times add up to where it starts).
fn main_chain(ifo: &[u8]) -> Option<(f64, Vec<f64>)> {
    if !ifo.starts_with(b"DVDVIDEO-VTS") {
        return None;
    }
    let u16_at = |at: usize| Some(u16::from_be_bytes(ifo.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_be_bytes(ifo.get(at..at + 4)?.try_into().ok()?));
    // VTS_PGCI, the title set's program chains, starts at the sector 0xCC names.
    let table = u32_at(0xCC)? as usize * 2048;
    let count = u16_at(table)? as usize;
    let mut best: Option<(f64, Vec<f64>)> = None;
    for i in 0..count {
        let pgc = table + u32_at(table + 8 + i * 8 + 4)? as usize;
        let programs = *ifo.get(pgc + 2)? as usize;
        let cells = *ifo.get(pgc + 3)? as usize;
        let length = bcd_time(ifo.get(pgc + 4..pgc + 8)?);
        let map = pgc + u16_at(pgc + 0xE6)? as usize;
        let playback = pgc + u16_at(pgc + 0xE8)? as usize;
        // Each cell's length, then where each program's first cell starts.
        let mut starts = vec![0.0];
        for c in 0..cells {
            let at = playback + c * 24 + 4;
            starts.push(starts[c] + bcd_time(ifo.get(at..at + 4)?));
        }
        let chapters: Option<Vec<f64>> = (0..programs)
            .map(|p| {
                let cell = *ifo.get(map + p)? as usize;
                starts.get(cell.checked_sub(1)?).copied()
            })
            .collect();
        if best.as_ref().is_none_or(|b| length > b.0) {
            best = Some((length, chapters?));
        }
    }
    best
}

/// A DVD playback time: hours, minutes, seconds and frames in BCD, the frame rate in the top two
/// bits of the last byte (1 is 25, 3 is 29.97).
fn bcd_time(b: &[u8]) -> f64 {
    let bcd = |x: u8| f64::from((x >> 4) * 10 + (x & 0x0F));
    let fps = if b[3] >> 6 == 1 {
        25.0
    } else {
        30000.0 / 1001.0
    };
    bcd(b[0]) * 3600.0 + bcd(b[1]) * 60.0 + bcd(b[2]) + bcd(b[3] & 0x3F) / fps
}

/// The longest playlist of the Blu-ray whose BDMV is in `root`: its clips, length and chapters.
pub fn bluray(root: &Path) -> Option<Title> {
    let bdmv = root.join("BDMV");
    let mut playlists: Vec<PathBuf> = std::fs::read_dir(bdmv.join("PLAYLIST"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        // AVCHD camcorders write the same format as .mpl.
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("mpls") || e.eq_ignore_ascii_case("mpl"))
        })
        .collect();
    playlists.sort();
    let mut best: Option<Title> = None;
    for playlist in playlists {
        // A playlist is a few kilobytes; a bigger file is no playlist worth reading.
        let Ok(bytes) = std::fs::read(&playlist) else {
            continue;
        };
        if bytes.len() > 1 << 20 {
            continue;
        }
        let Some((clips, length, chapters)) = playlist_title(&bytes) else {
            continue;
        };
        let vobs: Option<Vec<PathBuf>> = clips.iter().map(|c| clip_file(&bdmv, c)).collect();
        let Some(vobs) = vobs.filter(|v| !v.is_empty()) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| length > b.length) {
            best = Some(Title {
                vobs,
                length,
                chapters,
            });
        }
    }
    best
}

/// The stream file of the clip called `name` (five digits): BDMV/STREAM/<name>.m2ts, or .MTS
/// on an AVCHD disc.
fn clip_file(bdmv: &Path, name: &str) -> Option<PathBuf> {
    if name.len() != 5 || !name.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let stream = bdmv.join("STREAM");
    ["m2ts", "M2TS", "MTS", "mts"]
        .iter()
        .map(|ext| stream.join(format!("{name}.{ext}")))
        .find(|p| p.is_file())
}

/// A Blu-ray playlist's clips in play order, its length, and its chapters' start times. Times
/// count in 45 kHz ticks; each play item plays its clip from IN_time to OUT_time, and each entry
/// mark is a chapter at a time inside one play item.
fn playlist_title(mpls: &[u8]) -> Option<(Vec<String>, f64, Vec<f64>)> {
    if !mpls.starts_with(b"MPLS") {
        return None;
    }
    let u16_at = |at: usize| Some(u16::from_be_bytes(mpls.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_be_bytes(mpls.get(at..at + 4)?.try_into().ok()?));
    const TICKS: f64 = 45_000.0;
    let list = u32_at(8)? as usize;
    let marks = u32_at(12)? as usize;
    let items = u16_at(list + 6)? as usize;
    let (mut clips, mut starts, mut ins) = (Vec::new(), Vec::new(), Vec::new());
    let (mut at, mut length) = (list + 10, 0.0);
    for _ in 0..items {
        let size = u16_at(at)? as usize;
        let name = std::str::from_utf8(mpls.get(at + 2..at + 7)?).ok()?;
        // A multi-angle item lists its other angles after these fields; the first angle plays.
        let (from, to) = (u32_at(at + 14)?, u32_at(at + 18)?);
        starts.push(length);
        ins.push(from);
        length += f64::from(to.checked_sub(from)?) / TICKS;
        clips.push(name.to_owned());
        at = at.checked_add(2 + size)?;
    }
    let mut chapters = Vec::new();
    for m in 0..u16_at(marks + 4)? as usize {
        let mark = marks + 6 + m * 14;
        // Type 1 is an entry mark, a chapter; type 2 a link point, which isn't.
        if *mpls.get(mark + 1)? != 1 {
            continue;
        }
        let item = u16_at(mark + 2)? as usize;
        let (Some(start), Some(from)) = (starts.get(item), ins.get(item)) else {
            continue;
        };
        let time = start + f64::from(u32_at(mark + 4)?.saturating_sub(*from)) / TICKS;
        if chapters.last().is_none_or(|last| time > *last) && time < length {
            chapters.push(time);
        }
    }
    Some((clips, length, chapters))
}

/// Blu-ray images mounted this run, by image, so each is mounted once.
static MOUNTED: Mutex<Option<HashMap<PathBuf, PathBuf>>> = Mutex::new(None);

/// Mounts the disc image `iso` read-only where Finder doesn't show it, once per run, and gives
/// back where its files are. The images go when wushang exits.
fn mount(iso: &Path) -> Result<PathBuf, String> {
    let mut mounted = MOUNTED.lock().unwrap();
    let first = mounted.is_none();
    let mounted = mounted.get_or_insert_with(HashMap::new);
    if let Some(at) = mounted.get(iso).filter(|at| at.is_dir()) {
        return Ok(at.clone());
    }
    let under = std::env::temp_dir().join("wushang-discs");
    std::fs::create_dir_all(&under).map_err(|e| format!("can't mount the image: {e}"))?;
    let out = Command::new("/usr/bin/hdiutil")
        .args([
            "attach",
            "-readonly",
            "-nobrowse",
            "-noautoopen",
            "-noverify",
        ])
        .arg("-mountrandom")
        .arg(&under)
        .arg("-plist")
        .arg(iso)
        .output()
        .map_err(|e| format!("can't mount the image: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!("can't mount the image: {}", why.trim()));
    }
    let plist = String::from_utf8_lossy(&out.stdout);
    let at = mount_point(&plist).ok_or("the image holds no disc wushang can read")?;
    if first {
        extern "C" fn unmount_all() {
            let mounted = MOUNTED.lock().map(|m| m.clone()).unwrap_or_default();
            for at in mounted.unwrap_or_default().values() {
                _ = Command::new("/usr/bin/hdiutil")
                    .args(["detach", "-quiet"])
                    .arg(at)
                    .status();
            }
        }
        unsafe { libc::atexit(unmount_all) };
    }
    mounted.insert(iso.to_path_buf(), at.clone());
    Ok(at)
}

/// The first mount point in hdiutil's `-plist` answer.
fn mount_point(plist: &str) -> Option<PathBuf> {
    let after = plist.split("<key>mount-point</key>").nth(1)?;
    let value = after.trim_start().strip_prefix("<string>")?;
    let value = value.split("</string>").next()?;
    let value = value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    Some(PathBuf::from(value))
}

/// The volume name of a mounted disc (`/Volumes/<label>`).
fn volume_label(path: &Path) -> Option<String> {
    if path.parent()? != Path::new("/Volumes") {
        return None;
    }
    Some(path.file_name()?.to_string_lossy().into_owned())
}

/// The mounted volumes that look like discs: an optical disc's file system (UDF, ISO 9660), or
/// a DVD's or Blu-ray's folders at the top, as on a disc copied to a USB stick. Unlike a Windows
/// drive letter, an empty drive shows nothing.
pub fn drives() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/Volumes") else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        // The startup disk appears as a link to /.
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| optical(p) || p.join("VIDEO_TS").is_dir() || p.join("BDMV").is_dir())
        .collect();
    found.sort();
    found
}

/// Whether the volume at `path` has an optical disc's file system.
fn optical(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut fs) } != 0 {
        return false;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(fs.f_fstypename.as_ptr()) };
    matches!(name.to_bytes(), b"udf" | b"cd9660" | b"cddafs")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A title set IFO with one program chain of `cells` cells of `secs` seconds each (29.97
    /// fps), every cell a chapter.
    fn ifo(cells: u8, secs: u8) -> Vec<u8> {
        let mut d = vec![0u8; 4096];
        d[..12].copy_from_slice(b"DVDVIDEO-VTS");
        d[0xCC..0xD0].copy_from_slice(&1u32.to_be_bytes());
        let table = 2048;
        d[table..table + 2].copy_from_slice(&1u16.to_be_bytes());
        d[table + 12..table + 16].copy_from_slice(&16u32.to_be_bytes());
        let pgc = table + 16;
        let bcd = |v: u8| (v / 10) << 4 | (v % 10);
        let total = u32::from(cells) * u32::from(secs);
        d[pgc + 2] = cells;
        d[pgc + 3] = cells;
        d[pgc + 4..pgc + 8].copy_from_slice(&[
            bcd((total / 3600) as u8),
            bcd((total / 60 % 60) as u8),
            bcd((total % 60) as u8),
            0xC0,
        ]);
        d[pgc + 0xE6..pgc + 0xE8].copy_from_slice(&0xECu16.to_be_bytes());
        d[pgc + 0xE8..pgc + 0xEA].copy_from_slice(&0x100u16.to_be_bytes());
        for c in 0..cells as usize {
            d[pgc + 0xEC + c] = c as u8 + 1;
            let cell = pgc + 0x100 + c * 24 + 4;
            d[cell..cell + 4].copy_from_slice(&[0, bcd(secs / 60), bcd(secs % 60), 0xC0]);
        }
        d
    }

    /// A Blu-ray playlist of `items` (clip, IN_time, OUT_time) and `marks` (type, play item,
    /// time), times in 45 kHz ticks.
    fn mpls(items: &[(&str, u32, u32)], marks: &[(u8, u16, u32)]) -> Vec<u8> {
        let mut d = b"MPLS0200".to_vec();
        let list = 40usize;
        let marks_at = list + 10 + items.len() * 22;
        d.extend((list as u32).to_be_bytes());
        d.extend((marks_at as u32).to_be_bytes());
        d.resize(list, 0);
        d.extend([0u8; 6]);
        d.extend((items.len() as u16).to_be_bytes());
        d.extend([0u8; 2]);
        for (clip, from, to) in items {
            d.extend(20u16.to_be_bytes());
            d.extend(clip.as_bytes());
            d.extend(b"M2TS");
            d.extend([0u8; 3]);
            d.extend(from.to_be_bytes());
            d.extend(to.to_be_bytes());
        }
        d.extend([0u8; 4]);
        d.extend((marks.len() as u16).to_be_bytes());
        for (kind, item, time) in marks {
            d.extend([0, *kind]);
            d.extend(item.to_be_bytes());
            d.extend(time.to_be_bytes());
            d.extend([0u8; 6]);
        }
        d
    }

    #[test]
    fn reads_the_main_title_and_its_chapters() {
        let root = std::env::temp_dir().join(format!("wushang-dvd-{}", std::process::id()));
        let dir = root.join("VIDEO_TS");
        std::fs::create_dir_all(&dir).unwrap();
        // An extra in title set 1, the film (two VOBs) in title set 2.
        std::fs::write(dir.join("VTS_01_0.IFO"), ifo(1, 10)).unwrap();
        std::fs::write(dir.join("VTS_01_1.VOB"), b"x").unwrap();
        std::fs::write(dir.join("VTS_02_0.IFO"), ifo(4, 75)).unwrap();
        for n in 1..=2 {
            std::fs::write(dir.join(format!("VTS_02_{n}.VOB")), b"x").unwrap();
        }
        let title = dvd(&root).unwrap();
        assert_eq!(
            title.vobs,
            [dir.join("VTS_02_1.VOB"), dir.join("VTS_02_2.VOB")]
        );
        assert_eq!(title.length, 300.0);
        assert_eq!(title.chapters, [0.0, 75.0, 150.0, 225.0]);

        // The folder, or VIDEO_TS itself, is a DVD; its address opens the film's VOBs.
        let address = super::address(&root).unwrap();
        assert_eq!(address, super::address(&dir).unwrap());
        assert!(is_disc_path(&address));
        let url = url(&address).unwrap();
        assert!(
            url.starts_with("concat:") && url.ends_with("VTS_02_2.VOB"),
            "{url}"
        );
        assert_eq!(
            super::title(&address),
            format!("wushang-dvd-{}", std::process::id())
        );
        assert!(super::address(&std::env::temp_dir()).is_none());

        // A Blu-ray folder (or its BDMV) plays its longest playlist's clips, with its chapters.
        let bd = root.join("Film BD");
        let bdmv = bd.join("BDMV");
        for dir in ["PLAYLIST", "STREAM"] {
            std::fs::create_dir_all(bdmv.join(dir)).unwrap();
        }
        std::fs::write(bdmv.join("index.bdmv"), b"INDX0200").unwrap();
        for clip in ["00001", "00002", "00009"] {
            std::fs::write(bdmv.join(format!("STREAM/{clip}.m2ts")), b"x").unwrap();
        }
        let s = 45_000;
        // A trailer, the film (two clips, chapters at 0, 60 s and 130 s, a link point at 90 s),
        // and a longer playlist naming a clip that isn't there.
        std::fs::write(
            bdmv.join("PLAYLIST/00000.mpls"),
            mpls(&[("00009", 0, 30 * s)], &[]),
        )
        .unwrap();
        let film = mpls(
            &[("00001", 10 * s, 100 * s), ("00002", 0, 120 * s)],
            &[
                (1, 0, 10 * s),
                (1, 0, 70 * s),
                (2, 0, 100 * s),
                (1, 1, 40 * s),
            ],
        );
        std::fs::write(bdmv.join("PLAYLIST/00001.mpls"), film).unwrap();
        std::fs::write(
            bdmv.join("PLAYLIST/00002.mpls"),
            mpls(&[("00404", 0, 900 * s)], &[]),
        )
        .unwrap();
        let disc = super::address(&bdmv).unwrap();
        assert_eq!(disc, PathBuf::from(format!("bluray://{}", bd.display())));
        let streams = bdmv.join("STREAM");
        assert_eq!(
            super::url(&disc).unwrap(),
            format!(
                "concat:{}|{}",
                streams.join("00001.m2ts").display(),
                streams.join("00002.m2ts").display()
            )
        );
        let title = dvd_title(&disc).unwrap();
        assert_eq!(title.length, 210.0);
        assert_eq!(title.chapters, [0.0, 60.0, 130.0]);
        assert_eq!(super::title(&disc), "Film BD");

        // A Blu-ray image mounts and plays the same way.
        let iso = root.join("film.iso");
        let made = std::process::Command::new("/usr/bin/hdiutil")
            .args(["makehybrid", "-quiet", "-udf", "-o"])
            .arg(&iso)
            .arg(&bd)
            .status()
            .unwrap();
        assert!(made.success(), "hdiutil makehybrid failed");
        let image = super::address(&iso).unwrap();
        assert_eq!(image, PathBuf::from(format!("bluray://{}", iso.display())));
        let url = super::url(&image).unwrap();
        assert!(
            url.starts_with("concat:") && url.ends_with("/BDMV/STREAM/00002.m2ts"),
            "{url}"
        );
        assert!(
            !url.contains(&root.display().to_string()),
            "{url} is the folder, not the image"
        );
        assert_eq!(dvd_title(&image).unwrap().chapters, [0.0, 60.0, 130.0]);
        assert_eq!(super::title(&image), "film");
        // Anything else is no disc.
        std::fs::write(root.join("notes.iso"), b"not an image").unwrap();
        assert!(super::url(&super::address(&root.join("notes.iso")).unwrap()).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

//! Discs, VLC's "Open Disc": DVD and Blu-ray folders (a disc's files copied off, or the disc in
//! its drive) and Blu-ray images. A disc plays from an address of its own, like a stream:
//! `dvd://<folder>` or `bluray://<folder or .iso>`.
//!
//! Blu-ray goes through libbluray (FFmpeg's `bluray:` protocol), which picks the main title. For
//! DVD, libdvdnav would make the FFmpeg build GPL, so wushang reads the disc's own index files
//! instead: each title set's IFO says how long its main program chain runs and where its
//! chapters start, and the longest one's VOB files play back to back through FFmpeg's `concat`.
//! No menus either way, and encrypted discs (CSS, AACS) don't play.

use std::path::{Path, PathBuf};

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
    let p = path.to_str().unwrap_or_default();
    if let Some(root) = p.strip_prefix(BLURAY) {
        return Ok(format!("bluray:{root}"));
    }
    let root = p.strip_prefix(DVD).ok_or("not a disc address")?;
    let title = dvd(Path::new(root)).ok_or("no DVD video in that folder")?;
    let parts: Vec<String> = title.vobs.iter().map(|v| v.display().to_string()).collect();
    Ok(format!("concat:{}", parts.join("|")))
}

/// The main title of the DVD at a `dvd://` address.
pub fn dvd_title(path: &Path) -> Option<Title> {
    path.to_str()?
        .strip_prefix(DVD)
        .and_then(|root| dvd(Path::new(root)))
}

/// A DVD's main title: its VOB files in order, its length, and where its chapters start, in
/// seconds.
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

/// The volume label of a drive root such as `E:\`.
fn volume_label(path: &Path) -> Option<String> {
    use windows::Win32::Storage::FileSystem::GetVolumeInformationW;
    use windows::core::HSTRING;
    let p = path.to_str()?;
    let root = p.trim_end_matches('\\');
    if root.len() != 2 || !root.ends_with(':') {
        return None;
    }
    let mut name = [0u16; 261];
    let root = HSTRING::from(format!("{root}\\"));
    unsafe { GetVolumeInformationW(&root, Some(&mut name), None, None, None, None).ok()? };
    let label = String::from_utf16_lossy(&name);
    let label = label.trim_end_matches('\0').trim();
    (!label.is_empty()).then(|| label.to_owned())
}

/// The drives Windows sees as optical, like `E:\`; whether a disc is in them is for opening to find out.
pub fn drives() -> Vec<PathBuf> {
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    use windows::core::HSTRING;
    const DRIVE_CDROM: u32 = 5;
    let mask = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| format!("{}:\\", (b'A' + i as u8) as char))
        .filter(|root| unsafe { GetDriveTypeW(&HSTRING::from(root.as_str())) } == DRIVE_CDROM)
        .map(PathBuf::from)
        .collect()
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

        // A Blu-ray folder (or its BDMV) and a Blu-ray image open through libbluray.
        let bd = root.join("Film BD");
        std::fs::create_dir_all(bd.join("BDMV")).unwrap();
        std::fs::write(bd.join("BDMV").join("index.bdmv"), b"INDX0200").unwrap();
        let disc = super::address(&bd.join("BDMV")).unwrap();
        assert_eq!(
            super::url(&disc).unwrap(),
            format!("bluray:{}", bd.display())
        );
        assert_eq!(super::title(&disc), "Film BD");
        let iso = root.join("film.iso");
        std::fs::write(&iso, b"").unwrap();
        let image = super::address(&iso).unwrap();
        assert_eq!(
            super::url(&image).unwrap(),
            format!("bluray:{}", iso.display())
        );
        assert_eq!(super::title(&image), "film");
        std::fs::remove_dir_all(&root).unwrap();
    }
}

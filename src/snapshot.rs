//! Snapshots: the frame on screen saved as a PNG or JPEG (in Pictures\wushang unless Settings
//! says otherwise), named after the file and the moment in it, like "Holiday 1m05s.png".

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use windows::Win32::Foundation::{E_FAIL, GENERIC_WRITE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Shell::{FOLDERID_Desktop, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::{GUID, HSTRING};

/// Writes `picture` (width, height, BGRA rows) into `dir` on a thread of its own, so a 4K PNG
/// doesn't stall playback, then sends the line the player shows about it. `jpeg` picks JPEG.
pub fn save(
    picture: (u32, u32, Vec<u8>),
    dir: PathBuf,
    stem: &str,
    seconds: f64,
    jpeg: bool,
    done: Sender<String>,
) {
    let name = file_name(stem, seconds);
    std::thread::spawn(move || {
        let saved = (|| -> windows::core::Result<PathBuf> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
            let (ext, container) = if jpeg {
                ("jpg", GUID_ContainerFormatJpeg)
            } else {
                ("png", GUID_ContainerFormatPng)
            };
            let path = free_path(&dir, &name, ext);
            write_image(&path, &picture, &container)?;
            Ok(path)
        })();
        _ = done.send(match saved {
            Ok(path) => format!("Snapshot saved to {}", path.display()),
            Err(e) => format!("Can't save the snapshot ({})", e.message()),
        });
    });
}

/// "Holiday 1m05s", or "Holiday 1h02m05s" past an hour.
pub fn file_name(stem: &str, seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    let time = if s >= 3600 {
        format!("{}h{:02}m{:02}s", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    };
    format!("{stem} {time}")
}

/// What names a file made from `path`: a file's own name, else (a stream address) its `title`,
/// without the characters Windows file names can't hold.
pub fn stem(path: &Path, title: &str) -> String {
    if !crate::net::is_address(path)
        && let Some(stem) = path.file_stem()
    {
        return stem.to_string_lossy().into_owned();
    }
    // A stream names itself, so its title is made safe as a file name: no characters Windows
    // forbids, no control characters (a NUL from `%00` would cut the name short where Windows
    // reads it), no trailing dots or spaces, no device names like CON, and not too long.
    let name: String = title
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(120)
        .collect();
    let name = name.trim_end_matches(['.', ' ']).trim_start();
    let device = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&device.as_str())
        || (device.len() == 4
            && (device.starts_with("COM") || device.starts_with("LPT"))
            && device.as_bytes()[3].is_ascii_digit());
    if name.is_empty() || reserved {
        "stream".into()
    } else {
        name.to_owned()
    }
}

/// `dir\name.ext`, or "name (2).ext" and up when that is taken.
pub fn free_path(dir: &Path, name: &str, ext: &str) -> PathBuf {
    (1..)
        .map(|n| match n {
            1 => dir.join(format!("{name}.{ext}")),
            n => dir.join(format!("{name} ({n}).{ext}")),
        })
        .find(|p| !p.exists())
        .unwrap()
}

/// The folder to save in for a `prefs::SNAP_DIR` or `RECORD_DIR` key, made if missing: a wushang
/// folder in `own` (Pictures or Videos), the desktop, or the folder `media` is in. A stream has
/// no folder, so "beside" sends it to `own` too.
pub fn output_dir(key: &str, own: &GUID, media: &Path) -> windows::core::Result<PathBuf> {
    let beside = (!crate::net::is_address(media))
        .then(|| media.parent())
        .flatten()
        .filter(|p| !p.as_os_str().is_empty());
    let dir = match (key, beside) {
        ("desktop", _) => known_folder(&FOLDERID_Desktop)?,
        ("beside", Some(dir)) => dir.to_owned(),
        _ => known_folder(own)?.join("wushang"),
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A known folder such as Pictures, wherever it was moved to (OneDrive often has it).
pub fn known_folder(id: &GUID) -> windows::core::Result<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)?;
        let path = PathBuf::from(p.to_string()?);
        CoTaskMemFree(Some(p.0 as _));
        Ok(path)
    }
}

fn write_image(
    path: &Path,
    (w, h, bgra): &(u32, u32, Vec<u8>),
    container: &GUID,
) -> windows::core::Result<()> {
    // Neither PNG nor JPEG has a use for the alpha the video processor leaves opaque.
    let bgr: Vec<u8> = (bgra.as_chunks::<4>().0.iter())
        .flat_map(|&[b, g, r, _]| [b, g, r])
        .collect();
    unsafe {
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = wic.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;
        let encoder = wic.CreateEncoder(container, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or(windows::core::Error::from(E_FAIL))?;
        frame.Initialize(None)?;
        frame.SetSize(*w, *h)?;
        let mut format = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut format)?;
        if format != GUID_WICPixelFormat24bppBGR {
            return Err(E_FAIL.into());
        }
        frame.WritePixels(*h, *w * 3, &bgr)?;
        frame.Commit()?;
        encoder.Commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_file_and_time() {
        let video = Path::new(r"C:\Videos\Holiday.mkv");
        assert_eq!(file_name(&stem(video, "x"), 65.4), "Holiday 1m05s");
        assert_eq!(file_name("Holiday", 3725.0), "Holiday 1h02m05s");
        assert_eq!(
            stem(Path::new("https://x.example/live"), "Rock: 24/7"),
            "Rock_ 24_7"
        );
        let stream = Path::new("https://x.example/live");
        assert_eq!(stem(stream, "report.docx\0"), "report.docx_");
        assert_eq!(stem(stream, "line\nbreak. . "), "line_break");
        assert_eq!(stem(stream, "con.txt"), "stream");
        assert_eq!(stem(stream, "COM1"), "stream");
        assert_eq!(stem(stream, "Combo"), "Combo");
        assert_eq!(stem(stream, " ..."), "stream");
        assert_eq!(stem(stream, &"x".repeat(500)).len(), 120);

        let dir = std::env::temp_dir().join(format!("wushang-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_path(&dir, "a", "png"), dir.join("a.png"));
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap() };
        let picture = (2, 1, vec![0, 0, 255, 255, 255, 0, 0, 255]);
        write_image(&dir.join("a.png"), &picture, &GUID_ContainerFormatPng).unwrap();
        assert_eq!(free_path(&dir, "a", "png"), dir.join("a (2).png"));
        write_image(&dir.join("a.jpg"), &picture, &GUID_ContainerFormatJpeg).unwrap();
        let jpeg = std::fs::read(dir.join("a.jpg")).unwrap();
        assert_eq!(jpeg[..2], [0xFF, 0xD8], "a JPEG starts with SOI");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

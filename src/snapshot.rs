//! Snapshots: the frame on screen saved as a PNG or JPEG (in ~/Pictures/wushang unless Settings
//! says otherwise), named after the file and the moment in it, like "Holiday 1m05s.png".

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use image::{ExtendedColorType, ImageEncoder};

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
        let path = free_path(&dir, &name, if jpeg { "jpg" } else { "png" });
        _ = done.send(match write_image(&path, &picture, jpeg) {
            Ok(()) => format!("Snapshot saved to {}", path.display()),
            Err(e) => format!("Can't save the snapshot ({e})"),
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
/// without the characters exFAT and FAT file names can't hold, so the file can go to any disk.
pub fn stem(path: &Path, title: &str) -> String {
    if !crate::net::is_address(path)
        && let Some(stem) = path.file_stem()
    {
        return stem.to_string_lossy().into_owned();
    }
    // A stream names itself, so its title is made safe as a file name: no characters exFAT
    // forbids (a "/" would also split the path), no control characters (a NUL from `%00` makes
    // the path unusable), no trailing dots or spaces, and not too long.
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
    if name.is_empty() {
        "stream".into()
    } else {
        name.to_owned()
    }
}

/// `dir/name.ext`, or "name (2).ext" and up when that is taken.
pub fn free_path(dir: &Path, name: &str, ext: &str) -> PathBuf {
    (1..)
        .map(|n| match n {
            1 => dir.join(format!("{name}.{ext}")),
            n => dir.join(format!("{name} ({n}).{ext}")),
        })
        .find(|p| !p.exists())
        .unwrap()
}

/// The user's folders a file can be saved in, as Windows' known folders were.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Folder {
    Pictures,
    Desktop,
    /// ~/Movies, the Videos folder of macOS.
    Videos,
    Music,
    Downloads,
}

/// The folder to save in for a `prefs::SNAP_DIR` or `RECORD_DIR` key, made if missing: a wushang
/// folder in `own` (Pictures or Videos), the desktop, or the folder `media` is in. A stream has
/// no folder, so "beside" sends it to `own` too.
pub fn output_dir(key: &str, own: Folder, media: &Path) -> std::io::Result<PathBuf> {
    let beside = (!crate::net::is_address(media))
        .then(|| media.parent())
        .flatten()
        .filter(|p| !p.as_os_str().is_empty());
    let dir = match (key, beside) {
        ("desktop", _) => known_folder(Folder::Desktop)?,
        ("beside", Some(dir)) => dir.to_owned(),
        _ => known_folder(own)?.join("wushang"),
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// One of the user's folders, in their home folder.
pub fn known_folder(folder: Folder) -> std::io::Result<PathBuf> {
    let home = std::env::home_dir().ok_or(std::io::ErrorKind::NotFound)?;
    Ok(home.join(match folder {
        Folder::Pictures => "Pictures",
        Folder::Desktop => "Desktop",
        Folder::Videos => "Movies",
        Folder::Music => "Music",
        Folder::Downloads => "Downloads",
    }))
}

/// JPEG quality: WIC's default on Windows, high enough that a still shows no blocks.
const JPEG_QUALITY: u8 = 90;

fn write_image(
    path: &Path,
    (w, h, bgra): &(u32, u32, Vec<u8>),
    jpeg: bool,
) -> image::ImageResult<()> {
    // Neither PNG nor JPEG has a use for the alpha the renderer leaves opaque.
    let rgb: Vec<u8> = (bgra.as_chunks::<4>().0.iter())
        .flat_map(|&[b, g, r, _]| [r, g, b])
        .collect();
    let mut file = BufWriter::new(std::fs::File::create(path)?);
    let mut write = || -> image::ImageResult<()> {
        if jpeg {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, JPEG_QUALITY)
                .write_image(&rgb, *w, *h, ExtendedColorType::Rgb8)?;
        } else {
            image::codecs::png::PngEncoder::new(&mut file).write_image(
                &rgb,
                *w,
                *h,
                ExtendedColorType::Rgb8,
            )?;
        }
        // The last bytes go out here, where a full disk says so; left to the writer's drop,
        // that error would be lost and the snapshot reported saved.
        file.flush()?;
        Ok(())
    };
    let written = write();
    if written.is_err() {
        // A cut-off picture is no use to anyone.
        _ = std::fs::remove_file(path);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_file_and_time() {
        let video = Path::new("/Users/me/Movies/Holiday.mkv");
        assert_eq!(file_name(&stem(video, "x"), 65.4), "Holiday 1m05s");
        assert_eq!(file_name("Holiday", 3725.0), "Holiday 1h02m05s");
        assert_eq!(
            stem(Path::new("https://x.example/live"), "Rock: 24/7"),
            "Rock_ 24_7"
        );
        let stream = Path::new("https://x.example/live");
        assert_eq!(stem(stream, "report.docx\0"), "report.docx_");
        assert_eq!(stem(stream, "line\nbreak. . "), "line_break");
        assert_eq!(stem(stream, "con.txt"), "con.txt");
        assert_eq!(stem(stream, "COM1"), "COM1");
        assert_eq!(stem(stream, "Combo"), "Combo");
        assert_eq!(stem(stream, " ..."), "stream");
        assert_eq!(stem(stream, &"x".repeat(500)).len(), 120);

        let dir = std::env::temp_dir().join(format!("wushang-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_path(&dir, "a", "png"), dir.join("a.png"));
        let picture = (2, 1, vec![0, 0, 255, 255, 255, 0, 0, 255]);
        write_image(&dir.join("a.png"), &picture, false).unwrap();
        assert_eq!(free_path(&dir, "a", "png"), dir.join("a (2).png"));
        write_image(&dir.join("a.jpg"), &picture, true).unwrap();
        let jpeg = std::fs::read(dir.join("a.jpg")).unwrap();
        assert_eq!(jpeg[..2], [0xFF, 0xD8], "a JPEG starts with SOI");
        // Red stays red: BGRA in, RGB out.
        let png = image::open(dir.join("a.png")).unwrap().to_rgb8();
        assert_eq!(
            (png.get_pixel(0, 0).0, png.get_pixel(1, 0).0),
            ([255, 0, 0], [0, 0, 255])
        );
        assert!(known_folder(Folder::Videos).unwrap().ends_with("Movies"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A snapshot that can't be written leaves no broken file behind.
    #[test]
    fn a_failed_snapshot_leaves_no_file() {
        let dir = std::env::temp_dir().join(format!("wushang-snap-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Wider than JPEG can hold.
        let picture = (70_000, 1, vec![0; 70_000 * 4]);
        let path = dir.join("wide.jpg");
        assert!(write_image(&path, &picture, true).is_err());
        assert!(!path.exists(), "{} was left behind", path.display());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

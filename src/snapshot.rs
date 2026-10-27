//! Snapshots: the frame on screen saved as a PNG in Pictures\myvlc, named after the file and the
//! moment in it, like "Holiday 1m05s.png".

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use windows::Win32::Foundation::{E_FAIL, GENERIC_WRITE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Shell::{FOLDERID_Pictures, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::HSTRING;

/// Writes `picture` (width, height, BGRA rows) on a thread of its own, so a 4K PNG doesn't
/// stall playback, then sends the line the player shows about it.
pub fn save(picture: (u32, u32, Vec<u8>), video: &Path, seconds: f64, done: Sender<String>) {
    let name = file_name(video, seconds);
    std::thread::spawn(move || {
        let saved = (|| -> windows::core::Result<PathBuf> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
            let dir = pictures()?.join("myvlc");
            std::fs::create_dir_all(&dir)?;
            let path = free_path(&dir, &name);
            write_png(&path, &picture)?;
            Ok(path)
        })();
        _ = done.send(match saved {
            Ok(path) => format!("Snapshot saved to {}", path.display()),
            Err(e) => format!("Can't save the snapshot ({})", e.message()),
        });
    });
}

/// "Holiday 1m05s", or "Holiday 1h02m05s" past an hour.
fn file_name(video: &Path, seconds: f64) -> String {
    let stem = video.file_stem().unwrap_or_default().to_string_lossy();
    let s = seconds.max(0.0) as u64;
    let time = if s >= 3600 {
        format!("{}h{:02}m{:02}s", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    };
    format!("{stem} {time}")
}

/// `dir\name.png`, or "name (2).png" and up when that is taken.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    (1..)
        .map(|n| match n {
            1 => dir.join(format!("{name}.png")),
            n => dir.join(format!("{name} ({n}).png")),
        })
        .find(|p| !p.exists())
        .unwrap()
}

/// The Pictures folder, wherever it was moved to (OneDrive often has it).
fn pictures() -> windows::core::Result<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None)?;
        let path = PathBuf::from(p.to_string()?);
        CoTaskMemFree(Some(p.0 as _));
        Ok(path)
    }
}

fn write_png(path: &Path, (w, h, bgra): &(u32, u32, Vec<u8>)) -> windows::core::Result<()> {
    // PNG has no use for the alpha the video processor leaves opaque.
    let bgr: Vec<u8> = (bgra.as_chunks::<4>().0.iter())
        .flat_map(|&[b, g, r, _]| [b, g, r])
        .collect();
    unsafe {
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = wic.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;
        let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
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
        assert_eq!(file_name(video, 65.4), "Holiday 1m05s");
        assert_eq!(file_name(video, 3725.0), "Holiday 1h02m05s");

        let dir = std::env::temp_dir().join(format!("myvlc-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_path(&dir, "a"), dir.join("a.png"));
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap() };
        write_png(
            &dir.join("a.png"),
            &(2, 1, vec![0, 0, 255, 255, 255, 0, 0, 255]),
        )
        .unwrap();
        assert_eq!(free_path(&dir, "a"), dir.join("a (2).png"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

//! Capture devices, VLC's "Open Capture Device": cameras, capture cards and microphones, and the
//! screens, all through FFmpeg's `avfoundation` input. A device plays like a stream, from an
//! address of its own (`avfoundation://video=<device>`, `avfoundation://audio=<device>` or
//! `screen://`, `screen://1` for a second display), so the queue, the recent list and recording
//! treat it like any other.
//!
//! macOS asks the user once before an app may use a camera or microphone (Info.plist says why)
//! or record the screen; until they agree, opening the device fails.

use std::ffi::CString;
use std::path::Path;

use ff::ffi;
use ffmpeg_next as ff;
use objc2::msg_send;
use objc2_av_foundation::{
    AVCaptureDevice, AVMediaType, AVMediaTypeAudio, AVMediaTypeMuxed, AVMediaTypeVideo,
};
use objc2_core_media::{CMFormatDescription, CMVideoFormatDescriptionGetDimensions};

const DEVICE: &str = "avfoundation://";
const SCREEN: &str = "screen://";

/// One entry of the capture menu.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    pub address: String,
}

pub fn is_capture_path(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|p| p.starts_with(DEVICE) || p.starts_with(SCREEN))
}

/// The cameras and capture cards AVFoundation sees, then its microphones, then the screens. The
/// devices come by name, which is how FFmpeg's avfoundation input picks one. Listing them asks
/// the system, which can take a moment, so call this off the UI thread.
pub fn devices() -> Vec<Device> {
    let mut out = Vec::new();
    // Muxed devices (DV and other capture boxes) carry video and sound in one; FFmpeg lists
    // them with the cameras.
    let video = [unsafe { AVMediaTypeVideo }, unsafe { AVMediaTypeMuxed }];
    for (kind, types) in [
        ("video", &video[..]),
        ("audio", &[unsafe { AVMediaTypeAudio }][..]),
    ] {
        for name in types.iter().flatten().flat_map(|t| names(t)) {
            let address = format!("{DEVICE}{kind}={name}");
            if !out.iter().any(|d: &Device| d.address == address) {
                out.push(Device { name, address });
            }
        }
    }
    for i in 0..screens().max(1) {
        out.push(Device {
            name: if i == 0 {
                "Screen".into()
            } else {
                format!("Screen {}", i + 1)
            },
            address: if i == 0 {
                SCREEN.into()
            } else {
                format!("{SCREEN}{i}")
            },
        });
    }
    out
}

/// The names of the devices of one media type.
fn names(kind: &AVMediaType) -> Vec<String> {
    #[allow(deprecated)]
    let devices = unsafe { AVCaptureDevice::devicesWithMediaType(kind) };
    devices
        .iter()
        .map(|d| unsafe { d.localizedName() }.to_string())
        .collect()
}

/// How many displays are on, the screens FFmpeg can capture.
fn screens() -> u32 {
    let mut count = 0;
    let error =
        unsafe { objc2_core_graphics::CGGetActiveDisplayList(0, std::ptr::null_mut(), &mut count) };
    if error == objc2_core_graphics::CGError::Success {
        count
    } else {
        0
    }
}

/// The size and frame rate to open the camera `name` at: its biggest picture up to 1080p that
/// runs at 30 frames a second, else its biggest at the fastest rate it has. FFmpeg's own choice,
/// 29.97 at any size, is one many cameras (Apple's own) refuse.
fn mode(name: &str) -> Option<(i32, i32, f64)> {
    let kind = unsafe { AVMediaTypeVideo }?;
    #[allow(deprecated)]
    let devices = unsafe { AVCaptureDevice::devicesWithMediaType(kind) };
    let device = devices
        .iter()
        .find(|d| unsafe { d.localizedName() }.to_string() == name)?;
    let mut best: Option<(bool, i64, i32, i32, f64)> = None;
    for format in unsafe { device.formats() }.iter() {
        // objc2-av-foundation only has `formatDescription` with a feature this crate leaves off.
        let desc: *const CMFormatDescription = unsafe { msg_send![&*format, formatDescription] };
        let Some(desc) = (unsafe { desc.as_ref() }) else {
            continue;
        };
        let size = unsafe { CMVideoFormatDescriptionGetDimensions(desc) };
        let ranges = unsafe { format.videoSupportedFrameRateRanges() };
        let thirty = ranges
            .iter()
            .any(|r| unsafe { r.minFrameRate() <= 30.01 && 29.99 <= r.maxFrameRate() });
        let fastest = ranges
            .iter()
            .map(|r| unsafe { r.maxFrameRate() })
            .fold(0.0, f64::max);
        let rate = if thirty { 30.0 } else { fastest };
        let area = i64::from(size.width) * i64::from(size.height);
        // Bigger than 1080p only when nothing smaller is there.
        let fits = area <= 1920 * 1080;
        let key = (thirty && fits, if fits { area } else { -area });
        if rate > 0.0 && best.is_none_or(|b| key > (b.0, b.1)) {
            best = Some((key.0, key.1, size.width, size.height, rate));
        }
    }
    best.map(|(_, _, w, h, rate)| (w, h, rate))
}

/// Opens the device at a capture address.
pub fn open(path: &Path) -> Result<ff::format::context::Input, String> {
    let address = path.to_str().unwrap_or_default();
    let mut options: Vec<(&str, String)> = Vec::new();
    // avfoundation reads "<video>:<audio>", either side a device name, a screen or "none".
    let url = if let Some(screen) = address.strip_prefix(SCREEN) {
        let index: u32 = match screen {
            "" => 0,
            n => n.parse().map_err(|_| "bad capture address".to_owned())?,
        };
        options.extend([("framerate", "30".into()), ("capture_cursor", "1".into())]);
        format!("Capture screen {index}:none")
    } else if let Some(name) = address
        .strip_prefix(DEVICE)
        .and_then(|a| a.strip_prefix("video="))
    {
        // The first ':' ends the video device's name.
        if name.contains(':') {
            return Err(format!("can't open {name}: its name has a colon in it"));
        }
        // NV12 is what the renderer shows without converting; a camera without it gets
        // FFmpeg's pick of what it has.
        options.push(("pixel_format", "nv12".into()));
        if let Some((w, h, rate)) = mode(name) {
            options.extend([
                ("video_size", format!("{w}x{h}")),
                ("framerate", format!("{rate}")),
            ]);
        }
        format!("{name}:none")
    } else if let Some(name) = address
        .strip_prefix(DEVICE)
        .and_then(|a| a.strip_prefix("audio="))
    {
        format!("none:{name}")
    } else {
        return Err("bad capture address".into());
    };
    let fail = |e: i32| format!("can't open the capture device: {}", ff::Error::from(e));
    let url = CString::new(url).map_err(|_| "bad capture address".to_owned())?;
    let mut dict = ff::Dictionary::new();
    for (key, value) in &options {
        dict.set(key, value);
    }
    unsafe {
        let format = ffi::av_find_input_format(c"avfoundation".as_ptr());
        if format.is_null() {
            return Err("this build of FFmpeg can't capture".into());
        }
        let mut ctx = ffi::avformat_alloc_context();
        let mut opts = dict.disown();
        let opened = ffi::avformat_open_input(&mut ctx, url.as_ptr(), format, &mut opts);
        ff::Dictionary::own(opts);
        if opened < 0 {
            return Err(fail(opened));
        }
        let probed = ffi::avformat_find_stream_info(ctx, std::ptr::null_mut());
        if probed < 0 {
            ffi::avformat_close_input(&mut ctx);
            return Err(fail(probed));
        }
        Ok(ff::format::context::Input::wrap(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_screen_and_refuses_unknown_devices() {
        ff::init().unwrap();
        assert!(!unsafe { ffi::av_find_input_format(c"avfoundation".as_ptr()) }.is_null());
        let devices = devices();
        let first_screen = devices.iter().position(|d| d.address == SCREEN).unwrap();
        assert!(
            devices[first_screen..]
                .iter()
                .all(|d| d.address.starts_with(SCREEN)),
            "the screens come last"
        );
        assert!(
            devices
                .iter()
                .all(|d| is_capture_path(Path::new(&d.address)))
        );
        assert!(!is_capture_path(Path::new("/Users/me/video.mp4")));
        assert!(open(Path::new("avfoundation://video=No Such Camera")).is_err());
        assert!(open(Path::new("avfoundation://video=a:b")).is_err());
        assert!(open(Path::new("screen://x")).is_err());
        assert!(open(Path::new("avfoundation://webcam")).is_err());
    }
}

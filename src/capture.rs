//! Capture devices, VLC's "Open Capture Device": cameras, capture cards and microphones through
//! DirectShow, and the desktop through GDI. A device plays like a stream, from an address of its
//! own (`dshow://video=<device>`, `dshow://audio=<device>` or `screen://`), so the queue, the
//! recent list and recording treat it like any other.

use std::ffi::{CStr, CString, c_char};
use std::path::Path;

use ff::ffi;
use ffmpeg_next as ff;

const DSHOW: &str = "dshow://";
const SCREEN: &str = "screen://";

/// One entry of the capture menu.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    pub address: String,
}

pub fn is_capture_path(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|p| p.starts_with(DSHOW) || p == SCREEN)
}

/// The cameras and capture cards DirectShow sees, then its microphones, then the screen.
/// DirectShow enumerates through COM, which takes a moment, so call this off the UI thread.
pub fn devices() -> Vec<Device> {
    let mut found = Vec::new();
    unsafe {
        let format = ffi::av_find_input_format(c"dshow".as_ptr());
        let mut list: *mut ffi::AVDeviceInfoList = std::ptr::null_mut();
        if !format.is_null()
            && ffi::avdevice_list_input_sources(
                format,
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut list,
            ) >= 0
            && !list.is_null()
        {
            let text = |p: *const c_char| CStr::from_ptr(p).to_string_lossy().into_owned();
            for i in 0..(*list).nb_devices as usize {
                let d = &**(*list).devices.add(i);
                let types = std::slice::from_raw_parts(d.media_types, d.nb_media_types as usize);
                let video = types.contains(&ffi::AVMediaType::AVMEDIA_TYPE_VIDEO);
                // The device's moniker names it uniquely; two identical cameras share a name.
                let kind = if video { "video" } else { "audio" };
                found.push((
                    !video,
                    Device {
                        name: text(d.device_description),
                        address: format!("{DSHOW}{kind}={}", text(d.device_name)),
                    },
                ));
            }
            ffi::avdevice_free_list_devices(&mut list);
        }
    }
    found.sort_by_key(|(audio, _)| *audio);
    let mut out: Vec<Device> = found.into_iter().map(|(_, d)| d).collect();
    out.push(Device {
        name: "Screen".into(),
        address: SCREEN.into(),
    });
    out
}

/// Opens the device at a capture address.
pub fn open(path: &Path) -> Result<ff::format::context::Input, String> {
    let address = path.to_str().unwrap_or_default();
    let (format, url, options): (&CStr, &str, &[(&str, &str)]) = if address == SCREEN {
        (
            c"gdigrab",
            "desktop",
            &[("framerate", "30"), ("draw_mouse", "1")],
        )
    } else {
        // A big real-time buffer, so a busy moment drops no frames.
        let device = address.strip_prefix(DSHOW).unwrap_or(address);
        (c"dshow", device, &[("rtbufsize", "128M")])
    };
    let fail = |e: i32| format!("can't open the capture device: {}", ff::Error::from(e));
    let url = CString::new(url).map_err(|_| "bad capture address".to_owned())?;
    let mut dict = ff::Dictionary::new();
    for (key, value) in options {
        dict.set(key, value);
    }
    unsafe {
        let format = ffi::av_find_input_format(format.as_ptr());
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
        let devices = devices();
        assert_eq!(devices.last().map(|d| d.address.as_str()), Some(SCREEN));
        assert!(
            devices
                .iter()
                .all(|d| is_capture_path(Path::new(&d.address)))
        );
        assert!(!is_capture_path(Path::new(r"C:\video.mp4")));
        assert!(open(Path::new("dshow://video=No Such Camera")).is_err());
    }
}

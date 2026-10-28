//! Whether the window's screen is in HDR mode, how bright it gets, and how bright Windows shows
//! SDR content on it (Settings > Display > HDR > SDR content brightness).

use windows::Win32::Devices::Display::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::core::Interface;

/// Peak used when a screen doesn't report one, as many TVs don't.
const UNKNOWN_PEAK: f32 = 1000.0;
/// SDR white when Windows won't say: HDR's reference white (BT.2408).
const UNKNOWN_WHITE: f32 = 203.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    /// The brightest the screen gets, in nits.
    pub peak: f32,
    /// Where Windows puts SDR white, in nits.
    pub white: f32,
}

/// The screen showing `monitor`, when it is in HDR mode. A new factory each time: an old one
/// keeps reporting the colour space the screen had when it was made.
pub fn screen(monitor: HMONITOR) -> Option<Screen> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        for adapter in (0..).map_while(|i| factory.EnumAdapters1(i).ok()) {
            for output in (0..).map_while(|i| adapter.EnumOutputs(i).ok()) {
                let Ok(desc) = output.cast::<IDXGIOutput6>().and_then(|o| o.GetDesc1()) else {
                    continue;
                };
                if desc.Monitor != monitor {
                    continue;
                }
                if desc.ColorSpace != DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 {
                    return None;
                }
                let peak = Some(desc.MaxLuminance).filter(|&n| n > 0.0);
                return Some(Screen {
                    peak: peak.unwrap_or(UNKNOWN_PEAK),
                    white: sdr_white(&desc.DeviceName).unwrap_or(UNKNOWN_WHITE),
                });
            }
        }
        None
    }
}

/// The SDR white level of the display behind GDI name `device` (like `\\.\DISPLAY1`), in nits.
fn sdr_white(device: &[u16; 32]) -> Option<f32> {
    unsafe {
        let (mut path_count, mut mode_count) = (0, 0);
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
            .ok()
            .ok()?;
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
        .ok()
        .ok()?;
        let header = |kind, size: usize, adapter, id| DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: kind,
            size: size as u32,
            adapterId: adapter,
            id,
        };
        for path in &paths[..path_count as usize] {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
                    path.sourceInfo.adapterId,
                    path.sourceInfo.id,
                ),
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0
                || source.viewGdiDeviceName != *device
            {
                continue;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                    size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(),
                    path.targetInfo.adapterId,
                    path.targetInfo.id,
                ),
                SDRWhiteLevel: 0,
            };
            if DisplayConfigGetDeviceInfo(&mut white.header) != 0 || white.SDRWhiteLevel == 0 {
                return None;
            }
            // Thousandths of scRGB's 80 nits.
            return Some(white.SDRWhiteLevel as f32 / 1000.0 * 80.0);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};

    /// The primary screen is found, and so is its display for the SDR white query (80 nits on
    /// an SDR screen).
    #[test]
    fn finds_the_primary_screen() {
        let monitor = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
        if let Some(s) = screen(monitor) {
            assert!(s.peak > 0.0 && s.white > 0.0);
        }
        let mut info = windows::Win32::Graphics::Gdi::MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of_val(&info) as u32;
        unsafe {
            windows::Win32::Graphics::Gdi::GetMonitorInfoW(monitor, &mut info.monitorInfo)
                .ok()
                .unwrap()
        };
        let white = sdr_white(&info.szDevice);
        assert!(white.is_some_and(|w| w >= 80.0), "{white:?}");
    }
}

//! Whether the window's screen can show EDR (light above SDR white), and how much: macOS reports
//! it as headroom, how many times SDR white the brightest pixel can be right now. It moves with
//! the brightness setting and grows once a layer asks for EDR, so it is read on every pass.

use objc2_app_kit::{NSScreen, NSWindow};

/// SDR white in nits when nothing says better: HDR's reference white (BT.2408), so HDR10's
/// diffuse white shows at the white of the rest of the screen.
const UNKNOWN_WHITE: f32 = crate::tonemap::HDR_WHITE;
/// SDR white in the reference presets of Apple's XDR screens (such as "HDR Video (P3-ST 2084)").
const REFERENCE_WHITE: f32 = 100.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    /// The brightest the screen gets now, in nits.
    pub peak: f32,
    /// Where SDR white is, in nits; an EDR layer's 1.0.
    pub white: f32,
}

/// The EDR screen the window is on, or None for an SDR screen.
pub fn screen(window: &NSWindow) -> Option<Screen> {
    let screen = window.screen()?;
    describe(&screen)
}

fn describe(screen: &NSScreen) -> Option<Screen> {
    if screen.maximumPotentialExtendedDynamicRangeColorComponentValue() <= 1.0 {
        return None;
    }
    // A reference preset fixes SDR white and the peak, so PQ shows at its own nits.
    let reference = screen.maximumReferenceExtendedDynamicRangeColorComponentValue() as f32;
    if reference > 1.0 {
        return Some(Screen {
            peak: REFERENCE_WHITE * reference,
            white: REFERENCE_WHITE,
        });
    }
    let headroom = screen.maximumExtendedDynamicRangeColorComponentValue() as f32;
    Some(Screen {
        peak: UNKNOWN_WHITE * headroom.max(1.0),
        white: UNKNOWN_WHITE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every screen reads, with a peak no dimmer than its white. Needs the main thread, which
    /// `cargo test -- --test-threads=1` runs tests on.
    #[test]
    fn reads_every_screen() {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        for screen in NSScreen::screens(mtm) {
            if let Some(s) = describe(&screen) {
                assert!(s.peak >= s.white && s.white > 0.0, "{s:?}");
            }
        }
    }
}

//! Container and stream facts for the Properties dialog. Reads headers only, without decoding.

use std::ffi::CStr;
use std::path::Path;

use ff::ffi;
use ff::format::stream::Disposition;
use ff::media::Type;
use ffmpeg_next as ff;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Details {
    /// The container's long name, like "QuickTime / MOV".
    pub container: String,
    /// Seconds; 0 when unknown.
    pub duration: f64,
    /// Bits per second; 0 when unknown.
    pub bit_rate: i64,
    /// One (kind, description) per stream: ("Video", "h264, 1920x1080, 30 fps").
    pub streams: Vec<(&'static str, String)>,
}

/// Opens `path` and describes it; None when FFmpeg can't open it.
pub fn details(path: &Path) -> Option<Details> {
    let input = crate::session::open_file(path).ok()?;
    Some(Details {
        container: input.format().description().to_owned(),
        // AV_NOPTS_VALUE is i64::MIN, so an unknown duration lands on 0.
        duration: input.duration().max(0) as f64 / f64::from(ffi::AV_TIME_BASE),
        bit_rate: input.bit_rate().max(0),
        streams: input.streams().filter_map(|s| describe(&s)).collect(),
    })
}

fn describe(s: &ff::Stream) -> Option<(&'static str, String)> {
    let p = s.parameters();
    let mut parts = vec![p.id().name().to_owned()];
    // SAFETY: the parameters live as long as the stream, which outlives this borrow.
    let raw = unsafe { &*p.as_ptr() };
    let kind = match p.medium() {
        Type::Video if s.disposition().contains(Disposition::ATTACHED_PIC) => {
            parts.push(format!("{}x{}", raw.width, raw.height));
            "Cover"
        }
        Type::Video => {
            parts.push(format!("{}x{}", raw.width, raw.height));
            let rate = s.avg_frame_rate();
            if rate.denominator() > 0 && rate.numerator() > 0 {
                let fps = f64::from(rate.numerator()) / f64::from(rate.denominator());
                parts.push(format!("{} fps", trim_number(fps, 3)));
            }
            "Video"
        }
        Type::Audio => {
            parts.push(channels(raw.ch_layout.nb_channels));
            if raw.sample_rate > 0 {
                let khz = f64::from(raw.sample_rate) / 1000.0;
                parts.push(format!("{} kHz", trim_number(khz, 1)));
            }
            "Audio"
        }
        Type::Subtitle => "Subtitles",
        _ => return None,
    };
    // SAFETY: as above; the dictionary belongs to the stream.
    let language = unsafe { tag((*s.as_ptr()).metadata, c"language") };
    if let Some(l) = language.filter(|l| !l.eq_ignore_ascii_case("und")) {
        parts.push(l);
    }
    Some((kind, parts.join(", ")))
}

fn channels(n: i32) -> String {
    match n {
        1 => "mono".into(),
        2 => "stereo".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        n => format!("{n} channels"),
    }
}

/// `v` with at most `decimals` decimals and no trailing zeros: 29.97, 30, 44.1.
fn trim_number(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

/// A tag from a raw dictionary, read lossily, None when missing or blank.
unsafe fn tag(dict: *const ffi::AVDictionary, key: &CStr) -> Option<String> {
    let entry = unsafe { ffi::av_dict_get(dict, key.as_ptr(), std::ptr::null(), 0) };
    if entry.is_null() {
        return None;
    }
    let value = unsafe { CStr::from_ptr((*entry).value) }.to_string_lossy();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_channels() {
        assert_eq!(trim_number(30000.0 / 1001.0, 3), "29.97");
        assert_eq!(trim_number(30.0, 3), "30");
        assert_eq!(trim_number(44.1, 1), "44.1");
        assert_eq!(trim_number(48.0, 1), "48");
        assert_eq!(channels(2), "stereo");
        assert_eq!(channels(3), "3 channels");
    }

    #[test]
    fn describes_the_test_clip() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(r"testdata\test_tracks.mkv");
        ff::init().unwrap();
        let d = details(&path).expect("test_tracks.mkv opens");
        assert!(d.container.contains("Matroska"), "{}", d.container);
        assert!(d.duration > 0.0);
        let kinds: Vec<&str> = d.streams.iter().map(|s| s.0).collect();
        assert!(
            kinds.contains(&"Video") && kinds.contains(&"Audio"),
            "{kinds:?}"
        );
        assert!(details(&path.with_extension("missing")).is_none());
    }
}

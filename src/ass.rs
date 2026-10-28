//! ASS and SSA subtitles drawn by libass, as VLC and mpv draw them: positions, styles, fonts,
//! colours, borders, rotation, karaoke and animation. libass ships as one DLL beside myvlc.exe
//! (`scripts/build-libass.sh`), loaded at run time; without it, ASS falls back to the plain text
//! `subs.rs` makes of every format.
//!
//! A [`Track`] renders at the video's own size into one picture of what is on screen, which
//! `subs::Cues` hands the caption painter like a DVD or Blu-ray subtitle.

use std::ffi::{CString, c_char, c_int, c_longlong, c_void};
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{PCSTR, w};

use crate::subs::BitmapRect;

#[repr(C)]
struct Image {
    w: c_int,
    h: c_int,
    stride: c_int,
    bitmap: *const u8,
    /// RGBA, the alpha as transparency: 0 is opaque.
    color: u32,
    dst_x: c_int,
    dst_y: c_int,
    next: *const Image,
    kind: c_int,
}

type MessageCb = unsafe extern "C" fn(c_int, *const c_char, *mut c_void, *mut c_void);

/// The libass functions myvlc calls, from the DLL.
struct Api {
    library_init: unsafe extern "C" fn() -> *mut c_void,
    set_message_cb: unsafe extern "C" fn(*mut c_void, Option<MessageCb>, *mut c_void),
    add_font: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, c_int),
    renderer_init: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    renderer_done: unsafe extern "C" fn(*mut c_void),
    set_frame_size: unsafe extern "C" fn(*mut c_void, c_int, c_int),
    set_storage_size: unsafe extern "C" fn(*mut c_void, c_int, c_int),
    set_fonts: unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        *const c_char,
        c_int,
        *const c_char,
        c_int,
    ),
    render_frame:
        unsafe extern "C" fn(*mut c_void, *mut c_void, c_longlong, *mut c_int) -> *const Image,
    new_track: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    free_track: unsafe extern "C" fn(*mut c_void),
    process_codec_private: unsafe extern "C" fn(*mut c_void, *const c_char, c_int),
    process_chunk: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, c_longlong, c_longlong),
    read_memory:
        unsafe extern "C" fn(*mut c_void, *mut c_char, usize, *const c_char) -> *mut c_void,
}

/// The loaded DLL and its one ASS_Library, which holds the fonts files bring along.
struct Lib {
    api: Api,
    library: Mutex<*mut c_void>,
}

// libass objects are only touched under the mutexes here and in `Track`.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

/// FontProvider ASS_FONTPROVIDER_AUTODETECT: DirectWrite on Windows.
const AUTODETECT: c_int = 1;

/// libass, or None when its DLL isn't there.
fn lib() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(|| unsafe {
        let module = LoadLibraryW(w!("libass-9.dll")).ok()?;
        macro_rules! load {
            ($name:literal) => {
                symbol(module, concat!($name, "\0"))?
            };
        }
        let api = Api {
            library_init: load!("ass_library_init"),
            set_message_cb: load!("ass_set_message_cb"),
            add_font: load!("ass_add_font"),
            renderer_init: load!("ass_renderer_init"),
            renderer_done: load!("ass_renderer_done"),
            set_frame_size: load!("ass_set_frame_size"),
            set_storage_size: load!("ass_set_storage_size"),
            set_fonts: load!("ass_set_fonts"),
            render_frame: load!("ass_render_frame"),
            new_track: load!("ass_new_track"),
            free_track: load!("ass_free_track"),
            process_codec_private: load!("ass_process_codec_private"),
            process_chunk: load!("ass_process_chunk"),
            read_memory: load!("ass_read_memory"),
        };
        let library = (api.library_init)();
        if library.is_null() {
            return None;
        }
        // libass narrates font matching and every odd tag; myvlc's log has no use for it.
        unsafe extern "C" fn quiet(_: c_int, _: *const c_char, _: *mut c_void, _: *mut c_void) {}
        (api.set_message_cb)(library, Some(quiet), std::ptr::null_mut());
        Some(Lib {
            api,
            library: Mutex::new(library),
        })
    })
    .as_ref()
}

/// The function `name` (NUL-terminated) of `module`, as the function pointer type `F`.
unsafe fn symbol<F: Copy>(module: HMODULE, name: &str) -> Option<F> {
    let f = unsafe { GetProcAddress(module, PCSTR(name.as_ptr()))? };
    // Every `F` here is an `unsafe extern "C" fn`, the size of the pointer GetProcAddress gives.
    Some(unsafe { std::mem::transmute_copy::<_, F>(&f) })
}

pub fn available() -> bool {
    lib().is_some()
}

/// Makes a font a file carries (a Matroska attachment) available to the tracks made after.
pub fn add_font(name: &str, data: &[u8]) {
    let (Some(lib), Ok(name)) = (lib(), CString::new(name)) else {
        return;
    };
    let Ok(size) = c_int::try_from(data.len()) else {
        return;
    };
    let library = lib.library.lock().unwrap();
    unsafe { (lib.api.add_font)(*library, name.as_ptr(), data.as_ptr().cast(), size) };
}

/// One subtitle track and the renderer that draws it, at `size`, the video's size.
pub struct Track(Mutex<Inner>);

struct Inner {
    track: *mut c_void,
    renderer: *mut c_void,
    size: (u32, u32),
    /// The last picture and the time it was rendered for: a frame with nothing new reuses it.
    last: Option<(f64, Option<BitmapRect>)>,
}

unsafe impl Send for Inner {}

impl Track {
    /// A track to feed event by event, with `header` (the codec private data: script info and
    /// styles) up front.
    pub fn new(header: &[u8], size: (u32, u32)) -> Option<Track> {
        let lib = lib()?;
        let track = unsafe { (lib.api.new_track)(*lib.library.lock().unwrap()) };
        if track.is_null() {
            return None;
        }
        unsafe {
            (lib.api.process_codec_private)(track, header.as_ptr().cast(), header.len() as c_int)
        };
        Track::with(track, size)
    }

    /// A whole .ass or .ssa file.
    pub fn from_file(text: &[u8], size: (u32, u32)) -> Option<Track> {
        let lib = lib()?;
        let mut copy = text.to_vec();
        let track = unsafe {
            let library = *lib.library.lock().unwrap();
            (lib.api.read_memory)(
                library,
                copy.as_mut_ptr().cast(),
                copy.len(),
                std::ptr::null(),
            )
        };
        if track.is_null() {
            return None;
        }
        Track::with(track, size)
    }

    fn with(track: *mut c_void, size: (u32, u32)) -> Option<Track> {
        let lib = lib()?;
        unsafe {
            let renderer = (lib.api.renderer_init)(*lib.library.lock().unwrap());
            if renderer.is_null() {
                (lib.api.free_track)(track);
                return None;
            }
            let family = c"Arial";
            (lib.api.set_fonts)(
                renderer,
                std::ptr::null(),
                family.as_ptr(),
                AUTODETECT,
                std::ptr::null(),
                1,
            );
            let (w, h) = (size.0.max(2) as c_int, size.1.max(2) as c_int);
            (lib.api.set_frame_size)(renderer, w, h);
            (lib.api.set_storage_size)(renderer, w, h);
            Some(Track(Mutex::new(Inner {
                track,
                renderer,
                size,
                last: None,
            })))
        }
    }

    /// One event, as Matroska stores it ("ReadOrder,Layer,Style,..."), from `start` for
    /// `duration` seconds. libass drops an event it already has, so a seek can send it again.
    pub fn add_event(&self, event: &[u8], start: f64, duration: f64) {
        let Some(lib) = lib() else { return };
        let inner = self.0.lock().unwrap();
        let ms = |s: f64| (s * 1000.0).round() as c_longlong;
        unsafe {
            (lib.api.process_chunk)(
                inner.track,
                event.as_ptr().cast(),
                event.len() as c_int,
                ms(start),
                ms(duration),
            )
        };
    }

    /// What shows at `t` seconds: one picture on the video-sized canvas, or nothing, with the
    /// time it was first rendered for. That time stays the same while the picture does, so the
    /// painter can keep its bitmap.
    pub fn render(&self, t: f64) -> Option<(f64, BitmapRect)> {
        let lib = lib()?;
        let mut inner = self.0.lock().unwrap();
        let mut changed = 0;
        let images = unsafe {
            (lib.api.render_frame)(
                inner.renderer,
                inner.track,
                (t * 1000.0).round() as c_longlong,
                &mut changed,
            )
        };
        if changed == 0
            && let Some((at, last)) = &inner.last
        {
            return last.clone().map(|p| (*at, p));
        }
        let picture = unsafe { composite(images) };
        inner.last = Some((t, picture.clone()));
        picture.map(|p| (t, p))
    }

    pub fn size(&self) -> (u32, u32) {
        self.0.lock().unwrap().size
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        let Some(lib) = lib() else { return };
        let inner = self.0.lock().unwrap();
        unsafe {
            (lib.api.renderer_done)(inner.renderer);
            (lib.api.free_track)(inner.track);
        }
    }
}

/// Paints libass's images, back to front, into one premultiplied BGRA picture covering them all.
/// Each image is a coverage mask in one colour.
unsafe fn composite(mut images: *const Image) -> Option<BitmapRect> {
    let mut list = Vec::new();
    while let Some(image) = unsafe { images.as_ref() } {
        if image.w > 0 && image.h > 0 {
            list.push(image);
        }
        images = image.next;
    }
    let left = list.iter().map(|i| i.dst_x).min()?;
    let top = list.iter().map(|i| i.dst_y).min()?;
    let right = list.iter().map(|i| i.dst_x + i.w).max()?;
    let bottom = list.iter().map(|i| i.dst_y + i.h).max()?;
    let (w, h) = ((right - left) as usize, (bottom - top) as usize);
    let mut bgra = vec![0u8; w * h * 4];
    for image in list {
        let opacity = 255 - (image.color & 0xFF);
        let [r, g, b] = [
            image.color >> 24,
            (image.color >> 16) & 0xFF,
            (image.color >> 8) & 0xFF,
        ];
        let mask = unsafe {
            std::slice::from_raw_parts(
                image.bitmap,
                (image.stride * (image.h - 1) + image.w) as usize,
            )
        };
        for y in 0..image.h as usize {
            let row = (y + (image.dst_y - top) as usize) * w + (image.dst_x - left) as usize;
            for x in 0..image.w as usize {
                let a = u32::from(mask[y * image.stride as usize + x]) * opacity / 255;
                if a == 0 {
                    continue;
                }
                let px = &mut bgra[(row + x) * 4..(row + x) * 4 + 4];
                let keep = 255 - a;
                for (dst, src) in px.iter_mut().zip([b, g, r, 255]) {
                    *dst = ((src * a + u32::from(*dst) * keep + 127) / 255) as u8;
                }
            }
        }
    }
    Some(BitmapRect {
        x: left,
        y: top,
        w: w as u32,
        h: h as u32,
        bgra: Arc::from(bgra),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Premultiplied BGRA at canvas point (x, y) of `picture`, or None outside it.
    fn pixel(picture: &BitmapRect, x: i32, y: i32) -> Option<[u8; 4]> {
        let (px, py) = (x - picture.x, y - picture.y);
        if px < 0 || py < 0 || px >= picture.w as i32 || py >= picture.h as i32 {
            return None;
        }
        let i = ((py as u32 * picture.w + px as u32) * 4) as usize;
        picture.bgra[i..i + 4].try_into().ok()
    }

    /// Whether any pixel in the canvas box is mostly the colour `want` (r, g, b).
    fn has_colour(p: &BitmapRect, (x0, y0, x1, y1): (i32, i32, i32, i32), want: [u8; 3]) -> bool {
        (y0..y1).any(|y| {
            (x0..x1).any(|x| {
                pixel(p, x, y).is_some_and(|[b, g, r, a]| {
                    a > 200 && [r, g, b].iter().zip(want).all(|(c, w)| c.abs_diff(w) < 60)
                })
            })
        })
    }

    #[test]
    fn renders_positions_colours_and_timing() {
        assert!(
            available(),
            "libass-9.dll should sit next to the test binary"
        );
        let file =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/styled.ass")).unwrap();
        let track = Track::from_file(&file, (1920, 1080)).unwrap();
        assert!(
            track.render(0.2).is_none(),
            "nothing before the first event"
        );
        let (at, p) = track.render(1.5).unwrap();
        assert_eq!(at, 1.5);
        // The sign is yellow around its \pos(200,150), top left; the red line sits at the top
        // centre; the white line at the bottom.
        assert!(
            has_colour(&p, (200, 150, 700, 260), [255, 255, 0]),
            "yellow sign"
        );
        assert!(
            has_colour(&p, (700, 0, 1220, 160), [255, 0, 0]),
            "red line at the top"
        );
        assert!(
            has_colour(&p, (500, 900, 1420, 1040), [255, 255, 255]),
            "white line at the bottom"
        );
        // An unchanged frame comes back as it was, with its first time.
        assert_eq!(track.render(1.6), Some((1.5, p)));
        assert!(track.render(7.0).is_none(), "nothing after the last event");
    }

    #[test]
    fn takes_events_one_at_a_time() {
        let header = b"[Script Info]\nPlayResX: 640\nPlayResY: 360\n\n[V4+ Styles]\n\
            Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, \
            BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, \
            BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
            Style: Default,Arial,40,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,0,0,0,0,100,100,\
            0,0,1,2,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, \
            MarginR, MarginV, Effect, Text\n";
        let track = Track::new(header, (1280, 720)).unwrap();
        track.add_event(b"0,0,Default,,0,0,0,,Hello", 1.0, 2.0);
        // Sent again after a seek: still one line.
        track.add_event(b"0,0,Default,,0,0,0,,Hello", 1.0, 2.0);
        let (_, p) = track.render(1.5).unwrap();
        assert!(p.y > 500, "the line sits at the bottom, at {}", p.y);
        assert!(track.render(3.5).is_none());
    }
}

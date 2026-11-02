//! ASS and SSA subtitles drawn by libass, as VLC and mpv draw them: positions, styles, fonts,
//! colours, borders, rotation, karaoke and animation. libass ships in wushang.app's Frameworks
//! folder (`scripts/bundle-macos.sh`), or comes from Homebrew for `cargo run`, and is loaded at
//! run time; without it, ASS falls back to the plain text `subs.rs` makes of every format.
//!
//! A [`Track`] renders at the video's own size into one picture of what is on screen, which
//! `subs::Cues` hands the caption painter like a DVD or Blu-ray subtitle.

use std::ffi::{CString, c_char, c_int, c_longlong, c_void};
use std::sync::{Arc, Mutex, OnceLock};

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

/// The libass functions wushang calls, from the dylib.
struct Api {
    library_init: unsafe extern "C" fn() -> *mut c_void,
    library_done: unsafe extern "C" fn(*mut c_void),
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

/// The loaded dylib, and the ASS_Library new tracks are made from, which holds the fonts the
/// playing file brings along.
struct Lib {
    api: Api,
    library: Mutex<Arc<Library>>,
}

/// An ASS_Library. Each file gets its own, as libass reads added fonts in place: a renderer keeps
/// pointing into the library's copy, so the fonts can only go with the last track made from it.
struct Library(*mut c_void);

impl Library {
    fn new(api: &Api) -> Option<Arc<Library>> {
        let library = unsafe { (api.library_init)() };
        if library.is_null() {
            return None;
        }
        // libass narrates font matching and every odd tag; wushang's log has no use for it.
        unsafe extern "C" fn quiet(_: c_int, _: *const c_char, _: *mut c_void, _: *mut c_void) {}
        unsafe { (api.set_message_cb)(library, Some(quiet), std::ptr::null_mut()) };
        Some(Arc::new(Library(library)))
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        if let Some(lib) = lib() {
            unsafe { (lib.api.library_done)(self.0) };
        }
    }
}

// libass objects are only touched under the mutexes here and in `Track`.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

/// FontProvider ASS_FONTPROVIDER_AUTODETECT: Core Text on macOS, so libass finds the fonts the
/// system and the user have installed.
const AUTODETECT: c_int = 1;

/// Where libass is looked for, in order: the app bundle's own copy, then Homebrew's (Apple
/// Silicon, then Intel). Only full paths, so no library planted in the current folder can stand
/// in for them.
fn candidates() -> Vec<std::path::PathBuf> {
    const NAME: &str = "libass.9.dylib";
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        paths.push(dir.join("../Frameworks").join(NAME));
    }
    for dir in ["/opt/homebrew/lib", "/usr/local/lib"] {
        paths.push(std::path::Path::new(dir).join(NAME));
    }
    paths
}

/// libass, or None when its dylib isn't there.
fn lib() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(|| unsafe {
        let module = candidates()
            .into_iter()
            .filter(|p| p.is_file())
            .find_map(|p| libloading::Library::new(p).ok())?;
        // Kept loaded for the life of the process: the function pointers below point into it.
        let module: &'static libloading::Library = Box::leak(Box::new(module));
        macro_rules! load {
            ($name:literal) => {
                *module.get(concat!($name, "\0").as_bytes()).ok()?
            };
        }
        let api = Api {
            library_init: load!("ass_library_init"),
            library_done: load!("ass_library_done"),
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
        let library = Library::new(&api)?;
        Some(Lib {
            api,
            library: Mutex::new(library),
        })
    })
    .as_ref()
}

pub fn available() -> bool {
    lib().is_some()
}

/// Settings' "Embedded Sub Format": off shows ASS and SSA as plain text.
pub static STYLES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Whether new ASS and SSA tracks go to libass.
pub fn styling() -> bool {
    STYLES.load(std::sync::atomic::Ordering::Relaxed) && available()
}

/// Starts the fonts over for a new file, so it doesn't get the fonts of the one played before it.
/// Tracks made earlier keep the fonts they have.
pub fn clear_fonts() {
    if let Some(lib) = lib()
        && let Some(fresh) = Library::new(&lib.api)
    {
        *lib.library.lock().unwrap() = fresh;
    }
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
    unsafe { (lib.api.add_font)(library.0, name.as_ptr(), data.as_ptr().cast(), size) };
}

/// The picture styled subtitles are drawn for: the video's stored size, and the size it shows
/// at, wider for anamorphic video, whose pixels aren't square (a 16:9 DVD stores 720x576).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Canvas {
    pub stored: (u32, u32),
    pub shown: (u32, u32),
}

/// Square pixels: shown as stored.
impl From<(u32, u32)> for Canvas {
    fn from(size: (u32, u32)) -> Self {
        Canvas {
            stored: size,
            shown: size,
        }
    }
}

/// One subtitle track and the renderer that draws it, at the video's shown size.
pub struct Track(Mutex<Inner>);

struct Inner {
    track: *mut c_void,
    renderer: *mut c_void,
    size: (u32, u32),
    /// The last picture and the time it was rendered for: a frame with nothing new reuses it.
    last: Option<(f64, Option<BitmapRect>)>,
    /// The library the track and renderer were made from, kept until they are freed.
    _library: Arc<Library>,
}

unsafe impl Send for Inner {}

impl Track {
    /// A track to feed event by event, with `header` (the codec private data: script info and
    /// styles) up front.
    pub fn new(header: &[u8], canvas: Canvas) -> Option<Track> {
        let lib = lib()?;
        let library = lib.library.lock().unwrap().clone();
        let track = unsafe { (lib.api.new_track)(library.0) };
        if track.is_null() {
            return None;
        }
        unsafe {
            (lib.api.process_codec_private)(track, header.as_ptr().cast(), header.len() as c_int)
        };
        Track::with(track, library, canvas)
    }

    /// A whole .ass or .ssa file.
    pub fn from_file(text: &[u8], canvas: Canvas) -> Option<Track> {
        let lib = lib()?;
        let mut copy = text.to_vec();
        let library = lib.library.lock().unwrap().clone();
        let track = unsafe {
            (lib.api.read_memory)(
                library.0,
                copy.as_mut_ptr().cast(),
                copy.len(),
                std::ptr::null(),
            )
        };
        if track.is_null() {
            return None;
        }
        Track::with(track, library, canvas)
    }

    fn with(track: *mut c_void, library: Arc<Library>, canvas: Canvas) -> Option<Track> {
        let lib = lib()?;
        unsafe {
            let renderer = (lib.api.renderer_init)(library.0);
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
            // libass draws at the shown size and undoes the stretch the stored size implies, so
            // text on anamorphic video keeps its shape.
            let px = |(w, h): (u32, u32)| (w.max(2) as c_int, h.max(2) as c_int);
            let ((fw, fh), (sw, sh)) = (px(canvas.shown), px(canvas.stored));
            (lib.api.set_frame_size)(renderer, fw, fh);
            (lib.api.set_storage_size)(renderer, sw, sh);
            Some(Track(Mutex::new(Inner {
                track,
                renderer,
                size: canvas.shown,
                last: None,
                _library: library,
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
            "libass should be installed (brew install libass)"
        );
        let file =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/styled.ass")).unwrap();
        let track = Track::from_file(&file, (1920, 1080).into()).unwrap();
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
        let track = Track::new(header, (1280, 720).into()).unwrap();
        track.add_event(b"0,0,Default,,0,0,0,,Hello", 1.0, 2.0);
        // Sent again after a seek: still one line.
        track.add_event(b"0,0,Default,,0,0,0,,Hello", 1.0, 2.0);
        let (_, p) = track.render(1.5).unwrap();
        assert!(p.y > 500, "the line sits at the bottom, at {}", p.y);
        assert!(track.render(3.5).is_none());
    }

    /// A file's fonts serve its tracks, and are gone for the tracks of the file after it. Inter,
    /// which wushang bundles and macOS doesn't have, stands in for an attached font.
    #[test]
    fn a_files_fonts_go_with_it() {
        let header = b"[Script Info]\nPlayResX: 640\nPlayResY: 360\n\n[V4+ Styles]\n\
            Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, \
            BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, \
            BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
            Style: Default,Inter,40,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,0,0,0,0,100,100,\
            0,0,1,0,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, \
            MarginR, MarginV, Effect, Text\n";
        let width = || {
            let track = Track::new(header, (1280, 720).into()).unwrap();
            track.add_event(
                b"0,0,Default,,0,0,0,,Wide wavy windows 0123456789",
                1.0,
                2.0,
            );
            track.render(1.5).unwrap().1.w
        };
        let inter = include_bytes!("../assets/fonts/Inter-Regular.ttf");
        add_font("Inter-Regular.ttf", inter);
        let with = width();
        clear_fonts();
        let without = width();
        let ratio = with as f32 / without as f32;
        assert!(
            !(0.98..=1.02).contains(&ratio),
            "{with} and {without} px wide"
        );
    }

    #[test]
    fn anamorphic_video_follows_the_scripts_layout() {
        // Letters keep the shape they had on the picture the script was laid out on (LayoutRes,
        // else the stored size), as VSFilter drew them and old scripts make up for: a script laid
        // out at the shown shape keeps its letters, one laid out on the stored pixels stretches.
        let header = |layout: &str| {
            format!(
                "[Script Info]\nPlayResX: 720\nPlayResY: 576\n{layout}\n[V4+ Styles]\n\
                 Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, \
                 OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, \
                 Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, \
                 MarginV, Encoding\n\
                 Style: Default,Arial,40,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,0,0,0,0,\
                 100,100,0,0,1,0,0,2,10,10,10,1\n\n[Events]\n\
                 Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
            )
        };
        let laid_out = header("LayoutResX: 1024\nLayoutResY: 576\n");
        // A line's width over its height, drawn on `canvas`.
        let shape = |header: &str, canvas: Canvas| {
            let track = Track::new(header.as_bytes(), canvas).unwrap();
            track.add_event(b"0,0,Default,,0,0,0,,Hello there", 1.0, 2.0);
            let (_, p) = track.render(1.5).unwrap();
            f64::from(p.w) / f64::from(p.h)
        };
        // 720x576 stored, shown 1024 wide: 1024/720 = 1.42 times wider.
        let anamorphic = Canvas {
            stored: (720, 576),
            shown: (1024, 576),
        };
        let square = shape(&laid_out, (1024, 576).into());
        let kept = shape(&laid_out, anamorphic) / square;
        assert!(
            (kept - 1.0).abs() < 0.08,
            "laid out: {kept:.2} times as wide"
        );
        let stretched = shape(&header(""), anamorphic) / square;
        assert!(
            stretched > 1.25,
            "VSFilter's stretch: {stretched:.2} times as wide"
        );
    }
}

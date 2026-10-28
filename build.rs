// Copies the FFmpeg DLLs, and libass's when it's built, next to the built exe and next to test
// binaries in deps/, so `cargo run` and `cargo test` work without touching PATH.
use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    // OUT_DIR is <target>/<profile>/build/<pkg>/out; the exe lands in <target>/<profile>.
    let exe_dir = out.ancestors().nth(3).unwrap();
    for var in ["FFMPEG_DIR", "LIBASS_DIR"] {
        println!("cargo:rerun-if-env-changed={var}");
        let Ok(dir) = env::var(var) else { continue };
        let bin = PathBuf::from(dir).join("bin");
        // Copy again whenever a library is rebuilt in place, not only when the variable changes.
        println!("cargo:rerun-if-changed={}", bin.display());
        // libass is optional: myvlc draws plain subtitles without it.
        let Ok(entries) = fs::read_dir(&bin) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "dll") {
                for dir in [exe_dir.to_path_buf(), exe_dir.join("deps")] {
                    fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
                }
            }
        }
    }
}

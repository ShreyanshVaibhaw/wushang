// Copies the FFmpeg DLLs next to the built exe, and next to test binaries in deps/, so
// `cargo run` and `cargo test` work without touching PATH.
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    let Ok(ffmpeg) = env::var("FFMPEG_DIR") else {
        return;
    };
    let bin = PathBuf::from(ffmpeg).join("bin");
    // Copy again whenever FFmpeg is rebuilt in place, not only when FFMPEG_DIR changes.
    println!("cargo:rerun-if-changed={}", bin.display());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    // OUT_DIR is <target>/<profile>/build/<pkg>/out; the exe lands in <target>/<profile>.
    let exe_dir = out.ancestors().nth(3).unwrap();
    for entry in fs::read_dir(&bin).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "dll") {
            for dir in [exe_dir.to_path_buf(), exe_dir.join("deps")] {
                fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
            }
        }
    }
}

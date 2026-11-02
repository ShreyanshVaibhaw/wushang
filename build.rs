// Link settings for the macOS app. FFmpeg comes from Homebrew through pkg-config (ffmpeg-sys-next),
// the system frameworks from the objc2 crates, and libass is loaded at run time (src/ass.rs), so
// `cargo run` and `cargo test` need nothing copied. What is left is for scripts/bundle-macos.sh,
// which copies the dylibs into wushang.app/Contents/Frameworks and points the exe at them.
fn main() {
    // Room in the exe's load commands for install_name_tool to swap Homebrew's library paths
    // for longer @executable_path/../Frameworks ones.
    println!("cargo:rustc-link-arg-bins=-Wl,-headerpad_max_install_names");
    // Where the bundled dylibs sit, for anything that names them by @rpath.
    println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Frameworks");
}

// Copies the FFmpeg DLLs, and libass's when it's built, next to the built exe and next to test
// binaries in deps/, so `cargo run` and `cargo test` work without touching PATH. Also embeds
// assets/wushang.ico as the exe's icon.
use std::{env, fs, path::Path, path::PathBuf};

/// Writes `ico` as a compiled resource file (.res) that link.exe takes as input: each image as an
/// RT_ICON numbered from 1, and the RT_GROUP_ICON 1 that lists them, which Explorer, the Start
/// menu and LoadIconW(instance, 1) use. This is what rc.exe would make, without needing the SDK.
fn icon_res(ico: &[u8], res: &Path) {
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(ico[i..i + 4].try_into().unwrap());
    // One resource: a 32-byte header naming it by type and number, then its data padded to 4.
    fn entry(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(32u32.to_le_bytes());
        out.extend(
            [0xFFFF, kind, 0xFFFF, id]
                .iter()
                .flat_map(|v: &u16| v.to_le_bytes()),
        );
        out.extend(0u32.to_le_bytes()); // data version
        out.extend(flags.to_le_bytes());
        out.extend(0x0409u16.to_le_bytes()); // English (US)
        out.extend([0u8; 8]); // version, characteristics
        out.extend(data);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    let count = u16_at(4);
    let mut out = Vec::new();
    entry(&mut out, 0, 0, 0, &[]); // the empty resource every .res starts with
    let mut group = [0u16, 1, count]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    for i in 0..count as usize {
        let e = 6 + i * 16;
        let (size, offset) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
        let id = i as u16 + 1;
        entry(&mut out, 3, id, 0x1010, &ico[offset..offset + size]); // RT_ICON
        // The group's entry is the file's, with the resource number in place of the offset.
        group.extend(&ico[e..e + 12]);
        group.extend(id.to_le_bytes());
    }
    entry(&mut out, 14, 1, 0x1030, &group); // RT_GROUP_ICON
    fs::write(res, out).unwrap();
}

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=assets/wushang.ico");
    let res = out.join("icon.res");
    icon_res(&fs::read("assets/wushang.ico").unwrap(), &res);
    println!("cargo:rustc-link-arg-bins={}", res.display());
    // OUT_DIR is <target>/<profile>/build/<pkg>/out; the exe lands in <target>/<profile>.
    let exe_dir = out.ancestors().nth(3).unwrap();
    for var in ["FFMPEG_DIR", "LIBASS_DIR"] {
        println!("cargo:rerun-if-env-changed={var}");
        let Ok(dir) = env::var(var) else { continue };
        let bin = PathBuf::from(dir).join("bin");
        // Copy again whenever a library is rebuilt in place, not only when the variable changes.
        println!("cargo:rerun-if-changed={}", bin.display());
        // libass is optional: wushang draws plain subtitles without it.
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

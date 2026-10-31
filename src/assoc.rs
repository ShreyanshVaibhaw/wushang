//! Tells Windows that wushang opens video and audio files, so it is listed in Explorer's "Open
//! with" and in Settings > Apps > Default apps. Everything goes under HKEY_CURRENT_USER, so it needs
//! no admin rights, and it follows the exe: the copy that ran last is the one files open in.

use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_NONE, REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_SZ, RegGetValueW,
    RegSetKeyValueW,
};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::core::{HSTRING, PCWSTR};

use crate::library::{AUDIO_EXTS, VIDEO_EXTS};

const APP: &str = r"Software\Classes\Applications\wushang.exe";
const CAPABILITIES: &str = r"Software\wushang\Capabilities";
/// The file types wushang offers, and what Explorer calls a file of each.
const KINDS: [(&str, &str, &[&str]); 2] = [
    ("wushang.video", "Video file", VIDEO_EXTS),
    ("wushang.audio", "Audio file", AUDIO_EXTS),
];

/// Registers the running exe, unless it already is.
pub fn register() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // A build in Cargo's output folder isn't the copy to open files with.
    if exe
        .parent()
        .is_some_and(|d| d.join(".fingerprint").is_dir())
    {
        return;
    }
    let Some(exe) = exe.to_str() else {
        return;
    };
    let open = format!("\"{exe}\" \"%1\"");
    let command = format!(r"{APP}\shell\open\command");
    if read(&command).as_deref() == Some(open.as_str()) {
        return;
    }
    let icon = format!("\"{exe}\",0");

    set(APP, "FriendlyAppName", "wushang");
    set(CAPABILITIES, "ApplicationName", "wushang");
    set(
        CAPABILITIES,
        "ApplicationDescription",
        "A video and music player",
    );
    set(CAPABILITIES, "ApplicationIcon", &icon);
    for (progid, name, exts) in KINDS {
        let class = format!(r"Software\Classes\{progid}");
        set(&class, "", name);
        set(&format!(r"{class}\DefaultIcon"), "", &icon);
        set(&format!(r"{class}\shell\open\command"), "", &open);
        for ext in exts {
            set(&format!(r"{APP}\SupportedTypes"), &format!(".{ext}"), "");
            set(
                &format!(r"{CAPABILITIES}\FileAssociations"),
                &format!(".{ext}"),
                progid,
            );
            set_value(
                &format!(r"Software\Classes\.{ext}\OpenWithProgids"),
                progid,
                REG_NONE,
                &[],
            );
        }
    }
    set(r"Software\RegisteredApplications", "wushang", CAPABILITIES);
    // Last, so a registration cut off halfway is redone on the next start.
    set(&command, "", &open);
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

/// A key's string value (`""` for its default), if it has one.
fn read(key: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Sets a string value, creating the key if needed. `""` names the key's default value.
fn set(key: &str, name: &str, value: &str) {
    let data: Vec<u8> = value
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    set_value(key, name, REG_SZ, &data);
}

fn set_value(key: &str, name: &str, kind: REG_VALUE_TYPE, data: &[u8]) {
    let name = (!name.is_empty()).then(|| HSTRING::from(name));
    let result = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            kind.0,
            (!data.is_empty()).then_some(data.as_ptr().cast()),
            data.len() as u32,
        )
    };
    if result.is_err() {
        eprintln!("assoc: cannot set {key} {name:?} ({result:?})");
    }
}

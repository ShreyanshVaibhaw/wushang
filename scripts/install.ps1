# Installs the release build for this user, with no admin rights: wushang.exe and the DLLs it loads
# go to %LOCALAPPDATA%\Programs\wushang, and a shortcut to the Start menu. Run it again after a
# `cargo build --release` to update the installed copy; close wushang first.
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$target = (cargo metadata --format-version 1 --no-deps --manifest-path "$repo\Cargo.toml" |
    ConvertFrom-Json).target_directory
$src = Join-Path $target 'release'
$dst = Join-Path $env:LOCALAPPDATA 'Programs\wushang'
$files = 'wushang.exe', 'avcodec-63.dll', 'avdevice-63.dll', 'avformat-63.dll', 'avutil-61.dll',
         'swresample-7.dll', 'swscale-10.dll', 'libass-9.dll'
New-Item -ItemType Directory -Force $dst | Out-Null
foreach ($f in $files) { Copy-Item (Join-Path $src $f) $dst -Force }
Copy-Item "$repo\assets\fonts\OFL.txt" (Join-Path $dst 'Inter-OFL.txt') -Force

$shortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\wushang.lnk'
$link = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
$link.TargetPath = Join-Path $dst 'wushang.exe'
$link.WorkingDirectory = $dst
$link.Description = 'wushang media player'
$link.Save()
"wushang installed to $dst, with a Start menu shortcut"

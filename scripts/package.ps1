# Builds the release and packs it into wushang-<version>-windows-x64.zip in the build folder:
# the exe, the DLLs it loads, and the licence files. This zip is what a GitHub release carries
# and what the in-app updater downloads, so the files sit at its root, as they sit beside the
# installed exe.
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$meta = cargo metadata --format-version 1 --no-deps --manifest-path "$repo\Cargo.toml" | ConvertFrom-Json
$version = $meta.packages[0].version
$src = Join-Path $meta.target_directory 'release'
$stage = Join-Path $meta.target_directory "package\wushang-$version"
$zip = Join-Path $meta.target_directory "wushang-$version-windows-x64.zip"

# Build here, with the user folder cut out of the source paths Rust embeds for panics (the
# registry crates and the standard library live under it), so the zip doesn't carry it.
$env:RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=~"
cargo build --release --manifest-path "$repo\Cargo.toml"
$built = $LASTEXITCODE
Remove-Item Env:RUSTFLAGS
if ($built -ne 0) { throw 'cargo build failed' }

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force (Join-Path $stage 'licenses') | Out-Null
$files = 'wushang.exe', 'avcodec-63.dll', 'avdevice-63.dll', 'avformat-63.dll', 'avutil-61.dll',
         'swresample-7.dll', 'swscale-10.dll', 'libass-9.dll'
foreach ($f in $files) { Copy-Item (Join-Path $src $f) $stage }
Copy-Item "$repo\assets\fonts\OFL.txt" (Join-Path $stage 'Inter-OFL.txt')
Copy-Item "$repo\LICENSE", "$repo\THIRD_PARTY_NOTICES.md" $stage
Copy-Item "$repo\licenses\*" (Join-Path $stage 'licenses')

if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path "$stage\*" -DestinationPath $zip
"$zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"

# Builds the release and packs it two ways in the build folder: wushang-<version>-windows-x64.zip
# (the exe, the DLLs it loads, and the licence files, at its root as they sit beside the exe) and
# wushang-<version>-windows-x64.msi, the installer (installer/wushang.wxs) with the same files.
# A GitHub release carries both; the in-app updater downloads the MSI for a copy it installed
# and the zip for one someone unzipped. Needs WiX 6: dotnet tool install --global wix --version 6.0.2
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
# Cargo reads the repo's .cargo/config.toml (FFmpeg's folder, the static CRT) from the current
# folder, so build from there.
Set-Location $repo
$meta = cargo metadata --format-version 1 --no-deps --manifest-path "$repo\Cargo.toml" | ConvertFrom-Json
$version = $meta.packages[0].version
$src = Join-Path $meta.target_directory 'release'
$stage = Join-Path $meta.target_directory "package\wushang-$version"
$zip = Join-Path $meta.target_directory "wushang-$version-windows-x64.zip"
$msi = Join-Path $meta.target_directory "wushang-$version-windows-x64.msi"
$wix = (Get-Command wix -ErrorAction SilentlyContinue).Source
if (-not $wix) { $wix = Join-Path $env:USERPROFILE '.dotnet	ools\wix.exe' }
if (-not (Test-Path $wix)) { throw 'WiX not found: dotnet tool install --global wix --version 6.0.2' }

# Build here, with the user folder cut out of the source paths Rust embeds for panics (the
# registry crates and the standard library live under it), so the zip doesn't carry it.
# Passed with --config, which Cargo adds to the rustflags in .cargo/config.toml; RUSTFLAGS
# would replace them.
$remap = "target.x86_64-pc-windows-msvc.rustflags=['--remap-path-prefix=$env:USERPROFILE=~']"
cargo build --release --manifest-path "$repo\Cargo.toml" --config $remap
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

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

# ICE38 and ICE91 are about roaming profiles, which %LOCALAPPDATA% isn't part of; see the .wxs.
& $wix build "$repo\installer\wushang.wxs" -d "Version=$version" -bindpath "stage=$stage\" -arch x64 -o $msi
if ($LASTEXITCODE -ne 0) { throw 'wix build failed' }
& $wix msi validate -sice ICE38 -sice ICE91 $msi
if ($LASTEXITCODE -ne 0) { throw 'the MSI failed its checks' }
Remove-Item ([IO.Path]::ChangeExtension($msi, '.wixpdb')) -ErrorAction SilentlyContinue
"$msi ($([math]::Round((Get-Item $msi).Length / 1MB, 1)) MB)"

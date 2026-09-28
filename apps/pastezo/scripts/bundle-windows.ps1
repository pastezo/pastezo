# Builds Pastezo for Windows into a folder and a zip. Run on Windows (x64 builds
# both; arm64 is cross-compiled with the MSVC ARM64 tools):
#   powershell -ExecutionPolicy Bypass -File apps\pastezo\scripts\bundle-windows.ps1 [-Arch x64|arm64]
# Output: target\<triple>\release\bundle\Pastezo\ and ...\bundle\Pastezo_<version>_windows_<arch>.zip
#   Pastezo.exe, pastezo-agent.exe   (the .exe icon is embedded by build.rs)
#   icons\                           alternative app icons (Settings -> App Icon)
#   MiSans-License.pdf               the font licenses travel with the fonts
#   JetBrainsMono-OFL.txt
param([ValidateSet("x64", "arm64")][string]$Arch = "x64")
$ErrorActionPreference = "Stop"
$triple = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$root = Resolve-Path "$PSScriptRoot\..\..\.."
$app = Resolve-Path "$PSScriptRoot\.."
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value

rustup target add $triple
cargo build --release --target $triple -p pastezo -p pastezo-agent --manifest-path "$root\Cargo.toml"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$bin = "$root\target\$triple\release"
$out = "$bin\bundle"
$dir = "$out\Pastezo"
if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
New-Item -ItemType Directory -Force "$dir\icons" | Out-Null
Copy-Item "$bin\Pastezo.exe", "$bin\pastezo-agent.exe" $dir
Copy-Item "$app\icons\variants\*.png" "$dir\icons"
Copy-Item "$app\fonts\MiSans-License.pdf", "$app\fonts\JetBrainsMono-OFL.txt" $dir

$zip = "$out\Pastezo_${version}_windows_$Arch.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $dir -DestinationPath $zip
Write-Output $dir
Write-Output $zip

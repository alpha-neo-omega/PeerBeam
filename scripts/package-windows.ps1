# Package the Windows app as MSIX. Run on Windows with the Flutter + Rust
# toolchains. Signing cert (optional) via env: PB_CERT_PATH / PB_CERT_PASSWORD.
#   powershell -File scripts/package-windows.ps1
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

Write-Host "== build engine (release) =="
cargo build --manifest-path rust/Cargo.toml --release -p peerbeam-ffi

Write-Host "== build flutter (release) =="
Push-Location flutter
flutter build windows --release

# Flutter names the build directory after the architecture it built for, and
# it builds for the host: `x64` on an Intel/AMD runner, `arm64` on an ARM one.
# Discovered rather than hardcoded, so the same script packages both and an ARM
# build does not silently produce an x64-shaped path that nothing wrote to.
$arch = if (Test-Path "build\windows\arm64\runner\Release") { "arm64" } else { "x64" }
Write-Host "== packaging windows-$arch =="

# Copy the engine DLL beside the runner so it loads at runtime.
$dll = "..\rust\target\release\peerbeam_ffi.dll"
$runner = "build\windows\$arch\runner\Release"
if (Test-Path $dll) { Copy-Item $dll $runner -Force } else { Write-Warning "peerbeam_ffi.dll not found" }

Write-Host "== create MSIX =="
$args = @()
if ($env:PB_CERT_PATH) {
  $args += @("--certificate-path", $env:PB_CERT_PATH)
  if ($env:PB_CERT_PASSWORD) { $args += @("--certificate-password", $env:PB_CERT_PASSWORD) }
} else {
  Write-Warning "No PB_CERT_PATH - producing an unsigned MSIX (test-install only)."
}
dart run msix:create @args
Pop-Location

Write-Host "== portable zip (no signing needed; unzip and run peerbeam.exe) =="
$ver = (Get-Content VERSION -Raw).Trim()
if ($env:GITHUB_REF_NAME -and $env:GITHUB_REF_NAME.StartsWith("v")) {
  $ver = $env:GITHUB_REF_NAME.Substring(1)
}
New-Item -ItemType Directory -Force -Path dist | Out-Null
$release = "flutter\build\windows\$arch\runner\Release"
$zip = "dist\peerbeam-$ver-windows-$arch-portable.zip"
# Everything in the runner output except the MSIX (shipped separately).
# (-Path with an array: piping into Compress-Archive -Force would recreate
# the archive per item and keep only the last one.)
$items = (Get-ChildItem $release -Exclude *.msix).FullName
Compress-Archive -Path $items -DestinationPath $zip -Force
Write-Host "== done. MSIX under $release; portable zip under dist/ =="

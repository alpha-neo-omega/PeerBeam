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

# Copy the engine DLL beside the runner so it loads at runtime.
$dll = "..\rust\target\release\peerbeam_ffi.dll"
$runner = "build\windows\x64\runner\Release"
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

Write-Host "== done. MSIX under flutter/build/windows/x64/runner/Release/ =="

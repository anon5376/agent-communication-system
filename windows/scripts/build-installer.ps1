# build-installer.ps1 — build the ACS Windows app and its NSIS installer.
#
# Layout it produces:
#   windows/dist/                                 Vite frontend bundle
#   windows/src-tauri/binaries/                   staged sidecars
#   windows/src-tauri/target/release/bundle/nsis/ ACS-<version>-x64-setup.exe
#   .../ACS-<version>-x64-setup.exe.sha256        checksum beside it
#
# Helper staging: when the real Rust core compiles for this platform
# (rust/ cargo build --locked --release succeeds), acs-desktop.exe, qagent.exe
# and aos.exe are built from source and bundled. Until the Windows port lands,
# the script falls back to the protocol-identical fake-acs-desktop and the app
# reports "no bundled qagent" if asked to start agents — nothing pretends.
#
# Unsigned: the installer is not Authenticode-signed; SmartScreen will warn.
# Usage: powershell -ExecutionPolicy Bypass -File windows\scripts\build-installer.ps1
#        (run from the repository root)

param(
    [switch]$SkipNpm,      # reuse existing windows/dist and node_modules
    [switch]$FakeHelper    # force the fake helper even if the real one builds
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
$Windows = Join-Path $Root "windows"
$Tauri = Join-Path $Windows "src-tauri"
$Rust = Join-Path $Root "rust"
$BinDir = Join-Path $Tauri "binaries"
$Triple = "x86_64-pc-windows-msvc"

Push-Location $Root
try {
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null

    # --- sidecars -----------------------------------------------------------
    $staged = @{}
    if (-not $FakeHelper -and (Test-Path (Join-Path $Rust "Cargo.toml"))) {
        Write-Host "Building acs-desktop/qagent/aos from rust/ (release, locked)..."
        Push-Location $Rust
        cargo build --locked --release --bin acs-desktop --bin qagent --bin aos 2>&1 | Write-Host
        $coreOk = $LASTEXITCODE -eq 0
        Pop-Location
        if ($coreOk) {
            foreach ($name in @("acs-desktop", "qagent", "aos")) {
                $src = Join-Path $Rust "target\release\$name.exe"
                $dst = Join-Path $BinDir "$name-$Triple.exe"
                Copy-Item $src $dst -Force
                $staged[$name] = "real"
            }
        } else {
            Write-Warning "rust/ does not compile on Windows yet; falling back to the fake helper."
        }
    }

    if (-not $staged.ContainsKey("acs-desktop")) {
        Write-Host "Staging fake-acs-desktop as the acs-desktop sidecar..."
        Push-Location $Tauri
        cargo build --release --bin fake-acs-desktop | Write-Host
        if ($LASTEXITCODE -ne 0) { throw "fake-acs-desktop build failed" }
        Pop-Location
        Copy-Item (Join-Path $Tauri "target\release\fake-acs-desktop.exe") `
                  (Join-Path $BinDir "acs-desktop-$Triple.exe") -Force
        $staged["acs-desktop"] = "fake"
    }

    # --- frontend -----------------------------------------------------------
    if (-not $SkipNpm) {
        Push-Location $Windows
        if (-not (Test-Path "node_modules")) { npm ci | Write-Host }
        npm run build | Write-Host
        Pop-Location
    }

    # --- tauri bundle -------------------------------------------------------
    Push-Location $Windows
    npm run tauri -- build | Write-Host
    Pop-Location
    if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }

    # --- checksum -----------------------------------------------------------
    $nsis = Join-Path $Tauri "target\release\bundle\nsis"
    $setup = Get-ChildItem $nsis -Filter "*-setup.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $setup) { throw "no NSIS installer produced under $nsis" }
    $hash = (Get-FileHash $setup.FullName -Algorithm SHA256).Hash.ToLower()
    $shaFile = "$($setup.FullName).sha256"
    "$hash  $($setup.Name)" | Set-Content -NoNewline $shaFile

    Write-Host ""
    Write-Host "Installer: $($setup.FullName)"
    Write-Host "SHA256:    $shaFile"
    Write-Host "Sidecars:  $(($staged.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ', ')"
    Write-Host "Unsigned: SmartScreen will warn on download. This is expected."
} finally {
    Pop-Location
}

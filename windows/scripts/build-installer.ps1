# build-installer.ps1 — build the ACS Windows app and its NSIS installer.
#
# Layout it produces:
#   windows/dist/                                 Vite frontend bundle
#   windows/src-tauri/binaries/                   staged sidecars
#   windows/src-tauri/target/release/bundle/nsis/ ACS-<version>-x64-setup.exe
#   .../ACS-<version>-x64-setup.exe.sha256        checksum beside it
#
# Fail closed: the installer bundles the real acs-desktop.exe (plus qagent.exe
# and aos.exe) built from rust/ with --locked --release. If that build fails,
# THIS SCRIPT FAILS — nothing silently substitutes.
#
# The only escape hatch is an explicit -FakeHelper, which bundles
# fake-acs-desktop (a protocol-identical JSON-bus double). That preview build
# is visibly marked three ways:
#   * installer renamed  ACS_<ver>_x64-PREVIEW-fake-backend-setup.exe
#   * build-flavor.txt bundled next to the app, read by acs_build_flavor
#     (the fake lives in src-tauri/examples/ so Tauri never bundles it even
#     in real builds — cargo would package any [[bin]] target)
#   * a persistent, non-dismissable "Preview build — not connected to a real
#     ACS workspace" banner on every screen of the app
#
# Unsigned: the installer is not Authenticode-signed; SmartScreen will warn.
# Usage: powershell -ExecutionPolicy Bypass -File windows\scripts\build-installer.ps1
#        (run from the repository root)

param(
    [switch]$SkipNpm,      # reuse existing windows/dist and node_modules
    [switch]$FakeHelper    # explicit opt-in: bundle the fake double, marked PREVIEW
)

$ErrorActionPreference = "Stop"
# Cargo and Node commonly live outside the system PATH on dev machines.
foreach ($dir in @("$HOME\.cargo\bin", "C:\hostedtoolcache\node\20.19.0\x64")) {
    if ((Test-Path $dir) -and ($env:Path -notlike "*$dir*")) { $env:Path = "$dir;$env:Path" }
}
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$Windows = Join-Path $Root "windows"
$Tauri = Join-Path $Windows "src-tauri"
$Rust = Join-Path $Root "rust"
$BinDir = Join-Path $Tauri "binaries"
$Triple = "x86_64-pc-windows-msvc"
$FlavorFile = Join-Path $Tauri "resources\build-flavor.txt"

Push-Location $Root
try {
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    # Clear stale staged sidecars so a preview run can never pick up an old
    # real acs-desktop/qagent/aos (and vice versa).
    Remove-Item (Join-Path $BinDir "*-$Triple.exe") -Force -ErrorAction SilentlyContinue

    # --- sidecars -----------------------------------------------------------
    $staged = @{}
    if ($FakeHelper) {
        Write-Warning "-FakeHelper: building a PREVIEW installer with the fake backend."
        Push-Location $Tauri
        try {
            cargo build --release --example fake-acs-desktop
            if ($LASTEXITCODE -ne 0) { throw "fake-acs-desktop build failed" }
        } finally {
            Pop-Location
        }
        Copy-Item (Join-Path $Tauri "target\release\examples\fake-acs-desktop.exe") `
                  (Join-Path $BinDir "acs-desktop-$Triple.exe") -Force
        $staged["acs-desktop"] = "fake"
        "fake" | Set-Content -NoNewline $FlavorFile
    } else {
        Write-Host "Building acs-desktop/qagent/aos from rust/ (release, locked)..."
        Push-Location $Rust
        try {
            cargo build --locked --release --bin acs-desktop --bin qagent --bin aos
            $coreOk = $LASTEXITCODE -eq 0
        } finally {
            Pop-Location
        }
        if (-not $coreOk) {
            throw "rust/ did not build on Windows. Fail closed by design: rerun with -FakeHelper only if you intend a PREVIEW build."
        }
        foreach ($name in @("acs-desktop", "qagent", "aos")) {
            $src = Join-Path $Rust "target\release\$name.exe"
            $dst = Join-Path $BinDir "$name-$Triple.exe"
            Copy-Item $src $dst -Force
            $staged[$name] = "real"
        }
        "real" | Set-Content -NoNewline $FlavorFile
    }

    # --- frontend -----------------------------------------------------------
    if (-not $SkipNpm) {
        Push-Location $Windows
        try {
            if (-not (Test-Path "node_modules")) { npm ci }
            npm run build
        } finally {
            Pop-Location
        }
    }

    # --- tauri bundle -------------------------------------------------------
    # externalBin is patched into tauri.conf.json for the build and restored
    # afterwards: only the sidecars actually staged above get bundled, so a
    # real build ships acs-desktop/qagent/aos and a -FakeHelper build ships
    # the fake acs-desktop alone. (Config-file overlays don't reliably merge
    # externalBin.)
    $confPath = Join-Path $Tauri "tauri.conf.json"
    $confOrig = [System.IO.File]::ReadAllBytes($confPath)
    $tauriExit = 1
    try {
        $confText = [System.IO.File]::ReadAllText($confPath)
        $binList = ($staged.Keys | Sort-Object | ForEach-Object { "`"binaries/$_`"" }) -join ", "
        $patchedConfig = $confText -replace '"externalBin"\s*:\s*\[[^\]]*\]', "`"externalBin`": [$binList]"
        [System.IO.File]::WriteAllText($confPath, $patchedConfig, [System.Text.UTF8Encoding]::new($false))
        $pushedWindowsLocation = $false
        try {
            Push-Location $Windows
            $pushedWindowsLocation = $true
            npm run tauri -- build
            $tauriExit = $LASTEXITCODE
        } finally {
            if ($pushedWindowsLocation) { Pop-Location }
        }
    } finally {
        [System.IO.File]::WriteAllBytes($confPath, $confOrig)
    }
    if ($tauriExit -ne 0) { throw "tauri build failed" }

    # --- name + checksum ----------------------------------------------------
    $nsis = Join-Path $Tauri "target\release\bundle\nsis"
    $setup = Get-ChildItem $nsis -Filter "*-setup.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $setup) { throw "no NSIS installer produced under $nsis" }
    $version = (Get-Content (Join-Path $Tauri "tauri.conf.json") -Raw | ConvertFrom-Json).version
    if ($staged["acs-desktop"] -eq "fake") {
        $preview = Join-Path $nsis "ACS_$($version)_x64-PREVIEW-fake-backend-setup.exe"
        Move-Item $setup.FullName $preview -Force
        $setup = Get-Item $preview
    }
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

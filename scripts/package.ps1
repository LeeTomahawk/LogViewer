# Builds logview for distribution:
#   dist\logview-<version>-windows-x64-portable.zip   (logview.exe + portable.txt + README)
#   dist\logview-<version>-setup.exe                  (installer, needs Inno Setup 6)
#
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1 [-NoInstaller]

param([switch]$NoInstaller)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
Write-Host "logview $version" -ForegroundColor Cyan

# 1. Build (profile `dist`: no debug info, LTO, static C runtime).
cargo build --profile dist -p logview-gui
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$exe = Join-Path $root "target\dist\logview.exe"

$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force $dist | Out-Null

# 2. Portable zip.
$name = "logview-$version-windows-x64-portable"
$stage = Join-Path $dist $name
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory $stage | Out-Null
Copy-Item $exe $stage
Copy-Item README.md $stage
@"
logview $version – wersja portable

Ten plik włącza tryb portable: profile i sesja są zapisywane w folderze
"data" obok logview.exe (a nie w %APPDATA%), więc cały folder można
przenosić, np. na pendrive. Usuń ten plik, aby używać ustawień użytkownika
Windows.

Uruchomienie: logview.exe [plik.log ...] albo przeciągnij pliki na okno.
Program nie wymaga instalacji ani dodatkowych bibliotek (Windows 10/11, x64).
"@ | Set-Content -Encoding UTF8 (Join-Path $stage "portable.txt")
$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip
Remove-Item -Recurse -Force $stage
Write-Host "portable:  $zip" -ForegroundColor Green

# 3. Installer.
if ($NoInstaller) { return }
$iscc = @(
    (Get-Command ISCC.exe -ErrorAction SilentlyContinue | ForEach-Object Source),
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) {
    Write-Warning "Inno Setup 6 not found (winget install JRSoftware.InnoSetup); installer skipped."
    return
}
& $iscc /Q "/DAppVersion=$version" "/DSourceExe=$exe" "/DOutputDir=$dist" installer\logview.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
Write-Host "installer: $(Join-Path $dist "logview-$version-setup.exe")" -ForegroundColor Green

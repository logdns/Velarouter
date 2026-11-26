# Build and package the Windows client.
# Run on Windows with Rust (MSVC or GNU toolchain) installed.
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Set-Location $Root

cargo build --release --locked -p vr-desktop -p vr-terminal -p velarouter
if ($LASTEXITCODE -ne 0) { throw "Cargo build failed: $LASTEXITCODE" }
$Metadata = cargo metadata --locked --no-deps --format-version 1
if ($LASTEXITCODE -ne 0) { throw "Could not read Cargo metadata" }
$Info = $Metadata | ConvertFrom-Json
$Version = ($Info.packages | Where-Object { $_.name -eq "vr-desktop" }).version
$Exe = Join-Path $Info.target_directory "release\velarouter-desktop.exe"
$Dist = "dist\Velarouter-windows-x64"
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
Copy-Item $Exe "$Dist\Velarouter.exe" -Force
foreach ($Cli in @('velarouter', 'velarouter-view')) {
    New-Item -ItemType Directory -Force -Path "$Dist\tools" | Out-Null
    $Destination = Join-Path "$Dist\tools" "$Cli.exe"
    Copy-Item (Join-Path $Info.target_directory "release\$Cli.exe") $Destination -Force
    $Actual = & $Destination --version
    if ($LASTEXITCODE -ne 0 -or $Actual -cne "$Cli $Version") {
        throw "Version check failed for $Cli"
    }
}
Copy-Item "LICENSE" "$Dist\LICENSE" -Force
Copy-Item "docs/clients/DESKTOP-APP.md" "$Dist\README.txt" -Force
Copy-Item "docs/clients/TERMINAL.md" "$Dist\TERMINAL.md" -Force
Copy-Item "assets\brand\app-icons\windows\velarouter.ico" "$Dist\Velarouter.ico" -Force

$Zip = "dist\Velarouter-$Version-windows-x64.zip"
Compress-Archive -Path "$Dist\*" -DestinationPath $Zip -Force

if (Get-Command makensis -ErrorAction SilentlyContinue) {
    & makensis "/DVERSION=$Version" "packaging\windows\installer.nsi"
    if ($LASTEXITCODE -ne 0) { throw "NSIS build failed: $LASTEXITCODE" }
} else {
    Write-Host "makensis not found; produced portable folder at $Dist"
}
Write-Host "done: $Dist"

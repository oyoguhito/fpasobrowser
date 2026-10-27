# Copy the ANGLE runtime that Servo's Windows `no-wgl` feature builds beside
# fpasobrowser.exe. The executable deliberately uses ANGLE/Direct3D instead of
# WGL so it can render through RDP and on machines without an OpenGL driver.
[CmdletBinding()]
param(
    [ValidateSet("debug", "release")]
    [string]$Profile = "debug"
)

$ErrorActionPreference = "Stop"
$repoDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$profileDir = Join-Path $repoDir (Join-Path "target" $Profile)
if (-not (Test-Path -LiteralPath (Join-Path $profileDir "fpasobrowser.exe") -PathType Leaf)) {
    throw "fpasobrowser.exe was not found in $profileDir; build it before preparing the Windows runtime."
}

$buildDir = Join-Path $profileDir "build"
foreach ($name in @("libEGL.dll", "libGLESv2.dll")) {
    $source = Get-ChildItem -LiteralPath $buildDir -Filter $name -File -Recurse |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1
    if (-not $source) {
        throw "Servo ANGLE runtime $name was not produced under $buildDir."
    }
    Copy-Item -LiteralPath $source.FullName -Destination (Join-Path $profileDir $name) -Force
    Write-Host "fpasobrowser: staged $name"
}

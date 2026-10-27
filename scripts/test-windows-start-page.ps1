# Verify that the embedded Servo runtime produces document pixels, not merely
# a successfully written PNG containing ANGLE's initial white clear surface.
[CmdletBinding()]
param(
    [ValidateSet("debug", "release")]
    [string]$Profile = "release"
)

$ErrorActionPreference = "Stop"
$repoDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$binary = Join-Path $repoDir (Join-Path "target" (Join-Path $Profile "fpasobrowser.exe"))
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "fpasobrowser.exe was not found at $binary. Build and stage the Windows runtime first."
}

$screenshot = Join-Path ([System.IO.Path]::GetTempPath()) "fpasobrowser-servo-start-page.png"
try {
    Remove-Item -LiteralPath $screenshot -Force -ErrorAction SilentlyContinue
    & $binary --capture-only --viewport 640x360 --dump-screenshot $screenshot
    if ($LASTEXITCODE -ne 0) {
        throw "fpasobrowser capture-only exited with code $LASTEXITCODE."
    }
    if (-not (Test-Path -LiteralPath $screenshot -PathType Leaf)) {
        throw "Servo did not write the built-in start page screenshot."
    }

    Add-Type -AssemblyName System.Drawing
    $image = [System.Drawing.Bitmap]::new($screenshot)
    try {
        # The bundled page has a dark background and coloured content. Sample
        # widely separated pixels so a blank ANGLE clear surface (uniform
        # opaque white) is caught without asserting on font rasterization.
        # Keep the arithmetic outside an array expression. PowerShell otherwise
        # treats the final `- 1` as applying to the object array formed by the
        # comma expression on some PowerShell 7 versions.
        $lastX = [int]$image.Width - 1
        $lastY = [int]$image.Height - 1
        $points = @(
            @(0, 0),
            @([int]($image.Width / 2), [int]($image.Height / 2)),
            @($lastX, $lastY)
        )
        $allWhite = $true
        foreach ($point in $points) {
            $pixel = $image.GetPixel([int]$point[0], [int]$point[1])
            if ($pixel.A -eq 0 -or $pixel.R -lt 250 -or $pixel.G -lt 250 -or $pixel.B -lt 250) {
                $allWhite = $false
                break
            }
        }
        if ($allWhite) {
            throw "Servo start-page screenshot is a uniform white surface, not a painted document."
        }
    } finally {
        $image.Dispose()
    }
} finally {
    Remove-Item -LiteralPath $screenshot -Force -ErrorAction SilentlyContinue
}

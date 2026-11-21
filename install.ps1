[CmdletBinding()]
param(
    [switch]$BuildOnly,
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
$repoDir = $PSScriptRoot

function Test-CommandOrPath {
    param([string]$Command, [string]$Path)
    if ($null -ne (Get-Command $Command -ErrorAction SilentlyContinue)) {
        return $true
    }
    return -not [string]::IsNullOrEmpty($Path) -and
        (Test-Path -LiteralPath $Path -PathType Leaf)
}

function Install-WingetPackage {
    param([string]$Id, [string]$Description)
    $winget = Get-Command winget.exe -ErrorAction SilentlyContinue
    if (-not $winget) {
        $winget = Get-Command winget -ErrorAction SilentlyContinue
    }
    if (-not $winget) {
        throw "fpasobrowser install: Windows Package Manager (winget) is required to install $Description. Install it, or install the prerequisite manually and rerun .\install.cmd --build-only."
    }
    Write-Host "fpasobrowser install: installing $Description..."
    & $winget.Source install --id $Id --exact --silent --accept-package-agreements --accept-source-agreements
    if ($LASTEXITCODE -ne 0) {
        throw "fpasobrowser install: winget could not install $Description (exit code $LASTEXITCODE)."
    }
}

if (-not $BuildOnly) {
    $cargoPath = Join-Path $HOME ".cargo\bin\cargo.exe"
    if (-not (Test-CommandOrPath -Command "cargo" -Path $cargoPath)) {
        Install-WingetPackage -Id "Rustlang.Rustup" -Description "Rust stable"
    }

    $pythonPath = Join-Path $env:LocalAppData "Programs\Python\Python312\python.exe"
    if (-not (Test-CommandOrPath -Command "python" -Path $pythonPath) -and
        -not (Test-CommandOrPath -Command "py" -Path "")) {
        Install-WingetPackage -Id "Python.Python.3.12" -Description "Python 3"
    }

    $libclang = Join-Path $env:ProgramFiles "LLVM\bin\libclang.dll"
    if (-not (Test-Path -LiteralPath $libclang -PathType Leaf)) {
        Install-WingetPackage -Id "LLVM.LLVM" -Description "LLVM"
    }
}

if ($NoBuild) {
    Write-Host "fpasobrowser install: prerequisites are ready; run .\fpasobrowser.cmd --rebuild when ready to compile."
    exit 0
}

# Call the source launcher directly. `fpasobrowser.cmd` also supports a
# release archive, where a root fpasobrowser.exe is correct; an installer in a
# source checkout must not select that stale copy instead of target\debug.
& (Join-Path $repoDir "bin\fpasobrowser.ps1") --rebuild
exit $LASTEXITCODE

# Run fpasobrowser from a Windows PowerShell source checkout without exposing
# Cargo's target path. The first invocation (or an out-of-date binary) builds
# the debug binary, then forwards all command-line arguments to it.
$ErrorActionPreference = "Stop"

$repoDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$binary = if ($env:FPASOBROWSER_BINARY) {
    $env:FPASOBROWSER_BINARY
} else {
    Join-Path $repoDir "target\debug\fpasobrowser.exe"
}

$needsBuild = -not (Test-Path -LiteralPath $binary -PathType Leaf)
if (-not $needsBuild) {
    $binaryTime = (Get-Item -LiteralPath $binary).LastWriteTimeUtc
    $inputs = @(
        (Join-Path $repoDir "Cargo.toml"),
        (Join-Path $repoDir "Cargo.lock"),
        (Join-Path $repoDir "build.rs"),
        (Join-Path $repoDir ".cargo\config.toml")
    ) | Where-Object { Test-Path -LiteralPath $_ }
    $needsBuild = $inputs | Where-Object {
        (Get-Item -LiteralPath $_).LastWriteTimeUtc -gt $binaryTime
    } | Select-Object -First 1
    if (-not $needsBuild) {
        $needsBuild = Get-ChildItem -LiteralPath (Join-Path $repoDir "src") -File -Recurse |
            Where-Object { $_.LastWriteTimeUtc -gt $binaryTime } |
            Select-Object -First 1
    }
}

if ($needsBuild) {
    $cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargoCommand) {
        $cargoHome = if ($env:CARGO_HOME) {
            $env:CARGO_HOME
        } else {
            Join-Path $HOME ".cargo"
        }
        $standardCargo = Join-Path $cargoHome "bin\cargo.exe"
        if (Test-Path -LiteralPath $standardCargo -PathType Leaf) {
            $cargoCommand = Get-Item -LiteralPath $standardCargo
        }
    }
    if (-not $cargoCommand) {
        Write-Error "fpasobrowser: Cargo was not found. Install Rust stable, or use a release binary."
        exit 127
    }
    # Servo's Stylo CSS component runs generated Python code while compiling.
    # Prefer an explicit user setting, then normal python.exe, and finally the
    # Windows Python launcher. Passing an absolute interpreter path through
    # PYTHON3 makes Stylo independent of the PowerShell command resolution.
    if (-not $env:PYTHON3) {
        $pythonCommand = Get-Command python.exe -ErrorAction SilentlyContinue
        if (-not $pythonCommand) {
            $pythonCommand = Get-Command python -ErrorAction SilentlyContinue
        }
        if ($pythonCommand) {
            $env:PYTHON3 = $pythonCommand.Source
        } else {
            $pythonLauncher = Get-Command py.exe -ErrorAction SilentlyContinue
            if (-not $pythonLauncher) {
                $pythonLauncher = Get-Command py -ErrorAction SilentlyContinue
            }
            if ($pythonLauncher) {
                $pythonPath = & $pythonLauncher.Source -3 -c "import sys; print(sys.executable)" 2>$null
                if ($LASTEXITCODE -eq 0 -and $pythonPath) {
                    $env:PYTHON3 = ([string]$pythonPath).Trim()
                }
            }
        }
    }
    if (-not $env:PYTHON3) {
        Write-Error "fpasobrowser: a source build of embedded Servo requires Python 3 for Stylo's build step."
        Write-Error "Install Python 3 (for example: winget install Python.Python.3.12), reopen PowerShell, then run this launcher again."
        Write-Error "If Python is outside PATH, set PYTHON3 to its python.exe path before building. A release binary does not require Python."
        exit 127
    }
    Write-Host "fpasobrowser: building the local debug binary (first build can take a while)..."
    Push-Location $repoDir
    try {
        & $cargoCommand.Source build --bin fpasobrowser
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    } finally {
        Pop-Location
    }
}

if (-not $env:FPASOBROWSER_BINARY) {
    $runtimePreparer = Join-Path $repoDir "scripts\prepare-windows-runtime.ps1"
    & $runtimePreparer -Profile debug
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}

& $binary @args
exit $LASTEXITCODE

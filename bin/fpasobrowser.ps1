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

function Resolve-FpasobrowserCargo {
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
    return $cargoCommand
}

function Test-FpasobrowserNeedsBuild {
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
        return $true
    }
    $binaryTime = (Get-Item -LiteralPath $binary).LastWriteTimeUtc
    $inputs = @(
        (Join-Path $repoDir "Cargo.toml"),
        (Join-Path $repoDir "Cargo.lock"),
        (Join-Path $repoDir "build.rs"),
        (Join-Path $repoDir ".cargo\config.toml")
    ) | Where-Object { Test-Path -LiteralPath $_ }
    foreach ($input in $inputs) {
        if ((Get-Item -LiteralPath $input).LastWriteTimeUtc -gt $binaryTime) {
            return $true
        }
    }
    $newerSource = Get-ChildItem -LiteralPath (Join-Path $repoDir "src") -File -Recurse |
        Where-Object { $_.LastWriteTimeUtc -gt $binaryTime } |
        Select-Object -First 1
    return $null -ne $newerSource
}

function Resolve-FpasobrowserPython3 {
    if ($env:PYTHON3 -and (Test-Path -LiteralPath $env:PYTHON3 -PathType Leaf)) {
        return $env:PYTHON3
    }
    $pythonCommand = Get-Command python.exe -ErrorAction SilentlyContinue
    if (-not $pythonCommand) {
        $pythonCommand = Get-Command python -ErrorAction SilentlyContinue
    }
    if ($pythonCommand) {
        return $pythonCommand.Source
    }
    $pythonLauncher = Get-Command py.exe -ErrorAction SilentlyContinue
    if (-not $pythonLauncher) {
        $pythonLauncher = Get-Command py -ErrorAction SilentlyContinue
    }
    if ($pythonLauncher) {
        $pythonPath = & $pythonLauncher.Source -3 -c "import sys; print(sys.executable)" 2>$null
        if ($LASTEXITCODE -eq 0 -and $pythonPath) {
            return ([string]$pythonPath).Trim()
        }
    }
    return $null
}

function Resolve-FpasobrowserLibclangDirectory {
    $libclangCandidates = @()
    if ($env:LIBCLANG_PATH) {
        if ((Split-Path -Leaf $env:LIBCLANG_PATH) -ieq "libclang.dll") {
            $libclangCandidates += (Split-Path -Parent $env:LIBCLANG_PATH)
        } else {
            $libclangCandidates += $env:LIBCLANG_PATH
        }
    }
    if ($env:ProgramFiles) {
        $libclangCandidates += (Join-Path $env:ProgramFiles "LLVM\bin")
    }
    $clangCommand = Get-Command clang.exe -ErrorAction SilentlyContinue
    if (-not $clangCommand) {
        $clangCommand = Get-Command clang -ErrorAction SilentlyContinue
    }
    if ($clangCommand) {
        $libclangCandidates += (Split-Path -Parent $clangCommand.Source)
    }
    $libclangCandidates |
        Where-Object {
            $_ -and (Test-Path -LiteralPath (Join-Path $_ "libclang.dll") -PathType Leaf)
        } |
        Select-Object -First 1
}

function Invoke-FpasobrowserDoctor {
    Write-Host "fpasobrowser doctor (Windows source checkout)"
    Write-Host "Cargo resolves Rust crates, but it cannot explain missing native tools before Servo starts compiling."
    $missing = $false

    if (-not $needsBuild) {
        Write-Host "  INFO    a current local binary exists; Rust, Python, and LLVM are not required to launch it."
    } else {
        $cargoCommand = Resolve-FpasobrowserCargo
        if ($cargoCommand) {
            Write-Host "  OK      Cargo: $($cargoCommand.Source)"
        } else {
            Write-Host "  MISSING Cargo / Rust stable. Install: winget install Rustlang.Rustup"
            $missing = $true
        }

        $pythonPath = Resolve-FpasobrowserPython3
        if ($pythonPath) {
            Write-Host "  OK      Python 3: $pythonPath"
        } else {
            Write-Host "  MISSING Python 3 for Servo's Stylo source-build step. Install: winget install Python.Python.3.12"
            $missing = $true
        }

        $libclangDirectory = Resolve-FpasobrowserLibclangDirectory
        if ($libclangDirectory) {
            Write-Host "  OK      LLVM libclang.dll: $(Join-Path $libclangDirectory 'libclang.dll')"
        } else {
            Write-Host "  MISSING LLVM libclang.dll for Servo's ANGLE/bindgen source-build step. Install: winget install --id LLVM.LLVM --exact"
            Write-Host "           After a custom install, set LIBCLANG_PATH to the directory containing libclang.dll."
            $missing = $true
        }
    }

    if (Test-Path -LiteralPath $binary -PathType Leaf) {
        foreach ($runtimeName in @("libEGL.dll", "libGLESv2.dll")) {
            $runtimePath = Join-Path (Split-Path -Parent $binary) $runtimeName
            if (Test-Path -LiteralPath $runtimePath -PathType Leaf) {
                Write-Host "  OK      Windows ANGLE runtime: $runtimeName"
            } else {
                Write-Host "  MISSING Windows ANGLE runtime $runtimeName beside fpasobrowser.exe. Re-extract the release archive, or rebuild through this launcher."
                $missing = $true
            }
        }
    } else {
        Write-Host "  INFO    no built executable at $binary; runtime DLLs will be checked after the first successful build."
    }

    if ($missing) {
        Write-Host "result: prerequisites are missing. Install the listed items, reopen PowerShell, and run .\bin\fpasobrowser.ps1 --doctor again."
        return $false
    }
    Write-Host "result: source-build prerequisites look ready. Start with .\bin\fpasobrowser.ps1 --version."
    return $true
}

$needsBuild = Test-FpasobrowserNeedsBuild

if ($args.Count -eq 1 -and $args[0] -eq "--doctor") {
    if (Invoke-FpasobrowserDoctor) {
        exit 0
    }
    exit 1
}

if ($needsBuild) {
    $cargoCommand = Resolve-FpasobrowserCargo
    if (-not $cargoCommand) {
        Write-Error "fpasobrowser: Cargo was not found. Install Rust stable, or use a release binary."
        exit 127
    }
    # Servo's Stylo CSS component runs generated Python code while compiling.
    # Prefer an explicit user setting, then normal python.exe, and finally the
    # Windows Python launcher. Passing an absolute interpreter path through
    # PYTHON3 makes Stylo independent of the PowerShell command resolution.
    $pythonPath = Resolve-FpasobrowserPython3
    if (-not $pythonPath) {
        Write-Error "fpasobrowser: a source build of embedded Servo requires Python 3 for Stylo's build step."
        Write-Error "Install Python 3 (for example: winget install Python.Python.3.12), reopen PowerShell, then run this launcher again."
        Write-Error "If Python is outside PATH, set PYTHON3 to its python.exe path before building. A release binary does not require Python."
        exit 127
    }
    $env:PYTHON3 = $pythonPath
    # Servo's Windows ANGLE build uses bindgen. The official LLVM installer
    # puts libclang.dll in Program Files\LLVM\bin, but does not always make
    # that directory available to the current PowerShell session. Prefer a
    # caller-supplied path, then the LLVM default and the clang.exe directory.
    $libclangDirectory = Resolve-FpasobrowserLibclangDirectory
    if (-not $libclangDirectory) {
        Write-Error "fpasobrowser: a source build of embedded Servo requires LLVM libclang.dll for bindgen."
        Write-Error "Install LLVM (for example: winget install --id LLVM.LLVM --exact), reopen PowerShell, then run this launcher again."
        Write-Error "For a custom LLVM installation, set LIBCLANG_PATH to the directory containing libclang.dll. A release binary does not require LLVM."
        exit 127
    }
    $env:LIBCLANG_PATH = $libclangDirectory
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

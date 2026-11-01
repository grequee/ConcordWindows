param(
    [switch]$PrepareOnly,
    [switch]$SkipDependencies,
    [switch]$PortableOnly
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Program failed with exit code $LASTEXITCODE"
    }
}

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "Execute este script no Windows 10/11 x64. Consulte docs/WINDOWS.md."
}

$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location $projectRoot
try {
    # Load the x64 Microsoft compiler and Windows SDK into this process only.
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (!(Test-Path $vswhere)) {
        throw "Instale Visual Studio Build Tools com Desktop development with C++ e Windows SDK."
    }
    $vsPath = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (!$vsPath) { throw "Visual Studio C++ x64 Build Tools nao encontrados." }
    $devCmd = Join-Path $vsPath "Common7\Tools\VsDevCmd.bat"
    $compilerEnv = & $env:COMSPEC /s /c "`"$devCmd`" -no_logo -arch=x64 -host_arch=x64 >nul && set"
    if ($LASTEXITCODE -ne 0) { throw "Nao foi possivel iniciar o compilador MSVC." }
    foreach ($line in $compilerEnv) {
        if ($line -match '^([^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($matches[1], $matches[2], "Process")
        }
    }

    foreach ($tool in @("git", "rustup", "cargo", "node", "npm.cmd", "cmake")) {
        if (!(Get-Command $tool -ErrorAction SilentlyContinue)) {
            throw "Ferramenta ausente: $tool. Consulte docs/WINDOWS.md."
        }
    }
    if (!$env:LIBCLANG_PATH) {
        $env:LIBCLANG_PATH = Join-Path $env:ProgramFiles "LLVM\bin"
    }
    if (!(Test-Path (Join-Path $env:LIBCLANG_PATH "libclang.dll"))) {
        throw "Instale LLVM x64 e configure LIBCLANG_PATH para a pasta com libclang.dll."
    }

    Invoke-Checked "rustup" @("show", "active-toolchain")
    Invoke-Checked "rustup" @("target", "add", "x86_64-pc-windows-msvc")
    # Keep codecs and the MSVC runtime static, avoiding extra codec/CRT DLLs.
    $env:VCPKG_ROOT = Join-Path $projectRoot "target\windows-tools\vcpkg"
    $env:VCPKGRS_TRIPLET = "x64-windows-static"
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = "-C target-feature=+crt-static"
    if (!$SkipDependencies) {
        if (!(Test-Path (Join-Path $env:VCPKG_ROOT ".git"))) {
            Invoke-Checked "git" @("clone", "--depth", "1", "--branch", "2026.10.16",
                "https://github.com/microsoft/vcpkg.git", $env:VCPKG_ROOT)
        }
        Invoke-Checked (Join-Path $env:VCPKG_ROOT "bootstrap-vcpkg.bat") @("-disableMetrics")
        Invoke-Checked (Join-Path $env:VCPKG_ROOT "vcpkg.exe") @("install",
            "--triplet=x64-windows-static",
            "--x-manifest-root=$projectRoot\deploy\windows",
            "--x-install-root=$env:VCPKG_ROOT\installed")
    }
    $env:OPUS_LIB_DIR = Join-Path $env:VCPKG_ROOT "installed\x64-windows-static\lib"
    if (!(Test-Path (Join-Path $env:OPUS_LIB_DIR "opus.lib"))) {
        throw "Bibliotecas ausentes. Execute novamente sem -SkipDependencies."
    }
    $env:OPUS_STATIC = "1"
    if ($PrepareOnly) { return }

    Invoke-Checked "npm.cmd" @("ci")
    Invoke-Checked "npm.cmd" @("run", "check")
    Invoke-Checked "npm.cmd" @("test")
    Invoke-Checked "cargo" @("fmt", "--all", "--", "--check")
    $windowsTarget = @("--locked", "--target", "x86_64-pc-windows-msvc")
    Invoke-Checked "cargo" (@("check", "--workspace", "--all-targets") + $windowsTarget)
    Invoke-Checked "cargo" (@("test", "--workspace", "--lib") + $windowsTarget)
    Invoke-Checked "cargo" (@("clippy", "--workspace", "--all-targets") + $windowsTarget + @("--", "-D", "warnings"))
    # Produce a runnable archive before bundling. An NSIS failure must not
    # prevent distributing an executable that compiled successfully.
    Invoke-Checked "npm.cmd" @("run", "tauri", "--", "build", "--target", "x86_64-pc-windows-msvc", "--no-bundle")
    & (Join-Path $PSScriptRoot "package-windows.ps1")
    if ($PortableOnly) { return }
    Invoke-Checked "npm.cmd" @("run", "tauri", "--", "bundle", "--target", "x86_64-pc-windows-msvc")

    $installers = Get-ChildItem "target\x86_64-pc-windows-msvc\release\bundle\nsis\*-setup.exe"
    if (!$installers) { throw "O build nao gerou o instalador NSIS." }
    $installers | ForEach-Object { Get-FileHash $_.FullName -Algorithm SHA256 }
} finally {
    Pop-Location
}

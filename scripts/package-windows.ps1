param(
    [string]$BinaryPath = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$projectRoot = Split-Path -Parent $PSScriptRoot
if (!$BinaryPath) {
    $BinaryPath = Join-Path $projectRoot "target\x86_64-pc-windows-msvc\release\concord-app.exe"
}
if (!(Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
    throw "Executavel Windows ausente: $BinaryPath. Compile o app antes de gerar o ZIP."
}
# A renamed Linux binary is not a runnable Windows distribution.
$stream = [IO.File]::OpenRead($BinaryPath)
try {
    if ($stream.ReadByte() -ne 0x4D -or $stream.ReadByte() -ne 0x5A) {
        throw "O arquivo informado nao possui o cabecalho de um executavel Windows."
    }
} finally {
    $stream.Dispose()
}

$outputDirectory = Join-Path $projectRoot "target\windows"
New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
$stagingRoot = Join-Path $outputDirectory ("staging-" + [guid]::NewGuid().ToString("N"))
$portableDirectory = Join-Path $stagingRoot "Concord"
New-Item -ItemType Directory -Path $portableDirectory -Force | Out-Null
try {
    Copy-Item -LiteralPath $BinaryPath -Destination (Join-Path $portableDirectory "Concord.exe")
    Copy-Item -LiteralPath (Join-Path $projectRoot "deploy\windows\LEIA-ME-portatil.txt") `
        -Destination (Join-Path $portableDirectory "LEIA-ME.txt")

    # The MSVC build links codecs, CRT and WebView2Loader statically. Only the
    # WebView2 Runtime may need installing on the destination PC. Include its
    # official online installer; it is not the complete offline runtime.
    $bootstrapper = Join-Path $portableDirectory "MicrosoftEdgeWebview2Setup.exe"
    Invoke-WebRequest -UseBasicParsing -Uri "https://go.microsoft.com/fwlink/p/?LinkId=2124703" `
        -OutFile $bootstrapper
    $signature = Get-AuthenticodeSignature -LiteralPath $bootstrapper
    if ($signature.Status -ne "Valid" -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation(?:,|$)') {
        throw "O instalador WebView2 baixado nao possui uma assinatura valida da Microsoft."
    }

    $archive = Join-Path $outputDirectory "Concord-Windows-x64.zip"
    Compress-Archive -LiteralPath $portableDirectory -DestinationPath $archive -Force
    Get-FileHash -LiteralPath $archive -Algorithm SHA256
} finally {
    # This unique staging directory was created by this invocation only.
    Remove-Item -LiteralPath $stagingRoot -Recurse -Force
}

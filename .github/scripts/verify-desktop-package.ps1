$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ProjectDirectory = "client/android-app/desktopApp"
$BinariesDirectory = Join-Path $ProjectDirectory "build/compose/binaries/main"
$ImageDirectory = Join-Path $BinariesDirectory "app"

function Get-SingleFile([string] $Root, [string] $Filter) {
    $Matches = @(Get-ChildItem -Path $Root -Recurse -File -Filter $Filter)
    if ($Matches.Count -ne 1) {
        throw "Expected one $Filter below $Root, found $($Matches.Count): $($Matches.FullName -join ', ')"
    }
    return $Matches[0].FullName
}

function Get-Dumpbin {
    $VsWhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
    if (-not (Test-Path $VsWhere)) {
        throw "vswhere.exe is unavailable; cannot locate the host PE inspection tool"
    }
    $VisualStudio = & $VsWhere -latest -products * `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $VisualStudio) {
        throw "A Visual Studio installation with x64 C++ tools is required"
    }
    $Dumpbin = Get-ChildItem -Path (Join-Path $VisualStudio "VC/Tools/MSVC") `
        -Recurse -File -Filter "dumpbin.exe" |
        Where-Object { $_.FullName -match '\\bin\\Hostx64\\x64\\dumpbin\.exe$' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if (-not $Dumpbin) {
        throw "The x64 dumpbin.exe binary is missing from $VisualStudio"
    }
    return $Dumpbin.FullName
}

function Test-PeBinary([string] $Binary, [string] $Dumpbin) {
    $Headers = & $Dumpbin /headers $Binary | Out-String
    $Headers | Write-Host
    if ($LASTEXITCODE -ne 0 -or $Headers -notmatch "8664 machine \(x64\)") {
        throw "$Binary is not an x64 PE binary"
    }

    $Dependencies = & $Dumpbin /dependents $Binary | Out-String
    $Dependencies | Write-Host
    if ($LASTEXITCODE -ne 0) {
        throw "dumpbin could not inspect dependencies for $Binary"
    }
}

$Dumpbin = Get-Dumpbin
$ImageLibrary = Get-SingleFile $ImageDirectory "client.dll"
Test-PeBinary $ImageLibrary $Dumpbin

$Msi = Get-SingleFile (Join-Path $BinariesDirectory "msi") "Cpxy-*.msi"
$ExtractDirectory = Join-Path $env:RUNNER_TEMP "cpxy-msi"
New-Item -ItemType Directory -Path $ExtractDirectory -Force | Out-Null
$MsiProcess = Start-Process msiexec.exe -Wait -PassThru -ArgumentList @(
    "/a", "`"$Msi`"", "/qn", "TARGETDIR=`"$ExtractDirectory`""
)
if ($MsiProcess.ExitCode -ne 0) {
    throw "Administrative MSI extraction failed with exit code $($MsiProcess.ExitCode)"
}
$MsiLibrary = Get-SingleFile $ExtractDirectory "client.dll"
Test-PeBinary $MsiLibrary $Dumpbin

Write-Host "Verified Desktop package for windows-x64"

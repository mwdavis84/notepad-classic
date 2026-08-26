<#
.SYNOPSIS
    Builds and packages Notepad Classic into a Windows Store-compatible MSIX package.

.DESCRIPTION
    Compiles the release binary, creates the MSIX staging layout, validates and updates
    the package version and target architecture from Cargo.toml and parameters, and runs makeappx.exe
    with full semantic validation enabled.
    Optionally signs the package with a local self-signed certificate for local testing.

.PARAMETER Configuration
    Build configuration (default: 'release').

.PARAMETER Target
    Rust target triple (default: 'x86_64-pc-windows-msvc'). Supported: 'x86_64-pc-windows-msvc' (x64),
    'aarch64-pc-windows-msvc' (arm64), 'i686-pc-windows-msvc' (x86).

.PARAMETER MakeAppxPath
    Explicit path to makeappx.exe if not in PATH or standard Windows SDK directories.

.PARAMETER SignToolPath
    Explicit path to signtool.exe if not in PATH or standard Windows SDK directories.

.PARAMETER SignForLocalTesting
    Generates/uses a local self-signed test certificate matching the manifest Publisher
    and signs the package for sideload testing.

.PARAMETER SkipBuild
    Skips the `cargo build` step and packages the existing binary.

.EXAMPLE
    .\package-msix.ps1
    Builds the binary and packages an unsigned MSIX ready for Microsoft Partner Center.

.EXAMPLE
    .\package-msix.ps1 -SignForLocalTesting
    Builds, packages, and signs the MSIX with a local test certificate for immediate installation.
#>

[CmdletBinding()]
param (
    [string]$Configuration = "release",
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$MakeAppxPath,
    [string]$SignToolPath,
    [switch]$SignForLocalTesting,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

# 1. Map target architecture
$targetToArch = @{
    "x86_64-pc-windows-msvc"  = "x64"
    "aarch64-pc-windows-msvc" = "arm64"
    "i686-pc-windows-msvc"    = "x86"
}

if (-not $targetToArch.ContainsKey($Target)) {
    Write-Error "Unsupported target '$Target'. Supported targets are: $($targetToArch.Keys -join ', ')."
}
$msixArch = $targetToArch[$Target]

function Find-SdkTool {
    param (
        [string]$ToolName,
        [string]$ExplicitPath
    )

    if ($ExplicitPath -and (Test-Path $ExplicitPath)) {
        return (Resolve-Path $ExplicitPath).Path
    }

    # Check PATH
    $cmd = Get-Command $ToolName -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }

    # Check WindowsSdkVerBinPath / WindowsSdkDir
    if ($env:WindowsSdkVerBinPath) {
        $candidate = Join-Path $env:WindowsSdkVerBinPath "x64\$ToolName"
        if (Test-Path $candidate) { return $candidate }
    }

    if ($env:WindowsSdkDir -and $env:WindowsSDKVersion) {
        $sdkVer = $env:WindowsSDKVersion.TrimEnd('\', '/')
        $candidate = Join-Path $env:WindowsSdkDir "bin\$sdkVer\x64\$ToolName"
        if (Test-Path $candidate) { return $candidate }
    }

    # Check standard Windows Kits path
    $sdkBinRoot = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    if (Test-Path $sdkBinRoot) {
        $versions = Get-ChildItem -Path $sdkBinRoot -Directory |
            Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
            Sort-Object { [version]$_.Name } -Descending

        foreach ($ver in $versions) {
            $candidate = Join-Path $ver.FullName "x64\$ToolName"
            if (Test-Path $candidate) {
                return $candidate
            }
        }
    }

    return $null
}

# 2. Locate makeappx.exe
$makeappx = Find-SdkTool -ToolName "makeappx.exe" -ExplicitPath $MakeAppxPath
if (-not $makeappx) {
    Write-Error "Could not find 'makeappx.exe'. Please install the Windows 10/11 SDK or specify -MakeAppxPath."
}
Write-Host "Found makeappx: $makeappx" -ForegroundColor Cyan

# 3. Extract and validate version from Cargo.toml
$cargoTomlPath = Join-Path $PSScriptRoot "Cargo.toml"
if (-not (Test-Path $cargoTomlPath)) {
    Write-Error "Cargo.toml not found at $cargoTomlPath"
}
$cargoContent = Get-Content $cargoTomlPath -Raw
if ($cargoContent -notmatch '(?m)^\s*version\s*=\s*"([^"]+)"') {
    Write-Error "Could not parse version from Cargo.toml"
}
$rawVersion = $matches[1]

# Parse version components (strip semver pre-release tags e.g. -beta.1)
$cleanVersion = ($rawVersion -split '-')[0]
$parts = $cleanVersion -split '\.'
if ($parts.Count -eq 3) {
    $msixVersion = "$cleanVersion.0"
} elseif ($parts.Count -eq 4) {
    $msixVersion = $cleanVersion
} else {
    Write-Error "Version '$rawVersion' cannot be mapped to a 4-part MSIX version (Major.Minor.Build.Revision)."
}

# Validate each numeric component is 0..65535, with Major >= 1 for Microsoft Store compliance
$versionSegments = $msixVersion -split '\.'
for ($i = 0; $i -lt $versionSegments.Count; $i++) {
    [int]$num = 0
    if (-not [int]::TryParse($versionSegments[$i], [ref]$num) -or $num -lt 0 -or $num -gt 65535) {
        Write-Error "MSIX version segment '$($versionSegments[$i])' in '$msixVersion' must be an integer between 0 and 65535."
    }
    if ($i -eq 0 -and $num -lt 1) {
        Write-Error "Microsoft Store MSIX packages require the major version component to be >= 1 (e.g. 1.0.0.0). Found major version: $num."
    }
}
Write-Host "App version: $rawVersion -> MSIX version: $msixVersion (Arch: $msixArch)" -ForegroundColor Cyan

# 4. Build executable
$binaryDir = Join-Path $PSScriptRoot "target\$Target\$Configuration"
$binaryPath = Join-Path $binaryDir "notepad-classic.exe"
if (-not (Test-Path $binaryPath)) {
    # Check non-target-specific release path as fallback
    $fallbackBinary = Join-Path $PSScriptRoot "target\$Configuration\notepad-classic.exe"
    if (Test-Path $fallbackBinary) {
        $binaryPath = $fallbackBinary
    }
}

if (-not $SkipBuild) {
    Write-Host "Building notepad-classic ($Configuration, target: $Target)..." -ForegroundColor Cyan
    $buildArgs = @("build", "--target", $Target)
    if ($Configuration -eq "release") {
        $buildArgs += "--release"
    }
    & cargo @buildArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Cargo build failed with exit code $LASTEXITCODE"
    }
    $binaryPath = Join-Path $PSScriptRoot "target\$Target\$Configuration\notepad-classic.exe"
}

if (-not (Test-Path $binaryPath)) {
    Write-Error "Executable not found at $binaryPath"
}
Write-Host "Using binary: $binaryPath" -ForegroundColor Cyan

# 5. Prepare MSIX staging directory
$layoutDir = Join-Path $PSScriptRoot "target\msix-layout"
if (Test-Path $layoutDir) {
    Remove-Item -Path $layoutDir -Recurse -Force
}
New-Item -ItemType Directory -Path $layoutDir -Force | Out-Null

# Copy binary
Copy-Item -Path $binaryPath -Destination (Join-Path $layoutDir "notepad-classic.exe") -Force

# Copy Assets
$assetsSource = Join-Path $PSScriptRoot "msix\Assets"
if (-not (Test-Path $assetsSource)) {
    Write-Host "Assets not found. Generating assets from notepad-classic.ico..." -ForegroundColor Yellow
    & "$PSScriptRoot\scripts\generate-assets.ps1"
}
$assetsDest = Join-Path $layoutDir "Assets"
Copy-Item -Path $assetsSource -Destination $assetsDest -Recurse -Force

# Process and copy AppxManifest.xml
$manifestSource = Join-Path $PSScriptRoot "msix\AppxManifest.xml"
if (-not (Test-Path $manifestSource)) {
    Write-Error "AppxManifest.xml not found at $manifestSource"
}
$manifestContent = Get-Content $manifestSource -Raw
# Replace version and architecture in manifest Identity element only
$manifestContent = [regex]::Replace($manifestContent, '(<Identity[\s\S]*?\bVersion=")[^"]+', '${1}' + $msixVersion)
$manifestContent = [regex]::Replace($manifestContent, '(<Identity[\s\S]*?\bProcessorArchitecture=")[^"]+', '${1}' + $msixArch)
$manifestDest = Join-Path $layoutDir "AppxManifest.xml"
Set-Content -Path $manifestDest -Value $manifestContent -Encoding Utf8

# 6. Pack with makeappx.exe (Full semantic validation enabled, /nv omitted)
$packageOutputDir = Join-Path $PSScriptRoot "target"
$packageName = "notepad-classic_${msixVersion}_${msixArch}.msix"
$packagePath = Join-Path $packageOutputDir $packageName

Write-Host "Packing MSIX package with semantic validation..." -ForegroundColor Cyan
& $makeappx pack /d "$layoutDir" /p "$packagePath" /o
if ($LASTEXITCODE -ne 0) {
    Write-Error "makeappx pack failed with exit code $LASTEXITCODE"
}

Write-Host "`nPackage created successfully: $packagePath" -ForegroundColor Green

# 7. Local Testing Signing (Optional)
if ($SignForLocalTesting) {
    $signtool = Find-SdkTool -ToolName "signtool.exe" -ExplicitPath $SignToolPath
    if (-not $signtool) {
        Write-Error "Could not find 'signtool.exe'. Please install the Windows 10/11 SDK or specify -SignToolPath."
    }

    $publisherCN = "CN=7AD5BA06-4A9B-4992-8CB4-C9DB75B358B0"
    $cert = Get-ChildItem -Path "Cert:\CurrentUser\My" |
        Where-Object { $_.Subject -eq $publisherCN } |
        Select-Object -First 1

    if (-not $cert) {
        Write-Host "Creating self-signed test certificate matching $publisherCN..." -ForegroundColor Yellow
        $cert = New-SelfSignedCertificate `
            -Type Custom `
            -Subject $publisherCN `
            -KeyUsage DigitalSignature `
            -FriendlyName "Notepad Classic Dev Test" `
            -CertStoreLocation "Cert:\CurrentUser\My" `
            -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3")
    }

    Write-Host "Signing package with certificate thumbprint: $($cert.Thumbprint)..." -ForegroundColor Cyan
    & $signtool sign /fd SHA256 /sha1 $cert.Thumbprint /v "$packagePath"
    if ($LASTEXITCODE -ne 0) {
        Write-Error "signtool sign failed with exit code $LASTEXITCODE"
    }

    Write-Host "`n[SUCCESS] Package signed with local test certificate!" -ForegroundColor Green
    Write-Host "Before installing, ensure the test certificate is trusted in 'Trusted People' on this machine:" -ForegroundColor Yellow
    Write-Host "  1. Export and trust the certificate (run as Administrator if using LocalMachine):"
    Write-Host "     Export-Certificate -Cert (Get-Item Cert:\CurrentUser\My\$($cert.Thumbprint)) -FilePath target\dev-test.cer"
    Write-Host "     Import-Certificate -FilePath target\dev-test.cer -CertStoreLocation Cert:\CurrentUser\TrustedPeople"
    Write-Host "  2. Install the package:"
    Write-Host "     Add-AppxPackage -Path `"$packagePath`""
} else {
    Write-Host "`n[NOTE] Package is unsigned. For Microsoft Store release, submit this .msix directly to Partner Center (Microsoft signs it automatically upon ingestion)." -ForegroundColor DarkGray
    Write-Host "To test locally on this machine, run: .\package-msix.ps1 -SignForLocalTesting" -ForegroundColor DarkGray
}

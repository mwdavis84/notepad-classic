param (
    [string]$IcoPath = "$PSScriptRoot\..\assets\notepad-classic.ico",
    [string]$OutputDir = "$PSScriptRoot\..\msix\Assets"
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

if (-not (Test-Path $IcoPath)) {
    Write-Error "Source ICO file not found at '$IcoPath'"
}

if (-not (Test-Path $OutputDir)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}

function Get-IconBitmap {
    param (
        [string]$Path,
        [int]$Width,
        [int]$Height
    )

    # Attempt to load the best matching native frame from the ICO
    $ico = New-Object System.Drawing.Icon($Path, $Width, $Height)
    $rawBmp = $ico.ToBitmap()

    # If the native frame exactly matches the requested dimensions and is 32bpp ARGB, return it
    if ($rawBmp.Width -eq $Width -and $rawBmp.Height -eq $Height -and $rawBmp.PixelFormat -eq [System.Drawing.Imaging.PixelFormat]::Format32bppArgb) {
        $ico.Dispose()
        return $rawBmp
    }

    # Otherwise, extract the largest frame (256x256) and resample with high quality bicubic interpolation
    $ico.Dispose()
    $rawBmp.Dispose()

    $srcIcon = New-Object System.Drawing.Icon($Path, 256, 256)
    $srcBmp = $srcIcon.ToBitmap()

    $destBmp = New-Object System.Drawing.Bitmap($Width, $Height, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($destBmp)
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $graphics.DrawImage($srcBmp, 0, 0, $Width, $Height)

    $graphics.Dispose()
    $srcBmp.Dispose()
    $srcIcon.Dispose()

    return $destBmp
}

# Define the asset matrix
# Note: Unqualified PNGs (e.g. Square44x44Logo.png) act as the 100% scale candidates.
# To prevent duplicate qualifier candidate conflicts in MakePri, do NOT generate .scale-100.png.

$assetDefinitions = [System.Collections.Generic.List[PSCustomObject]]::new()

# 1. Square44x44Logo - Scale assets
$assetDefinitions.Add([PSCustomObject]@{ Name = "Square44x44Logo.png"; Width = 44; Height = 44 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "Square44x44Logo.scale-125.png"; Width = 55; Height = 55 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "Square44x44Logo.scale-150.png"; Width = 66; Height = 66 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "Square44x44Logo.scale-200.png"; Width = 88; Height = 88 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "Square44x44Logo.scale-400.png"; Width = 176; Height = 176 })

# 2. Square44x44Logo - Target sizes (plated, altform-unplated, altform-lightunplated)
$targetSizes = @(16, 20, 24, 30, 32, 36, 40, 44, 48, 60, 64, 72, 80, 96, 256)
foreach ($size in $targetSizes) {
    $assetDefinitions.Add([PSCustomObject]@{ Name = "Square44x44Logo.targetsize-$size.png"; Width = $size; Height = $size })
    $assetDefinitions.Add([PSCustomObject]@{ Name = "Square44x44Logo.targetsize-${size}_altform-unplated.png"; Width = $size; Height = $size })
    $assetDefinitions.Add([PSCustomObject]@{ Name = "Square44x44Logo.targetsize-${size}_altform-lightunplated.png"; Width = $size; Height = $size })
}

# 3. Square150x150Logo - Scale assets (up to 200% / 300x300; 400% omitted to respect the 200KB WACK limit on 256px source)
$assetDefinitions.Add([PSCustomObject]@{ Name = "Square150x150Logo.png"; Width = 150; Height = 150 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "Square150x150Logo.scale-125.png"; Width = 188; Height = 188 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "Square150x150Logo.scale-150.png"; Width = 225; Height = 225 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "Square150x150Logo.scale-200.png"; Width = 300; Height = 300 })

# 4. StoreLogo - Scale assets
$assetDefinitions.Add([PSCustomObject]@{ Name = "StoreLogo.png"; Width = 50; Height = 50 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "StoreLogo.scale-125.png"; Width = 63; Height = 63 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "StoreLogo.scale-150.png"; Width = 75; Height = 75 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "StoreLogo.scale-200.png"; Width = 100; Height = 100 })
$assetDefinitions.Add([PSCustomObject]@{ Name = "StoreLogo.scale-400.png"; Width = 200; Height = 200 })

Write-Host "Generating visual assets into '$OutputDir'..." -ForegroundColor Cyan

$maxAllowedSizeBytes = 204800 # 200 KB Windows App Certification Kit limit

foreach ($item in $assetDefinitions) {
    $targetPath = Join-Path $OutputDir $item.Name
    $bmp = Get-IconBitmap -Path $IcoPath -Width $item.Width -Height $item.Height
    $bmp.Save($targetPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()

    # Validation
    if (-not (Test-Path $targetPath)) {
        Write-Error "Failed to generate $targetPath"
    }

    $fileInfo = Get-Item $targetPath
    if ($fileInfo.Length -ge $maxAllowedSizeBytes) {
        Write-Error "Asset '$($item.Name)' size ($($fileInfo.Length) bytes) exceeds maximum allowed size ($maxAllowedSizeBytes bytes)."
    }

    # Validate image dimensions and format
    $verifyImg = [System.Drawing.Image]::FromFile($targetPath)
    if ($verifyImg.Width -ne $item.Width -or $verifyImg.Height -ne $item.Height) {
        $verifyImg.Dispose()
        Write-Error "Asset '$($item.Name)' has dimensions $($verifyImg.Width)x$($verifyImg.Height), expected $($item.Width)x$($item.Height)."
    }
    if ($verifyImg.PixelFormat -ne [System.Drawing.Imaging.PixelFormat]::Format32bppArgb) {
        $verifyImg.Dispose()
        Write-Error "Asset '$($item.Name)' has pixel format $($verifyImg.PixelFormat), expected Format32bppArgb."
    }
    $verifyImg.Dispose()

    Write-Host "  Generated $($item.Name) ($($item.Width)x$($item.Height), $($fileInfo.Length) bytes)"
}

Write-Host "`nSuccessfully generated and validated $($assetDefinitions.Count) visual assets." -ForegroundColor Green

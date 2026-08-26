param (
    [string]$IcoPath = "$PSScriptRoot\..\assets\notepad-classic.ico",
    [string]$OutputDir = "$PSScriptRoot\..\msix\Assets"
)

Add-Type -AssemblyName System.Drawing

if (-not (Test-Path $OutputDir)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}

$sizes = @(
    @{ Name = "Square44x44Logo.png"; Width = 44; Height = 44 },
    @{ Name = "Square150x150Logo.png"; Width = 150; Height = 150 },
    @{ Name = "StoreLogo.png"; Width = 50; Height = 50 }
)

$srcIcon = New-Object System.Drawing.Icon($IcoPath, 256, 256)
$srcBmp = $srcIcon.ToBitmap()

foreach ($item in $sizes) {
    $targetWidth = $item.Width
    $targetHeight = $item.Height
    $targetPath = Join-Path $OutputDir $item.Name

    $destBmp = New-Object System.Drawing.Bitmap($targetWidth, $targetHeight, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($destBmp)
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $graphics.DrawImage($srcBmp, 0, 0, $targetWidth, $targetHeight)

    $destBmp.Save($targetPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $destBmp.Dispose()

    Write-Host "Generated $targetPath ($($targetWidth)x$($targetHeight))"
}

$srcBmp.Dispose()
$srcIcon.Dispose()

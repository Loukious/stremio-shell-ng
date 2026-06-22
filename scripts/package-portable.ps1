param(
    [Parameter(Mandatory = $true)]
    [string] $Target,

    [Parameter(Mandatory = $true)]
    [string] $OutputPath,

    [switch] $IncludeSteam
)

$ErrorActionPreference = "Stop"

$releaseDir = Join-Path -Path "target" -ChildPath "$Target\release"
$stageDir = Join-Path -Path ([System.IO.Path]::GetTempPath()) -ChildPath "stremio-portable-$([System.Guid]::NewGuid())"

function Add-PortableFile {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,

        [Parameter(Mandatory = $true)]
        [string] $Name,

        [switch] $Optional
    )

    if (!(Test-Path -LiteralPath $Path)) {
        if ($Optional) {
            return
        }
        throw "Portable input is missing: $Path"
    }

    Copy-Item -LiteralPath $Path -Destination (Join-Path -Path $stageDir -ChildPath $Name)
}

try {
    New-Item -ItemType Directory -Path $stageDir | Out-Null

    Add-PortableFile -Path (Join-Path -Path $releaseDir -ChildPath "stremio-shell-ng.exe") -Name "stremio-shell-ng.exe"
    Add-PortableFile -Path (Join-Path -Path $releaseDir -ChildPath "libmpv-2.dll") -Name "libmpv-2.dll"
    Add-PortableFile -Path "RPCconfig.ini" -Name "RPCconfig.ini"
    Add-PortableFile -Path "mpv.conf" -Name "mpv.conf" -Optional

    if ($IncludeSteam) {
        Add-PortableFile -Path (Join-Path -Path $releaseDir -ChildPath "steam_api64.dll") -Name "steam_api64.dll"
    }

    $outputDir = Split-Path -Parent $OutputPath
    if ($outputDir) {
        New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
    }
    if (Test-Path -LiteralPath $OutputPath) {
        Remove-Item -LiteralPath $OutputPath -Force
    }

    Compress-Archive -Path (Join-Path -Path $stageDir -ChildPath "*") -DestinationPath $OutputPath -CompressionLevel Optimal
    Write-Host "Created $OutputPath"
}
finally {
    if (Test-Path -LiteralPath $stageDir) {
        Remove-Item -LiteralPath $stageDir -Recurse -Force
    }
}

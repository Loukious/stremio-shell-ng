[CmdletBinding(DefaultParameterSetName = "Build")]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc")]
    [string] $Target,

    [Parameter(Mandatory = $true, ParameterSetName = "Build")]
    [string] $ShellDirectory,

    [Parameter(Mandatory = $true, ParameterSetName = "Build")]
    [string] $RuntimePath,

    [Parameter(Mandatory = $true, ParameterSetName = "Stage")]
    [string] $PayloadDirectory,

    [switch] $IncludeSteam
)

$ErrorActionPreference = "Stop"
$expectedMachine = if ($Target -eq "x86_64-pc-windows-msvc") { 0x8664 } else { 0xAA64 }
$forbiddenFile = '^(?:(?:lib)?ffmpeg|ffprobe|ffmprobe)(?:[-_.].*)?\.(?:exe|dll)$|^(?:(?:lib)?av(?:codec|device|filter|format|util)|(?:lib)?sw(?:resample|scale))[-\d.]*\.dll$|^(?:vcruntime|msvcp|msvcr|concrt|vcomp|vccorlib)\d[^\\/]*\.dll$|^ucrtbased\.dll$'
$redistImport = '^(?:vcruntime|msvcp|msvcr|concrt|vcomp|vccorlib)\d[^\\/]*\.dll$|^ucrtbased\.dll$'

function Get-PeBinaryInfo {
    param([Parameter(Mandatory = $true)][string] $Path)

    $bytes = [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Path).Path)
    function Read-U16([long] $Offset) {
        if ($Offset -lt 0 -or $Offset + 2 -gt $bytes.LongLength) { throw "Truncated PE header: $Path" }
        [BitConverter]::ToUInt16($bytes, [int]$Offset)
    }
    function Read-U32([long] $Offset) {
        if ($Offset -lt 0 -or $Offset + 4 -gt $bytes.LongLength) { throw "Truncated PE header: $Path" }
        [BitConverter]::ToUInt32($bytes, [int]$Offset)
    }
    function Read-U64([long] $Offset) {
        if ($Offset -lt 0 -or $Offset + 8 -gt $bytes.LongLength) { throw "Truncated PE header: $Path" }
        [BitConverter]::ToUInt64($bytes, [int]$Offset)
    }

    if ((Read-U16 0) -ne 0x5A4D) { throw "Missing DOS header: $Path" }
    $pe = Read-U32 0x3C
    if ((Read-U32 $pe) -ne 0x00004550) { throw "Missing PE header: $Path" }
    $machine = Read-U16 ($pe + 4)
    $sectionCount = Read-U16 ($pe + 6)
    $optionalSize = Read-U16 ($pe + 20)
    $optional = $pe + 24
    if ((Read-U16 $optional) -ne 0x20B -or $optionalSize -lt 240) {
        throw "Expected a PE32+ binary: $Path"
    }
    $imageBase = Read-U64 ($optional + 24)
    $headerSize = Read-U32 ($optional + 60)
    $directoryCount = Read-U32 ($optional + 108)
    if ($directoryCount -lt 14) { throw "Missing PE data directories: $Path" }
    $directories = $optional + 112
    $sections = @()
    for ($index = 0; $index -lt $sectionCount; $index++) {
        $offset = $optional + $optionalSize + 40 * $index
        $sections += [pscustomobject]@{
            VirtualSize = Read-U32 ($offset + 8)
            Address = Read-U32 ($offset + 12)
            RawSize = Read-U32 ($offset + 16)
            RawOffset = Read-U32 ($offset + 20)
        }
    }
    function Convert-RvaToOffset([long] $Rva) {
        if ($Rva -ge 0 -and $Rva -lt $headerSize -and $Rva -lt $bytes.LongLength) { return $Rva }
        foreach ($section in $sections) {
            $size = [Math]::Max([long]$section.VirtualSize, [long]$section.RawSize)
            if ($Rva -ge $section.Address -and $Rva -lt [long]$section.Address + $size) {
                $relative = $Rva - $section.Address
                if ($relative -ge $section.RawSize) { throw "PE RVA has no file data: $Path" }
                $fileOffset = [long]$section.RawOffset + $relative
                if ($fileOffset -ge $bytes.LongLength) { throw "PE RVA exceeds the file: $Path" }
                return $fileOffset
            }
        }
        throw "Invalid PE RVA: $Path"
    }
    function Read-DllName([long] $Rva) {
        $start = Convert-RvaToOffset $Rva
        $end = $start
        while ($end -lt $bytes.LongLength -and $bytes[$end] -ne 0 -and $end - $start -lt 260) { $end++ }
        if ($end -ge $bytes.LongLength -or $bytes[$end] -ne 0 -or $end -eq $start) {
            throw "Invalid PE import name: $Path"
        }
        [Text.Encoding]::ASCII.GetString($bytes, [int]$start, [int]($end - $start))
    }

    $imports = @()
    foreach ($directory in @(
        @{Index = 1; DescriptorSize = 20; NameOffset = 12; Delay = $false},
        @{Index = 13; DescriptorSize = 32; NameOffset = 4; Delay = $true}
    )) {
        $rva = Read-U32 ($directories + 8 * $directory.Index)
        $size = Read-U32 ($directories + 8 * $directory.Index + 4)
        if ($rva -eq 0) { continue }
        if ($size -lt $directory.DescriptorSize) { throw "Truncated PE import table: $Path" }
        $terminated = $false
        for ($relative = 0; $relative + $directory.DescriptorSize -le $size; $relative += $directory.DescriptorSize) {
            $descriptor = Convert-RvaToOffset ([long]$rva + $relative)
            $nonzero = $false
            for ($field = 0; $field -lt $directory.DescriptorSize; $field += 4) {
                if ((Read-U32 ($descriptor + $field)) -ne 0) { $nonzero = $true }
            }
            if (-not $nonzero) { $terminated = $true; break }
            [long]$nameRva = Read-U32 ($descriptor + $directory.NameOffset)
            if ($directory.Delay -and ((Read-U32 $descriptor) -band 1) -eq 0) {
                if ([uint64]$nameRva -lt $imageBase) { throw "Invalid delayed import address: $Path" }
                $nameRva = [long]([uint64]$nameRva - $imageBase)
            }
            $imports += Read-DllName $nameRva
        }
        if (-not $terminated) { throw "Unterminated PE import table: $Path" }
    }
    [pscustomobject]@{Machine = $machine; Imports = @($imports | Sort-Object -Unique)}
}

$binaryNames = @("stremio-shell-ng.exe", "libmpv-2.dll", "stremio-runtime.exe")
if ($IncludeSteam) { $binaryNames += "steam_api64.dll" }
$paths = @{}
if ($PSCmdlet.ParameterSetName -eq "Stage") {
    $allowed = $binaryNames + @("RPCconfig.ini", "mpv.conf")
    foreach ($entry in Get-ChildItem -LiteralPath $PayloadDirectory -Force) {
        if ($entry.PSIsContainer -or $entry.Name -notin $allowed) {
            throw "Unexpected native shell payload: $($entry.Name)"
        }
    }
    foreach ($name in $binaryNames) { $paths[$name] = Join-Path $PayloadDirectory $name }
} else {
    foreach ($directory in @($ShellDirectory, (Split-Path -Parent $RuntimePath)) | Select-Object -Unique) {
        foreach ($file in Get-ChildItem -LiteralPath $directory -File) {
            if ($file.Name -match $forbiddenFile) { throw "Excluded native shell binary is present: $($file.FullName)" }
        }
    }
    foreach ($name in $binaryNames) {
        $paths[$name] = if ($name -eq "stremio-runtime.exe") { $RuntimePath } else { Join-Path $ShellDirectory $name }
    }
}

foreach ($name in $binaryNames) {
    $info = Get-PeBinaryInfo -Path $paths[$name]
    if ($info.Machine -ne $expectedMachine) {
        throw ("Architecture mismatch for {0}: expected {1}, got PE machine 0x{2:X4}" -f $name, $Target, $info.Machine)
    }
    $redist = @($info.Imports | Where-Object { $_ -match $redistImport })
    if ($redist.Count -gt 0) {
        throw "Native shell payload $name requires separately distributed VC runtime DLLs: $($redist -join ', '). Build the shell and runtime with +crt-static."
    }
    Write-Host "Verified $name ($Target; no VC redistributable imports)"
}

[CmdletBinding()]
param(
    [string]$Channel = $env:TANUKI_CHANNEL,
    [string]$ManifestUrl = $env:TANUKI_MANIFEST_URL,
    [string]$InstallDir = $env:TANUKI_INSTALL_DIR,
    [int]$Retain = 3
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if ([string]::IsNullOrWhiteSpace($Channel)) {
    $Channel = "stable"
}

if ($Channel -notin @("stable", "preview")) {
    Write-Error "Invalid Tanuki channel '$Channel'. Use 'stable' or 'preview'."
    exit 1
}

function Write-Step {
    param([string]$Message)
    Write-Host "==> $Message"
}

function Write-WarningStep {
    param([string]$Message)
    Write-Warning $Message
}

function Get-TanukiCommandSource {
    $existing = Get-Command tanuki -ErrorAction SilentlyContinue
    if ($null -eq $existing) {
        return $null
    }

    return $existing.Source
}

function Test-PathStartsWith {
    param(
        [string]$Path,
        [string]$Prefix
    )

    if ([string]::IsNullOrWhiteSpace($Path) -or [string]::IsNullOrWhiteSpace($Prefix)) {
        return $false
    }

    try {
        $normalizedPath = [System.IO.Path]::GetFullPath($Path)
        $normalizedPrefix = [System.IO.Path]::GetFullPath($Prefix).TrimEnd("\") + "\"
        return $normalizedPath.StartsWith($normalizedPrefix, [System.StringComparison]::OrdinalIgnoreCase)
    } catch {
        return $false
    }
}

function Path-Contains {
    param(
        [string]$PathValue,
        [string]$Entry
    )

    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return $false
    }

    $needle = $Entry.TrimEnd("\")
    foreach ($segment in $PathValue.Split(";", [System.StringSplitOptions]::RemoveEmptyEntries)) {
        if ($segment.TrimEnd("\") -ieq $needle) {
            return $true
        }
    }

    return $false
}

function Prepend-PathEntry {
    param(
        [string]$PathValue,
        [string]$Entry
    )

    $needle = $Entry.TrimEnd("\")
    $segments = @($Entry)
    if (-not [string]::IsNullOrWhiteSpace($PathValue)) {
        $segments += $PathValue.Split(";", [System.StringSplitOptions]::RemoveEmptyEntries) |
            Where-Object { $_.TrimEnd("\") -ine $needle }
    }

    return ($segments -join ";")
}

function Get-ManifestAsset {
    param(
        [object]$Manifest,
        [string]$Target
    )

    $property = $Manifest.assets.PSObject.Properties[$Target]
    if ($null -eq $property) {
        throw "Release manifest does not include a binary for $Target."
    }

    $asset = $property.Value
    if ($asset -is [string]) {
        return [PSCustomObject]@{
            Url = $asset
            Sha256 = $null
        }
    }

    if ([string]::IsNullOrWhiteSpace([string]$asset.url)) {
        throw "Release manifest asset $Target is missing a URL."
    }

    return [PSCustomObject]@{
        Url = [string]$asset.url
        Sha256 = if ([string]::IsNullOrWhiteSpace([string]$asset.sha256)) { $null } else { [string]$asset.sha256 }
    }
}

function ConvertTo-ManifestObject {
    param([object]$Manifest)

    if ($Manifest -isnot [string]) {
        return $Manifest
    }

    $json = $Manifest.TrimStart([char]0xFEFF)
    if ($json.StartsWith("ï»¿")) {
        $json = $json.Substring(3)
    }

    return $json | ConvertFrom-Json
}

function Test-FileDigest {
    param(
        [string]$Path,
        [string]$ExpectedDigest
    )

    if ([string]::IsNullOrWhiteSpace($ExpectedDigest)) {
        return
    }

    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.IO.File]::ReadAllBytes($Path)
        $actual = [System.BitConverter]::ToString($sha256.ComputeHash($bytes)).Replace("-", "").ToLowerInvariant()
    } finally {
        $sha256.Dispose()
    }
    if ($actual -ne $ExpectedDigest.ToLowerInvariant()) {
        throw "Downloaded Tanuki checksum did not match. Expected $ExpectedDigest but got $actual."
    }
}

function Invoke-WithInstallLock {
    param(
        [string]$LockPath,
        [scriptblock]$Script
    )

    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $LockPath) | Out-Null
    $lock = $null
    while ($null -eq $lock) {
        try {
            $lock = [System.IO.File]::Open(
                $LockPath,
                [System.IO.FileMode]::OpenOrCreate,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
        } catch [System.IO.IOException] {
            Start-Sleep -Milliseconds 250
        }
    }

    try {
        & $Script
    } finally {
        $lock.Dispose()
    }
}

function Test-IsJunction {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return $false
    }

    $item = Get-Item -LiteralPath $Path -Force
    return ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -and $item.LinkType -eq "Junction"
}

function Set-ManagedJunction {
    param(
        [string]$LinkPath,
        [string]$TargetPath,
        [string]$ManagedTargetPrefix,
        [bool]$AllowLegacyBinMigration = $false
    )

    if (Test-Path -LiteralPath $LinkPath) {
        $item = Get-Item -LiteralPath $LinkPath -Force
        if (Test-IsJunction -Path $LinkPath) {
            $existingTarget = [string]$item.Target
            if (-not [string]::IsNullOrWhiteSpace($ManagedTargetPrefix)) {
                $ownedPrefix = $ManagedTargetPrefix.TrimEnd("\")
                if (-not $existingTarget.StartsWith($ownedPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
                    throw "Refusing to retarget junction at $LinkPath because it is not managed by this installer."
                }
            }
            if ($existingTarget.Equals($TargetPath, [System.StringComparison]::OrdinalIgnoreCase)) {
                return
            }
            Remove-Item -LiteralPath $LinkPath -Recurse -Force
        } elseif ($item.PSIsContainer) {
            if ((Get-ChildItem -LiteralPath $LinkPath -Force | Select-Object -First 1) -ne $null) {
                if (-not (Move-LegacyBinDirectory -Path $LinkPath -AllowMigration $AllowLegacyBinMigration)) {
                    throw "Refusing to replace non-empty directory at $LinkPath with a junction."
                }
            } else {
                Remove-Item -LiteralPath $LinkPath -Recurse -Force
            }
        } else {
            throw "Refusing to replace file at $LinkPath with a junction."
        }
    }

    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $LinkPath) | Out-Null
    New-Item -ItemType Junction -Path $LinkPath -Target $TargetPath | Out-Null
}

function Move-LegacyBinDirectory {
    param(
        [string]$Path,
        [bool]$AllowMigration
    )

    if (-not $AllowMigration) {
        return $false
    }

    $entries = @(Get-ChildItem -LiteralPath $Path -Force)
    if (($entries | Where-Object { $_.PSIsContainer } | Select-Object -First 1) -ne $null) {
        return $false
    }

    if (($entries | Where-Object { $_.Name -ieq "tanuki.exe" } | Select-Object -First 1) -eq $null) {
        return $false
    }

    $legacyPath = "$Path.legacy.$([System.Guid]::NewGuid().ToString("N"))"
    Move-Item -LiteralPath $Path -Destination $legacyPath
    Write-Step "Moved legacy Tanuki bin directory to $legacyPath."
    return $true
}

function Remove-StaleInstallArtifacts {
    param([string]$ReleasesDir)

    if (-not (Test-Path -LiteralPath $ReleasesDir -PathType Container)) {
        return
    }

    Get-ChildItem -LiteralPath $ReleasesDir -Force -Directory -Filter ".staging.*" -ErrorAction SilentlyContinue |
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
}

function Remove-OldReleases {
    param(
        [string]$ReleasesDir,
        [string]$CurrentReleaseDir,
        [int]$Keep
    )

    if ($Keep -lt 1 -or -not (Test-Path -LiteralPath $ReleasesDir -PathType Container)) {
        return
    }

    $currentFullPath = [System.IO.Path]::GetFullPath($CurrentReleaseDir)
    $releaseDirs = Get-ChildItem -LiteralPath $ReleasesDir -Force -Directory -ErrorAction SilentlyContinue |
        Where-Object { -not $_.Name.StartsWith(".staging.") } |
        Sort-Object LastWriteTimeUtc -Descending
    $kept = 0
    foreach ($dir in $releaseDirs) {
        $dirFullPath = [System.IO.Path]::GetFullPath($dir.FullName)
        if ($dirFullPath.Equals($currentFullPath, [System.StringComparison]::OrdinalIgnoreCase)) {
            $kept += 1
            continue
        }
        if ($kept -lt $Keep) {
            $kept += 1
            continue
        }
        Remove-Item -LiteralPath $dir.FullName -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Resolve-TanukiVersion {
    param(
        [object]$Manifest,
        [string]$SelectedChannel
    )

    if ($SelectedChannel -eq "preview") {
        if ([string]::IsNullOrWhiteSpace([string]$Manifest.base_version) -or [string]::IsNullOrWhiteSpace([string]$Manifest.build_id)) {
            throw "Preview manifest is missing base_version or build_id."
        }
        return "$($Manifest.base_version)-preview.$($Manifest.build_id)"
    }

    if ([string]::IsNullOrWhiteSpace([string]$Manifest.version)) {
        throw "Stable manifest is missing version."
    }
    return [string]$Manifest.version
}

if ($env:OS -ne "Windows_NT") {
    Write-Error "install.ps1 supports Windows only. Use install.sh on Linux or macOS."
    exit 1
}

if (-not [Environment]::Is64BitOperatingSystem) {
    Write-Error "Tanuki requires 64-bit Windows."
    exit 1
}

$architecture = [System.Runtime.InteropServices.RuntimeInformation,mscorlib]::OSArchitecture.ToString()
switch ($architecture) {
    "X64" {
        $target = "windows-x86_64"
        $targetTriple = "x86_64-pc-windows-msvc"
    }
    "Arm64" {
        $target = "windows-x86_64"
        $targetTriple = "x86_64-pc-windows-msvc"
        Write-Step "Windows ARM64 detected; installing the x86_64 build under Windows emulation."
    }
    default {
        Write-Error "Unsupported Windows architecture: $architecture"
        exit 1
    }
}

if ([string]::IsNullOrWhiteSpace($ManifestUrl)) {
    $ManifestUrl = if ($Channel -eq "preview") {
        "https://github.com/BabylenMagnus/tanuki/releases/latest/download/manifest.json"
    } else {
        "https://github.com/BabylenMagnus/tanuki/releases/latest/download/manifest.json"
    }
}

$tanukiHome = if ([string]::IsNullOrWhiteSpace($env:TANUKI_HOME)) {
    Join-Path $env:USERPROFILE ".tanuki"
} else {
    $env:TANUKI_HOME
}
$standaloneRoot = Join-Path $tanukiHome "packages\standalone"
$releasesDir = Join-Path $standaloneRoot "releases"
$currentDir = Join-Path $standaloneRoot "current"
$lockPath = Join-Path $standaloneRoot "install.lock"

$defaultVisibleBinDir = Join-Path $env:LOCALAPPDATA "Programs\Tanuki\bin"
$visibleBinDir = if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $defaultVisibleBinDir
} else {
    $InstallDir
}
$allowLegacyVisibleBinMigration = $false
try {
    $allowLegacyVisibleBinMigration = [System.IO.Path]::GetFullPath($visibleBinDir).TrimEnd("\").Equals(
        [System.IO.Path]::GetFullPath($defaultVisibleBinDir).TrimEnd("\"),
        [System.StringComparison]::OrdinalIgnoreCase
    )
} catch {
    $allowLegacyVisibleBinMigration = $false
}

$existingTanuki = Get-TanukiCommandSource
if (-not [string]::IsNullOrWhiteSpace($existingTanuki) -and -not (Test-PathStartsWith -Path $existingTanuki -Prefix $visibleBinDir)) {
    Write-Step "Detected existing Tanuki command at $existingTanuki"
    Write-WarningStep "PATH order decides which Tanuki runs. This installer will put $visibleBinDir first for future and current PowerShell sessions."
}

Write-Step "Fetching Tanuki $Channel manifest"
$manifest = ConvertTo-ManifestObject -Manifest (Invoke-RestMethod -Uri $ManifestUrl)
$versionIdentity = Resolve-TanukiVersion -Manifest $manifest -SelectedChannel $Channel
$asset = Get-ManifestAsset -Manifest $manifest -Target $target
$safeVersionIdentity = $versionIdentity -replace '[^0-9A-Za-z._-]', '-'
$releaseName = "$safeVersionIdentity-$targetTriple"
$releaseDir = Join-Path $releasesDir $releaseName

Write-Step "Installing Tanuki $versionIdentity for $targetTriple"
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("tanuki-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $tempDir | Out-Null

try {
    Invoke-WithInstallLock -LockPath $lockPath -Script {
        Remove-StaleInstallArtifacts -ReleasesDir $releasesDir

        if (-not (Test-Path -LiteralPath (Join-Path $releaseDir "tanuki.exe") -PathType Leaf)) {
            if (Test-Path -LiteralPath $releaseDir) {
                Remove-Item -LiteralPath $releaseDir -Recurse -Force
            }

            $downloadPath = Join-Path $tempDir "tanuki.exe"
            $stagingDir = Join-Path $releasesDir ".staging.$releaseName.$PID"
            Write-Step "Downloading Tanuki"
            Invoke-WebRequest -Uri $asset.Url -OutFile $downloadPath
            Test-FileDigest -Path $downloadPath -ExpectedDigest $asset.Sha256

            New-Item -ItemType Directory -Force -Path $stagingDir | Out-Null
            Copy-Item -LiteralPath $downloadPath -Destination (Join-Path $stagingDir "tanuki.exe")
            Move-Item -LiteralPath $stagingDir -Destination $releaseDir
        }

        Set-ManagedJunction -LinkPath $currentDir -TargetPath $releaseDir -ManagedTargetPrefix $releasesDir
        Set-ManagedJunction -LinkPath $visibleBinDir -TargetPath $releaseDir -ManagedTargetPrefix $standaloneRoot -AllowLegacyBinMigration $allowLegacyVisibleBinMigration

        $tanukiCommand = Join-Path $visibleBinDir "tanuki.exe"
        & $tanukiCommand --version *> $null
        if ($LASTEXITCODE -ne 0) {
            throw "Installed Tanuki command failed verification: $tanukiCommand --version"
        }

        Remove-OldReleases -ReleasesDir $releasesDir -CurrentReleaseDir $releaseDir -Keep $Retain
    }
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$newUserPath = Prepend-PathEntry -PathValue $userPath -Entry $visibleBinDir
if ($newUserPath -cne $userPath) {
    [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
    Write-Step "PATH updated for future PowerShell sessions."
} else {
    Write-Step "$visibleBinDir is already first on PATH."
}

$newProcessPath = Prepend-PathEntry -PathValue $env:Path -Entry $visibleBinDir
if ($newProcessPath -cne $env:Path) {
    $env:Path = $newProcessPath
}

$resolvedTanuki = Get-TanukiCommandSource
if (-not (Test-PathStartsWith -Path $resolvedTanuki -Prefix $visibleBinDir)) {
    Write-WarningStep "PowerShell still resolves tanuki to $resolvedTanuki. Open a new PowerShell window or inspect PATH order manually."
}

Write-Step "Current PowerShell session: tanuki"
Write-Step "Future PowerShell windows: open a new PowerShell window and run: tanuki"
Write-Host "Tanuki $versionIdentity installed successfully."

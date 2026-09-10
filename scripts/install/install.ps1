<#
.SYNOPSIS
xmux installer for Windows.

.DESCRIPTION
Downloads the release build for this machine, verifies its SHA-256 against the
release's own SHA256SUMS, puts it in a directory named after the version, and
points a launcher at it.

A running executable is locked against overwrite on Windows but not against
rename, so an update that finds the launcher locked renames it aside and puts the
new build in its place. The renamed file is left for a later run to clean up,
because the process still holding it cannot delete its own image.

.EXAMPLE
irm https://raw.githubusercontent.com/zer0ken/xmux/main/scripts/install/install.ps1 | iex

.EXAMPLE
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/zer0ken/xmux/main/scripts/install/install.ps1))) -Version 0.9.6
#>
[CmdletBinding()]
param(
    # The version to install. "latest" asks GitHub which release is latest.
    [string] $Version = $(if ($env:XMUX_VERSION) { $env:XMUX_VERSION } else { 'latest' }),

    # Where the versions live.
    [string] $Root = $(if ($env:XMUX_INSTALL_ROOT) { $env:XMUX_INSTALL_ROOT } else { Join-Path $env:LOCALAPPDATA 'xmux' }),

    # Where the launcher goes. It is this directory that belongs on PATH.
    [string] $BinDir = $(if ($env:XMUX_BIN_DIR) { $env:XMUX_BIN_DIR } else { Join-Path (Join-Path $env:LOCALAPPDATA 'xmux') 'bin' }),

    # Leave the user PATH alone and only report what to add.
    [switch] $NoModifyPath,

    # Print errors only.
    [switch] $Quiet
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'zer0ken/xmux'
$apiBase = "https://api.github.com/repos/$repo"
$downloadBase = "https://github.com/$repo/releases/download"

if ($env:XMUX_NO_MODIFY_PATH -eq '1') { $NoModifyPath = $true }

function Say([string] $Message) {
    if (-not $Quiet) { Write-Host $Message }
}

function Fail([string] $Message) {
    throw "install.ps1: $Message"
}

# --- what to download --------------------------------------------------------

# The release asset for this machine. Only the targets the release workflow
# actually publishes are named; anything else is reported rather than guessed at,
# because the alternative is downloading a binary that cannot run.
function Get-TargetTriple {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    switch ($arch) {
        'X64' { return 'x86_64-pc-windows-msvc' }
        default {
            Fail "no xmux release build for Windows/$arch; build from source with ``cargo install xmux``"
        }
    }
}

function Resolve-Version {
    if ($Version -ne 'latest') { return $Version.TrimStart('v') }
    # The release GitHub itself calls latest, so an unpinned install never lands
    # on a prerelease.
    try {
        $release = Invoke-RestMethod -Uri "$apiBase/releases/latest" -TimeoutSec 60 -Headers @{
            'User-Agent' = 'xmux-installer'
        }
    } catch {
        Fail "cannot read the latest release tag from GitHub: $($_.Exception.Message)"
    }
    if (-not $release.tag_name) { Fail 'the latest release has no version tag' }
    return ([string] $release.tag_name).TrimStart('v')
}

# --- launcher replacement ----------------------------------------------------

# Removes launchers an earlier update renamed aside. The process that held one is
# usually gone by now; one still running keeps its file locked, and that file is
# retried on the next install rather than reported as a failure.
function Remove-StaleLaunchers([string] $Dir, [string] $Name) {
    if (-not (Test-Path -LiteralPath $Dir)) { return }
    Get-ChildItem -LiteralPath $Dir -Filter "$Name.old-*" -File -ErrorAction SilentlyContinue |
        ForEach-Object {
            try { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction Stop } catch { }
        }
}

# Puts $Source at $Target. A target that is not there, or that no process holds,
# is simply written. A target locked by a running xmux is renamed aside first,
# which Windows allows on a running image even though it refuses the overwrite.
function Install-Launcher([string] $Source, [string] $Target) {
    $dir = Split-Path -Parent $Target
    $name = Split-Path -Leaf $Target
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    Remove-StaleLaunchers -Dir $dir -Name $name

    try {
        Copy-Item -LiteralPath $Source -Destination $Target -Force -ErrorAction Stop
        return $false
    } catch [System.IO.IOException] {
        # Fall through to the rename path below.
    } catch [System.UnauthorizedAccessException] {
        # Same: a locked image reports this on some Windows builds.
    }

    $aside = Join-Path $dir ("{0}.old-{1}" -f $name, [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
    try {
        Move-Item -LiteralPath $Target -Destination $aside -Force -ErrorAction Stop
    } catch {
        Fail "cannot replace $Target while it is in use, and cannot move it aside: $($_.Exception.Message)"
    }
    try {
        Copy-Item -LiteralPath $Source -Destination $Target -Force -ErrorAction Stop
    } catch {
        # Put the old launcher back rather than leaving the user with no xmux.
        try { Move-Item -LiteralPath $aside -Destination $Target -Force -ErrorAction Stop } catch { }
        Fail "cannot write $Target : $($_.Exception.Message)"
    }
    return $true
}

# --- PATH --------------------------------------------------------------------

function Test-OnPath([string] $Dir) {
    $entries = ($env:PATH -split ';') | Where-Object { $_ -ne '' }
    foreach ($e in $entries) {
        if ($e.TrimEnd('\') -ieq $Dir.TrimEnd('\')) { return $true }
    }
    return $false
}

# Appends to the USER PATH in the registry, never the machine one, so the install
# needs no elevation and touches nothing another account relies on.
function Add-ToUserPath([string] $Dir) {
    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($null -eq $current) { $current = '' }
    $entries = ($current -split ';') | Where-Object { $_ -ne '' }
    foreach ($e in $entries) {
        if ($e.TrimEnd('\') -ieq $Dir.TrimEnd('\')) {
            Say "PATH already carries $Dir"
            return
        }
    }
    $updated = if ($current.Trim() -eq '') { $Dir } else { "$($current.TrimEnd(';'));$Dir" }
    [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
    $env:PATH = "$env:PATH;$Dir"
    Say "added $Dir to your user PATH"
    Say 'open a new terminal for it to take effect'
}

# --- install -----------------------------------------------------------------

$resolved = Resolve-Version
$triple = Get-TargetTriple
$asset = "xmux-v$resolved-$triple.exe"
$versionDir = Join-Path (Join-Path $Root 'versions') $resolved

Say "xmux $resolved for $triple"

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("xmux-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
    $archive = Join-Path $tmp $asset
    Say "downloading $asset"
    try {
        Invoke-WebRequest -Uri "$downloadBase/v$resolved/$asset" -OutFile $archive -TimeoutSec 300 -UseBasicParsing
    } catch {
        Fail "cannot download $asset; check that v$resolved has a build for $triple"
    }

    $sumsPath = Join-Path $tmp 'SHA256SUMS'
    try {
        Invoke-WebRequest -Uri "$downloadBase/v$resolved/SHA256SUMS" -OutFile $sumsPath -TimeoutSec 60 -UseBasicParsing
    } catch {
        Fail "cannot download the checksums for v$resolved"
    }

    $expected = $null
    foreach ($line in Get-Content -LiteralPath $sumsPath) {
        $parts = $line -split '\s+' | Where-Object { $_ -ne '' }
        if ($parts.Count -ge 2 -and $parts[1] -eq $asset) { $expected = $parts[0]; break }
    }
    if (-not $expected) { Fail "the release lists no checksum for $asset" }

    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected.ToLowerInvariant()) {
        Fail "checksum mismatch for $asset : expected $expected, got $actual"
    }
    Say 'checksum verified'

    # The version directory is built under a staging name and moved into place, so
    # a reader never sees a half-written directory under the version's own name.
    New-Item -ItemType Directory -Path (Join-Path $Root 'versions') -Force | Out-Null
    $staging = Join-Path (Join-Path $Root 'versions') (".staging-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $staging -Force | Out-Null
    Copy-Item -LiteralPath $archive -Destination (Join-Path $staging 'xmux.exe') -Force
    if (Test-Path -LiteralPath $versionDir) {
        Remove-Item -LiteralPath $versionDir -Recurse -Force -ErrorAction SilentlyContinue
    }
    Move-Item -LiteralPath $staging -Destination $versionDir -Force

    $launcher = Join-Path $BinDir 'xmux.exe'
    $wasLocked = Install-Launcher -Source (Join-Path $versionDir 'xmux.exe') -Target $launcher

    Say "installed $versionDir\xmux.exe"
    Say "launcher   $launcher"
    if ($wasLocked) {
        Say 'the previous launcher was in use and has been moved aside; a later install removes it'
    }

    if (Test-OnPath $BinDir) {
        # Already reachable.
    } elseif ($NoModifyPath) {
        Say ''
        Say "$BinDir is not on PATH. Add it with:"
        Say "  [Environment]::SetEnvironmentVariable('Path', (([Environment]::GetEnvironmentVariable('Path','User')) + ';$BinDir'), 'User')"
    } else {
        Add-ToUserPath $BinDir
    }

    # Proof the thing that was installed runs, rather than a claim that it was
    # written. A binary that landed but cannot execute is a failed install.
    $installed = & (Join-Path $versionDir 'xmux.exe') version 2>$null
    if (-not $installed) { Fail "the installed binary did not run; $versionDir\xmux.exe" }
    Say ''
    Say ([string] $installed)
    Say 'Run `xmux doctor` to check what it can reach.'
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

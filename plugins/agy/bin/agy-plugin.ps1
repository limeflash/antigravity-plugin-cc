# agy-plugin launcher for native Windows PowerShell (5.1+), for machines
# without Git Bash. Claude Code itself runs the slash commands through Git
# Bash and the POSIX launcher (./agy-plugin); this is the same logic.
#
# Downloads the agy-plugin binary for this machine once, from the GitHub
# release pinned in .\VERSION, and on EVERY run checks it against the
# SHA-256 committed in .\SHA256SUMS. A tampered asset or cache entry is
# refused, never executed.
#
# Overrides: AGY_PLUGIN_BIN, AGY_PLUGIN_CACHE, AGY_PLUGIN_DOWNLOAD_BASE.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

# Windows PowerShell 5.1 does not escape embedded quotes when it starts a
# native program, so a prompt containing " would reach agy mangled. Build
# the command line ourselves with CommandLineToArgvW quoting rules.
function ConvertTo-NativeArg([string]$a) {
  if ($a -eq '') { return '""' }
  if ($a -notmatch '[\s"]') { return $a }
  $s = $a -replace '(\\*)"', '$1$1\"'   # double backslashes before a quote, escape the quote
  $s = $s -replace '(\\+)$', '$1$1'     # double a trailing backslash run (precedes the closing quote)
  return '"' + $s + '"'
}

function Invoke-Native([string]$exe, [string[]]$argv) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $exe
  $psi.Arguments = (@($argv | ForEach-Object { ConvertTo-NativeArg $_ }) -join ' ')
  $psi.UseShellExecute = $false
  $p = [System.Diagnostics.Process]::Start($psi)
  $p.WaitForExit()
  return $p.ExitCode
}

function Get-Sha256([string]$path) {
  return (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
}

$argv = @($args | ForEach-Object { [string]$_ })

if ($env:AGY_PLUGIN_BIN) {
  exit (Invoke-Native $env:AGY_PLUGIN_BIN $argv)
}

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$version = (Get-Content -Raw -LiteralPath (Join-Path $here 'VERSION')).Trim()

switch ($env:PROCESSOR_ARCHITECTURE) {
  'AMD64' { $arch = 'x86_64' }
  'ARM64' { $arch = 'aarch64' }
  default {
    [Console]::Error.WriteLine("agy-plugin: unsupported CPU architecture '$env:PROCESSOR_ARCHITECTURE'")
    exit 1
  }
}
$asset = "agy-plugin-$arch-pc-windows-msvc.exe"

$expected = $null
foreach ($line in Get-Content -LiteralPath (Join-Path $here 'SHA256SUMS')) {
  $parts = $line.Trim() -split '\s+', 2
  if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $asset) {
    $expected = $parts[0].ToLowerInvariant()
    break
  }
}
if (-not $expected) {
  [Console]::Error.WriteLine("agy-plugin: no checksum for $asset in $here\SHA256SUMS (unsupported platform?)")
  exit 1
}

if ($env:AGY_PLUGIN_CACHE) { $cacheRoot = $env:AGY_PLUGIN_CACHE } else { $cacheRoot = Join-Path $env:LOCALAPPDATA 'agy-plugin' }
$cache = Join-Path $cacheRoot $version
$bin = Join-Path $cache $asset

if ((Test-Path -LiteralPath $bin) -and ((Get-Sha256 $bin) -eq $expected)) {
  exit (Invoke-Native $bin $argv)
}

if ($env:AGY_PLUGIN_DOWNLOAD_BASE) {
  $base = $env:AGY_PLUGIN_DOWNLOAD_BASE
} else {
  $base = 'https://github.com/limeflash/antigravity-plugin-cc/releases/download'
}
$url = "$base/v$version/$asset"
New-Item -ItemType Directory -Force -Path $cache | Out-Null
$tmp = "$bin.download.$PID"
[Console]::Error.WriteLine("agy-plugin: downloading v$version for this machine ($asset)...")
try {
  if ($url -like 'file://*') {
    Copy-Item -LiteralPath ([Uri]$url).LocalPath -Destination $tmp
  } else {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -UseBasicParsing -TimeoutSec 300 -Uri $url -OutFile $tmp
  }
  $actual = Get-Sha256 $tmp
  if ($actual -ne $expected) {
    [Console]::Error.WriteLine("agy-plugin: checksum mismatch for $asset - refusing to run it.")
    [Console]::Error.WriteLine("            expected $expected")
    [Console]::Error.WriteLine("            got      $actual")
    exit 1
  }
  # A concurrent first run may have installed it meanwhile (a running .exe
  # can't be replaced): use theirs if it verifies.
  if ((Test-Path -LiteralPath $bin) -and ((Get-Sha256 $bin) -eq $expected)) {
    Remove-Item -Force -LiteralPath $tmp
  } else {
    try { Move-Item -Force -LiteralPath $tmp -Destination $bin }
    catch {
      if (-not ((Test-Path -LiteralPath $bin) -and ((Get-Sha256 $bin) -eq $expected))) { throw }
    }
  }
} finally {
  if (Test-Path -LiteralPath $tmp) { Remove-Item -Force -LiteralPath $tmp }
}
exit (Invoke-Native $bin $argv)

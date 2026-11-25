# Tests for plugins/agy/bin/agy-plugin.ps1 (Windows). Run under both
# `pwsh` and Windows PowerShell 5.1. Arg 1: path to a built stub_agy.exe,
# used as the fake release "binary" (it records its argv).
param([Parameter(Mandatory = $true)][string]$Stub)
$ErrorActionPreference = 'Stop'

function Fail([string]$m) { [Console]::Error.WriteLine("FAIL: $m"); exit 1 }

# Run the launcher in a child Windows PowerShell 5.1.
# - Arguments go INSIDE the child's script (-EncodedCommand), as PowerShell
#   string literals. Passing them on the command line would cross PS 5.1's
#   own native-argument boundary, which mangles embedded quotes and a
#   trailing backslash before the launcher ever sees them — the test must
#   exercise the launcher's quoting, not PowerShell's.
# - Start-Process with stdout/stderr redirected to files, not `& powershell
#   2>&1`: when one PowerShell runs another, the parent treats the child's
#   stderr as serialized error records (CLIXML) and fails on the launcher's
#   plain "checksum mismatch" lines ("Cannot process the XML from the
#   'Error' stream"); PS 5.1 also turns stderr lines into terminating
#   NativeCommandErrors under ErrorActionPreference=Stop. Files sidestep both.
function Invoke-Launcher([string[]]$launcherArgs, [switch]$MergeStderr) {
  $quoted = @($launcherArgs | ForEach-Object { "'" + ($_ -replace "'", "''") + "'" }) -join ' '
  $script = "& '" + ($launcher -replace "'", "''") + "' $quoted; exit `$LASTEXITCODE"
  $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($script))
  $outFile = Join-Path $work ("out-" + [Guid]::NewGuid() + ".txt")
  $errFile = Join-Path $work ("err-" + [Guid]::NewGuid() + ".txt")
  # Started from pwsh 7, Windows PowerShell inherits pwsh's PSModulePath and
  # loads pwsh's modules (Get-FileHash & co. then fail); let it use its own.
  $savedModulePath = $env:PSModulePath
  $env:PSModulePath = $null
  try {
    $p = Start-Process -FilePath 'powershell' -Wait -PassThru -NoNewWindow `
      -ArgumentList @('-NoProfile', '-NonInteractive', '-OutputFormat', 'Text', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $enc) `
      -RedirectStandardOutput $outFile -RedirectStandardError $errFile
  } finally {
    $env:PSModulePath = $savedModulePath
  }
  $o = "$(Get-Content -Raw -LiteralPath $outFile)"
  $e = "$(Get-Content -Raw -LiteralPath $errFile)"
  if ($MergeStderr) { $o = "$o`n$e" }
  Remove-Item -Force -LiteralPath $outFile, $errFile -ErrorAction SilentlyContinue
  return @{ Out = $o; Err = $e; Code = $p.ExitCode }
}

$repo = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$work = Join-Path ([IO.Path]::GetTempPath()) ("agy-ps1-test-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path "$work\plugin\bin", "$work\release\v9.9.9" | Out-Null
try {
  switch ($env:PROCESSOR_ARCHITECTURE) { 'ARM64' { $arch = 'aarch64' } default { $arch = 'x86_64' } }
  $asset = "agy-plugin-$arch-pc-windows-msvc.exe"
  Copy-Item -LiteralPath "$repo\plugins\agy\bin\agy-plugin.ps1" -Destination "$work\plugin\bin\"
  Set-Content -LiteralPath "$work\plugin\bin\VERSION" -Value '9.9.9'
  Copy-Item -LiteralPath $Stub -Destination "$work\release\v9.9.9\$asset"
  $good = (Get-FileHash -Algorithm SHA256 -LiteralPath "$work\release\v9.9.9\$asset").Hash.ToLowerInvariant()
  Set-Content -LiteralPath "$work\plugin\bin\SHA256SUMS" -Value "$good  $asset"
  $launcher = "$work\plugin\bin\agy-plugin.ps1"
  $env:AGY_PLUGIN_DOWNLOAD_BASE = ([Uri]"$work\release").AbsoluteUri
  $env:AGY_PLUGIN_CACHE = "$work\cache"
  $env:STUB_RECORD = "$work\record.txt"
  $env:STUB_RESPONSE = 'hello from stub'

  # 1. download + verify + exec, with an argument containing quotes and spaces.
  $tricky = 'say "hi" to C:\path with spaces\'
  $r = Invoke-Launcher @('ask', $tricky)
  if ($r.Code -ne 0) { Fail "first run exit $($r.Code): $($r.Err)" }
  if (-not ($r.Out -match 'hello from stub')) { Fail "unexpected output: $($r.Out)" }
  $argv = ((Get-Content -Raw -LiteralPath "$work\record.txt").Split("`t")[1]).Split([char]0x1f)
  if ($argv[0] -ne 'ask' -or $argv[1] -ne $tricky) { Fail "argv mangled: $($argv -join ' | ')" }
  Write-Output 'ok 1 download + verify + exec, quotes preserved'

  # 2. tampered cache is not executed (re-downloaded instead).
  Set-Content -LiteralPath "$work\cache\9.9.9\$asset" -Value 'garbage'
  $r = Invoke-Launcher @('version')
  if ($r.Code -ne 0) { Fail "re-download failed: $($r.Err)" }
  if ((Get-FileHash -Algorithm SHA256 -LiteralPath "$work\cache\9.9.9\$asset").Hash.ToLowerInvariant() -ne $good) { Fail 'cache not repaired' }
  Write-Output 'ok 2 tampered cache refused'

  # 3. tampered release asset is refused.
  Remove-Item -Recurse -Force "$work\cache"
  Set-Content -LiteralPath "$work\release\v9.9.9\$asset" -Value 'evil'
  $r = Invoke-Launcher @('x') -MergeStderr
  if ($r.Code -eq 0) { Fail 'tampered asset accepted' }
  if (-not ($r.Out -match 'checksum mismatch')) { Fail "no mismatch message: $($r.Out)" }
  if (Test-Path -LiteralPath "$work\cache\9.9.9\$asset") { Fail 'tampered asset cached' }
  Write-Output 'ok 3 tampered asset refused'
  Write-Output 'launcher.ps1: all tests passed'
} finally {
  Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
}

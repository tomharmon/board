param([Parameter(Mandatory)][string]$Tag, [switch]$Signed)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
if ($env:CI -ne 'true') { throw 'Run this installer test only on a disposable Windows CI runner.' }
$Msi = (Resolve-Path 'target/packages/Board-Windows-x64.msi').Path
$Install = Join-Path $env:LOCALAPPDATA 'Programs\Board'
$Link = Join-Path ([Environment]::GetFolderPath('Programs')) 'Board.lnk'
$OriginalPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ((Test-Path $Install) -or (Test-Path $Link)) { throw 'Board already installed on this runner' }
function Check-Signature($File) {
    $Sig = Get-AuthenticodeSignature -LiteralPath $File
    if ($Sig.Status -ne 'Valid' -or !$Sig.TimeStamperCertificate) { throw "Invalid/missing timestamped Authenticode signature: $File" }
}
function Run-Msi($Command) {
    $Log = Join-Path $env:RUNNER_TEMP "board-$($Command.TrimStart('/')).log"
    $Proc = Start-Process msiexec.exe -ArgumentList "$Command `"$Msi`" /qn /norestart /L*v `"$Log`"" -Wait -PassThru
    if ($Proc.ExitCode -ne 0) { Get-Content $Log -Tail 100; throw "msiexec failed: $($Proc.ExitCode)" }
}
if ($Signed) { Check-Signature $Msi }
Run-Msi /i
try {
    $Ui = Join-Path $Install 'board-ui.exe'
    $Cli = Join-Path $Install 'board.exe'
    if ($Signed) { Check-Signature $Cli; Check-Signature $Ui }
    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $Entries = @($UserPath -split ';' | ForEach-Object { $_.TrimEnd('\') })
    if ($Entries -notcontains $Install) { throw 'CLI directory not registered on user PATH' }
    if (!(Test-Path $Link)) { throw 'Start menu shortcut was not installed' }
    $Launcher = (New-Object -ComObject WScript.Shell).CreateShortcut($Link)
    if ($Launcher.TargetPath -ne $Ui -or !$Launcher.IconLocation) { throw 'Missing GUI target or launcher icon' }
    if ((& $Cli --version) -notmatch [regex]::Escape($Tag.Substring(1))) { throw 'Wrong CLI version' }
    python packaging/smoke_init.py $Cli
    $env:BOARD_DB = Join-Path $env:RUNNER_TEMP 'board-smoke.sqlite3'
    $Notices = Join-Path $env:RUNNER_TEMP 'board-installed-notices.txt'
    $Proc = Start-Process $Ui -ArgumentList '--ui-notices' -RedirectStandardOutput $Notices -Wait -PassThru
    if ($Proc.ExitCode -ne 0 -or !(Select-String -Path $Notices -Pattern 'Rune')) { throw 'Packaged Rune binary failed' }
    if (!(Test-Path (Join-Path $Install 'NOTICES.txt'))) { throw 'Missing dependency notices' }
} finally { Run-Msi /x }
if ((Test-Path $Install) -or (Test-Path $Link)) { throw 'Uninstall left installed app files or shortcut' }
if ([Environment]::GetEnvironmentVariable('Path', 'User') -ne $OriginalPath) { throw 'Uninstall did not restore user PATH' }

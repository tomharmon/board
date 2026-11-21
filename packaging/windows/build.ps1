param([ValidateSet('binaries','msi')][string]$Step, [Parameter(Mandatory)][string]$Tag)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$Version = python packaging/release.py version $Tag
$Work = Join-Path $PWD 'target/package-win'
$Output = Join-Path $PWD 'target/packages'
$Target = 'x86_64-pc-windows-msvc'
$Binaries = Join-Path $PWD "target/$Target/release"
$Icon = Join-Path $PWD 'packaging/icons/board.ico'
New-Item -Force -ItemType Directory $Work, $Output | Out-Null
if ($Step -eq 'binaries') {
    $Kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    $Rc = (Get-ChildItem "$Kits\*\x64\rc.exe" | Sort-Object { [version]$_.Directory.Parent.Name } -Descending | Select-Object -First 1).FullName
    if (!$Rc) { throw 'Windows SDK Resource Compiler not found' }
    $Resource = (Resolve-Path 'packaging/windows/board.rc').Path
    Push-Location (Split-Path $Icon)
    try { & $Rc /nologo /fo "$Work\Board.res" $Resource } finally { Pop-Location }
    # Do not depend on a separately-installed Visual C++ Redistributable.
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-Ctarget-feature=+crt-static'
    cargo build --locked --release --target $Target -p board-cli --bin board
    # The GUI subsystem avoids a console window when opened by the Start menu.
    cargo rustc --locked --release --target $Target -p board-ui --bin board-ui -- "-Clink-arg=$Work\Board.res" '-Clink-arg=/SUBSYSTEM:WINDOWS' '-Clink-arg=/ENTRY:mainCRTStartup'
    $ReadObj = (Get-Command llvm-readobj.exe -ErrorAction SilentlyContinue).Source
    if (!$ReadObj) { $ReadObj = "$env:ProgramFiles\LLVM\bin\llvm-readobj.exe" }
    $Imports = & $ReadObj --coff-imports "$Binaries/board.exe" "$Binaries/board-ui.exe"
    if ($Imports -match 'Name: (vcruntime|msvcp)[0-9]') { throw 'A binary requires a separately-installed MSVC runtime' }
    python packaging/release.py notices $Target "$Work/NOTICES.txt"
} else {
    $Wix = if ($env:WIX) { Join-Path $env:WIX 'bin' } else { "${env:ProgramFiles(x86)}\WiX Toolset v3.14\bin" }
    & "$Wix/candle.exe" -nologo -arch x64 "-dVersion=$Version" "-dBinDir=$Binaries" "-dIcon=$Icon" "-dNotices=$Work\NOTICES.txt" -out "$Work\Board.wixobj" packaging/windows/Board.wxs
    & "$Wix/light.exe" -nologo -pdbout "$Work\Board.wixpdb" -out "$Output\Board-Windows-x64.msi" "$Work\Board.wixobj"
}

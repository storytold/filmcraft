@echo off
rem Launch FilmCraft from a source checkout (release build).
rem
rem   launch-filmcraft.bat [--log] [--rebuild] [FilmCraft arguments...]
rem
rem Runs target\release\filmcraft.exe. If it has not been built yet, builds it first
rem (cargo build --release -p filmcraft; a first build takes a long time).
rem
rem   --rebuild  rebuild before launching.
rem   --log      troubleshooting mode: debug logging and full backtraces, everything the app
rem              prints plus its exit code is written to log.txt in the repository root
rem              (overwritten each run). After FilmCraft exits, the tail of its own log file and
rem              any crash logs are appended. Send log.txt when reporting a crash.
rem
rem Any other arguments go to FilmCraft (for example --control 9876 or a project file).
rem Needs Rust (rustup). cargo is looked up on PATH, then in %USERPROFILE%\.cargo\bin.

setlocal EnableExtensions
set "ROOT=%~dp0..\.."
pushd "%ROOT%" || exit /b 1
set "EXE=%CD%\target\release\filmcraft.exe"
set "LOGTXT=%CD%\log.txt"
set "APPLOGS=%APPDATA%\FilmCraft\Logs"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

set "REBUILD="
set "LOGMODE="
set "ARGS="
:parse
if "%~1"=="" goto parsed
if /i "%~1"=="--rebuild" (set "REBUILD=1") else if /i "%~1"=="--log" (set "LOGMODE=1") else set ARGS=%ARGS% %1
shift
goto parse
:parsed
if not exist "%EXE%" set "REBUILD=1"

if defined REBUILD (
  where cargo >nul 2>nul || (
    echo cargo not found. Install Rust from https://rustup.rs and try again.
    pause
    popd
    exit /b 1
  )
  echo Building FilmCraft ^(release^)...
  if defined LOGMODE (
    cargo build --release -p filmcraft > "%LOGTXT%" 2>&1
  ) else (
    cargo build --release -p filmcraft
  )
  if errorlevel 1 (
    echo Build failed.
    if defined LOGMODE echo Build output is in "%LOGTXT%"
    pause
    popd
    exit /b 1
  )
)

if not defined LOGMODE (
  rem The release binary is a Windows GUI app: start it detached so this window can close.
  start "" "%EXE%" %ARGS%
  popd
  exit /b 0
)

rem ---- Troubleshooting mode: write everything to log.txt ----
set "RUST_LOG=warn,filmcraft*=debug"
set "RUST_BACKTRACE=full"
(
  echo ==== FilmCraft log %DATE% %TIME% ====
  echo exe: %EXE%
  echo args:%ARGS%
  echo RUST_LOG=%RUST_LOG%
  echo ---- app output ----
) > "%LOGTXT%"
echo Running FilmCraft with logging to "%LOGTXT%" ...
start "" /wait /b "%EXE%" %ARGS% >> "%LOGTXT%" 2>&1
set "EXITCODE=%ERRORLEVEL%"
(
  echo ---- FilmCraft exited with code %EXITCODE% ----
  echo ^(0 = normal exit; -1073741819 = access violation; -1073740791 = stack buffer overrun;
  echo  -1073741571 = stack overflow; -1073740940 = heap corruption; 101 = Rust panic^)
  echo.
  echo ---- app log: %APPLOGS%\filmcraft.log ^(last 200 lines^) ----
) >> "%LOGTXT%"
if exist "%APPLOGS%\filmcraft.log" powershell -NoProfile -Command "Get-Content -LiteralPath '%APPLOGS%\filmcraft.log' -Tail 200" >> "%LOGTXT%" 2>&1
echo. >> "%LOGTXT%"
echo ---- crash logs from the last day ---- >> "%LOGTXT%"
powershell -NoProfile -Command "Get-ChildItem -LiteralPath '%APPLOGS%' -Filter 'crash-*' -ErrorAction SilentlyContinue | Where-Object { $_.LastWriteTime -gt (Get-Date).AddDays(-1) } | ForEach-Object { '== ' + $_.FullName; Get-Content -LiteralPath $_.FullName -Tail 150 }" >> "%LOGTXT%" 2>&1
echo ---- Windows crash events for filmcraft.exe ^(last day^) ---- >> "%LOGTXT%"
powershell -NoProfile -Command "Get-WinEvent -FilterHashtable @{LogName='Application';StartTime=(Get-Date).AddDays(-1)} -ErrorAction SilentlyContinue | Where-Object { $_.Message -match 'filmcraft' -and $_.ProviderName -match 'Application Error|Windows Error Reporting' } | ForEach-Object { $_.TimeCreated; $_.Message }" >> "%LOGTXT%" 2>&1
echo Done. FilmCraft exited with code %EXITCODE%. Log: "%LOGTXT%"
if not "%EXITCODE%"=="0" pause
popd
endlocal

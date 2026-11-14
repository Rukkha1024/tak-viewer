@echo off
rem TAK viewer: build again after code changes. Double-click this file.
rem Option /nopause = do not wait for a key at the end (for agents).
rem Open viewers may stay open: a running tak_viewer.exe is renamed aside (Windows allows
rem renaming a running exe) and keeps running; the new build takes the usual name.
chcp 65001 >nul
setlocal

if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
"%SystemRoot%\System32\where.exe" cargo >nul 2>nul
if errorlevel 1 (
  echo Rust 빌드 도구^(cargo^)를 찾지 못했어요. README.md "처음 설치"를 보세요.
  goto :fail
)

set "CARGO_TARGET_DIR=%LOCALAPPDATA%\tak_viewer\target_public"
set "REL=%CARGO_TARGET_DIR%\release"
set "MOVED="
rem release\X.exe and release\deps\X.exe are the same file (cargo links them), so both names move.
for %%D in ("%REL%" "%REL%\deps") do (
  for %%N in (tak_viewer tak_dump) do (
    call :aside "%%~D" %%N
    if errorlevel 1 goto :fail
  )
)

echo 빌드 중이에요. 처음에는 몇 분 걸려요...
cargo build --release --manifest-path "%~dp0Cargo.toml"
if errorlevel 1 (
  echo.
  echo 빌드 실패. 위쪽의 error 줄을 확인하세요.
  goto :fail
)
if not exist "%REL%\tak_viewer.exe" (
  echo.
  echo 빌드는 끝났는데 %REL%\tak_viewer.exe가 없어요.
  goto :fail
)

echo.
echo 완료: %REL%\tak_viewer.exe
if defined MOVED echo 켜 둔 뷰어는 예전 버전 그대로 돌아요. 껐다 켜면 새 버전이에요.
echo 실행 파일: "%REL%\tak_viewer.exe"
if /I not "%~1"=="/nopause" pause
exit /b 0

:fail
if /I not "%~1"=="/nopause" pause
exit /b 1

rem call :aside <folder> <name>
rem Removes copies moved aside by earlier builds once they are closed (running ones stay).
rem If <name>.exe is running (cannot be opened for writing), renames it to <name>.old-N.exe.
:aside
del /q "%~1\%~2.old-*.exe" >nul 2>nul
if not exist "%~1\%~2.exe" exit /b 0
2>nul (>>"%~1\%~2.exe" (call )) && exit /b 0
set /a N=1
:aside_next
if exist "%~1\%~2.old-%N%.exe" (
  set /a N+=1
  goto :aside_next
)
ren "%~1\%~2.exe" "%~2.old-%N%.exe" 2>nul
if errorlevel 1 (
  echo %~1\%~2.exe 이름을 바꾸지 못했어요.
  exit /b 1
)
set "MOVED=1"
exit /b 0

@echo off
setlocal
rem Run from an x64 Visual Studio Developer Command Prompt.
if "%~1"=="" (
  echo Usage: build-fixtures.cmd OUTPUT_DIRECTORY
  exit /b 2
)
if not exist "%~f1" mkdir "%~f1"
if errorlevel 1 exit /b %errorlevel%
pushd "%~f1"
cl /nologo /Fe:noop_python.exe "%~dp0noop.c"
if errorlevel 1 goto failed
cl /nologo /Fe:uvshim.exe "%~dp0uvshim.c"
if errorlevel 1 goto failed
popd
echo Set PARITY_BOOTSTRAP_TOOLS_DIR=%~f1 before running bootstrap_t01 and bootstrap_t02.
exit /b 0
:failed
popd
exit /b 1

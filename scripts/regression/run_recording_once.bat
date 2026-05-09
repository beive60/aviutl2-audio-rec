@echo off
setlocal enabledelayedexpansion

if "%~1"=="" goto :usage

set CASE_ID=%~1
set DURATION_SEC=%~2
if "%DURATION_SEC%"=="" set DURATION_SEC=5

set OUTPUT_DIR=%~3
if "%OUTPUT_DIR%"=="" set OUTPUT_DIR=regression\artifacts

set CLI_PATH=%~4
if "%CLI_PATH%"=="" set CLI_PATH=target\debug\audio_rec_cli.exe

if not exist "%CLI_PATH%" (
  echo [ERROR] CLI が見つかりません: %CLI_PATH%
  exit /b 2
)

if not exist "%OUTPUT_DIR%" mkdir "%OUTPUT_DIR%"
if not exist "%OUTPUT_DIR%\logs" mkdir "%OUTPUT_DIR%\logs"

for /f %%i in ('powershell -NoProfile -Command "Get-Date -Format yyyyMMdd-HHmmss"') do set TS=%%i

set WAV_FILE=%OUTPUT_DIR%\%CASE_ID%-%TS%.wav
set LOG_FILE=%OUTPUT_DIR%\logs\%CASE_ID%-%TS%.log

echo [INFO] CASE_ID=%CASE_ID% > "%LOG_FILE%"
echo [INFO] DURATION_SEC=%DURATION_SEC% >> "%LOG_FILE%"
echo [INFO] WAV_FILE=%WAV_FILE% >> "%LOG_FILE%"
echo [INFO] CLI_PATH=%CLI_PATH% >> "%LOG_FILE%"

echo [INFO] start 実行中...
"%CLI_PATH%" start "%WAV_FILE%" >> "%LOG_FILE%" 2>&1
if errorlevel 1 (
  echo [ERROR] start に失敗しました。詳細: %LOG_FILE%
  exit /b 3
)

echo [INFO] %DURATION_SEC% 秒待機します...
timeout /t %DURATION_SEC% /nobreak >nul

echo [INFO] stop 実行中...
"%CLI_PATH%" stop >> "%LOG_FILE%" 2>&1
if errorlevel 1 (
  echo [ERROR] stop に失敗しました。詳細: %LOG_FILE%
  exit /b 4
)

echo [INFO] 完了
echo [INFO] WAV: %WAV_FILE%
echo [INFO] LOG: %LOG_FILE%
exit /b 0

:usage
echo 使用方法:
echo   run_recording_once.bat CASE_ID [DURATION_SEC] [OUTPUT_DIR] [CLI_PATH]
echo 例:
echo   run_recording_once.bat CASE01 5
echo   run_recording_once.bat CASE01 5 regression\artifacts .aviutl2-cli\development\data\audio_rec_cli.exe
exit /b 1

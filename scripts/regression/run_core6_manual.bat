@echo off
setlocal

set CLI_PATH=%~1
if "%CLI_PATH%"=="" set CLI_PATH=target\debug\audio_rec_cli.exe

set RUNNER=scripts\regression\run_recording_once.bat
set OUTPUT_DIR=regression\artifacts

if not exist "%RUNNER%" (
  echo [ERROR] runner が見つかりません: %RUNNER%
  exit /b 2
)

if not exist "%CLI_PATH%" (
  echo [ERROR] CLI が見つかりません: %CLI_PATH%
  echo [INFO] 引数で指定してください。
  echo [INFO] 例: run_core6_manual.bat .aviutl2-cli\development\data\audio_rec_cli.exe
  exit /b 3
)

echo ==================================================
echo  コア 6 実機回帰テスト
echo ==================================================
echo CLI: %CLI_PATH%
echo
echo 注意:
echo - 各ケース開始前に、指定の AviUtl2 プロジェクトを手動で開いてください。
echo - カーソルは frame=16, layer=1 に合わせてください。
echo - ケース失敗時はそこで終了します。
echo

call :run_case CASE01 5 "core01_empty_target.aup2 を開く。target(frame=16,layer=1) は空。"
if errorlevel 1 goto :failed

call :run_case CASE02 5 "core02_blocked_1f.aup2 を開く。frame=16 を1フレームだけ占有。"
if errorlevel 1 goto :failed

call :run_case CASE03 5 "core03_blocked_20f.aup2 を開く。frame=16から20フレーム連続占有。"
if errorlevel 1 goto :failed

call :run_case CASE04 5 "core04_blocked_over_limit.aup2 を開く。1000フレーム超の連続占有。"
if errorlevel 1 goto :failed

call :run_idempotency_case
if errorlevel 1 goto :failed

call :run_three_sessions
if errorlevel 1 goto :failed

echo.
echo [OK] コア 6 ケースが完了しました。
echo [INFO] ログ: %OUTPUT_DIR%\logs
exit /b 0

:run_case
set CASE_ID=%~1
set DURATION=%~2
set PREP_TEXT=%~3

echo.
echo --------------------------------------------------
echo [%CASE_ID%]
echo 準備: %PREP_TEXT%
echo 準備できたら何かキーを押してください。
pause >nul

call "%RUNNER%" %CASE_ID% %DURATION% "%OUTPUT_DIR%" "%CLI_PATH%"
if errorlevel 1 (
  echo [ERROR] %CASE_ID% に失敗しました。
  exit /b 1
)

echo [INFO] %CASE_ID% 実行完了。AviUtl2 上で結果を目視確認してください。
echo 次へ進む場合は何かキーを押してください。
pause >nul
exit /b 0

:run_idempotency_case
echo.
echo --------------------------------------------------
echo [CASE05] 冪等性（start/stop 二重実行）
echo 準備: core01_empty_target.aup2 を開き、frame=16,layer=1 にカーソルを置く。
echo 準備できたら何かキーを押してください。
pause >nul

for /f %%i in ('powershell -NoProfile -Command "Get-Date -Format yyyyMMdd-HHmmss"') do set TS=%%i
set LOG_FILE=%OUTPUT_DIR%\logs\CASE05-%TS%.log
set WAV_FILE=%OUTPUT_DIR%\CASE05-%TS%.wav

if not exist "%OUTPUT_DIR%" mkdir "%OUTPUT_DIR%"
if not exist "%OUTPUT_DIR%\logs" mkdir "%OUTPUT_DIR%\logs"

echo [INFO] WAV_FILE=%WAV_FILE% > "%LOG_FILE%"
"%CLI_PATH%" start "%WAV_FILE%" >> "%LOG_FILE%" 2>&1
if errorlevel 1 exit /b 1
"%CLI_PATH%" start "%WAV_FILE%" >> "%LOG_FILE%" 2>&1
if errorlevel 1 exit /b 1
timeout /t 2 /nobreak >nul
"%CLI_PATH%" stop >> "%LOG_FILE%" 2>&1
if errorlevel 1 exit /b 1
"%CLI_PATH%" stop >> "%LOG_FILE%" 2>&1
if errorlevel 1 exit /b 1

echo [INFO] CASE05 完了。ログ: %LOG_FILE%
echo 目視確認後、次へ進む場合は何かキーを押してください。
pause >nul
exit /b 0

:run_three_sessions
echo.
echo --------------------------------------------------
echo [CASE06] 連続3セッション
echo 準備: core01_empty_target.aup2 を開き、frame=16,layer=1 にカーソルを置く。
echo 準備できたら何かキーを押してください。
pause >nul

call "%RUNNER%" CASE06A 3 "%OUTPUT_DIR%" "%CLI_PATH%"
if errorlevel 1 exit /b 1

call "%RUNNER%" CASE06B 3 "%OUTPUT_DIR%" "%CLI_PATH%"
if errorlevel 1 exit /b 1

call "%RUNNER%" CASE06C 3 "%OUTPUT_DIR%" "%CLI_PATH%"
if errorlevel 1 exit /b 1

echo [INFO] CASE06 完了。3セッション連続成功を確認してください。
echo 何かキーを押すと終了します。
pause >nul
exit /b 0

:failed
echo.
echo [NG] コア 6 ケースに失敗しました。
echo [INFO] ログを確認してください: %OUTPUT_DIR%\logs
exit /b 1

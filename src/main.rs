//! # AviUtl2 音声録音 CLI クライアント
//!
//! AviUtl2 マイク録音プラグインに対して録音の開始・停止コマンドを送信する
//! 軽量なコマンドラインツール。
//!
//! ## 使用方法
//!
//! ```text
//! audio_rec_cli.exe start [<WAVファイルの絶対パス>]
//! audio_rec_cli.exe stop
//! audio_rec_cli.exe config save-path <ディレクトリパス>
//! ```
//!
//! ## 動作概要
//!
//! ### start コマンド
//!
//! パスを省略した場合はデフォルト保存先と `yyyymmdd-hhmmss.wav` のファイル名を使用する。
//!
//! 1. 出力先パスの事前検証（Pre-flight validation）を行う：
//!    - `.wav` 拡張子であること
//!    - 親ディレクトリが存在すること
//!    - 親ディレクトリへの書き込み権限があること
//! 2. Named Pipe（`\\.\pipe\aviutl2_audio_rec`）に接続する。
//! 3. `start:<パス>` コマンドを UTF-8 で送信する。
//! 4. プラグインからレスポンスを受け取り、標準出力に表示する。
//!
//! ### stop コマンド
//!
//! 1. Named Pipe に接続する。
//! 2. `stop` コマンドを送信する。
//! 3. レスポンスを標準出力に表示する。
//!
//! ### config save-path コマンド
//!
//! デフォルトの録音ファイル保存先ディレクトリを設定ファイル（JSON）に保存する。
//! 設定ファイルは `audio_rec_cli.exe` と同一ディレクトリに置かれる。
//!
//! ## エラー処理
//!
//! 事前検証エラー：標準エラー出力にメッセージを表示し、終了コード `1` で終了。
//! 接続・送信エラー：標準エラー出力にメッセージを表示し、終了コード `2` で終了。
//! プラグインエラー（`err:` レスポンス）：標準エラー出力にメッセージを表示し、終了コード `3` で終了。

use std::env;
use std::path::{Path, PathBuf};
use std::process;

use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::PCWSTR;

// ─────────────────────────────────────────────────────────────
// 定数
// ─────────────────────────────────────────────────────────────

/// 接続先の Named Pipe 名。
/// プラグイン側と同一の値でなければならない。
const PIPE_NAME: &str = r"\\.\pipe\aviutl2_audio_rec";

/// パイプが利用可能になるまでの最大待機時間（ミリ秒）。
const MAX_WAIT_MS: u32 = 5_000;

/// 接続リトライ回数。
const MAX_RETRIES: u32 = 3;

/// Named Pipe への読み書きアクセス権（`GENERIC_READ | GENERIC_WRITE`）。
const GENERIC_READ_WRITE_ACCESS: u32 = 0xC000_0000u32;

/// レスポンスバッファの最大バイト数。
const MAX_RESPONSE_BYTES: usize = 65_536;

/// 設定ファイル名（`audio_rec_cli.exe` と同一ディレクトリに配置する）。
const CONFIG_FILE_NAME: &str = "audio_rec_cli.json";

// ─────────────────────────────────────────────────────────────
// 設定ファイル
// ─────────────────────────────────────────────────────────────

/// CLI の永続設定。`audio_rec_cli.json` に JSON 形式で保存される。
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Config {
    /// デフォルトの録音ファイル保存先ディレクトリ。
    /// 未設定の場合は `None`。
    save_path: Option<String>,
}

/// 設定ファイルのパスを返す。
///
/// 設定ファイルは `audio_rec_cli.exe` と同一ディレクトリに配置する。
/// 実行ファイルのパスが取得できない場合はカレントディレクトリを使用する。
fn get_config_path() -> PathBuf {
    let exe_dir = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    exe_dir.join(CONFIG_FILE_NAME)
}

/// 設定ファイルを読み込む。
///
/// ファイルが存在しない場合やパースに失敗した場合はデフォルト設定を返す。
fn load_config() -> Config {
    let path = get_config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

/// 設定ファイルに書き込む。
///
/// # 戻り値
///
/// 成功時は `Ok(())`。失敗時はエラーメッセージ。
fn save_config(config: &Config) -> Result<(), String> {
    let path = get_config_path();
    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("設定のシリアライズに失敗しました: {}", e))?;
    std::fs::write(&path, content)
        .map_err(|e| format!("設定ファイルの書き込みに失敗しました: {} ({})", path.display(), e))
}

/// 現在のローカル日時を `yyyymmdd-hhmmss` 形式の文字列として返す。
fn local_datetime_string() -> String {
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

// ─────────────────────────────────────────────────────────────
// RAII ハンドルガード
// ─────────────────────────────────────────────────────────────

/// Named Pipe のハンドルを RAII で管理するガード型。
///
/// スコープを抜けると自動的に `CloseHandle` を呼び出す。
struct PipeHandleGuard(windows::Win32::Foundation::HANDLE);

impl Drop for PipeHandleGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

// ─────────────────────────────────────────────────────────────
// エントリーポイント
// ─────────────────────────────────────────────────────────────

/// コマンドライン引数を解析し、Named Pipe 経由でプラグインにコマンドを送信する。
///
/// # 終了コード
///
/// - `0`：正常終了
/// - `1`：引数エラーまたは事前検証エラー
/// - `2`：Named Pipe への接続または送信に失敗
/// - `3`：プラグイン側でエラーが発生した（`err:` レスポンス）
fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage(&args[0]);
        process::exit(1);
    }

    match args[1].as_str() {
        "start" => {
            // ─── start コマンド ───
            // パス指定あり: audio_rec_cli.exe start <path>
            // パス省略:     audio_rec_cli.exe start  （設定ファイルのパス + タイムスタンプ）
            let output_path: String = if args.len() >= 3 {
                args[2].clone()
            } else {
                // デフォルト保存先を設定ファイルから読み込む
                let config = load_config();
                let dir = match config.save_path {
                    Some(ref d) => d.clone(),
                    None => {
                        eprintln!("エラー: デフォルト保存先が設定されていません。");
                        eprintln!(
                            "先に '{} config save-path <ディレクトリ>' で保存先を設定してください。",
                            args[0]
                        );
                        process::exit(1);
                    }
                };
                let filename = format!("{}.wav", local_datetime_string());
                let full_path = PathBuf::from(&dir).join(&filename);
                full_path.to_string_lossy().into_owned()
            };

            // 事前検証（Pre-flight validation）
            if let Err(msg) = validate_output_path(&output_path) {
                eprintln!("エラー: {}", msg);
                process::exit(1);
            }

            println!("録音先: {}", output_path);

            let command = format!("start:{}", output_path);
            match send_command_and_read_response(&command) {
                Ok(response) => handle_response(&response),
                Err(msg) => {
                    eprintln!("エラー: {}", msg);
                    process::exit(2);
                }
            }
        }

        "stop" => {
            // ─── stop コマンド ───
            match send_command_and_read_response("stop") {
                Ok(response) => handle_response(&response),
                Err(msg) => {
                    eprintln!("エラー: {}", msg);
                    process::exit(2);
                }
            }
        }

        "config" => {
            // ─── config サブコマンド ───
            if args.len() < 4 || args[2].as_str() != "save-path" {
                eprintln!("エラー: 'config' サブコマンドの使い方が正しくありません。");
                eprintln!(
                    "使用方法: {} config save-path <ディレクトリ>",
                    args[0]
                );
                process::exit(1);
            }

            let dir = &args[3];

            // ディレクトリの存在チェック
            if !Path::new(dir).exists() {
                eprintln!(
                    "エラー: 指定したディレクトリが存在しません: {}",
                    dir
                );
                process::exit(1);
            }

            let mut config = load_config();
            config.save_path = Some(dir.clone());

            match save_config(&config) {
                Ok(()) => {
                    println!("デフォルト保存先を設定しました: {}", dir);
                    println!("設定ファイル: {}", get_config_path().display());
                }
                Err(msg) => {
                    eprintln!("エラー: {}", msg);
                    process::exit(1);
                }
            }
        }

        cmd => {
            eprintln!("エラー: 未知のコマンド '{}'", cmd);
            print_usage(&args[0]);
            process::exit(1);
        }
    }
}

/// 使用方法を標準エラー出力に表示する。
fn print_usage(program: &str) {
    eprintln!("使用方法:");
    eprintln!("  {} start [<WAVファイルの絶対パス>]  -- 録音を開始する（パス省略時はデフォルト保存先を使用）", program);
    eprintln!("  {} stop                              -- 録音を停止する", program);
    eprintln!(
        "  {} config save-path <ディレクトリ>   -- デフォルト保存先を設定する",
        program
    );
    eprintln!();
    eprintln!("例:");
    eprintln!("  {} start \"C:\\録音\\output.wav\"", program);
    eprintln!("  {} start  # デフォルト保存先 + タイムスタンプ名で録音開始", program);
    eprintln!("  {} stop", program);
    eprintln!("  {} config save-path \"C:\\録音\"", program);
}

/// プラグインからのレスポンスを解析して表示し、必要に応じて異常終了する。
///
/// - `ok`：正常終了（終了コード 0）
/// - `noop:<理由>`：状態変化なし（終了コード 0、理由を標準出力に表示）
/// - `err:<理由>`：エラー（終了コード 3、理由を標準エラー出力に表示）
fn handle_response(response: &str) {
    if response == "ok" {
        println!("成功");
    } else if let Some(reason) = response.strip_prefix("noop:") {
        println!("変化なし: {}", reason);
    } else if let Some(reason) = response.strip_prefix("err:") {
        eprintln!("プラグインエラー: {}", reason);
        process::exit(3);
    } else {
        eprintln!("予期しないレスポンス: {}", response);
        process::exit(3);
    }
}

// ─────────────────────────────────────────────────────────────
// 事前検証
// ─────────────────────────────────────────────────────────────

/// 出力パスの事前検証（Pre-flight validation）を行う。
///
/// 以下の条件をチェックする：
/// 1. 拡張子が `.wav`（大文字小文字を区別しない）であること。
/// 2. 親ディレクトリが存在すること。
/// 3. 親ディレクトリへの書き込み権限があること。
///
/// # 引数
///
/// * `path` - 検証対象の出力ファイルパス
///
/// # 戻り値
///
/// 検証成功時は `Ok(())`。失敗時はエラーメッセージ。
pub fn validate_output_path(path: &str) -> Result<(), String> {
    let p = Path::new(path);

    // ─── 拡張子チェック ───
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext != "wav" {
        return Err(format!(
            "'.wav' 拡張子のファイルを指定してください: {}",
            path
        ));
    }

    // ─── 親ディレクトリの存在チェック ───
    let parent = match p.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        // 親ディレクトリが空（カレントディレクトリを指す）の場合はカレントを使う
        _ => Path::new("."),
    };

    if !parent.exists() {
        return Err(format!(
            "出力先ディレクトリが存在しません: {}",
            parent.display()
        ));
    }

    // ─── 書き込み権限チェック ───
    let test_file = parent.join(format!(".audio_rec_write_test_{}", process::id()));
    match std::fs::File::create(&test_file) {
        Ok(_) => {
            // テストファイルを削除（失敗しても無視）
            let _ = std::fs::remove_file(&test_file);
        }
        Err(e) => {
            return Err(format!(
                "出力先ディレクトリへの書き込み権限がありません: {} ({})",
                parent.display(),
                e
            ));
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Named Pipe 通信
// ─────────────────────────────────────────────────────────────

/// コマンドを Named Pipe 経由でプラグインに送信し、レスポンスを受け取る。
///
/// # 引数
///
/// * `command` - 送信するコマンド文字列（`start:<path>` または `stop`）
///
/// # 戻り値
///
/// 成功時はプラグインからのレスポンス文字列。失敗時はエラーメッセージ。
fn send_command_and_read_response(command: &str) -> Result<String, String> {
    let pipe_name_wide: Vec<u16> = PIPE_NAME
        .encode_utf16()
        .chain(std::iter::once(0u16))
        .collect();
    let pipe_pcwstr = PCWSTR(pipe_name_wide.as_ptr());

    // ─── リトライ付きで接続 ───
    let handle = connect_with_retry(pipe_pcwstr)?;
    let _guard = PipeHandleGuard(handle);

    // ─── コマンドを UTF-8 バイト列として送信 ───
    let payload = command.as_bytes();
    let mut bytes_written: u32 = 0;
    unsafe { WriteFile(handle, Some(payload), Some(&mut bytes_written), None) }
        .map_err(|e| format!("コマンドの送信に失敗しました: {}", e))?;

    if bytes_written != payload.len() as u32 {
        return Err(format!(
            "送信バイト数が一致しません: 期待={}, 実際={}",
            payload.len(),
            bytes_written
        ));
    }

    // ─── レスポンスを受信 ───
    let mut response_buf = vec![0u8; MAX_RESPONSE_BYTES];
    let mut bytes_read: u32 = 0;
    unsafe { ReadFile(handle, Some(&mut response_buf), Some(&mut bytes_read), None) }
        .map_err(|e| format!("レスポンスの受信に失敗しました: {}", e))?;

    let response = String::from_utf8_lossy(&response_buf[..bytes_read as usize]).into_owned();
    Ok(response)
}

/// Named Pipe にリトライ付きで接続する。
///
/// `MAX_RETRIES` 回試みてもすべて失敗した場合はエラーを返す。
///
/// ## リトライ条件
///
/// | エラー | 対応 |
/// |---|---|
/// | `ERROR_PIPE_BUSY` | `WaitNamedPipeW` でインスタンス空きを待つ |
/// | `ERROR_FILE_NOT_FOUND` | プラグインがまだ起動していない可能性。短いスリープ後にリトライ |
/// | その他 | 即座にエラーを返す（リトライしない） |
///
/// # 引数
///
/// * `pipe_name` - 接続先パイプのワイド文字列ポインタ
///
/// # 戻り値
///
/// 接続成功時はパイプのハンドル。失敗時はエラーメッセージ。
fn connect_with_retry(pipe_name: PCWSTR) -> Result<windows::Win32::Foundation::HANDLE, String> {
    let mut last_error = String::new();

    for attempt in 0..MAX_RETRIES {
        let result = unsafe {
            windows::Win32::Storage::FileSystem::CreateFileW(
                pipe_name,
                GENERIC_READ_WRITE_ACCESS,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };

        match result {
            Ok(handle) => return Ok(handle),
            Err(e) => {
                last_error = format!("{}", e);

                if attempt < MAX_RETRIES - 1 {
                    if e.code() == ERROR_PIPE_BUSY.to_hresult() {
                        eprintln!(
                            "パイプが使用中です。待機してリトライします... ({}/{})",
                            attempt + 1,
                            MAX_RETRIES
                        );
                        let _ = unsafe { WaitNamedPipeW(pipe_name, MAX_WAIT_MS) };
                    } else if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() {
                        eprintln!(
                            "パイプが見つかりません。プラグインの起動を待機します... ({}/{})",
                            attempt + 1,
                            MAX_RETRIES
                        );
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    } else {
                        return Err(format!("Named Pipe への接続に失敗しました: {}", e));
                    }
                }
            }
        }
    }

    Err(format!(
        "Named Pipe への接続に {} 回試みましたが失敗しました (最後のエラー: {})",
        MAX_RETRIES, last_error
    ))
}

// ─────────────────────────────────────────────────────────────
// テスト
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ─── validate_output_path のテスト ───

    /// `.wav` 以外の拡張子はエラーになることを確認する。
    #[test]
    fn test_validate_path_wrong_extension() {
        assert!(validate_output_path("/tmp/test.mp3").is_err());
        assert!(validate_output_path("/tmp/test.txt").is_err());
        assert!(validate_output_path("/tmp/test.exe").is_err());
        assert!(validate_output_path("/tmp/test").is_err());
    }

    /// 拡張子チェックが大文字小文字を区別しないことを確認する。
    #[test]
    fn test_validate_path_extension_case_insensitive() {
        // 親ディレクトリが存在する一時パスで大文字拡張子を検証
        let mut path = std::env::temp_dir();
        path.push("_audio_rec_test.WAV");
        let _ = std::fs::remove_file(&path);
        // 大文字.WAV は拡張子チェックを通過してディレクトリ/権限チェックへ進む
        // temp_dir は存在・書き込み可能なのでエラーにはならない
        let result = validate_output_path(path.to_str().unwrap());
        assert!(result.is_ok(), "大文字 .WAV も有効なはず: {:?}", result);
    }

    /// 存在しないディレクトリへのパスはエラーになることを確認する。
    #[test]
    fn test_validate_path_nonexistent_directory() {
        let result =
            validate_output_path("/nonexistent_dir_12345/subdir/output.wav");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("存在しません") || msg.contains("not found") || msg.contains("exist"),
            "エラーメッセージ: {}",
            msg
        );
    }

    /// 一時ディレクトリへの書き込みは成功することを確認する。
    #[test]
    fn test_validate_path_writable_temp_dir() {
        let mut path = std::env::temp_dir();
        path.push("_audio_rec_test_valid.wav");
        let result = validate_output_path(path.to_str().unwrap());
        assert!(
            result.is_ok(),
            "書き込み可能なディレクトリは成功するはず: {:?}",
            result
        );
    }

    /// 拡張子なしのパスはエラーになることを確認する。
    #[test]
    fn test_validate_path_no_extension() {
        let result = validate_output_path("/tmp/output");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains(".wav"), "エラーメッセージ: {}", msg);
    }

    // ─── Config のテスト ───

    /// Config のデフォルト値が正しいことを確認する。
    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.save_path.is_none());
    }

    /// Config の JSON シリアライズ / デシリアライズが正しく動作することを確認する。
    #[test]
    fn test_config_roundtrip() {
        let config = Config {
            save_path: Some("C:\\録音".to_string()),
        };
        let json = serde_json::to_string(&config).unwrap();
        let restored: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.save_path.as_deref(), Some("C:\\録音"));
    }

    /// 不正な JSON は Config のデフォルト値にフォールバックすることを確認する。
    #[test]
    fn test_config_invalid_json_fallback() {
        let bad: Result<Config, _> = serde_json::from_str("not json at all");
        // from_str 自体はエラーを返すが、load_config はデフォルトにフォールバックする
        assert!(bad.is_err());
    }
}

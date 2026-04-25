//! # AviUtl2 マイク音声仮録音プラグイン
//!
//! AviUtl2 内でマイク音声を WAV ファイルへ仮録音するための汎用プラグイン（`.aux2`）。
//! CLI クライアントから Named Pipe 経由で録音の開始・停止を制御する。
//!
//! ## アーキテクチャ
//!
//! ```text
//! [CLIクライアント] --UTF-8(コマンド)--> [Named Pipe] --> [本プラグイン]
//!                                                            |
//!                                                  cpal（WASAPI）キャプチャ
//!                                                            |
//!                                                  hound（WAV）書き込み
//!                                                            |
//!                                                    [WAVファイル]
//! ```
//!
//! 1. プラグインロード時にワーカースレッドを起動し、Named Pipe サーバーを常駐させる。
//! 2. CLI クライアントが `start:<path>` または `stop` コマンドを送信する。
//! 3. ワーカースレッドが `CpalHoundRecorder` を介して録音を制御する。
//! 4. レスポンス（`ok` / `noop:<reason>` / `err:<reason>`）を CLI クライアントに返す。
//!
//! ## スレッドモデル
//!
//! - ワーカースレッド：Named Pipe の待ち受け・コマンド処理・録音制御を担当。
//! - cpal コールバックスレッド：高優先度で音声データを受け取り WAV に書き込む。
//!   パニックを完全に排除し、エラーは `Arc<Mutex<Option<String>>>` で通知する。
//!
//! ## IPC プロトコル
//!
//! - 通信方式：Named Pipe（`\\.\pipe\aviutl2_audio_rec`）双方向・メッセージモード
//! - エンコーディング：UTF-8（null 終端なし、メッセージ長で区切る）
//! - コマンド：`start:<絶対パス>` または `stop`
//! - レスポンス：`ok` / `noop:<理由>` / `err:<理由>`
//! - 最大ペイロード長：65,536 バイト

use std::io::BufWriter;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

use aviutl2::AnyResult;
use aviutl2::generic::{GenericPlugin, GenericPluginTable, HostAppHandle};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_NORMAL, FILE_FLAG_WRITE_THROUGH, FILE_SHARE_NONE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows::Win32::System::Pipes::{
    PIPE_ACCESS_DUPLEX, PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT, ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW,
};
use windows::core::PCWSTR;

// ─────────────────────────────────────────────────────────────
// 定数
// ─────────────────────────────────────────────────────────────

/// Named Pipe の名前。
/// Windows のローカルパイプ名前空間に固定値として登録する。
const PIPE_NAME: &str = r"\\.\pipe\aviutl2_audio_rec";

/// 受信バッファの最大バイト数（65,536 バイト）。
/// パスの最大長（約 32,768 文字）を考慮した上限。
const MAX_PAYLOAD_BYTES: usize = 65_536;

/// パイプ接続待機のタイムアウト（ミリ秒）。
/// シャットダウン時にダミークライアントが接続するまでの最大待機時間。
const PIPE_CONNECT_TIMEOUT_MS: u32 = 5_000;

/// ダミー接続に使用する書き込みアクセス権（`GENERIC_WRITE = 0x40000000`）。
const GENERIC_WRITE_ACCESS: u32 = 0x4000_0000u32;

// ─────────────────────────────────────────────────────────────
// スレッド間共有ハンドルラッパー
// ─────────────────────────────────────────────────────────────

/// `HANDLE` をスレッド間で安全に共有するためのラッパー型。
///
/// `HANDLE` は Windows カーネルオブジェクトへの不透明なポインタであり、
/// Rust では `Send`/`Sync` を実装しない。本型では `Mutex` による排他制御を前提に
/// `unsafe impl Send` を宣言し、安全にスレッド間受け渡しを可能にする。
struct SendableHandle(HANDLE);

// Mutex で保護するため、スレッド間送受信は安全。
unsafe impl Send for SendableHandle {}

// ─────────────────────────────────────────────────────────────
// AudioRecorder トレイト
// ─────────────────────────────────────────────────────────────

/// 音声録音器の抽象インターフェース。
///
/// 依存関係を抽象化し、テスト時にモック実装を差し込めるようにする。
/// 実装は `Send` を要求する（ワーカースレッドで保持するため）。
pub trait AudioRecorder: Send {
    /// 録音を開始する。
    ///
    /// 既に録音中の場合は冪等処理（No-op）として `Ok(())` を返す。
    ///
    /// # 引数
    ///
    /// * `output_path` - WAV ファイルの出力先パス
    ///
    /// # 戻り値
    ///
    /// 成功時は `Ok(())`。失敗時はエラーメッセージ。
    fn start(&mut self, output_path: &Path) -> Result<(), String>;

    /// 録音を停止する。
    ///
    /// 録音中でない場合は冪等処理（No-op）として `Ok(())` を返す。
    /// ストリームをドロップして WAV ファイルをファイナライズする。
    ///
    /// # 戻り値
    ///
    /// 成功時は `Ok(())`。コールバック中にエラーが発生していた場合はそのエラーメッセージ。
    fn stop(&mut self) -> Result<(), String>;

    /// 現在録音中かどうかを返す。
    fn is_recording(&self) -> bool;
}

// ─────────────────────────────────────────────────────────────
// CpalHoundRecorder 実装
// ─────────────────────────────────────────────────────────────

/// cpal（WASAPI）と hound（WAV）を使用する `AudioRecorder` 実装。
///
/// ## 設計上の許容事項
///
/// cpal のオーディオコールバック内で `hound` を使った同期ディスクI/Oを直接実行する。
/// これにより実装を単純化する。高負荷時のバッファアンダーランのリスクは受容済み。
///
/// ## エラーハンドリング
///
/// コールバック内でのI/Oエラーは `panic!` や `unwrap()` を使わず、
/// `Arc<Mutex<Option<String>>>` に格納してメインスレッドへ伝播する。
/// エラー発生後は書き込みを停止し、ストリームのクローズ処理へ移行する。
pub struct CpalHoundRecorder {
    state: RecordingState,
}

/// 録音の内部状態。
enum RecordingState {
    /// 録音していない状態。
    Idle,
    /// 録音中の状態。
    Active {
        /// cpal ストリーム（ドロップ時に録音が停止する）。
        _stream: cpal::Stream,
        /// cpal コールバックから伝播されるエラーメッセージ。
        /// コールバック内でエラーが発生した場合に `Some` がセットされる。
        callback_error: Arc<Mutex<Option<String>>>,
        /// hound の WAV ライター（ドロップ時にファイルがファイナライズされる）。
        /// コールバックとメインスレッドの両方からアクセスするため `Arc<Mutex>` で保護。
        writer: Arc<Mutex<Option<hound::WavWriter<BufWriter<std::fs::File>>>>>,
    },
}

impl CpalHoundRecorder {
    /// 新しい `CpalHoundRecorder` を作成する。
    pub fn new() -> Self {
        Self {
            state: RecordingState::Idle,
        }
    }
}

// SAFETY: `CpalHoundRecorder` は常にワーカースレッド（パイプサーバースレッド）内で
// 作成・使用・ドロップされる。`cpal::Stream` は一部プラットフォームの保守的マーカー
// により `Send` を実装しないが、WASAPI バックエンドはスレッドセーフであり、
// ストリームの作成・停止は同一スレッドで行われるため安全。
unsafe impl Send for CpalHoundRecorder {}

impl Default for CpalHoundRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioRecorder for CpalHoundRecorder {
    fn start(&mut self, output_path: &Path) -> Result<(), String> {
        // 冪等処理：既に録音中なら何もしない
        if self.is_recording() {
            return Ok(());
        }

        // ─── デバイスと設定を取得 ───
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "デフォルト入力デバイスが見つかりません".to_string())?;
        let supported_config = device
            .default_input_config()
            .map_err(|e| format!("入力設定の取得に失敗しました: {}", e))?;

        let channels = supported_config.channels();
        let sample_rate = supported_config.sample_rate().0;

        // ─── hound WAV スペックを決定 ───
        let (bits_per_sample, sample_format) = match supported_config.sample_format() {
            cpal::SampleFormat::I8 | cpal::SampleFormat::I16 | cpal::SampleFormat::I32 => {
                (16u16, hound::SampleFormat::Int)
            }
            _ => (32u16, hound::SampleFormat::Float),
        };
        let spec = hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample,
            sample_format,
        };

        // ─── WAV ライターを作成 ───
        let wav_writer = hound::WavWriter::create(output_path, spec)
            .map_err(|e| format!("WAVファイルの作成に失敗しました: {}", e))?;
        let writer: Arc<Mutex<Option<hound::WavWriter<BufWriter<std::fs::File>>>>> =
            Arc::new(Mutex::new(Some(wav_writer)));
        let callback_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        // ─── cpal ストリームを構築 ───
        let stream = build_input_stream(
            &device,
            &supported_config.into(),
            bits_per_sample,
            Arc::clone(&writer),
            Arc::clone(&callback_error),
        )?;

        stream
            .play()
            .map_err(|e| format!("録音ストリームの開始に失敗しました: {}", e))?;

        self.state = RecordingState::Active {
            _stream: stream,
            callback_error,
            writer,
        };
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        match std::mem::replace(&mut self.state, RecordingState::Idle) {
            RecordingState::Idle => {
                // 冪等処理：録音中でなければ何もしない
                Ok(())
            }
            RecordingState::Active {
                _stream,
                callback_error,
                writer,
            } => {
                // ストリームをドロップして録音を停止（コールバックが終了するまで待機）
                drop(_stream);

                // WAV ライターを取り出してファイナライズ（ヘッダを確定して閉じる）
                // Mutex が poison されていても into_inner() でパニックを回避する
                let writer_opt = match writer.lock() {
                    Ok(mut g) => g.take(),
                    Err(e) => e.into_inner().take(),
                };
                if let Some(w) = writer_opt {
                    w.finalize()
                        .map_err(|e| format!("WAVファイルのファイナライズに失敗しました: {}", e))?;
                }

                // コールバック内でエラーが発生していた場合はそれを返す
                // Mutex が poison されていても into_inner() でパニックを回避する
                let cb_err = match callback_error.lock() {
                    Ok(mut g) => g.take(),
                    Err(e) => e.into_inner().take(),
                };
                if let Some(err) = cb_err {
                    return Err(format!("録音中にコールバックエラーが発生しました: {}", err));
                }

                Ok(())
            }
        }
    }

    fn is_recording(&self) -> bool {
        matches!(self.state, RecordingState::Active { .. })
    }
}

/// サンプルフォーマットに応じた cpal 入力ストリームを構築する。
///
/// コールバック内でのパニックを完全に排除し、I/Oエラーは
/// `callback_error` を通じてメインスレッドに伝播する。
///
/// # 引数
///
/// * `device` - cpal 入力デバイス
/// * `config` - ストリーム設定
/// * `bits_per_sample` - WAV のビット深度（16 または 32）
/// * `writer` - 共有 WAV ライター
/// * `callback_error` - コールバックエラー通知用共有状態
fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    bits_per_sample: u16,
    writer: Arc<Mutex<Option<hound::WavWriter<BufWriter<std::fs::File>>>>>,
    callback_error: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, String> {
    let err_fn = |e: cpal::StreamError| {
        tracing::error!("cpal ストリームエラー: {}", e);
    };

    let stream = if bits_per_sample == 16 {
        // i16 サンプルとして録音
        let writer_clone = Arc::clone(&writer);
        let error_clone = Arc::clone(&callback_error);
        device
            .build_input_stream(
                config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    write_samples_i16(data, &writer_clone, &error_clone);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("ストリームの構築に失敗しました (i16): {}", e))?
    } else {
        // f32 サンプルとして録音
        let writer_clone = Arc::clone(&writer);
        let error_clone = Arc::clone(&callback_error);
        device
            .build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    write_samples_f32(data, &writer_clone, &error_clone);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("ストリームの構築に失敗しました (f32): {}", e))?
    };

    Ok(stream)
}

/// i16 サンプルを WAV ライターに書き込む（cpal コールバックから呼ばれる）。
///
/// エラーが発生した場合は `callback_error` に格納してライターをクローズする。
/// `panic!` や `unwrap()` は一切使用しない。
fn write_samples_i16(
    data: &[i16],
    writer: &Arc<Mutex<Option<hound::WavWriter<BufWriter<std::fs::File>>>>>,
    callback_error: &Arc<Mutex<Option<String>>>,
) {
    // ライターロックの取得に失敗した場合はサイレントスキップ
    let Ok(mut guard) = writer.try_lock() else {
        return;
    };

    let Some(ref mut w) = *guard else {
        // ライターが既にクローズされている（エラー後またはストップ後）
        return;
    };

    for &sample in data {
        if let Err(e) = w.write_sample(sample) {
            // エラーをメインスレッドへ通知
            if let Ok(mut err_guard) = callback_error.try_lock()
                && err_guard.is_none() {
                    *err_guard = Some(format!("サンプル書き込みエラー (i16): {}", e));
                }
            // ライターをドロップしてファイルを閉じる（以降の書き込みを停止）
            drop(guard.take());
            return;
        }
    }
}

/// f32 サンプルを WAV ライターに書き込む（cpal コールバックから呼ばれる）。
///
/// エラーが発生した場合は `callback_error` に格納してライターをクローズする。
/// `panic!` や `unwrap()` は一切使用しない。
fn write_samples_f32(
    data: &[f32],
    writer: &Arc<Mutex<Option<hound::WavWriter<BufWriter<std::fs::File>>>>>,
    callback_error: &Arc<Mutex<Option<String>>>,
) {
    // ライターロックの取得に失敗した場合はサイレントスキップ
    let Ok(mut guard) = writer.try_lock() else {
        return;
    };

    let Some(ref mut w) = *guard else {
        // ライターが既にクローズされている（エラー後またはストップ後）
        return;
    };

    for &sample in data {
        if let Err(e) = w.write_sample(sample) {
            // エラーをメインスレッドへ通知
            if let Ok(mut err_guard) = callback_error.try_lock()
                && err_guard.is_none() {
                    *err_guard = Some(format!("サンプル書き込みエラー (f32): {}", e));
                }
            // ライターをドロップしてファイルを閉じる（以降の書き込みを停止）
            drop(guard.take());
            return;
        }
    }
}

// ─────────────────────────────────────────────────────────────
// プラグイン構造体
// ─────────────────────────────────────────────────────────────

/// AviUtl2 マイク録音プラグインのメイン構造体。
///
/// `register()` が呼ばれた時点でワーカースレッドを起動し、
/// プラグインがアンロードされる（`Drop` が呼ばれる）時点でスレッドを安全に終了する。
///
/// ## シャットダウンフロー
///
/// 1. `shutdown_flag` を `true` に設定する。
/// 2. `active_pipe` が `Some` ならワーカーは `ReadFile` でブロック中であるため、
///    `DisconnectNamedPipe` でパイプを強制切断して `ReadFile` を中断させる。
/// 3. ダミークライアントを接続して `ConnectNamedPipe` のブロックを解除する。
/// 4. ワーカースレッドの終了を `join()` で待機する。
#[aviutl2::plugin(GenericPlugin)]
pub struct AudioRecPlugin {
    /// シャットダウン要求を伝えるアトミックフラグ。
    /// `true` に設定するとワーカースレッドはパイプ受信ループを終了する。
    shutdown_flag: Arc<AtomicBool>,

    /// Named Pipe サーバーを実行するワーカースレッドのハンドル。
    /// `Mutex` でラップして `Sync` を安全に満たす。
    /// `Drop` 時に `join()` して安全に終了を待機する。
    worker_thread: Mutex<Option<JoinHandle<()>>>,

    /// ワーカースレッドが現在接続中のパイプハンドル。
    /// `ReadFile` でブロック中の場合は `Some` が設定されており、
    /// `Drop` から `DisconnectNamedPipe` を呼び出して I/O を中断できる。
    active_pipe: Arc<Mutex<Option<SendableHandle>>>,
}

// ─────────────────────────────────────────────────────────────
// GenericPlugin トレイト実装
// ─────────────────────────────────────────────────────────────

impl GenericPlugin for AudioRecPlugin {
    /// プラグインインスタンスを生成する。
    ///
    /// ロギングの初期化のみを行い、スレッド起動は [`Self::register`] で実施する。
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        init_logging();
        tracing::info!("AviUtl2 マイク録音プラグインを初期化中...");
        Ok(Self {
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            worker_thread: Mutex::new(None),
            active_pipe: Arc::new(Mutex::new(None)),
        })
    }

    /// プラグインのメタ情報を返す。
    fn plugin_info(&self) -> GenericPluginTable {
        GenericPluginTable {
            name: "AviUtl2 Audio Recorder".to_string(),
            information: format!(
                "AviUtl2 マイク音声仮録音プラグイン v{} \
                / Named Pipe 経由で録音の開始・停止を制御します",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }

    /// プラグインをホストに登録し、ワーカースレッドを起動する。
    fn register(&mut self, _registry: &mut HostAppHandle) {
        tracing::info!("プラグインをホストに登録中...");

        let flag = Arc::clone(&self.shutdown_flag);
        let active_pipe = Arc::clone(&self.active_pipe);
        let thread = match std::thread::Builder::new()
            .name("audio_rec_pipe_server".to_string())
            .spawn(move || {
                tracing::info!("Named Pipe サーバースレッドを開始しました");
                let mut recorder = CpalHoundRecorder::new();
                pipe_server_loop(flag, active_pipe, &mut recorder);
                tracing::info!("Named Pipe サーバースレッドを終了しました");
            }) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("ワーカースレッドの起動に失敗しました（録音機能は無効）: {}", e);
                return;
            }
        };

        match self.worker_thread.lock() {
            Ok(mut guard) => {
                *guard = Some(thread);
                tracing::info!("Named Pipe サーバーを起動しました: {}", PIPE_NAME);
            }
            Err(e) => {
                tracing::error!("worker_thread Mutex が汚染されています。スレッドを記録できません: {}", e);
                // スレッドは起動済みだが参照を保持できないため、デタッチされる
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Drop 実装（終了処理）
// ─────────────────────────────────────────────────────────────

impl Drop for AudioRecPlugin {
    /// プラグインのアンロード時にワーカースレッドを安全に終了する。
    fn drop(&mut self) {
        tracing::info!("プラグインをシャットダウン中...");

        // シャットダウンフラグを設定
        self.shutdown_flag.store(true, Ordering::Relaxed);

        // ReadFile でブロック中のワーカーを解除（active_pipe が Some ならブロック中）
        // Mutex が poison されていても into_inner() で中身を取り出してパニックを回避する
        let maybe_handle = match self.active_pipe.lock() {
            Ok(mut g) => g.take(),
            Err(e) => {
                tracing::error!("active_pipe Mutex が汚染されています: {}", e);
                e.into_inner().take()
            }
        };
        if let Some(h) = maybe_handle {
            let _ = unsafe { DisconnectNamedPipe(h.0) };
        }

        // ブロック中の ConnectNamedPipe を解除するためにダミー接続を行う
        connect_shutdown_client();

        // ワーカースレッドの終了を待機
        // Mutex が poison されていても into_inner() で中身を取り出してパニックを回避する
        let thread_opt = match self.worker_thread.lock() {
            Ok(mut g) => g.take(),
            Err(e) => {
                tracing::error!("worker_thread Mutex が汚染されています: {}", e);
                e.into_inner().take()
            }
        };
        if let Some(thread) = thread_opt
            && thread.join().is_err() {
                tracing::error!("ワーカースレッドがパニックしました");
            }

        tracing::info!("プラグインのシャットダウンが完了しました");
    }
}

// ─────────────────────────────────────────────────────────────
// ロギング初期化
// ─────────────────────────────────────────────────────────────

/// AviUtl2 向けのロギングを初期化する。
///
/// デバッグビルドでは `DEBUG` レベル、リリースビルドでは `INFO` レベルで出力する。
fn init_logging() {
    let _ = aviutl2::tracing_subscriber::fmt()
        .with_max_level(if cfg!(debug_assertions) {
            aviutl2::tracing::Level::DEBUG
        } else {
            aviutl2::tracing::Level::INFO
        })
        .event_format(aviutl2::logger::AviUtl2Formatter)
        .with_writer(aviutl2::logger::AviUtl2LogWriter)
        .try_init();
}

// ─────────────────────────────────────────────────────────────
// Named Pipe サーバーループ
// ─────────────────────────────────────────────────────────────

/// Named Pipe サーバーのメインループ。
///
/// シャットダウンフラグが `true` になるまで、クライアントの接続→コマンド受信→
/// レスポンス送信を繰り返す。
///
/// # 引数
///
/// * `shutdown` - シャットダウン要求を示すアトミックフラグ
/// * `active_pipe` - 現在接続中のパイプハンドルを共有するコンテナ
/// * `recorder` - 音声録音器
fn pipe_server_loop(
    shutdown: Arc<AtomicBool>,
    active_pipe: Arc<Mutex<Option<SendableHandle>>>,
    recorder: &mut dyn AudioRecorder,
) {
    loop {
        // ─── パイプインスタンスを作成 ───
        let pipe = create_server_pipe();
        if pipe == INVALID_HANDLE_VALUE {
            tracing::error!(
                "Named Pipe インスタンスの作成に失敗しました。サーバーを終了します"
            );
            break;
        }

        // ─── クライアントの接続を待機（ブロッキング）───
        if !wait_for_client(pipe) {
            let _ = unsafe { DisconnectNamedPipe(pipe) };
            let _ = unsafe { CloseHandle(pipe) };
            break;
        }

        // ─── シャットダウンフラグを確認 ───
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("シャットダウンフラグを検出しました。ループを終了します");
            let _ = unsafe { DisconnectNamedPipe(pipe) };
            let _ = unsafe { CloseHandle(pipe) };
            break;
        }

        // ─── アクティブパイプハンドルを登録 ───
        *active_pipe.lock().unwrap() = Some(SendableHandle(pipe));

        // ─── コマンドを受信（ブロッキング; Drop から中断可能）───
        let received = read_pipe_message(pipe);

        // ─── アクティブパイプハンドルをクリア ───
        active_pipe.lock().unwrap().take();

        // ─── シャットダウンフラグを確認 ───
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("シャットダウンフラグを検出しました（ReadFile 後）");
            let _ = unsafe { DisconnectNamedPipe(pipe) };
            let _ = unsafe { CloseHandle(pipe) };
            break;
        }

        // ─── コマンドを処理してレスポンスを送信 ───
        if let Some(data) = received {
            let response = match String::from_utf8(data) {
                Ok(command) => {
                    tracing::info!("コマンドを受信しました: {}", command.trim());
                    process_command(command.trim(), recorder)
                }
                Err(e) => {
                    tracing::error!("UTF-8 デコードに失敗しました: {}", e);
                    format!("err:UTF-8デコードエラー: {}", e)
                }
            };

            tracing::info!("レスポンスを送信します: {}", response);
            let _ = write_pipe_message(pipe, response.as_bytes());
        }

        // ─── パイプを切断してクローズ ───
        let _ = unsafe { DisconnectNamedPipe(pipe) };
        let _ = unsafe { CloseHandle(pipe) };
    }
}

/// コマンドを解析して録音器を制御し、レスポンス文字列を返す。
///
/// # コマンド形式
///
/// - `start:<絶対パス>` — 指定パスへの録音を開始する
/// - `stop` — 録音を停止する
///
/// # レスポンス形式
///
/// - `ok` — 正常終了
/// - `noop:<理由>` — 冪等処理（状態変化なし）
/// - `err:<理由>` — エラー
///
/// # 引数
///
/// * `command` - 受信したコマンド文字列（トリム済み）
/// * `recorder` - 音声録音器
fn process_command(command: &str, recorder: &mut dyn AudioRecorder) -> String {
    if let Some(path_str) = command.strip_prefix("start:") {
        // ─── start コマンド ───
        if recorder.is_recording() {
            tracing::info!("既に録音中です（冪等処理）");
            return "noop:既に録音中です".to_string();
        }
        let path = Path::new(path_str);
        match recorder.start(path) {
            Ok(()) => {
                tracing::info!("録音を開始しました: {}", path_str);
                "ok".to_string()
            }
            Err(e) => {
                tracing::error!("録音の開始に失敗しました: {}", e);
                format!("err:{}", e)
            }
        }
    } else if command == "stop" {
        // ─── stop コマンド ───
        if !recorder.is_recording() {
            tracing::info!("録音していません（冪等処理）");
            return "noop:録音していません".to_string();
        }
        match recorder.stop() {
            Ok(()) => {
                tracing::info!("録音を停止しました");
                "ok".to_string()
            }
            Err(e) => {
                // コールバックエラーを含む場合もここで報告
                tracing::error!("録音の停止中にエラーが発生しました: {}", e);
                format!("err:{}", e)
            }
        }
    } else {
        tracing::warn!("未知のコマンドを受信しました: {}", command);
        format!("err:未知のコマンド: {}", command)
    }
}

/// Named Pipe のサーバーインスタンスを作成する（双方向・メッセージモード）。
///
/// 双方向（`PIPE_ACCESS_DUPLEX`）かつメッセージモードで作成することで、
/// CLI クライアントとの双方向通信（コマンド受信＋レスポンス送信）を実現する。
///
/// # 戻り値
///
/// 成功時はパイプのハンドル。失敗時は `INVALID_HANDLE_VALUE`。
fn create_server_pipe() -> HANDLE {
    let pipe_name_wide: Vec<u16> = PIPE_NAME
        .encode_utf16()
        .chain(std::iter::once(0u16))
        .collect();

    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(pipe_name_wide.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_WRITE_THROUGH,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            MAX_PAYLOAD_BYTES as u32,
            MAX_PAYLOAD_BYTES as u32,
            0,
            None,
        )
    };

    if pipe == INVALID_HANDLE_VALUE {
        tracing::error!("CreateNamedPipeW が失敗しました");
    }

    pipe
}

/// クライアントの接続を待機する（ブロッキング）。
///
/// `ERROR_PIPE_CONNECTED`（すでに接続済み）も成功として扱う。
fn wait_for_client(pipe: HANDLE) -> bool {
    match unsafe { ConnectNamedPipe(pipe, None) } {
        Ok(()) => true,
        Err(e) => {
            if e.code() == ERROR_PIPE_CONNECTED.to_hresult() {
                true
            } else {
                tracing::error!("ConnectNamedPipe が失敗しました: {}", e);
                false
            }
        }
    }
}

/// パイプから 1 メッセージ分のデータをすべて読み取る。
///
/// # 引数
///
/// * `pipe` - 接続済みの Named Pipe ハンドル
///
/// # 戻り値
///
/// 受信データのバイト列。読み取り中断時は `None`。
fn read_pipe_message(pipe: HANDLE) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::ERROR_MORE_DATA;
    let mut message: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; MAX_PAYLOAD_BYTES];

    loop {
        let mut bytes_read: u32 = 0;
        match unsafe { ReadFile(pipe, Some(&mut chunk), Some(&mut bytes_read), None) } {
            Ok(()) => {
                let bytes_read = bytes_read as usize;
                let new_len = message.len().checked_add(bytes_read)?;
                if new_len > MAX_PAYLOAD_BYTES {
                    return None;
                }
                message.extend_from_slice(&chunk[..bytes_read]);
                break;
            }
            Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => {
                let bytes_read = bytes_read as usize;
                let new_len = message.len().checked_add(bytes_read)?;
                if new_len > MAX_PAYLOAD_BYTES {
                    return None;
                }
                message.extend_from_slice(&chunk[..bytes_read]);
            }
            Err(_) => {
                return None;
            }
        }
    }

    if message.is_empty() { None } else { Some(message) }
}

/// パイプにバイト列を書き込む。
///
/// # 引数
///
/// * `pipe` - 接続済みの Named Pipe ハンドル
/// * `data` - 書き込むバイト列
fn write_pipe_message(pipe: HANDLE, data: &[u8]) -> Result<(), String> {
    let mut bytes_written: u32 = 0;
    unsafe { WriteFile(pipe, Some(data), Some(&mut bytes_written), None) }
        .map_err(|e| format!("WriteFile が失敗しました: {}", e))?;

    if bytes_written != data.len() as u32 {
        return Err(format!(
            "送信バイト数が一致しません: 期待={}, 実際={}",
            data.len(),
            bytes_written
        ));
    }
    Ok(())
}

/// シャットダウン時に `ConnectNamedPipe` のブロックを解除するためのダミー接続。
fn connect_shutdown_client() {
    let pipe_name_wide: Vec<u16> = PIPE_NAME
        .encode_utf16()
        .chain(std::iter::once(0u16))
        .collect();

    let _ = unsafe { WaitNamedPipeW(PCWSTR(pipe_name_wide.as_ptr()), PIPE_CONNECT_TIMEOUT_MS) };

    let handle = unsafe {
        windows::Win32::Storage::FileSystem::CreateFileW(
            PCWSTR(pipe_name_wide.as_ptr()),
            GENERIC_WRITE_ACCESS,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };

    if let Ok(h) = handle {
        let _ = unsafe { CloseHandle(h) };
    }
}

// ─────────────────────────────────────────────────────────────
// プラグイン登録マクロ
// ─────────────────────────────────────────────────────────────

// AviUtl2 汎用プラグインとして `AudioRecPlugin` を登録する。
//
// このマクロにより、以下の C エクスポート関数が自動生成される：
// - `RequiredVersion()` — 対応最小バージョンを返す
// - `InitializeLogger()` — ログハンドルを初期化する
// - `InitializePlugin()` — プラグインを初期化する（`new()` を呼び出す）
// - `GetCommonPluginTable()` — プラグイン情報テーブルを返す
// - `UninitializePlugin()` — プラグインをアンロードする（`drop()` を呼び出す）
// - `RegisterPlugin()` — プラグインをホストに登録する（`register()` を呼び出す）
aviutl2::register_generic_plugin!(AudioRecPlugin);

// ─────────────────────────────────────────────────────────────
// テスト
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ─── AudioRecorder トレイトのモック実装 ───

    struct MockRecorder {
        recording: bool,
        start_error: Option<String>,
        stop_error: Option<String>,
    }

    impl MockRecorder {
        fn new() -> Self {
            Self {
                recording: false,
                start_error: None,
                stop_error: None,
            }
        }

        fn with_start_error(mut self, msg: &str) -> Self {
            self.start_error = Some(msg.to_string());
            self
        }

        fn with_stop_error(mut self, msg: &str) -> Self {
            self.stop_error = Some(msg.to_string());
            self
        }

        fn already_recording(mut self) -> Self {
            self.recording = true;
            self
        }
    }

    impl AudioRecorder for MockRecorder {
        fn start(&mut self, _path: &Path) -> Result<(), String> {
            if let Some(ref e) = self.start_error {
                return Err(e.clone());
            }
            self.recording = true;
            Ok(())
        }

        fn stop(&mut self) -> Result<(), String> {
            if let Some(ref e) = self.stop_error {
                return Err(e.clone());
            }
            self.recording = false;
            Ok(())
        }

        fn is_recording(&self) -> bool {
            self.recording
        }
    }

    // ─── process_command のテスト ───

    /// `start` コマンドが正常に処理されることを確認する。
    #[test]
    fn test_process_command_start_ok() {
        let mut recorder = MockRecorder::new();
        let response = process_command("start:/tmp/test.wav", &mut recorder);
        assert_eq!(response, "ok");
        assert!(recorder.is_recording());
    }

    /// 録音中に `start` を送信した場合は冪等処理になることを確認する。
    #[test]
    fn test_process_command_start_noop_when_recording() {
        let mut recorder = MockRecorder::new().already_recording();
        let response = process_command("start:/tmp/test.wav", &mut recorder);
        assert!(response.starts_with("noop:"), "response was: {}", response);
    }

    /// `stop` コマンドが正常に処理されることを確認する。
    #[test]
    fn test_process_command_stop_ok() {
        let mut recorder = MockRecorder::new().already_recording();
        let response = process_command("stop", &mut recorder);
        assert_eq!(response, "ok");
        assert!(!recorder.is_recording());
    }

    /// 録音していない状態で `stop` を送信した場合は冪等処理になることを確認する。
    #[test]
    fn test_process_command_stop_noop_when_idle() {
        let mut recorder = MockRecorder::new();
        let response = process_command("stop", &mut recorder);
        assert!(response.starts_with("noop:"), "response was: {}", response);
    }

    /// `start` コマンドでエラーが発生した場合は `err:` プレフィックスが付くことを確認する。
    #[test]
    fn test_process_command_start_error() {
        let mut recorder = MockRecorder::new().with_start_error("デバイスエラー");
        let response = process_command("start:/tmp/test.wav", &mut recorder);
        assert!(response.starts_with("err:"), "response was: {}", response);
        assert!(response.contains("デバイスエラー"));
    }

    /// `stop` コマンドでエラーが発生した場合は `err:` プレフィックスが付くことを確認する。
    #[test]
    fn test_process_command_stop_error() {
        let mut recorder = MockRecorder::new()
            .already_recording()
            .with_stop_error("ファイナライズ失敗");
        let response = process_command("stop", &mut recorder);
        assert!(response.starts_with("err:"), "response was: {}", response);
        assert!(response.contains("ファイナライズ失敗"));
    }

    /// 未知のコマンドは `err:` として返されることを確認する。
    #[test]
    fn test_process_command_unknown() {
        let mut recorder = MockRecorder::new();
        let response = process_command("unknown_cmd", &mut recorder);
        assert!(response.starts_with("err:"), "response was: {}", response);
    }

    // ─── CpalHoundRecorder の冪等性テスト ───

    /// 録音していない状態で `stop()` を呼んでも `Ok(())` が返ることを確認する。
    #[test]
    fn test_cpal_recorder_stop_when_idle_is_ok() {
        let mut recorder = CpalHoundRecorder::new();
        assert!(!recorder.is_recording());
        let result = recorder.stop();
        assert!(result.is_ok(), "idle 状態での stop は Ok のはず: {:?}", result);
    }

    /// `is_recording()` の初期値が `false` であることを確認する。
    #[test]
    fn test_cpal_recorder_initial_state() {
        let recorder = CpalHoundRecorder::new();
        assert!(!recorder.is_recording());
    }
}

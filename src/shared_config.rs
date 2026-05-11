//! # 共有設定モジュール
//!
//! CLI クライアントとプラグインが共通で参照する設定ファイルの
//! 読み書きを提供する。

use std::path::{Path, PathBuf};

/// 共有設定ファイル名。
pub const CONFIG_FILE_NAME: &str = "audio_rec_cli.json";

/// CLI とプラグインで共通利用する永続設定。
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub struct Config {
    /// デフォルトの録音ファイル保存先ディレクトリ。
    pub save_path: Option<String>,
    /// cpal 入力ストリームのバッファサイズ（フレーム数）。
    pub buffer_size_frames: Option<u32>,
}

/// 共通設定ファイルのパスを返す。
///
/// 優先順位:
/// 1. `%PROGRAMDATA%\\AviUtl2\\audio_rec_cli.json`
/// 2. カレントディレクトリの `audio_rec_cli.json`
pub fn shared_config_path() -> PathBuf {
    if let Some(program_data) = std::env::var_os("PROGRAMDATA") {
        return PathBuf::from(program_data)
            .join("AviUtl2")
            .join(CONFIG_FILE_NAME);
    }
    PathBuf::from(CONFIG_FILE_NAME)
}

/// 設定ファイルを読み込む。
///
/// ファイルが存在しない場合やパースに失敗した場合はデフォルト設定を返す。
pub fn load_config() -> Config {
    load_config_from_path(&shared_config_path())
}

/// 指定したパスから設定ファイルを読み込む。
///
/// ファイルが存在しない場合やパースに失敗した場合はデフォルト設定を返す。
pub fn load_config_from_path(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

/// 共有設定ファイルが存在しない場合のみ、レガシー設定の内容を利用して返す。
pub fn load_with_legacy_fallback(legacy_path: &Path) -> Config {
    let shared_path = shared_config_path();
    if !shared_path.is_file() {
        return load_config_from_path(legacy_path);
    }
    load_config_from_path(&shared_path)
}

/// 共有設定ファイルに書き込む。
pub fn save_config(config: &Config) -> Result<PathBuf, String> {
    let path = shared_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "設定ディレクトリの作成に失敗しました: {} ({})",
                parent.display(),
                e
            )
        })?;
    }
    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("設定のシリアライズに失敗しました: {}", e))?;
    std::fs::write(&path, content).map_err(|e| {
        format!(
            "設定ファイルの書き込みに失敗しました: {} ({})",
            path.display(),
            e
        )
    })?;
    Ok(path)
}

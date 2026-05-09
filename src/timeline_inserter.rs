//! # タイムライン挿入モジュール
//!
//! 録音したオーディオファイルをタイムラインに挿入する際、
//! 目標位置にオブジェクトが存在する場合に挿入可能な位置を探索する機能を提供する。
//!
//! ## 設計方針
//!
//! - **独立モジュール**: このモジュールは AviUtl2 API に依存しない純粋なロジックとして実装する。
//!   実際の挿入操作はクロージャとして外部から注入し、テスト容易性を確保する。
//! - **フォールバック戦略**: 目標フレームへの挿入に失敗した場合、
//!   `retry_if` が `true` を返すエラーに限り 1 フレームずつ前進しながら
//!   挿入可能な位置を探索する。これにより、占有オブジェクトの末尾直後に
//!   自動的にフォールバックできる。
//! - **有界な試行回数**: 無限ループを防ぐため [`MAX_INSERT_ATTEMPTS`] で試行回数を制限する。
//!
//! ## 使用例
//!
//! ```ignore
//! let result = insert_at_available_frame(
//!     layer,
//!     target_frame,
//!     length_frames,
//!     |ins_layer, ins_frame, ins_length| {
//!         edit_section.create_object_from_alias(&alias, ins_layer, ins_frame, ins_length)
//!     },
//!     |error| {
//!         let msg = error.to_string().to_ascii_lowercase();
//!         msg.contains("occupied") || msg.contains("overlap")
//!     },
//! );
//! match result {
//!     TimelineInsertResult::Inserted { frame, .. } => {
//!         tracing::info!("フレーム {} に挿入しました", frame);
//!     }
//!     TimelineInsertResult::NotFound { last_error } => {
//!         tracing::warn!("挿入可能な位置が見つかりませんでした");
//!         tracing::debug!("last_error={:?}", last_error);
//!     }
//!     TimelineInsertResult::Failed { frame, error } => {
//!         tracing::error!("frame {} で再試行不能エラー: {}", frame, error);
//!     }
//! }
//! ```

/// タイムライン挿入の最大試行回数。
///
/// 目標フレームからこの回数分だけ 1 フレームずつ前進して挿入可能な位置を探索する。
/// 全試行が失敗した場合は [`TimelineInsertResult::NotFound`] を返す。
///
/// 1,000 回で 30fps プロジェクトなら約 33 秒、60fps なら約 16 秒分の範囲をカバーする。
pub const MAX_INSERT_ATTEMPTS: usize = 1_000;

/// 1 回の探索ステップで前進するフレーム数。
const STEP_FRAMES: usize = 1;

/// タイムラインへの挿入結果。
#[derive(Debug)]
pub enum TimelineInsertResult<T, E> {
    /// 挿入成功。
    Inserted {
        /// 実際に挿入した開始フレーム番号。
        frame: usize,
        /// 挿入操作（`try_insert` クロージャ）の戻り値。
        value: T,
    },
    /// 指定した試行回数（[`MAX_INSERT_ATTEMPTS`]）以内に挿入可能な位置が見つからなかった。
    ///
    /// `last_error` には最後に再試行対象と判定されたエラーを保持する。
    NotFound {
        /// 最後に再試行対象と判定されたエラー。
        last_error: Option<E>,
    },
    /// 再試行不能なエラーが発生したため探索を中断した。
    Failed {
        /// 失敗したフレーム番号。
        frame: usize,
        /// 再試行不能と判定されたエラー。
        error: E,
    },
}

/// タイムラインの指定フレームまたはそれ以降に挿入可能な位置にオブジェクトを挿入する。
///
/// 目標フレームへの挿入に失敗した場合、`retry_if` が `true` を返すエラーに限り
/// [`STEP_FRAMES`] ずつ前進しながら反復的に挿入可能な位置を探索する。
/// 最大 [`MAX_INSERT_ATTEMPTS`] 回試行して見つからなければ
/// [`TimelineInsertResult::NotFound`] を返す。
///
/// ## フォールバック動作
///
/// `try_insert` が `Err` を返した場合（位置が占有されている等）、
/// `retry_if` が `true` を返すとフォールバック先として `frame + 1` を次の候補として試行する。
/// `retry_if` が `false` を返したエラーは即時に
/// [`TimelineInsertResult::Failed`] として呼び出し側へ返す。
/// これにより呼び出し元が手動でリトライロジックを実装する必要がなくなる。
///
/// # 引数
///
/// * `layer` - 挿入先レイヤー番号（0 始まり）
/// * `start_frame` - 挿入目標フレーム番号（0 始まり）
/// * `length_frames` - 挿入するオブジェクトの長さ（フレーム数）
/// * `try_insert` - オブジェクト挿入を試みるクロージャ。
///   引数は `(layer, frame, length_frames)` で、成功時は `Ok(T)`、失敗時は `Err(E)` を返す。
/// * `retry_if` - エラーを再試行可能と見なすかを判定するクロージャ。
///   `true` を返したエラーのみフォールバック探索を継続する。
///
/// # 戻り値
///
/// - 挿入成功時は [`TimelineInsertResult::Inserted`]（実際の挿入フレームと戻り値を含む）
/// - 試行回数内に挿入可能位置が見つからなかった場合は [`TimelineInsertResult::NotFound`]
/// - 再試行不能なエラー発生時は [`TimelineInsertResult::Failed`]
pub fn insert_at_available_frame<T, E, F, R>(
    layer: usize,
    start_frame: usize,
    length_frames: usize,
    try_insert: F,
    retry_if: R,
) -> TimelineInsertResult<T, E>
where
    F: Fn(usize, usize, usize) -> Result<T, E>,
    R: Fn(&E) -> bool,
{
    let mut frame = start_frame;
    let mut last_error: Option<E> = None;

    for _attempt in 0..MAX_INSERT_ATTEMPTS {
        match try_insert(layer, frame, length_frames) {
            Ok(value) => {
                return TimelineInsertResult::Inserted { frame, value };
            }
            Err(error) => {
                if !retry_if(&error) {
                    return TimelineInsertResult::Failed { frame, error };
                }

                last_error = Some(error);
                frame = match frame.checked_add(STEP_FRAMES) {
                    Some(next_frame) => next_frame,
                    // usize オーバーフロー時は探索を打ち切る
                    None => return TimelineInsertResult::NotFound { last_error },
                };
            }
        }
    }

    TimelineInsertResult::NotFound { last_error }
}

// ─────────────────────────────────────────────────────────────
// テスト
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 最初の試行で挿入に成功した場合、目標フレームが返ることを確認する。
    #[test]
    fn test_insert_succeeds_at_first_attempt() {
        let result = insert_at_available_frame(
            0,
            100,
            60,
            |_layer, frame, _length| Ok::<usize, String>(frame),
            |_error| true,
        );
        match result {
            TimelineInsertResult::Inserted { frame, value } => {
                assert_eq!(frame, 100, "目標フレームで挿入されるはず");
                assert_eq!(value, 100);
            }
            TimelineInsertResult::NotFound { .. } => panic!("挿入は成功するはず"),
            TimelineInsertResult::Failed { .. } => panic!("挿入は成功するはず"),
        }
    }

    /// 目標フレームが占有されていて、1 フレーム後に挿入できる場合を確認する。
    #[test]
    fn test_insert_succeeds_after_one_skip() {
        let result = insert_at_available_frame(
            0,
            10,
            30,
            |_layer, frame, _length| {
                if frame == 10 {
                    Err("occupied")
                } else {
                    Ok(frame)
                }
            },
            |_error| true,
        );
        match result {
            TimelineInsertResult::Inserted { frame, .. } => {
                assert_eq!(frame, 11, "1 フレーム後に挿入されるはず");
            }
            TimelineInsertResult::NotFound { .. } => panic!("挿入は成功するはず"),
            TimelineInsertResult::Failed { .. } => panic!("挿入は成功するはず"),
        }
    }

    /// 複数フレームが連続して占有されていて、その後に挿入できる場合を確認する。
    #[test]
    fn test_insert_succeeds_after_multiple_skips() {
        let occupied_until = 15usize;
        let result = insert_at_available_frame(
            0,
            10,
            30,
            |_layer, frame, _length| {
                if frame <= occupied_until {
                    Err("occupied")
                } else {
                    Ok(frame)
                }
            },
            |_error| true,
        );
        match result {
            TimelineInsertResult::Inserted { frame, .. } => {
                assert_eq!(
                    frame,
                    occupied_until + 1,
                    "占有終了直後のフレームに挿入されるはず"
                );
            }
            TimelineInsertResult::NotFound { .. } => panic!("挿入は成功するはず"),
            TimelineInsertResult::Failed { .. } => panic!("挿入は成功するはず"),
        }
    }

    /// 全試行が失敗した場合は `NotFound` が返ることを確認する。
    #[test]
    fn test_insert_not_found_after_max_attempts() {
        let result = insert_at_available_frame(
            0,
            0,
            60,
            |_layer, _frame, _length| Err::<(), &str>("always occupied"),
            |_error| true,
        );
        match result {
            TimelineInsertResult::NotFound { last_error } => {
                assert_eq!(last_error, Some("always occupied"));
            }
            TimelineInsertResult::Inserted { .. } => panic!("全試行失敗時は NotFound のはず"),
            TimelineInsertResult::Failed { .. } => panic!("全試行失敗時は NotFound のはず"),
        }
    }

    /// ちょうど最大試行回数の直前（最後の試行）で成功する場合を確認する。
    #[test]
    fn test_insert_succeeds_at_last_attempt() {
        // attempt = MAX_INSERT_ATTEMPTS - 1（0-indexed）で成功するケース
        // frame = start_frame + (MAX_INSERT_ATTEMPTS - 1) * STEP_FRAMES で成功する
        let threshold = MAX_INSERT_ATTEMPTS - 1;
        let start = 0usize;
        let result = insert_at_available_frame(
            0,
            start,
            1,
            |_layer, frame, _length| {
                if frame < start + threshold {
                    Err("occupied")
                } else {
                    Ok(frame)
                }
            },
            |_error| true,
        );
        match result {
            TimelineInsertResult::Inserted { frame, .. } => {
                assert_eq!(frame, start + threshold, "最後の試行で成功するはず");
            }
            TimelineInsertResult::NotFound { .. } => panic!("最後の試行で成功するはず"),
            TimelineInsertResult::Failed { .. } => panic!("最後の試行で成功するはず"),
        }
    }

    /// `MAX_INSERT_ATTEMPTS` 回ちょうど失敗すると `NotFound` になることを確認する。
    ///
    /// threshold = MAX_INSERT_ATTEMPTS のとき: attempt 0..=MAX_INSERT_ATTEMPTS-1 で全て失敗し、
    /// attempt MAX_INSERT_ATTEMPTS は実行されずに NotFound を返す。
    #[test]
    fn test_insert_fails_one_past_max_attempts() {
        let threshold = MAX_INSERT_ATTEMPTS; // MAX_INSERT_ATTEMPTS フレーム先まで全て占有
        let start = 0usize;
        let result = insert_at_available_frame(
            0,
            start,
            1,
            |_layer, frame, _length| {
                if frame < start + threshold {
                    Err("occupied")
                } else {
                    Ok(frame)
                }
            },
            |_error| true,
        );
        assert!(
            matches!(result, TimelineInsertResult::NotFound { .. }),
            "MAX_INSERT_ATTEMPTS 回全て失敗した場合は NotFound のはず"
        );
    }

    /// 再試行不能なエラーは即座に `Failed` を返すことを確認する。
    #[test]
    fn test_insert_fails_immediately_on_non_retryable_error() {
        let result = insert_at_available_frame(
            0,
            10,
            30,
            |_layer, _frame, _length| Err::<(), &str>("invalid alias"),
            |error| *error == "occupied",
        );
        match result {
            TimelineInsertResult::Failed { frame, error } => {
                assert_eq!(frame, 10, "最初のフレームで中断されるはず");
                assert_eq!(error, "invalid alias");
            }
            TimelineInsertResult::Inserted { .. } => panic!("Failed のはず"),
            TimelineInsertResult::NotFound { .. } => panic!("Failed のはず"),
        }
    }

    /// `NotFound` が最後の再試行エラーを保持することを確認する。
    #[test]
    fn test_insert_not_found_keeps_last_retryable_error() {
        let start = 7usize;
        let result = insert_at_available_frame(
            0,
            start,
            1,
            |_layer, frame, _length| Err::<(), String>(format!("occupied@{}", frame)),
            |_error| true,
        );

        match result {
            TimelineInsertResult::NotFound { last_error } => {
                let expected_frame = start + (MAX_INSERT_ATTEMPTS - 1) * STEP_FRAMES;
                assert_eq!(
                    last_error,
                    Some(format!("occupied@{}", expected_frame)),
                    "最後に試行したフレームのエラーを保持するはず"
                );
            }
            TimelineInsertResult::Inserted { .. } => panic!("NotFound のはず"),
            TimelineInsertResult::Failed { .. } => panic!("NotFound のはず"),
        }
    }

    /// フレーム加算でオーバーフローした場合は `NotFound` で終了することを確認する。
    #[test]
    fn test_insert_not_found_when_frame_overflows() {
        let result = insert_at_available_frame(
            0,
            usize::MAX,
            1,
            |_layer, _frame, _length| Err::<(), &str>("occupied"),
            |_error| true,
        );
        match result {
            TimelineInsertResult::NotFound { last_error } => {
                assert_eq!(last_error, Some("occupied"));
            }
            TimelineInsertResult::Inserted { .. } => panic!("NotFound のはず"),
            TimelineInsertResult::Failed { .. } => panic!("NotFound のはず"),
        }
    }

    /// レイヤー番号がクロージャに正しく渡されることを確認する。
    #[test]
    fn test_correct_layer_passed_to_closure() {
        let result = insert_at_available_frame(
            3,
            0,
            10,
            |layer, frame, _length| Ok::<(usize, usize), String>((layer, frame)),
            |_error| true,
        );
        match result {
            TimelineInsertResult::Inserted {
                value: (layer, _), ..
            } => {
                assert_eq!(layer, 3, "レイヤー番号が正しく渡されるはず");
            }
            TimelineInsertResult::NotFound { .. } => panic!("挿入は成功するはず"),
            TimelineInsertResult::Failed { .. } => panic!("挿入は成功するはず"),
        }
    }

    /// オブジェクト長がクロージャに正しく渡されることを確認する。
    #[test]
    fn test_correct_length_passed_to_closure() {
        let expected_length = 120usize;
        let result = insert_at_available_frame(
            0,
            0,
            expected_length,
            |_layer, _frame, length| Ok::<usize, String>(length),
            |_error| true,
        );
        match result {
            TimelineInsertResult::Inserted { value: length, .. } => {
                assert_eq!(
                    length, expected_length,
                    "オブジェクト長が正しく渡されるはず"
                );
            }
            TimelineInsertResult::NotFound { .. } => panic!("挿入は成功するはず"),
            TimelineInsertResult::Failed { .. } => panic!("挿入は成功するはず"),
        }
    }

    /// 非ゼロの開始フレームから探索が始まることを確認する。
    #[test]
    fn test_nonzero_start_frame() {
        let result = insert_at_available_frame(
            0,
            500,
            60,
            |_layer, frame, _length| Ok::<usize, String>(frame),
            |_error| true,
        );
        match result {
            TimelineInsertResult::Inserted { frame, .. } => {
                assert_eq!(frame, 500, "指定した開始フレームから挿入されるはず");
            }
            TimelineInsertResult::NotFound { .. } => panic!("挿入は成功するはず"),
            TimelineInsertResult::Failed { .. } => panic!("挿入は成功するはず"),
        }
    }
}

//! # タイムライン挿入モジュール
//!
//! 録音したオーディオファイルをタイムラインに挿入する際、
//! 目標位置にオブジェクトが存在する場合に挿入可能な位置を再帰的に探索する機能を提供する。
//!
//! ## 設計方針
//!
//! - **独立モジュール**: このモジュールは AviUtl2 API に依存しない純粋なロジックとして実装する。
//!   実際の挿入操作はクロージャとして外部から注入し、テスト容易性を確保する。
//! - **フォールバック戦略**: 目標フレームへの挿入に失敗した場合、1 フレームずつ前進しながら
//!   挿入可能な位置を再帰的に探索する。これにより、占有オブジェクトの末尾直後に
//!   自動的にフォールバックする。
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
//! );
//! match result {
//!     TimelineInsertResult::Inserted { frame, .. } => {
//!         tracing::info!("フレーム {} に挿入しました", frame);
//!     }
//!     TimelineInsertResult::NotFound => {
//!         tracing::warn!("挿入可能な位置が見つかりませんでした");
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
pub enum TimelineInsertResult<T> {
    /// 挿入成功。
    Inserted {
        /// 実際に挿入した開始フレーム番号。
        frame: usize,
        /// 挿入操作（`try_insert` クロージャ）の戻り値。
        value: T,
    },
    /// 指定した試行回数（[`MAX_INSERT_ATTEMPTS`]）以内に挿入可能な位置が見つからなかった。
    NotFound,
}

/// タイムラインの指定フレームまたはそれ以降に挿入可能な位置にオブジェクトを挿入する。
///
/// 目標フレームへの挿入に失敗した場合、[`STEP_FRAMES`] ずつ前進しながら再帰的に
/// 挿入可能な位置を探索する。最大 [`MAX_INSERT_ATTEMPTS`] 回試行して見つからなければ
/// [`TimelineInsertResult::NotFound`] を返す。
///
/// ## フォールバック動作
///
/// `try_insert` が `Err` を返した場合（位置が占有されている等）、
/// 本関数はフォールバック先として `frame + 1` を次の候補とし、再帰的に試行する。
/// これにより呼び出し元が手動でリトライロジックを実装する必要がなくなる。
///
/// # 引数
///
/// * `layer` - 挿入先レイヤー番号（0 始まり）
/// * `start_frame` - 挿入目標フレーム番号（0 始まり）
/// * `length_frames` - 挿入するオブジェクトの長さ（フレーム数）
/// * `try_insert` - オブジェクト挿入を試みるクロージャ。
///   引数は `(layer, frame, length_frames)` で、成功時は `Ok(T)`、失敗時は `Err(E)` を返す。
///   このクロージャは複数回呼ばれる可能性があるため `Fn` 境界を要求する。
///
/// # 戻り値
///
/// - 挿入成功時は [`TimelineInsertResult::Inserted`]（実際の挿入フレームと戻り値を含む）
/// - 試行回数内に挿入可能位置が見つからなかった場合は [`TimelineInsertResult::NotFound`]
pub fn insert_at_available_frame<T, E, F>(
    layer: usize,
    start_frame: usize,
    length_frames: usize,
    try_insert: F,
) -> TimelineInsertResult<T>
where
    F: Fn(usize, usize, usize) -> Result<T, E>,
{
    insert_recursive(layer, start_frame, length_frames, &try_insert, 0)
}

/// [`insert_at_available_frame`] の内部再帰実装。
///
/// `attempt` が [`MAX_INSERT_ATTEMPTS`] に達した時点で探索を終了し、
/// [`TimelineInsertResult::NotFound`] を返す（有界再帰の保証）。
///
/// # 引数
///
/// * `layer` - 挿入先レイヤー番号
/// * `frame` - 今回試みる挿入フレーム番号
/// * `length_frames` - 挿入するオブジェクトの長さ（フレーム数）
/// * `try_insert` - 挿入クロージャへの参照
/// * `attempt` - 現在の試行回数（0 始まり）
fn insert_recursive<T, E, F>(
    layer: usize,
    frame: usize,
    length_frames: usize,
    try_insert: &F,
    attempt: usize,
) -> TimelineInsertResult<T>
where
    F: Fn(usize, usize, usize) -> Result<T, E>,
{
    // 試行回数の上限に達した場合は探索を打ち切る
    if attempt >= MAX_INSERT_ATTEMPTS {
        return TimelineInsertResult::NotFound;
    }

    match try_insert(layer, frame, length_frames) {
        Ok(value) => TimelineInsertResult::Inserted { frame, value },
        Err(_) => {
            // 挿入失敗: 1 フレーム前進してフォールバック先を再帰的に探索する
            match frame.checked_add(STEP_FRAMES) {
                Some(next_frame) => {
                    insert_recursive(layer, next_frame, length_frames, try_insert, attempt + 1)
                }
                // usize オーバーフロー時は探索を打ち切る
                None => TimelineInsertResult::NotFound,
            }
        }
    }
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
        let result = insert_at_available_frame(0, 100, 60, |_layer, frame, _length| {
            Ok::<usize, String>(frame)
        });
        match result {
            TimelineInsertResult::Inserted { frame, value } => {
                assert_eq!(frame, 100, "目標フレームで挿入されるはず");
                assert_eq!(value, 100);
            }
            TimelineInsertResult::NotFound => panic!("挿入は成功するはず"),
        }
    }

    /// 目標フレームが占有されていて、1 フレーム後に挿入できる場合を確認する。
    #[test]
    fn test_insert_succeeds_after_one_skip() {
        let result = insert_at_available_frame(0, 10, 30, |_layer, frame, _length| {
            if frame == 10 {
                Err("occupied")
            } else {
                Ok(frame)
            }
        });
        match result {
            TimelineInsertResult::Inserted { frame, .. } => {
                assert_eq!(frame, 11, "1 フレーム後に挿入されるはず");
            }
            TimelineInsertResult::NotFound => panic!("挿入は成功するはず"),
        }
    }

    /// 複数フレームが連続して占有されていて、その後に挿入できる場合を確認する。
    #[test]
    fn test_insert_succeeds_after_multiple_skips() {
        let occupied_until = 15usize;
        let result = insert_at_available_frame(0, 10, 30, |_layer, frame, _length| {
            if frame <= occupied_until {
                Err("occupied")
            } else {
                Ok(frame)
            }
        });
        match result {
            TimelineInsertResult::Inserted { frame, .. } => {
                assert_eq!(
                    frame,
                    occupied_until + 1,
                    "占有終了直後のフレームに挿入されるはず"
                );
            }
            TimelineInsertResult::NotFound => panic!("挿入は成功するはず"),
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
        );
        assert!(
            matches!(result, TimelineInsertResult::NotFound),
            "全試行失敗時は NotFound のはず"
        );
    }

    /// ちょうど最大試行回数の直前（最後の試行）で成功する場合を確認する。
    #[test]
    fn test_insert_succeeds_at_last_attempt() {
        // attempt = MAX_INSERT_ATTEMPTS - 1（0-indexed）で成功するケース
        // frame = start_frame + (MAX_INSERT_ATTEMPTS - 1) * STEP_FRAMES で成功する
        let threshold = MAX_INSERT_ATTEMPTS - 1;
        let start = 0usize;
        let result = insert_at_available_frame(0, start, 1, |_layer, frame, _length| {
            if frame < start + threshold {
                Err("occupied")
            } else {
                Ok(frame)
            }
        });
        match result {
            TimelineInsertResult::Inserted { frame, .. } => {
                assert_eq!(frame, start + threshold, "最後の試行で成功するはず");
            }
            TimelineInsertResult::NotFound => panic!("最後の試行で成功するはず"),
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
        let result = insert_at_available_frame(0, start, 1, |_layer, frame, _length| {
            if frame < start + threshold {
                Err("occupied")
            } else {
                Ok(frame)
            }
        });
        assert!(
            matches!(result, TimelineInsertResult::NotFound),
            "MAX_INSERT_ATTEMPTS 回全て失敗した場合は NotFound のはず"
        );
    }

    /// レイヤー番号がクロージャに正しく渡されることを確認する。
    #[test]
    fn test_correct_layer_passed_to_closure() {
        let result = insert_at_available_frame(3, 0, 10, |layer, frame, _length| {
            Ok::<(usize, usize), String>((layer, frame))
        });
        match result {
            TimelineInsertResult::Inserted {
                value: (layer, _), ..
            } => {
                assert_eq!(layer, 3, "レイヤー番号が正しく渡されるはず");
            }
            TimelineInsertResult::NotFound => panic!("挿入は成功するはず"),
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
        );
        match result {
            TimelineInsertResult::Inserted { value: length, .. } => {
                assert_eq!(length, expected_length, "オブジェクト長が正しく渡されるはず");
            }
            TimelineInsertResult::NotFound => panic!("挿入は成功するはず"),
        }
    }

    /// 非ゼロの開始フレームから探索が始まることを確認する。
    #[test]
    fn test_nonzero_start_frame() {
        let result = insert_at_available_frame(0, 500, 60, |_layer, frame, _length| {
            Ok::<usize, String>(frame)
        });
        match result {
            TimelineInsertResult::Inserted { frame, .. } => {
                assert_eq!(frame, 500, "指定した開始フレームから挿入されるはず");
            }
            TimelineInsertResult::NotFound => panic!("挿入は成功するはず"),
        }
    }
}

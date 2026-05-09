# 実機回帰テスト運用（初心者向け）

このドキュメントは、AviUtl2 実機で本プラグインを継続的に検証するための
最小運用ルールをまとめたものです。

## 目的

- 毎回同じ手順で確認し、見落としを減らす。
- 失敗したときに原因を追えるログを必ず残す。
- 個人開発でも無理なく続けられる時間で運用する。

## 運用ルール（固定）

1. 毎 PR で コア 6 ケースを実行する。
2. 1 ケースでも失敗したら、その PR はマージしない。
3. 失敗時はログファイルと再現手順をセットで残す。
4. 週 1 回またはリリース前に拡張 3 ケースを実行する。
5. 新しい不具合が出たら、コアか拡張に 1 ケース追加する。

## ディレクトリ構成

- `regression/projects/` : テスト用 AviUtl2 プロジェクトの置き場所
- `regression/projects/regression.aup2` : コアケースをシーンとしてまとめたテストプロジェクト
- `regression/artifacts/` : 生成 WAV とログの保存先（実行時に自動作成）
- `scripts/regression/run_recording_once.bat` : 単体ケース実行
- `scripts/regression/run_core6_manual.bat` : コア 6 ケース実行

## 事前準備

1. AviUtl2 を起動し、プラグインが読み込まれている状態にする。
2. `audio_rec_cli.exe` が実行可能であることを確認する。
3. `regression/projects/README.md` の定義に従って、
  `regression/projects/regression.aup2` に 4 ケースをシーンとして作成する。

## コア 6 ケース（毎 PR）

1. CASE01: 空き位置への挿入成功
2. CASE02: 1 フレーム衝突後のフォールバック成功
3. CASE03: 連続衝突後のフォールバック成功
4. CASE04: 試行上限到達（NotFound）
5. CASE05: start/stop の冪等性
6. CASE06: 連続 3 セッション安定性

## 拡張 3 ケース（週次 or リリース前）

1. 日本語・空白入りパスでの録音と挿入
2. 長時間録音（60 秒目安）
3. 30fps と 60fps での長さ換算確認

## 実行手順（最短）

PowerShell でリポジトリ直下に移動し、次を実行してください。

```powershell
scripts\regression\run_core6_manual.bat
```

必要に応じて CLI パスを指定できます。

```powershell
scripts\regression\run_core6_manual.bat \
  ".aviutl2-cli\development\data\audio_rec_cli.exe"
```

## 合格基準

1. バッチの終了コードが 0 である。
2. `regression/artifacts/logs/` に各ケースのログが残る。
3. ケースごとの期待結果を目視確認できる。

## 失敗時の記録テンプレート

以下を PR コメントに貼って記録してください。

```txt
[回帰失敗報告]
- ケース ID:
- 実行日時:
- CLI パス:
- プロジェクト名:
- 期待結果:
- 実結果:
- ログファイル:
- 再現手順:
- 暫定判断（マージ可否）:
```

## 補足

- ログに `api call failed` が出るケースでは、
  実装上 `ApiCallFailed` を再試行対象として扱う。
- それでも挿入できない場合は、
  プロジェクト側のオブジェクト配置とカーソル位置を再確認する。
- `regression/projects/regression.aup2` をコミットする前に、
  `[project]` セクションの `file=` 行（ローカル絶対パス）を削除する。

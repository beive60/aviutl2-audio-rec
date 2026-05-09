# テスト用プロジェクトの作り方

`regression/projects/regression.aup2` に、以下 4 ケースをシーンとして保存してください。

## 共通ルール

1. すべて同じ解像度・fps で作る。
2. カーソル初期位置は `frame=16, layer=1` に統一する。
3. 音声挿入対象レイヤーは layer=1 に統一する。
4. シーン名は以下の固定名を使う。
5. コミット時は `[project]` セクションの `file=` 行（ローカル絶対パス）を含めない。

## 必須シーン

1. `core01_empty_target`
   - frame=16, layer=1 が空。

2. `core02_blocked_1f`
   - frame=16, layer=1 を 1 フレームだけ埋めるオブジェクトを配置。

3. `core03_blocked_20f`
   - frame=16 から 20 フレーム分、layer=1 を連続で埋める。

4. `core04_blocked_over_limit`
   - frame=16 から `MAX_INSERT_ATTEMPTS` を超える範囲を埋める。
   - 現在の実装では 1000 フレーム超を目安に配置する。

## コミット前チェック

1. `regression/projects/regression.aup2` を開き、`[project]` セクションに `file=` 行があれば削除する。
2. 保存後に AviUtl2 で再度開けることを確認する。

## 目視確認の最小ポイント

1. CASE01: frame=16 に挿入される。
2. CASE02: frame=17 に挿入される。
3. CASE03: frame=36 付近（16 + 20）に挿入される。
4. CASE04: 挿入されず、NotFound 系ログが残る。

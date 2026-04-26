[![standard-readme compliant](https://img.shields.io/badge/readme%20style-standard-brightgreen.svg?style=flat-square)](https://github.com/RichardLitt/standard-readme) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

AviUtl2（ExEdit2）のタイムラインを再生しながら、ワンボタンでマイク音声を録音し WAV ファイルとして保存する Rust 製プラグインと CLI クライアント。

AviUtl2 の汎用プラグイン（`.aux2`）が Named Pipe サーバーとして常駐し、CLI クライアントからの録音コマンドを受け取って Windows WASAPI 経由で録音セッションを制御します。Stream Input のプロファイルと組み合わせることで、コントローラーの1ボタン操作で録音の開始・停止ができます。

## 背景

AviUtl2（ExEdit2、KENくん氏による 64bit 対応版）でナレーションやスクラッチトラックを収録する際、NLE と外部の録音ソフトウェア（DAW）を行き来するオペレーションが必要です。本プロジェクトはこのコンテキストスイッチを排除し、AviUtl2 を操作したまま外部ハードウェアのボタン1つで録音を完結させることを目的としています。

GUI の操作をエミュレートするのではなく、Named Pipe を通じて AviUtl2 プロセス内の録音処理を直接制御するため、ウィンドウのフォーカス状態や画面レイアウトに依存しない安定した自動化が実現できます。

> **対象**: 旧 AviUtl（32bit）ではなく **AviUtl2（ExEdit2、64bit）** のみを対象とします。

## インストール

### 前提条件

- Windows 10 または Windows 11（64bit）
- AviUtl2（ExEdit2）がインストール済みであること

> 動作確認は AviUtl2（ExEdit2）開発時点の最新版で行っています。AviUtl2 のバージョンアップ後に動作しない場合はリリースページで対応状況を確認してください。

### リリースからインストール（推奨）

1. [GitHub Releases](https://github.com/beive60/aviutl2-audio-rec/releases/latest) から最新版の zip をダウンロードする。
2. zip を展開する。
3. `aviutl2_audio_rec.aux2` を `C:\ProgramData\AviUtl2\plugins\` にコピーする。
4. `audio_rec_cli.exe` を任意の場所（例: `C:\ProgramData\AviUtl2\`）にコピーする。
5. AviUtl2 を起動するとプラグインが自動的に読み込まれ、Named Pipe サーバーが起動します。

### ソースからビルド

ビルドには追加で以下が必要です。

- [Rust toolchain](https://rustup.rs/) — `x86_64-pc-windows-msvc` ターゲット
- [aviutl2-cli](https://github.com/sevenc-nanashi/aviutl2-cli) — ビルド・デプロイ・パッケージング用 CLI ツール（`au2` コマンド）
- [aviutl2-rs](https://github.com/sevenc-nanashi/aviutl2-rs)（`Cargo.toml` で自動取得）

> aviutl2-rs は開発中の crate であり、API の破壊的変更が発生する可能性があります。

aviutl2-cli は [Releases](https://github.com/sevenc-nanashi/aviutl2-cli/releases/latest) からダウンロードするか、`cargo-binstall` でインストールできます。

```
cargo binstall aviutl2-cli
```

リポジトリをクローンし、初回セットアップを実行します。

```
git clone https://github.com/beive60/aviutl2-audio-rec.git
cd aviutl2-audio-rec
au2 prepare
```

`au2 prepare` は AviUtl2 本体のダウンロード・展開と、設定ファイルの JSON Schema 出力、成果物へのシンボリックリンク作成を一括で行います。

#### 開発ビルド

```
au2 develop
```

プラグイン（`.aux2`）と CLI（`.exe`）をビルドし、開発用 AviUtl2 ディレクトリに自動で配置します。

#### リリースパッケージの作成

```
au2 release
```

リリース用にビルドしたパッケージ（zip）を生成します。生成された zip には以下のファイルが含まれます。

| ファイル | 説明 |
|---|---|
| `aviutl2_audio_rec.aux2` | AviUtl2 プラグイン本体 |
| `audio_rec_cli.exe` | CLI クライアント |

## 使い方

### CLI

AviUtl2 が起動した状態で、コマンドと出力先の WAV ファイルパスを引数に指定して実行します。

録音を開始します。

```
audio_rec_cli.exe start "C:\recordings\take1.wav"
```

録音を停止します。

```
audio_rec_cli.exe stop
```

録音中であれば停止、停止中であれば開始します（トグル）。

```
audio_rec_cli.exe toggle "C:\recordings\take1.wav"
```

成功すると終了コード 0 で終了します。エラー時は標準エラー出力にメッセージが表示され、非ゼロの終了コードで終了します。

### Stream Deck 統合

Stream Deck の「システム: ウェブサイトを開く」または「システム: コマンドの実行」アクションに以下のように設定します。

```
audio_rec_cli.exe toggle "C:\recordings\take1.wav"
```

## アーキテクチャ

```
[Stream Deck ボタンなど]
       |
       v
[audio_rec_cli.exe] --UTF-8(command)--> [Named Pipe]
                                              |
                                  [aviutl2_audio_rec.aux2]
                                       (ワーカースレッド)
                                              |
                                      WASAPI 録音セッション
                                              |
                                       WAV ファイル出力
```

### IPC プロトコル

| 項目 | 仕様 |
|---|---|
| 通信方式 | Named Pipe（`\\.\pipe\aviutl2_audio_rec`） |
| 方向 | CLI クライアント → プラグイン（単方向） |
| エンコーディング | UTF-8 |
| ペイロード | コマンド（`start`/`stop`/`toggle`）+ 出力ファイルパス（`start`/`toggle` 時） |
| 最大ペイロード長 | 32,768 バイト |

## クレジット

- [sevenc-nanashi/aviutl2-rs](https://github.com/sevenc-nanashi/aviutl2-rs) — AviUtl2 Plugin SDK の Rust バインディング。本プロジェクトのプラグイン実装はこの crate に依存しています。
- [aviutl2-cli](https://github.com/sevenc-nanashi/aviutl2-cli) — AviUtl2 のプラグイン・スクリプト開発用コマンドラインツール。本プロジェクトのプラグインの開発はこのツールを使用して行われています。

## コントリビューション

Issue や Pull Request を歓迎します。[GitHub Issues](https://github.com/beive60/aviutl2-audio-rec/issues) で質問・提案を受け付けています。

詳細は [CONTRIBUTING.md](CONTRIBUTING.md) を参照してください。

## ライセンス

[MIT](LICENSE) © 2026 べいぶ
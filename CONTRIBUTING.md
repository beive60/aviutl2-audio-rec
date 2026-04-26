## コントリビューションガイド

Issue や Pull Request を歓迎します。このドキュメントでは、本プロジェクトへのコントリビューション手順とコーディングルールを説明します。

## 開発環境のセットアップ

### 前提条件

- [Rust toolchain](https://rustup.rs/) — `x86_64-pc-windows-msvc` ターゲット
- [aviutl2-cli](https://github.com/sevenc-nanashi/aviutl2-cli) — `au2` コマンド

### セットアップ手順

```
git clone https://github.com/beive60/aviutl2-audio-rec.git
cd aviutl2-audio-rec
cargo install aviutl2-cli
```

`au2 prepare` はシンボリックリンクの作成に管理者権限が必要です。

**Windows 11 24H2 (Build 26052) 以降**: OS ネイティブの `sudo` が利用できます。（管理者権限のターミナルから `sudo config --enable normal` を実行するか、Windows の設定（システム > 開発者向け）から有効化する必要があります）

```
sudo au2 prepare
```

**それより前のバージョン**: PowerShell を「管理者として実行」してから実行してください。

```
au2 prepare
```

### 開発ワークフロー

コードを変更しながら確認する日常の開発ループは `au2 develop`（または短縮形 `au2 dev`）を使います。デバッグビルドを生成し、開発用 AviUtl2 ディレクトリに自動で配置します。

```
au2 develop
```

動作確認には `audio_rec_cli.exe` を使用して録音の開始・停止を手動で実行してください。

```
.aviutl2-cli\development\data\audio_rec_cli.exe start "$PWD\output\test.wav"
.aviutl2-cli\development\data\audio_rec_cli.exe stop
```

PR 提出前にリリースビルドで動作を確認したい場合は `au2 preview` を使います。リリース設定（最適化あり）でビルドし、開発用 AviUtl2 ディレクトリに配置します。配布用の zip は生成しません。

```
au2 preview
```

## コントリビューションの流れ

1. [Issues](https://github.com/beive60/aviutl2-audio-rec/issues) で作業内容を事前に報告・議論する。
2. 本リポジトリを GitHub 上でフォークする。
3. フォークしたリポジトリをローカルにクローンし、`main` ブランチからフィーチャーブランチを作成する。
4. 変更を加えてコミットする。
5. フォーク先にプッシュし、上流リポジトリの `main` ブランチへ Pull Request を作成する。

## コーディングルール

### ドキュメントとコメント

**ドキュメントおよびコメントはすべて日本語で記載してください。**

ファイル、関数、構造体、列挙型、トレイトなど、すべての公開・非公開アイテムに Rustdoc 形式（`///` または `//!`）でコメントを記載してください。

ファイルレベルのドキュメントはモジュール属性 `//!` を使用します。

```rust
//! Named Pipe サーバーの実装。
//! ワーカースレッド上で接続を待ち受け、受信したコマンドを録音処理に渡す。
```

関数・メソッドには `///` を使用し、処理の概要・引数・戻り値・エラーを記載します。

```rust
/// 録音セッションを開始し、指定されたパスへ WAV ファイルを書き出す。
///
/// # 引数
///
/// * `output_path` - 録音結果を保存する WAV ファイルの絶対パス。
///
/// # 戻り値
///
/// 録音の開始に成功した場合は `Ok(())`、失敗した場合は `Err` を返す。
///
/// # エラー
///
/// デバイスの初期化に失敗した場合、または既に録音中の場合に
/// `RecordingError` を返す。
pub fn start_recording(output_path: &Path) -> Result<(), RecordingError> {
    // ...
}
```

構造体・列挙体のフィールドにもコメントを記載します。

```rust
/// Named Pipe サーバーの設定値。
pub struct PipeConfig {
    /// パイプ名（例: `\\.\pipe\aviutl2_audio_rec`）。
    pub name: String,
    /// 受信バッファの最大バイト数。
    pub max_payload_bytes: usize,
}
```

### アトリビュートの記述

複数のアトリビュートを持つ要素では、各アトリビュートを別の行に記載してインデントします。

```rust
// 良い例
#[derive(Debug)]
#[derive(Clone)]
pub struct Foo { ... }

// 悪い例
#[derive(Debug, Clone)]
pub struct Foo { ... }
```

ただし `derive` マクロのように意味的にひとまとめが自然な場合は、プロジェクト内で統一した記述に従ってください。

### 列の整列を使用しない

コード、ドキュメント、コメントにおいて、列を揃える整形（カラムアライメント）は使用しません。

```rust
// 良い例
let foo = 1;
let bar_baz = 2;
let qux = 3;

// 悪い例
let foo     = 1;
let bar_baz = 2;
let qux     = 3;
```

### 絵文字を使用しない

ドキュメント、コメント、コミットメッセージ、ターミナル出力など、すべての文章で絵文字を使用しません。

### エラーハンドリング

`unwrap()` や `expect()` の使用はテストコードを除き避けてください。エラーは `Result` または `Option` で適切に伝播させます。

```rust
// 良い例
let file = File::create(path)?;

// 悪い例
let file = File::create(path).unwrap();
```

### 不必要なクローンを避ける

借用で済む場面では `.clone()` を使用しません。

```rust
// 良い例
fn process(path: &Path) { ... }

// 悪い例
fn process(path: PathBuf) { ... }
process(path.clone());
```

### スレッドと録音処理

WASAPI の録音セッションはワーカースレッド上で管理します。AviUtl2 のメインスレッドを要する操作が必要な場合は、必ず `EDIT_HANDLE::call_edit_section_param()` を介してメインスレッドへディスパッチします。録音バッファへの書き込みと読み出しは排他制御（`Mutex` 等）を用いてスレッドセーフに行ってください。

## フォーマットとリント

PR を提出する前に以下を実行してください。

```
cargo fmt
cargo clippy -- -D warnings
```

## ライセンス

本プロジェクトへのコントリビューションは [MIT ライセンス](LICENSE) のもとで公開されます。

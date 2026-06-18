# notetui

ターミナル（TUI）Misskey クライアント。コアロジックは
[`notecli`](https://github.com/hitalin/notecli) ライブラリに委譲し、本リポジトリは
TUI フロントエンドのみを持つ。NoteDeck（GUI）と同列の「notecli を消費する別フロント
エンド」という位置づけ。

## 設計

- Misskey API・DB・認証・モデルはすべて `notecli` 側。notetui は端末状態と描画だけを所有
- アカウント DB は notecli/NoteDeck と共有（`~/.local/share/notecli/notecli.db`）
- 依存: `ratatui` / `tokio` / `notecli`（path 依存）

## 前提

先に notecli でログインしてアカウントを登録しておく:

```sh
notecli login <HOST>
```

## ビルド & 実行

`../notecli` が隣に存在する前提（`Cargo.toml` の path 依存）。

```sh
cargo run
```

## キーバインド

| キー | 動作 |
|------|------|
| `q` / `Esc` | 終了 |
| `j` / `k`（↓/↑） | カーソル移動 |
| `g` / `G` | 先頭 / 末尾 |
| `r` | 再取得 |
| `Tab` / `h` / `l` | タイムライン切替（Home / Social / Local / Global） |

# notetui 設計仕様 (DESIGN)

Misskey の TUI クライアント。notecli ライブラリ上の薄いフロントエンドとして、
notecli/notedeck とアカウント DB を共有する。本書はアーキテクチャ方針の合意事項を残す。

## 1. 方針サマリ（決定済み）

| 論点 | 決定 |
|------|------|
| スコープ | **フルクライアント**（閲覧 + 投稿/リプライ/リアクション/リノート/フォロー等の書き込み） |
| イベントループ | **TEA 風**（`Message` enum + `update()` + `view()`、tokio mpsc で集約） |
| リアルタイム更新 | **ストリーミング**（`notecli::streaming` の WebSocket） |
| レイアウト | **単一カラム**（詳細/投稿はオーバーレイ・ペインで表示） |

## 2. レイヤリング（責務境界）

- **notecli**: ドメイン/API 層。`MisskeyClient`(HTTP)、`streaming`(WS)、`db`、`models`、`keychain`。notetui からは**純粋に再利用**し、Misskey 固有のロジックは持ち込まない。
- **notetui**: 端末状態・入力・描画・画面遷移のみを所有。notecli を git rev で pin（再現性のため。rev 更新で追従）。

この境界は現状を踏襲・厳守する。

## 3. アーキテクチャ: TEA + async ランタイム

ポーリングループ（`event::poll`）を廃し、`tokio` の並行タスクを `mpsc` で 1 本の `Message` 流に集約する。`update()` が状態遷移し、必要なら副作用（API 呼び出し）を**別タスク**へ投げ、結果を再び `Message` として戻す。`view()` は `&App` を読むだけの純粋描画。

```
┌──────────────┐   Message    ┌───────────────┐
│ input task   │─────────────▶│               │
│ (crossterm   │              │   main loop   │── view(&App) ──▶ ratatui draw
│  EventStream)│              │               │
├──────────────┤   Message    │  update(&mut  │
│ stream task  │─────────────▶│  App, Message)│── spawn ─▶ API worker task
│ (EventBus rx)│              │   → Command   │              │
├──────────────┤   Message    │               │◀── Message ──┘ (結果/エラー)
│ tick timer   │─────────────▶└───────────────┘
└──────────────┘
```

中核ループ（擬似コード）:

```rust
let (tx, mut rx) = mpsc::channel::<Message>(256);
spawn_input_task(tx.clone());        // crossterm EventStream → Message::Key/Resize
spawn_stream_task(tx.clone(), ...);  // EventBus::subscribe() → Message::Stream
let mut app = App::new(...);
loop {
    terminal.draw(|f| view(&app, f))?;
    let Some(msg) = rx.recv().await else { break };
    match update(&mut app, msg, &tx) { Flow::Quit => break, Flow::Continue => {} }
}
```

- **副作用の出し方**: `update()` は UI スレッドをブロックしない。API 呼び出しは `tokio::spawn` し、完了時に `tx.send(Message::Loaded(...))`。実行中は対象にスピナー/ロード状態を表示。
- **TEA を選ぶ理由**: 状態遷移が 1 箇所に集約され、`update()` を純粋に保てるためテスト容易・拡張容易。フルクライアント規模に耐える。

### Message（骨子）

```rust
enum Message {
    // 入力
    Key(KeyEvent), Resize(u16, u16),
    // 非同期結果
    TimelineLoaded { tl: TimelineType, notes: Vec<NormalizedNote> },
    ThreadLoaded { note_id: String, ctx: Vec<NormalizedNote> },
    NotificationsLoaded(Vec<...>),
    ActionDone(ActionResult),   // 投稿/リアクション等の完了
    Error(String),              // safe_message() 済み
    // ストリーミング
    Stream(SseEvent),           // event_bus 経由（新着ノート/通知/mention 等）
    Tick,
}
```

## 4. 状態モデル

```rust
struct App {
    accounts: Vec<Account>, active: usize,   // 複数アカウント切替
    client: MisskeyClient,
    screen: Screen,                 // 現在の主画面
    overlay: Option<Overlay>,       // 詳細/投稿/確認などの重ね表示
    mode: InputMode,                // Normal / Insert / Command
    timelines: HashMap<TimelineType, TimelineState>, // TL ごとにキャッシュ+選択位置
    notifications: NotificationState,
    status: StatusLine,             // 接続状態・件数・エラー
    pending: usize,                 // 実行中の非同期数（スピナー用）
}

enum Screen { Timeline(TimelineType), Notifications, UserProfile(String) }
enum Overlay { Thread(String), Compose(ComposeState), Confirm(ConfirmState) }
enum InputMode { Normal, Insert, Command }
```

`view()` は `screen` を描画し、`overlay` があればその上に重ねる（単一カラム + ポップアップ方針）。

## 5. ストリーミング統合

`notecli::streaming::StreamingManager` は `EventBus`（`tokio::sync::broadcast<SseEvent>`）へイベントを流す。notetui はこれを購読して `Message::Stream` に変換するだけ。

```rust
let bus = Arc::new(EventBus::new());
let mgr = StreamingManager::new(Arc::new(NoopEmitter), bus.clone(), db.clone());
mgr.connect(&account_id, &host, &token).await?;
// forward task:
let mut rx = bus.subscribe();
while let Ok(ev) = rx.recv().await { tx.send(Message::Stream(ev)).await.ok(); }
```

- `SseEvent { event_type, data }` の `event_type` で分岐（note / notification / mention / …）し、該当 `TimelineState` の先頭へ挿入、未読カウント更新。
- 接続管理・再接続・ハートビートは notecli 側が担保。notetui は `StatusLine` に接続状態を表示。

## 6. 入力 / キーバインド

vim 風モーダル（現状の `j/k/g/G/Tab/h/l/r/q` を踏襲）。

- **Normal**: 移動・画面切替・アクション起動（`c` compose, `R` reply, `e` reaction, `b` renote, `f` favorite, `Enter` スレッド, `o` ユーザー）。
- **Insert**: compose/検索のテキスト入力（`tui-textarea` を採用）。
- **Command**: `:` で TL 指定・アカウント切替など（後続）。
- キーマップは将来 config 化（§9）。まずはハードコード。

## 7. 書き込み操作

notecli の write API を利用。代表対応:

| 操作 | notecli API |
|------|-------------|
| 投稿/リプライ/リノート | `create_note(CreateNoteParams { text, reply_id, renote_id, cw, visibility, file_ids, poll, .. })` |
| リアクション | `create_reaction` / `delete_reaction` |
| お気に入り | `create_favorite` / `delete_favorite` |
| 投票 | `vote_poll` |
| 削除 | `delete_note` |
| フォロー | `follow_user` / `unfollow_user` |
| 添付 | `upload_file` |
| スレッド | `get_note_children` / `get_note_conversation` |
| 通知 | `get_notifications` / `mark_all_notifications_as_read` |

- **楽観更新**: リアクション/お気に入り等は即座に UI 反映 → 失敗時 `Message::Error` でロールバック。
- 破壊的操作（削除）は `Overlay::Confirm` を挟む。

## 8. エラー / 接続 / セキュリティ

- API エラーは `NoteDeckError::safe_message()`（トークン秘匿済み）で `Message::Error` 化し `StatusLine` に表示。現状の方針を維持。
- 資格情報は `keychain::init_store()` 後に `get_credentials()` で解決（DB 空 + keychain 保存の notecli 仕様に対応済み）。
- トークンをログ/画面に出さない。

## 9. 設定 / 永続化

- アカウント DB は notecli と共有（`~/.local/share/notecli/notecli.db` + keychain）。
- notetui 固有設定（キーマップ、デフォルト TL、表示密度、relative time 等）は XDG config（`~/.config/notetui/config.toml`）。**MVP では未導入**、デフォルト値ハードコードで開始。

## 10. テスト戦略

- `update()` を純粋関数に保ち、`(App, Message) → App` をユニットテスト（ネットワーク不要）。
- 描画は `ratatui::backend::TestBackend` でバッファのスナップショット検証。
- notecli 由来のドメインロジックは notecli 側でテスト済みとして信頼。

## 11. 依存

- `notecli`（git rev pin）, `ratatui` 0.29, `crossterm`（EventStream 用に `event-stream` feature）, `tokio`, `anyhow`, `dirs`, `uuid`。
- 追加候補: `tui-textarea`（compose）, `throbber-widgets-tui`（スピナー）, 将来 `tui-input` / 設定用 `toml` + `serde`。

## 12. 実装ロードマップ（段階）

1. **基盤移行**: ポーリング → TEA（`Message`/`update`/`view` + 入力タスク）。挙動は現状維持のまま骨格だけ差し替え。
2. **非同期化**: TL 取得をワーカータスク化し UI 非ブロッキング + スピナー。
3. **ストリーミング**: EventBus 購読で新着リアルタイム反映・未読カウント。
4. **閲覧拡充**: スレッド表示（overlay）、通知画面、ユーザープロフィール。
5. **書き込み**: compose（投稿/リプライ/CW/可視性）、リアクション、リノート、お気に入り、フォロー（楽観更新 + Confirm）。
6. **仕上げ**: 複数アカウント切替、config 化（キーマップ/既定 TL）、画像/絵文字表現の改善。

各段は独立して動作確認可能な単位とし、diff を小さく保つ。

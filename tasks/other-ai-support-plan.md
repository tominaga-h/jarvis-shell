# Anthropic / OpenCode (Zen・Go) モデルサポート実装プラン

- 作成日: 2026-08-08
- 対象バージョン: v1.15.6 → **v1.16.0**(SemVer: 後方互換の機能追加 = minor bump)
- ステータス: 計画(未着手)
- 調査手法: Dynamic Workflow による並列調査(コードベース×2 / Anthropic API / OpenCode Zen API、各調査は一次ソースで反証検証済み)

---

## 1. ゴール

jarvish の AI Brain を OpenAI 一社決め打ちから **マルチプロバイダ対応** に拡張する。

| プロバイダ | API 方式 | 実装アプローチ |
| --- | --- | --- |
| OpenAI(現行) | Chat Completions | 現行の async-openai を維持 |
| **Anthropic** | Messages API(独自形式) | reqwest + SSE によるネイティブ実装 |
| **OpenCode Zen** | OpenAI Chat Completions 互換 | async-openai の base_url 差し替えで流用 |
| **OpenCode Go** | Zen と同一(別 base URL・別サブスク) | 同上(base URL のみ変更) |

既存の体験(エージェントループ / ツールコール / `| ai` パイプ / エラー自動調査 / ストリーミング表示)はプロバイダを問わず全て動作すること。

---

## 2. 調査結果サマリ

### 2.1 現状アーキテクチャ(コードベース調査)

AI 層は **async-openai 0.27 に全面結合** しており、プロバイダ中立の中間型はほぼ存在しない。

- **最大の結合点**: `src/ai/types.rs` の `ConversationState.messages: Vec<ChatCompletionRequestMessage>`(async-openai 型そのもの)。会話履歴の唯一の保持場所(プロセス内メモリのみ、Black Box 非永続)。
- **クライアント生成**: `src/ai/client/core.rs` の `JarvisAI::new()` に集約。`client: Client<OpenAIConfig>` を直接埋め込み、`OPENAI_API_KEY` 環境変数をハードコードで読む。base_url 上書き・タイムアウト・リトライは無し。
- **エージェントループ**: `src/ai/client/agent.rs` の `run_agent_loop()` に一本化(`max_rounds` 上限、stream:true、temperature 送信)。ツール分岐は ①テキストのみ→NaturalLanguage ②`execute_shell_command`→Command で脱出 ③ファイル操作ツール→ローカル実行して継続、の3系統。
- **ストリーム処理**: `src/ai/stream.rs` が `client.chat().create_stream()` と `chunk.choices[*].delta.{content,tool_calls}` 形状に密結合(`stream.rs:110` は `for choice in &response.choices` でマルチ choice 対応のリファクタが必要)。`process_stream`(エージェント用)と `process_ai_pipe_stream`(`| ai` 用)の2実装でロジックが重複。
- **既にプロバイダ中立な部品**(trait 導入の着地点として利用可能):
  - `StreamResult { full_text, tool_calls, interrupted }`(stream.rs)
  - `ToolCallAccumulator { id, function_name, arguments }`(tools/call.rs)
  - `AiResponse` / `ConversationOrigin` / `ConversationResult`(types.rs)
  - `tools/executor.rs`(文字列ベースのツール実行、無改修で流用可)
- **設定**: `AiConfig` に provider 概念なし(`model = "gpt-4o"` 等の自由文字列のみ)。config テンプレートは `defaults.rs` の TEMPLATE 定数と `mod.rs` の doc コメントに**重複定義**。
- **reload の制約**: `config_update.rs` の `update_config()` はフィールド上書きのみで **client を再構築しない**。プロバイダ切替を `source` で即時反映するには reload 経路の拡張が必要。
- **NL 分類器**(`src/engine/classifier/`)は完全ローカルヒューリスティックで AI API を呼ばない → 本件の影響なし。

### 2.2 Anthropic Messages API(一次ソースで全項目 confirmed)

- エンドポイント: `POST https://api.anthropic.com/v1/messages`(モデル一覧は `GET /v1/models`)。
- 認証: **`x-api-key` ヘッダ**(Bearer ではない)+ `anthropic-version: 2023-06-01`(固定文字列。「古い API」ではなくバージョニングスキームの初版識別子で、2026 年現在も最新モデルにこの値を使う)。
- リクエスト: `max_tokens` **必須**。`system` は messages 配列内ではなく**トップレベルパラメータ**。messages は role(user/assistant) + content(文字列 or content blocks 配列)。
- **ツールコールの構造的差分**(OpenAI 比):
  - OpenAI: `assistant.tool_calls[]` 配列 + `role:"tool"` の専用結果メッセージ
  - Anthropic: assistant の `content[]` 内に `tool_use` ブロックが混在、結果は**次の user メッセージの content 内に `tool_result` ブロック**として返す(複数結果は 1 user メッセージにまとめるのが必須慣行)
  - ツール定義は `{name, description, input_schema}` — JSON Schema 本体は OpenAI Function Calling とほぼ同形で転用容易(`parameters` → `input_schema` の名前替え程度)
- **SSE ストリーミング**: `message_start → (content_block_start → content_block_delta* → content_block_stop)* → message_delta* → message_stop`、随時 `ping` / `error`。デルタ型:
  - `text_delta`: テキスト増分
  - `input_json_delta`: ツール引数の **partial_json 断片**(content_block_stop 後に連結して serde_json でパース) — jarvish の `ToolCallAccumulator` の文字列連結方式がそのまま適用可能
  - `thinking_delta` / `signature_delta`: thinking 有効時
- **stop_reason**: `end_turn / max_tokens / stop_sequence / tool_use / pause_turn / refusal / model_context_window_exceeded`。**`refusal` は HTTP 200 で返る**ため、`content[0]` に無条件アクセスする実装はクラッシュする。stop_reason を先に見る実装が必須。
- **Claude 5 系の重要挙動**:
  - `temperature` / `top_p` / `top_k` は **400 エラーで拒否される**(Opus/Sonnet 4.6 以降)。jarvish は現在全リクエストに temperature を送っているため、Anthropic 経路では**送信しない**分岐が必須。
  - thinking はデフォルト ON(adaptive)。思考トークンは `max_tokens` から消費されるため、タイトな max_tokens だと応答が `stop_reason:max_tokens` で切れる。
- モデル(2026-08 時点、実在確認済み): `claude-sonnet-5`($3/$15、2026-08-31 まで導入価格 $2/$10)= **シェル用途の既定候補**、`claude-haiku-4-5`($1/$5、最速最安・context 200K)、`claude-opus-5`($5/$25)、`claude-fable-5`($10/$50、シェル用途には過剰)。
- エラー形式: `{"type":"error","error":{"type":"...","message":"..."},"request_id":"..."}`。レート制限は `anthropic-ratelimit-*` ヘッダ + `retry-after`(秒)。
- **OpenAI 互換レイヤーは採用しない**: 実在する(base_url を `https://api.anthropic.com/v1/` に向けると chat.completions が動く)が、公式が「テスト・モデル比較専用であり本番運用向けではない」と明記。プロンプトキャッシュ非対応・`response_format`/`strict` 無視・system 統合などの機能欠落があるため、**ネイティブ Messages API 実装を採用**する。
- **Rust SDK 事情**: 公式 SDK は存在しない。コミュニティクレート(anthropic-ai-sdk / async-anthropic / misanthropy / clust)はメンテ状況にばらつきがあり、Claude 5 系の新パラメータ追従が不安。Messages API は単一エンドポイントの JSON POST + SSE でシンプルなため、**reqwest + serde 手書き実装を推奨**(SSE パースは数十行で実装可能。`reqwest` は新規依存として追加)。

### 2.3 OpenCode Zen / Go(一次ソース確認済み。一部 unverified あり → §7)

- **OpenCode Zen** = OpenCode(SST 製コーディングエージェント CLI)チームが運営する検証済みモデルのゲートウェイ。
  - Base URL: `https://opencode.ai/zen/v1`(パス配下。`api.opencode.ai` というサブドメインは存在しない)
  - エンドポイント: OpenAI 互換 `/chat/completions`、`/responses`、Anthropic 互換 `/messages`、Google 互換 `/models/{id}`、モデル一覧 `GET /models`(ライブ確認で 61〜62 モデル: gpt-5.x 系、claude-fable-5/opus-5/sonnet-5/haiku-4-5、gemini-3.x、kimi、glm、deepseek、無料枠 8 種等)
  - 認証: `Authorization: Bearer <ZEN_API_KEY>` — **公式一次ソースでの明記は未確認**(OpenAI 互換を名乗る以上ほぼ確実だが、実装前スモークテスト必須)
  - API キー: opencode.ai のコンソールで発行
- **「go」の正体 = OpenCode Go**: Zen 上のモデル名でも Go 言語実装でもなく、**月額 $10(初月 $5)のサブスクリプション型プロバイダ**。Base URL `https://opencode.ai/zen/go/v1`、provider id `opencode-go`、18 モデルに絞った低コストプラン。API 形状は Zen と同じ → **実装上は base URL の差し替えのみで両対応可能**。
- **実装方式**: OpenAI Chat Completions 互換なので、既存の async-openai を `OpenAIConfig::new().with_api_base(...).with_api_key(...)` で流用するのが最小コスト。専用 Rust クレートは不要。
- **注意点**(§7 のリスク項目):
  - 公式 CLI は `x-opencode-client: cli` 等の識別ヘッダを送っており、無い場合は匿名扱いで厳しいレート制限(429)が課されるとのコミュニティ報告あり(公式ドキュメント未記載)。async-openai はカスタム reqwest クライアント差し替えでヘッダ注入可能 → 実装時に対応。
  - モデルごとのツールコール対応可否は二次情報(models.dev)の信頼性が低く**検証で反証された**(当初「gpt-5.4-pro/5.5-pro のみ非対応」→再取得で矛盾)。「ほぼ全モデル対応」を前提にせず、実機テストを必須とする。
  - 公式ドキュメントは OpenAI 系モデル(gpt-5 系)に `/responses` エンドポイントを案内しており、全モデルを `/chat/completions` 一本で通せる保証はない。**初期サポートは `/chat/completions` で動くモデルに限定**し、動作確認済みモデルを README に列挙する方針とする。
  - ストリーミング SSE 形式は公式未記載(標準 OpenAI SSE と推定)。

---

## 3. 設計方針

### 3.1 プロバイダ抽象: `AiBackend` enum + 中立型

trait object(`Box<dyn AiProvider>`)ではなく **enum ディスパッチ**を採用する。

```rust
// src/ai/provider/mod.rs (新設)
pub enum AiBackend {
    /// OpenAI 本家 + OpenCode Zen/Go + 任意の OpenAI 互換エンドポイント
    OpenAiCompat(OpenAiCompatBackend),   // async-openai ベース
    /// Anthropic Messages API ネイティブ
    Anthropic(AnthropicBackend),         // reqwest + SSE ベース
}

impl AiBackend {
    pub async fn create_stream(
        &self,
        req: ChatRequest,               // 中立型
        // SIGINT/スピナーは呼び出し側(stream.rs 後継)が担当
    ) -> Result<impl Stream<Item = Result<ChatChunk>>>;
}
```

採用理由: バリアントが 2 つで閉じており、`async fn` の object safety 問題や `async-trait` 依存追加を回避できる。OpenCode Zen/Go は `OpenAiCompat` バリアントの設定違い(base_url / キー環境変数 / 追加ヘッダ)として吸収する。

### 3.2 中立型(プロバイダ非依存の中間表現)

既存の中立部品(`StreamResult` / `ToolCallAccumulator` / `AiResponse`)を活かし、その手前を中立化する。

```rust
// src/ai/provider/types.rs (新設)
pub enum ChatMessage {
    System(String),
    User(String),
    Assistant { text: Option<String>, tool_calls: Vec<ToolCall> },
    ToolResult { tool_call_id: String, content: String },
}

/// 注: 連続する `ToolResult` は Anthropic 境界で 1 つの user メッセージの
/// `tool_result` content blocks として束ねる必要がある。`tool_use_id` は
/// `ToolCall.id`(=Anthropic 側の `tool_use.id` に対応)で識別する。
pub struct ToolCall { pub id: String, pub name: String, pub arguments: String }

pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,   // JSON Schema (OpenAI/Anthropic 共通の実体)
}

pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Option<Vec<ToolSpec>>,
    pub temperature: Option<f32>,        // Anthropic 実装側では無視(送信しない)
    pub max_tokens: Option<u32>,         // Anthropic では必須・Some を強制、OpenAI 互換系では None 可
}

pub struct ChatChunk {
    pub text_delta: Option<String>,
    pub tool_call_deltas: Vec<ToolCallDelta>,  // index キー付き断片
}
```

プロバイダ別の変換責務:

| 中立型 | OpenAI 互換側の変換 | Anthropic 側の変換 |
| --- | --- | --- |
| `ChatMessage::System` | messages 配列の system メッセージ | **トップレベル `system` パラメータ**に抽出 |
| `ChatMessage::Assistant.tool_calls` | `assistant.tool_calls[]` | content 内 `tool_use` ブロック |
| `ChatMessage::ToolResult` | `role:"tool"` メッセージ | **次の user メッセージの `tool_result` ブロック**(連続する ToolResult は 1 つの user メッセージに束ねる) |
| `ToolSpec.parameters` | `function.parameters` | `input_schema` |
| SSE → `ChatChunk` | `delta.content` / `delta.tool_calls[]` | `text_delta` / `input_json_delta`(index 対応) |

`ConversationState.messages` は `Vec<ChatCompletionRequestMessage>` → `Vec<ChatMessage>` に置換する。これが最大の書き換えだが、影響は `src/ai/` 内(agent.rs / conversation.rs / input_processing.rs / investigation.rs / pipe.rs)に閉じており、`shell/ai_router.rs` 以降は `AiResponse` のみに依存しているため無傷。

### 3.3 設定スキーマ

```toml
[ai]
# 使用するプロバイダ: "openai" | "anthropic" | "opencode-zen" | "opencode-go"
provider = "openai"          # 既定 "openai" で完全後方互換
model = "gpt-4o"             # プロバイダに応じたモデル名 (自由文字列、従来通り)
max_tokens = 8192            # 新設: ただしプロバイダ別既定を推奨 (後述)。Anthropic では必須。
max_rounds = 10
temperature = 0.5            # Anthropic 経路では送信されない (Claude 5 系は 400 拒否のため)
# ... 既存フィールドは変更なし

# 上級者向けオーバーライド (省略可、通常は不要)
# base_url = "https://..."        # OpenAI 互換の任意エンドポイント (Ollama/vLLM 等も視野)
# api_key_env = "MY_CUSTOM_KEY"   # キー環境変数名の上書き
```

**プロバイダ別 `max_tokens` 既定値**(Claude 5 系は thinking トークンが同バジェットを消費するため OpenAI より大きい値が必要):

| provider | 推奨 `max_tokens` 既定 | 理由 |
| --- | --- | --- |
| `openai` | `8192` | 出力トークン上限のみ。エージェントループで十分 |
| `anthropic` | `16384` | thinking 共有。8192 だと複雑なループで `stop_reason: max_tokens` が頻発 |
| `opencode-zen` | `8192` | OpenAI 互換、thinking 挙動は各モデル依存(基本 8192 で様子見) |
| `opencode-go` | `8192` | 同上 |

config.toml に `max_tokens` 未設定時は `provider` に応じて上記既定を適用する(`AiConfig::default_for_provider(provider)` 相当のヘルパで集中管理)。

- provider ごとの既定値:

| provider | キー環境変数(既定) | base_url(既定) | 推奨 model 例 |
| --- | --- | --- | --- |
| `openai` | `OPENAI_API_KEY` | async-openai 既定 | `gpt-4o` |
| `anthropic` | `ANTHROPIC_API_KEY` | `https://api.anthropic.com` | `claude-sonnet-5` |
| `opencode-zen` | `OPENCODE_API_KEY` | `https://opencode.ai/zen/v1` | Zen の `/models` で確認 |
| `opencode-go` | `OPENCODE_API_KEY` | `https://opencode.ai/zen/go/v1` | 同上(18 モデル) |

- **モデル名からの provider 自動判定はしない**(Zen 経由でも `claude-sonnet-5` を指定できる等、名前空間が衝突するため)。明示的な `provider` 設定を正とする。
- `provider` 未設定の既存 config.toml はそのまま動く(serde default で `openai`)。
- APIキーのマスキング: `storage/sanitizer.rs` は `_?API_KEY_?` 系の汎用正規表現のため、`ANTHROPIC_API_KEY` / `OPENCODE_API_KEY` も**追加改修なしで自動マスクされる**(調査で確認済み)。

### 3.4 reload / `source` の扱い

`update_config()` は現状フィールド上書きのみで client を再構築しない。本対応で:

- `reload_config`(src/shell/reload.rs)側で **provider / base_url / api_key_env / model / max_tokens の変更を検出したら `ai_client` を作り直す**経路を追加する(`JarvisAI::new()` を再実行)。
- キー未設定等で再構築に失敗した場合は従来同様 `ai_client = None` とし、その旨を summary に出力する。
- **`None → Some` 遷移対応**: 起動時にキー未設定で `ai_client = None` の場合、`source` で `export ANTHROPIC_API_KEY=...` 等を行った後に `ai_client` を新規構築すること。現状 `reload.rs:44-46` は `Some(ai)` 時しか `update_config` を呼ばないため、`Option<JarvisAI>` を剥がして再生成する経路を明示的に追加する。
- **プロバイダ変更時の後始末**: 旧 `JarvisAI` が保持する `ConversationState` はプロバイダ横断で形式互換(`ChatMessage` 中立型)なので、**会話履歴は引き継ぐ**。provider 切替時に履歴を捨てたい場合は `clear-history` 系の明示ビルトインを将来検討(本件スコープ外)。

---

## 4. 実装フェーズ

各フェーズは CLAUDE.md の Development Cycle(実装+テスト → 品質検証 → テスト追加 → `make check` → カバレッジ調査 → コミット)に従い、**フェーズ単位で develop にマージ可能な状態**を保つ。実装は Sonnet サブエージェント / Workflow に委譲し、レビューと統合判断を司令塔が行う。

### Phase 1: プロバイダ抽象化リファクタ(振る舞い不変)

**ゴール**: async-openai への直接依存を `AiBackend::OpenAiCompat` の内側に封じ込める。外部挙動は一切変えない。

1. `src/ai/provider/` ディレクトリ新設: `mod.rs`(AiBackend enum)、`types.rs`(ChatMessage/ToolSpec/ChatRequest/ChatChunk)、`openai_compat.rs`(既存ロジック移設)
2. `ConversationState.messages` を `Vec<ChatMessage>` に置換。`agent.rs` / `conversation.rs` / `input_processing.rs` / `investigation.rs` / `pipe.rs` の push 処理を中立型に書き換え
3. `tools/definitions.rs` の `build_tools()` 戻り値を `Vec<ToolSpec>` に変更(JSON Schema 本体は既存のまま)
4. `stream.rs` を「SSE 消費 + SIGINT + スピナー表示」の共通部と「チャンク→ChatChunk 変換」のプロバイダ部に分離。`process_stream` / `process_ai_pipe_stream` の重複を `tools: Option<...>` の有無に統合
5. `core.rs` の `JarvisAI` を `backend: AiBackend` 保持に変更。`JarvisAI::new(&AiConfig)` 内で provider 分岐(この時点では OpenAiCompat のみ)
6. テスト:
   - 中立型⇔async-openai 型の変換ユニットテスト
   - **ラウンドトリップテスト**: 既存の全 OpenAI リクエスト(典型的な NL 会話 / tool_call 往復 / system プロンプト + 履歴)を中立型に変換 → OpenAI 互換形式に再変換し、**JSON 出力の byte-identical 性を assert**(「外部挙動を変えない」を保証)
   - 既存テストの型追従
   - `ApiKeyGuard`(ai/client/tests.rs)の流用

**完了条件**: `make check` 全パス、ラウンドトリップテスト全通過、既存の全 AI フロー(NL 会話 / エラー調査 / `| ai` / `> ai` / ツールコール)が手動確認で従来通り動く。**カバレッジ調査を実施し、未テスト分岐があれば本フェーズ内で追加する**(CLAUDE.md Development Cycle ステップ 5 をフェーズ毎に遵守)。

### Phase 2: Anthropic ネイティブサポート(高リスク→先行)

**ゴール**: `provider = "anthropic"` で Claude モデルが全フロー動作する。

**Phase 3(Zen)より先に着手する理由**: ①新規依存 `reqwest` + 新規 SSE パーサ + 中立型の構造的ギャップ発覚リスクが Zen より遥かに高い ②最も難しい方を先に消化し、後半フェーズを confidence-building にする ③もし中立型に設計欠陥が見つかった場合、Zen 実装前に対処できる。

1. `Cargo.toml` に `reqwest`(features: json, stream)追加(新規依存はこれのみ。SSE は `bytes_stream()` の手動パースで賄い、eventsource 系クレートは追加しない)。**`Client::builder().read_timeout(Some(Duration::from_secs(120)))` を必須設定**(thinking 中の keepalive でデフォルトタイムアウトが発火する)
2. `src/ai/provider/anthropic.rs` 新設:
   - リクエスト変換: `ChatRequest` → Messages API JSON(system 抽出 / **連続 `ToolResult` を 1 user メッセージの `tool_result` blocks に束ねる** / `input_schema` 変換 / `max_tokens` 必須化 / **temperature 非送信**)
   - ヘッダ: `x-api-key` + `anthropic-version: 2023-06-01` + `content-type`
   - SSE パーサ(下記エッジケースを網羅):
     - `event:` / `data:` 行フレーミング(**複数行 data を `\n` 連結で防御的に対応**)
     - **4-byte UTF-8 文字が `bytes_stream()` の chunk 境界を跨ぐケース**(`String::from_utf8_lossy` は禁止、バッファリング必須)
     - **`ping` が `content_block_delta` 途中に挟まるケース**(`ToolCallAccumulator` 状態を絶対にリセットしない)
     - `content_block_start`(type==tool_use で ToolCall 開始検知) / `content_block_delta`(text_delta → text、input_json_delta → 同 index の引数断片) / `message_delta` / `message_stop` / `ping` / `error` を `ChatChunk` にマップ
   - `stop_reason` 処理: `refusal` は content 空でもパニックしない防御 + ユーザー向け説明文言、`max_tokens` は「max_tokens を増やして(`[ai].max_tokens` を 16384+ に)」のヒント表示、`pause_turn` / `model_context_window_exceeded` も個別メッセージ
   - エラーボディ(`{"type":"error",...}`)の型付きパースと `request_id` のログ出力
3. thinking 対応(最小限): Claude 5 系はデフォルト ON。`thinking_delta` は**表示せず読み捨て**、`text_delta` のみ蓄積する(将来 `--debug` 時のみ表示する拡張余地をコメントで残す)。`max_tokens` は §3.3 のプロバイダ別既定で **16384** を適用
4. テスト:
   - 変換層のユニットテスト(system 抽出 / **tool_result 束ね** / input_schema 変換 / **temperature が JSON に含まれないこと**)
   - SSE パーサのフィクスチャテスト(text のみ / tool_use 混在 / **ping 挟み込み** / **UTF-8 境界跨ぎ** / multi-line data / refusal / エラーイベントの各シーケンス)
   - **CI 用 mock サーバ統合テスト**(`tests/integration/anthropic_mock.rs` + `tests/fixtures/anthropic_sse/` に記録済み SSE フィクスチャを再生する最小 `axum` サーバ)。フルエージェントループ(テキスト → tool_use → tool_result → テキスト)を mock 経由で実行し、`make check` 内に組込む
5. 実機スモークテスト(任意): `claude-sonnet-5` で全フロー確認。**CI ゲートには含めず、レビュー時の任意確認**とする(mock テストで代用)

**完了条件**: ①mock サーバ統合テストが `make check` パス ②Claude でエージェントループ(ツールコール往復含む)・`| ai`・エラー調査が mock 経由で動作 ③`make check` 全パス。**カバレッジ調査を本フェーズ内で実施**。

### Phase 3: OpenCode Zen / Go サポート(低リスク→後段)

**ゴール**: `provider = "opencode-zen"` / `"opencode-go"` で Zen/Go のモデルが使える。

**Phase 2(Anthropic)の後段に配置する理由**: base_url 差し替え + ヘッダ注入のみで済み、新規依存も新規パーサも不要。Anthropic で中立型が固まった後に着手することで、Zen 起因の設計揺れを防ぐ。

1. `AiConfig` に `provider` / `max_tokens` / `base_url` / `api_key_env` を追加(serde default で後方互換)
2. `OpenAiCompatBackend` に base_url / キー環境変数 / 追加ヘッダの設定注入を実装。`OPENCODE_API_KEY` 解決
3. 識別ヘッダ対応: async-openai のカスタム reqwest クライアント差し替え(`with_http_client`)で `User-Agent: jarvish/<ver>` 等を付与できる構造にする(§7 リスク 1 の保険。x-opencode-* の偽装はしない)
4. **実機スモークテスト**(手動・要 API キー): ①認証ヘッダ形式の確認 ②ストリーミング応答 ③`tools` 付きリクエストを 1 モデル以上で実地検証 ④429 時のエラーメッセージ確認 → 結果を本ドキュメント §7 に追記
5. `input.rs:392` のエラーメッセージ(`requires OPENAI_API_KEY`)をプロバイダ対応の文言に一般化
6. テスト: provider 別キー解決・base_url 構築のユニットテスト、config パース(新フィールド + 未設定時の後方互換)テスト

**完了条件**: Zen の動作確認済みモデルで会話・ツールコールが通る。`make check` 全パス。**カバレッジ調査を本フェーズ内で実施**。

### Phase 4: reload・ドキュメント・仕上げ

1. `reload_config` にプロバイダ変更検出 → `ai_client` 再構築を実装。`source` の summary 出力(reload.rs:88-146)に新フィールド(provider / max_tokens / base_url / api_key_env)を追加
2. ドキュメント一括更新(§6 チェックリスト)
3. `.env.example` に `ANTHROPIC_API_KEY` / `OPENCODE_API_KEY` 追記
4. カバレッジ最終調査 → 不足パスのテスト追加
5. develop マージ → `v1.16.0` タグ → リリースフロー(GitHub Release は CI 自動生成、Homebrew formula 手動更新を忘れない)

---

## 5. 変更ファイル一覧(要約)

| 区分 | ファイル | 内容 |
| --- | --- | --- |
| 新設 | `src/ai/provider/mod.rs` | `AiBackend` enum とディスパッチ |
| 新設 | `src/ai/provider/types.rs` | `ChatMessage` / `ToolSpec` / `ChatRequest` / `ChatChunk` |
| 新設 | `src/ai/provider/openai_compat.rs` | async-openai ラッパ(OpenAI/Zen/Go 共用) |
| 新設 | `src/ai/provider/anthropic.rs` | Messages API クライアント + SSE パーサ |
| 新設 | `src/ai/provider/tests/roundtrip.rs` | **中立型⇔OpenAI JSON のラウンドトリップテスト** |
| 新設 | `src/ai/provider/anthropic/tests.rs` | 変換層ユニットテスト + SSE パーサフィクスチャ |
| 新設 | `tests/integration/anthropic_mock.rs` | **CI 用 mock サーバ統合テスト**(axum ベース) |
| 新設 | `tests/fixtures/anthropic_sse/*.txt` | 記録済み SSE フィクスチャ(text のみ / tool_use 混在 / ping 挟み / UTF-8 境界跨ぎ / refusal 等) |
| 改修 | `src/ai/client/core.rs` | `JarvisAI` の backend 化、provider 別キー解決、**各プロバイダ用の placeholder チェック**(`core.rs:46` の `"your_openai_api_key"` ガードを provider 別に複製) |
| 改修 | `src/ai/client/agent.rs` | 中立型でのループ再構成 |
| 改修 | `src/ai/client/{conversation,input_processing,investigation,pipe}.rs` | メッセージ構築の中立化 |
| 改修 | `src/ai/client/config_update.rs` | 新フィールド反映 |
| 改修 | `src/ai/stream.rs` | 共通 SSE 消費部とプロバイダ変換部の分離(マルチ choice ループ保持) |
| 改修 | `src/ai/types.rs` | `ConversationState.messages` の中立化 |
| 改修 | `src/ai/tools/{definitions,call}.rs` | `ToolSpec` 化 / 入出力型の中立化 |
| 改修 | `src/config/{types,defaults,mod,tests}.rs` | `provider` 等の新フィールド + テンプレート×2 + テスト |
| 改修 | `src/shell/reload.rs` | client 再構築経路(`None → Some` 遷移対応)+ summary 出力 |
| 改修 | `src/shell/input.rs` | エラーメッセージ一般化(:392) |
| 改修 | `Cargo.toml` | `reqwest` 追加(read_timeout 設定を含む) |

---

## 6. ドキュメント更新チェックリスト(CLAUDE.md ルール準拠)

config 値の追加・変更に伴い、以下を**実装と同一 PR で**更新する:

- [ ] `README.md` — `[ai]` セクション表(159-190 行付近)、`OPENAI_API_KEY` 記載(149 行)、Architecture 図の「AI Brain (OpenAI API / Tools)」文言(345 行)
- [ ] `docs/README_JA.md` — 同上の日本語版(148 / 158-188 / 344 行付近)
- [ ] `src/config/defaults.rs` — TEMPLATE 定数(コメント付きテンプレート)
- [ ] `src/config/mod.rs` — モジュール doc コメント内の TOML 例
- [ ] `src/config/types.rs` — フィールド doc コメント
- [ ] `src/shell/reload.rs` — `source` ビルトインの summary 出力
- [ ] `.env.example` — 新キー環境変数
- [ ] `docs/OVERVIEW.md` — 技術スタック節の「AI Client: async-openai」記述の更新
- [ ] `docs/CHANGELOG.md` — v1.16.0 エントリ
- [ ] `docs/MIGRATION_v1.16.md`(新設) — 既存ユーザー向け移行ガイド:`provider` フィールド新設 / `OPENAI_API_KEY` 既存ユーザーは無変更で動作 / 新たに `ANTHROPIC_API_KEY` / `OPENCODE_API_KEY` を使う手順
- [ ] 起動時マイグレーション通知(任意) — 既存 `config.toml` を読み込んだ v1.16.0 初回起動時に「New: [ai].provider で Anthropic / OpenCode Zen / Go に切替可能」を 1 度だけ stderr に表示

---

## 7. リスクと未確定事項(実装前・実装中に実機検証が必要)

| # | 項目 | 内容 | 対応 |
| --- | --- | --- | --- |
| 1 | Zen の認証ヘッダ | `Authorization: Bearer` は公式一次ソース未確認(状況証拠のみ) | Phase 3 冒頭のスモークテストで最初に確認 |
| 2 | Zen のツールコール対応 | モデル別対応可否の二次情報が検証で反証された。対応表は信頼できない | 使う予定のモデルで `tools` 付きリクエストを実地検証。README には動作確認済みモデルのみ記載 |
| 3 | Zen のエンドポイント出し分け | 公式は gpt-5 系に `/responses` を案内。`/chat/completions` で全モデルが通る保証なし | 初期サポートは `/chat/completions` で動くモデルに限定。`/responses` 対応は将来課題として明記 |
| 4 | Zen の匿名レート制限 | 識別ヘッダ無しだと厳格な 429 が課されるというコミュニティ報告(未実測) | User-Agent 付与 + 実測。問題が出たら追加ヘッダを検討 |
| 5 | Zen のストリーミング形式 | 公式未記載(標準 OpenAI SSE と推定) | スモークテストで確認。非標準なら async-openai がエラーを返すので検知可能 |
| 6 | Claude 5 の thinking と max_tokens | thinking デフォルト ON で思考トークンが max_tokens を消費 → タイトな値だと応答が切れる | **§3.3 のプロバイダ別既定で Anthropic は 16384**。`stop_reason:max_tokens` 検出時にヒント表示 |
| 7 | Claude の refusal | HTTP 200 + content 空/部分で返る | stop_reason 先行チェックを Phase 2 の必須実装項目に含めた |
| 8 | temperature 拒否 | Claude 5 / 4.6 系は temperature 送信で 400 | Anthropic 変換層で送信しない(実装済み方針) |
| 9 | モデル名の陳腐化 | 本ドキュメントのモデル ID は 2026-08-08 時点 | 実装時に `GET /v1/models`(両プロバイダ)で最新を確認 |
| 10 | **SSE パーサの UTF-8 chunk 境界跨ぎ** | 4-byte 文字(絵文字等)が `bytes_stream()` の任意境界を跨ぐ可能性 | `String::from_utf8_lossy` 禁止。`&[u8]` バッファで `from_utf8` を継続し incomplete 末尾を次 chunk に持ち越す。Phase 2 で実装 + フィクスチャテスト必須 |
| 11 | **SSE `ping` の割り込み** | `ping` イベントが `content_block_delta` 途中に到着しうる | `ToolCallAccumulator` 状態は `ping` で絶対にリセットしない。Phase 2 フィクスチャテスト |
| 12 | **SSE 複数行 data** | SSE 仕様では `data:` が複数行連結可(Anthropic は通常単行) | 複数行を `\n` 連結で防御的に対応。Phase 2 フィクスチャテスト |
| 13 | **reqwest デフォルトタイムアウト** | thinking 中の keepalive で 30 秒以上の無通信が発生しうる | Phase 2 ステップ 1 で `Client::builder().read_timeout(Some(Duration::from_secs(120)))` を必須設定 |
| 14 | **reload の `None → Some` 遷移** | 起動時キー未設定 → `source` で export したケースで `ai_client` が再構築されない | Phase 4 実装時に `Option<JarvisAI>` を剥がして再生成する経路を明示追加(§3.4 参照) |
| 15 | **Zen auth ヘッダ形式の divergence** | `Bearer` ではなく `x-api-key` 等の場合、`async-openai` の `OpenAIConfig` 単独では対応不可 | 該当時は `OpenAiCompatBackend` 内でカスタム reqwest クライアントでヘッダ注入。enum バリアント追加は最終手段 |
| 16 | **手動スモークテストの CI ゲート化** | 「実機で確認」は CI で再現できない | Phase 2 で axum mock サーバ統合テストを導入し、`make check` 内で完結。実機確認は任意レビューゲートに格下げ |

---

## 8. スコープ外(将来課題)

- Anthropic の prompt caching(`cache_control`)対応 — SYSTEM_PROMPT が安定プレフィックスなので効果は大きいが、初期実装では見送り
- thinking 内容の表示(`--debug` 連動)/ effort パラメータ(`output_config.effort`)の設定公開
- OpenCode Zen `/responses` エンドポイント対応(gpt-5 系フル対応)
- 429 / 5xx の自動リトライ(retry-after 尊重、指数バックオフ)
- Ollama / vLLM 等ローカル OpenAI 互換サーバの正式サポート(base_url 上書きで事実上動く見込みだが、動作保証はしない)
- 会話履歴(ConversationState)の Black Box 永続化 — 中立型 `ChatMessage` は serde 可能な設計にしておき、将来の serialize に備える
- **`--list-models` / `models` ビルトイン** — モデル名 typo 時の UX 改善。両プロバイダの `GET /v1/models` を叩いて一覧表示。プロバイダ別有効モデルのホワイトリスト保守コストを下げる
- **`--features=anthropic` cargo feature フラグ** — `reqwest` の依存・コンパイル時間・バイナリサイズ影響を OpenAI 専用ユーザーから隠す。オプトイン化する場合 `provider = "anthropic"` 設定時に `compile_error!` で誘導する
- **コスト概算表示** — `--debug` モードで `usage.prompt_tokens` / `usage.completion_tokens` をログ出力。`anthropic` の thinking トークン分も個別表示
- **プロバイダ変更時の会話履歴クリア** — `clear-history` 系の明示ビルトイン。現状は §3.4 の通り引き継ぐ方針

---

## 9. 参考資料

### Anthropic(全クレーム一次ソースで confirmed)
- Messages API: <https://platform.claude.com/docs/en/api/messages>
- Streaming: <https://platform.claude.com/docs/en/build-with-claude/streaming>
- Tool use: <https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview>
- OpenAI SDK 互換(非採用の根拠): <https://platform.claude.com/docs/en/api/openai-sdk>
- Rate limits: <https://platform.claude.com/docs/en/api/rate-limits>
- Models: <https://platform.claude.com/docs/en/about-claude/models/overview>

### OpenCode
- Zen: <https://opencode.ai/docs/zen/>
- Go: <https://opencode.ai/docs/go/>
- モデル一覧(ライブ): <https://opencode.ai/zen/v1/models>
- 識別ヘッダ問題の報告: <https://github.com/earendil-works/pi/issues/2824>

### Rust クレート
- async-openai(base_url 差し替え): <https://docs.rs/async-openai/latest/async_openai/config/struct.OpenAIConfig.html>
- Anthropic 系コミュニティクレート調査: anthropic-ai-sdk / async-anthropic / misanthropy / clust(いずれも非採用、reqwest 手書きを採用)

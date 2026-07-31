//! 遅い `CompletionProvider` を非ブロッキング化するデコレータ
//! [`AsyncCacheProvider`]
//!
//! # 背景
//! `reedline::Completer::complete` は完全同期・UI スレッド実行のトレイトで、
//! `mod.rs` の provider チェーン（`find_map`）は `None` を返したプロバイダの
//! タイムアウト予算を**加算**してしまう。carapace（設定値、既定 500ms）と
//! zsh ブリッジ（[`super::zsh_bridge::WARM_MIN_TIMEOUT_MS`] フロア、2000ms）が
//! 両方タイムアウトすると、最悪 UI が 2.5 秒近く固まる（詳細は `mod.rs`
//! モジュールドキュメント参照）。
//!
//! 実測では成功パス（zsh デーモン温存 55〜60ms、carapace 温存 110ms 程度）
//! は十分速く、コストが跳ねるのはタイムアウトを踏む経路だけ。したがって
//! タイムアウト自体を短縮する（`MIN_TIMEOUT_MS` / `WARM_MIN_TIMEOUT_MS` の
//! フロアは death-loop 対策として意図的に設定されており変更禁止）のではなく、
//! **重い処理を UI スレッドの外へ逃がす**のがこのモジュールの狙い。
//!
//! # 動作
//! `AsyncCacheProvider` は内部プロバイダを `Arc<dyn CompletionProvider + Send
//! + Sync>` として保持するデコレータ。`provide()` は:
//!
//! 1. `ctx` からキャッシュキーを計算する（[`cache_key`]）。
//! 2. キャッシュヒット（かつ TTL 内）→ その場で `Option<Vec<Candidate>>` を
//!    返す。`Some(vec![])`（担当したが候補なし）もそのまま返す —
//!    `CompletionProvider` の tri-state 契約（`None` = 対象外, `Some(vec![])`
//!    = 担当・候補なし）をキャッシュ越しでも保つ。
//! 3. キャッシュミス → 同じキーの fetch が in-flight でなければバックグラウンド
//!    スレッドを spawn して内部プロバイダの `provide()` を呼び、結果を
//!    キャッシュへ格納する。**呼び出し元へは即座に `None` を返す**
//!    （= 「このプロバイダは今回担当外」としてチェーンを `PathProvider` まで
//!    フォールスルーさせる。UI は一切ブロックしない）。
//! 4. バックグラウンド fetch が完了すると結果がキャッシュに載るので、
//!    同じキーへの次の Tab 押下は即座にキャッシュから返る。
//!
//! # キャッシュキー
//! `ctx.spans()`（現在のパイプラインセグメントの単語列）と
//! `std::env::current_dir()` の両方を含める。補完結果はカレントディレクトリに
//! 依存するため（`zsh_daemon.rs` の `completion_follows_jarvish_cwd_after_chdir`
//! が示す既知の落とし穴と同種）、cwd を落とすと別ディレクトリの古い結果を
//! 誤って返しかねない。
//!
//! # 有界性
//! - **エントリ数**: [`MAX_CACHE_ENTRIES`] を超えたら全消去する
//!   （clear-all 方式）。LRU 等の精密な追い出しは実装せず、
//!   「稀に起きる全消去でキャッシュウォームアップが少し遅れる」程度の
//!   コストは Tab 補完という UX の性質上ほぼ無視できるため、実装の単純さを
//!   優先した。
//! - **in-flight 重複排除**: 同じキーへの fetch が既に進行中なら新規 spawn
//!   しない（Tab 連打でスレッドの山を作らないため）。worker 終了時の後片付け
//!   （in-flight 集合からのキー除去とワーカーカウントの減算）は
//!   [`WorkerGuard`] の `Drop` に委ねており、内部プロバイダが panic して
//!   スレッドが巻き戻った場合でも必ず実行される（取りこぼすと外部補完が
//!   恒久的に沈黙する — [`WorkerGuard`] のドキュメント参照）。
//! - **同時ワーカー数**: [`MAX_CONCURRENT_WORKERS`] に達していたら spawn
//!   せず `None` を返す（Tab 連打によるスレッド砲対策）。
//! - **TTL**: [`DEFAULT_TTL`] を過ぎたエントリはミス扱いにして再フェッチする
//!   （ファイルシステム/git の変化が最終的に反映されるように）。テストでは
//!   実時間 5 秒を待たずに済むよう TTL を注入可能にしている
//!   ([`AsyncCacheProvider::with_ttl`])。
//!
//! # ロック汚染への対応
//! このプロバイダはユーザーのシェルの Tab 補完パスに乗るため、ロックが
//! poisoned でも**絶対に panic しない**（他プロバイダと同じ方針）。
//! 汚染を検出したら安全側に倒して `None` を返す。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::context::CompletionContext;
use super::provider::{Candidate, CompletionProvider};

/// キャッシュに保持する最大エントリ数。
///
/// 256 は「補完対象になりうる (コマンド, 引数プレフィックス, cwd) の
/// 組み合わせ」を 1 セッション中それなりに広くカバーしつつ、無制限成長を
/// 防ぐための経験的な上限。超過時は clear-all するため、この値を大きくし
/// すぎるとメモリを、小さくしすぎるとキャッシュヒット率を犠牲にする
/// トレードオフになる。
const MAX_CACHE_ENTRIES: usize = 256;

/// キャッシュエントリの有効期間。
///
/// この時間を過ぎたエントリはミス扱いになり再フェッチされる。carapace/zsh
/// 補完はファイルシステムや git の状態に依存するため、無期限キャッシュは
/// 「ブランチを切り替えたのに古い候補が出続ける」といった事故を招く。5 秒は
/// 「同じ Tab 連打・同じプレフィックスの試行錯誤には効く」程度の短さと、
/// 「体感できるほど古い結果を返さない」ことのバランスを取った経験値。
const DEFAULT_TTL: Duration = Duration::from_secs(5);

/// 同時に走らせるバックグラウンド fetch スレッドの最大数。
///
/// Tab を連打しても無制限にスレッドを spawn しないための上限。4 は
/// 「タイプ中に複数の異なるプレフィックスを試している」程度の並行度を
/// 吸収しつつ、スレッド砲を防ぐための経験値。上限に達した状態での
/// `provide()` は spawn せずそのまま `None` を返す（次の Tab で再試行される）。
const MAX_CONCURRENT_WORKERS: usize = 4;

/// キャッシュ 1 件分。`fetched_at` が [`DEFAULT_TTL`]（または注入された TTL）
/// を超えたら期限切れとして扱う。
#[derive(Clone)]
struct CacheEntry {
    value: Option<Vec<Candidate>>,
    fetched_at: Instant,
}

/// 内部プロバイダの共有状態。バックグラウンドスレッドと `provide()` 呼び出し
/// 元の両方から触るため、`Arc` で包んで共有する。
struct Shared {
    inner: Arc<dyn CompletionProvider + Send + Sync>,
    cache: Mutex<HashMap<String, CacheEntry>>,
    in_flight: Mutex<HashSet<String>>,
    active_workers: Mutex<usize>,
    ttl: Duration,
}

/// 遅い `CompletionProvider`（carapace / zsh ブリッジ想定）をラップし、
/// キャッシュ + バックグラウンドフェッチで非ブロッキング化するデコレータ。
///
/// `provide()` は常に即座に返る（ブロッキング I/O をこのメソッド内で行わない）。
/// キャッシュミス時は `None` を返してチェーンをフォールスルーさせつつ、
/// 裏でバックグラウンドスレッドが結果を取得してキャッシュへ書き込む。
pub(super) struct AsyncCacheProvider {
    shared: Arc<Shared>,
}

impl AsyncCacheProvider {
    /// 既定 TTL（[`DEFAULT_TTL`]）でラップする。
    pub(super) fn new(inner: Arc<dyn CompletionProvider + Send + Sync>) -> Self {
        Self::with_ttl(inner, DEFAULT_TTL)
    }

    /// TTL を明示指定してラップする（テストで実時間 5 秒を待たずに
    /// 期限切れ挙動を検証するために公開している）。
    pub(super) fn with_ttl(
        inner: Arc<dyn CompletionProvider + Send + Sync>,
        ttl: Duration,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                inner,
                cache: Mutex::new(HashMap::new()),
                in_flight: Mutex::new(HashSet::new()),
                active_workers: Mutex::new(0),
                ttl,
            }),
        }
    }
}

impl CompletionProvider for AsyncCacheProvider {
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        let key = cache_key(ctx);

        if let Some(entry) = self.shared.get_fresh(&key) {
            return entry;
        }

        self.shared.spawn_fetch_if_needed(key, ctx.clone());

        // キャッシュミス（または期限切れ）: このプロバイダは今回「対象外」
        // として即座にフォールスルーさせる。UI は一切ブロックしない。
        None
    }
}

impl Shared {
    /// `key` に対応する新鮮な（TTL 内の）キャッシュエントリがあれば返す。
    /// 期限切れエントリはミス扱い（`None`）にする。
    ///
    /// ロックが poisoned な場合は安全側に倒してミス扱いにする（panic しない）。
    fn get_fresh(&self, key: &str) -> Option<Option<Vec<Candidate>>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(key)?;
        if entry.fetched_at.elapsed() > self.ttl {
            return None;
        }
        Some(entry.value.clone())
    }

    /// `key` の fetch が in-flight でなく、同時ワーカー数の上限にも達して
    /// いなければ、バックグラウンドスレッドを spawn して内部プロバイダを
    /// 呼び出す。呼び出し元スレッドを一切ブロックしない。
    fn spawn_fetch_if_needed(self: &Arc<Self>, key: String, ctx: CompletionContext) {
        {
            let Ok(mut in_flight) = self.in_flight.lock() else {
                return;
            };
            if in_flight.contains(&key) {
                return;
            }

            let Ok(mut active) = self.active_workers.lock() else {
                return;
            };
            if *active >= MAX_CONCURRENT_WORKERS {
                return;
            }
            *active += 1;
            in_flight.insert(key.clone());
        }

        let shared = Arc::clone(self);
        thread::spawn(move || {
            // 後片付け（in-flight 集合からの除去とワーカーカウントの減算）は
            // RAII ガードに委ねる。`shared.inner.provide()` が panic して
            // スレッドが巻き戻っても `Drop` は必ず走るため、後片付けの
            // 取りこぼしが起きない（下記「なぜガードが必要か」参照）。
            let _guard = WorkerGuard {
                shared: Arc::clone(&shared),
                key: key.clone(),
            };

            let result = shared.inner.provide(&ctx);

            if let Ok(mut cache) = shared.cache.lock() {
                if cache.len() >= MAX_CACHE_ENTRIES && !cache.contains_key(&key) {
                    // 有界性を保つための単純な全消去（モジュールドキュメント
                    // 「有界性」節参照）。LRU 等の精密な追い出しより実装の
                    // 単純さを優先した。
                    cache.clear();
                }
                cache.insert(
                    key.clone(),
                    CacheEntry {
                        value: result,
                        fetched_at: Instant::now(),
                    },
                );
            }

            // 後片付けは `_guard` の `Drop` が行う（このスコープを抜けた
            // 時点で必ず走る）。
        });
    }
}

/// バックグラウンドワーカーの後片付けを `Drop` で保証する RAII ガード。
///
/// # なぜガードが必要か
/// 後片付け（`in_flight` からのキー除去と `active_workers` の減算）を
/// ワーカースレッド本体の末尾に直書きすると、`inner.provide()` が panic した
/// 場合にその行へ到達せずスレッドが巻き戻ってしまう。すると:
///
/// - `active_workers` が増えたまま戻らない → panic が
///   [`MAX_CONCURRENT_WORKERS`] 回積み重なると、以後このプロバイダは
///   **二度とワーカーを spawn できなくなる**（常に上限扱い）。
/// - `in_flight` にキーが残り続ける → そのキーは**恒久的に**「フェッチ中」と
///   見なされ、再フェッチされない。
///
/// いずれもユーザーには何のエラーも見えないまま「そのシェルセッションの間
/// ずっと外部補完が沈黙する」という形で現れるため、原因の特定が極めて
/// 困難な種類の不具合になる。`Drop` は panic による巻き戻し時にも実行される
/// ので、ガードにしておけば内部プロバイダが将来 panic しうる実装に変わっても
/// この不変条件が壊れない。
///
/// ロック取得に失敗した場合（poisoned）は諦める — このモジュールの
/// 「Tab 補完パスでは絶対に panic しない」方針に従う（`Drop` 内での panic は
/// 特に危険で、巻き戻し中に起きれば abort に直結する）。
struct WorkerGuard {
    shared: Arc<Shared>,
    key: String,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if let Ok(mut in_flight) = self.shared.in_flight.lock() {
            in_flight.remove(&self.key);
        }
        if let Ok(mut active) = self.shared.active_workers.lock() {
            *active = active.saturating_sub(1);
        }
    }
}

/// `ctx` からキャッシュキーを計算する。
///
/// `ctx.spans()`（現在セグメントの単語列）と現在の作業ディレクトリの両方を
/// 含める。cwd を含めないと、別ディレクトリで得た結果を誤って返しかねない
/// （`zsh_daemon.rs` の cwd 追従バグと同種の落とし穴）。
///
/// `std::env::current_dir()` が失敗した場合（削除済みディレクトリ等）は
/// キーに固定の目印を混ぜる。失敗時に cwd 部分が常に同じ値になるだけで、
/// 「別 cwd のキーと衝突しない」という不変条件は保たれる（あくまで
/// 「取得できなかった」ことを表すセンチネル）。センチネルには実パスに
/// 出現しえない `\0` を先頭に付けているため、実在するディレクトリ名
/// （`<unknown-cwd>` というディレクトリを作ることは可能）とも衝突しない。
///
/// # 区切りではなく長さプレフィックスを使う理由
/// 素朴に「区切り文字で連結する」方式は、span 自身がその区切り文字を
/// 含みうる限り曖昧さが残る。実際 `context.rs` の `lex_lenient` は NUL を
/// 一切除去しないため、`\0` を含む入力（貼り付け等）はそのまま span の値に
/// なる。区切りを `\0` にすると、例えば `["a\0"]` と `["a", ""]` はどちらも
/// `"a\0\0"` に潰れて**別の入力が同じキーになる**（= 片方の補完結果が
/// もう片方に対して返る）。
///
/// そこで各要素を「バイト長 + `:` + 中身」の形で書き出す。長さが先に
/// 確定するため中身に何が含まれていても境界が一意に定まり、区切り文字の
/// エスケープ漏れという種類のバグが原理的に発生しない。
fn cache_key(ctx: &CompletionContext) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "\0<unknown-cwd>".to_string());

    let mut key = String::with_capacity(cwd.len() + 16);
    push_length_prefixed(&mut key, &cwd);
    for span in ctx.spans() {
        push_length_prefixed(&mut key, &span);
    }
    key
}

/// `part` を「バイト長 + `:` + 中身」の形で `key` に追記する
/// （[`cache_key`] の曖昧さ回避エンコーディング — 理由はそちらのドキュメント）。
fn push_length_prefixed(key: &mut String, part: &str) {
    key.push_str(&part.len().to_string());
    key.push(':');
    key.push_str(part);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::super::context::extract_context;

    /// テスト用フェイク内部プロバイダ。
    ///
    /// - 呼び出し回数を `AtomicUsize` に記録する。
    /// - `delay` が設定されていればその時間だけスリープしてからレスポンスを返す
    ///   （バックグラウンド実行の検証、"呼ばれた後すぐ完了しない" 状況の再現用）。
    /// - `response` は `provide()` の戻り値として固定で返す。
    struct FakeInner {
        response: Option<Vec<Candidate>>,
        call_count: Arc<AtomicUsize>,
        delay: Duration,
    }

    impl CompletionProvider for FakeInner {
        fn provide(&self, _ctx: &CompletionContext) -> Option<Vec<Candidate>> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            if !self.delay.is_zero() {
                thread::sleep(self.delay);
            }
            self.response.clone()
        }
    }

    fn candidate(value: &str) -> Candidate {
        Candidate {
            value: value.to_string(),
            description: None,
            append_whitespace: true,
        }
    }

    fn ctx_for(line: &str) -> CompletionContext {
        extract_context(line, line.len())
    }

    /// `deadline` まで `poll` が `Some` を返すのをポーリングする。
    /// 固定 `sleep` 1 回だけに頼らず、達成できたら即座に打ち切る。
    fn poll_until<T>(deadline: Duration, mut poll: impl FnMut() -> Option<T>) -> Option<T> {
        let start = Instant::now();
        loop {
            if let Some(v) = poll() {
                return Some(v);
            }
            if start.elapsed() > deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    // ── 1. コールドミスで None を即座に返す（ブロックしない） ──

    #[test]
    #[serial]
    fn cold_miss_returns_none_without_blocking() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(FakeInner {
            response: Some(vec![candidate("foo")]),
            call_count: Arc::clone(&call_count),
            delay: Duration::from_millis(200),
        });
        let provider = AsyncCacheProvider::new(inner);

        let ctx = ctx_for("git checkout ma");
        let start = Instant::now();
        let result = provider.provide(&ctx);
        let elapsed = start.elapsed();

        assert_eq!(result, None, "cold miss should return None immediately");
        assert!(
            elapsed < Duration::from_millis(100),
            "provide() must not block on the inner provider's delay: {elapsed:?}"
        );
    }

    // ── 2. ミスはバックグラウンドで内部プロバイダをちょうど1回呼ぶ ──

    #[test]
    #[serial]
    fn cold_miss_triggers_exactly_one_background_call() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(FakeInner {
            response: Some(vec![candidate("foo")]),
            call_count: Arc::clone(&call_count),
            delay: Duration::from_millis(20),
        });
        let provider = AsyncCacheProvider::new(inner);

        let ctx = ctx_for("git checkout ma");
        provider.provide(&ctx);

        let seen = poll_until(Duration::from_secs(2), || {
            let n = call_count.load(Ordering::SeqCst);
            (n >= 1).then_some(n)
        });

        assert_eq!(seen, Some(1), "exactly one background call should occur");
    }

    // ── 3. バックグラウンド完了後、次の provide はキャッシュから返る ──

    #[test]
    #[serial]
    fn after_background_fetch_completes_next_provide_hits_cache() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(FakeInner {
            response: Some(vec![candidate("foo")]),
            call_count: Arc::clone(&call_count),
            delay: Duration::from_millis(20),
        });
        let provider = AsyncCacheProvider::new(inner);

        let ctx = ctx_for("git checkout ma");
        assert_eq!(provider.provide(&ctx), None);

        let result = poll_until(Duration::from_secs(2), || {
            let r = provider.provide(&ctx);
            r.is_some().then_some(r)
        })
        .flatten();

        assert_eq!(
            result,
            Some(vec![candidate("foo")]),
            "second provide() after background fetch completes should hit the cache"
        );
    }

    // ── 4. キャッシュされた Some(vec![]) は None にならない (tri-state 契約) ──

    #[test]
    #[serial]
    fn cached_handled_empty_stays_some_empty_not_none() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(FakeInner {
            response: Some(Vec::new()),
            call_count: Arc::clone(&call_count),
            delay: Duration::from_millis(10),
        });
        let provider = AsyncCacheProvider::new(inner);

        let ctx = ctx_for("git checkout ma");
        assert_eq!(provider.provide(&ctx), None, "cold miss is None");

        let result = poll_until(Duration::from_secs(2), || {
            let cache_key = cache_key(&ctx);
            // provide() 経由だと None (miss) と Some(vec![]) (hit, handled-empty)
            // の区別が付きにくいので、内部キャッシュを直接覗いて判定する。
            let cache = provider.shared.cache.lock().unwrap();
            cache.get(&cache_key).cloned()
        });

        let entry = result.expect("cache entry should eventually be populated");
        assert_eq!(
            entry.value,
            Some(Vec::new()),
            "handled-but-empty Some(vec![]) must survive caching, not collapse to None"
        );

        // provide() 越しでも Some(vec![]) がそのまま観測できることを確認する。
        // provide() の戻り値は「ミス(None)」と「ヒットして中身が空(Some(vec![]))」
        // を素の Option だけでは区別できないため、キャッシュに既に値が
        // 入っている（上のループで確認済み）ことを踏まえて直接呼び出す。
        let via_provide = provider.provide(&ctx);
        assert_eq!(
            via_provide,
            Some(Vec::new()),
            "provide() must return Some(vec![]) from cache, not None"
        );
    }

    // ── 5. in-flight dedup: 連打しても内部呼び出しは1回のみ ──

    /// テストが明示的に解放するまで内部プロバイダの呼び出しをブロックする
    /// フェイク。固定 `sleep` 時間に依存する代わりに `Condvar` で同期する
    /// ことで、CPU 負荷が高い環境（フルテストスイート並列実行時）でも
    /// タイミングに依存せず「ループ中は fetch が完了しない」ことを保証する。
    struct GatedInner {
        call_count: Arc<AtomicUsize>,
        gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl CompletionProvider for GatedInner {
        fn provide(&self, _ctx: &CompletionContext) -> Option<Vec<Candidate>> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let (lock, cvar) = &*self.gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cvar.wait(released).unwrap();
            }
            Some(vec![candidate("foo")])
        }
    }

    #[test]
    #[serial]
    fn rapid_repeated_provide_dedups_to_one_inner_call() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let inner = Arc::new(GatedInner {
            call_count: Arc::clone(&call_count),
            gate: Arc::clone(&gate),
        });
        let provider = AsyncCacheProvider::new(inner);

        let ctx = ctx_for("git checkout ma");
        for _ in 0..20 {
            provider.provide(&ctx);
        }

        // 内部プロバイダはまだゲートで止まっているはず。in-flight dedup が
        // 効いていれば、20 回の provide() でも呼び出しはちょうど 1 回。
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "rapid repeated provide() calls for the same key must dedup to a single inner call"
        );

        // ゲートを解放して fetch を完了させ、後始末する
        // （後続テストへのスレッドリークを避ける）。
        {
            let (lock, cvar) = &*gate;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        poll_until(Duration::from_secs(5), || {
            let cache = provider.shared.cache.lock().ok()?;
            cache.contains_key(&cache_key(&ctx)).then_some(())
        });
    }

    // ── 6. 異なるキーは衝突しない ──

    #[test]
    #[serial]
    fn different_spans_get_independent_cache_entries() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(FakeInner {
            response: Some(vec![candidate("foo")]),
            call_count: Arc::clone(&call_count),
            delay: Duration::from_millis(10),
        });
        let provider = AsyncCacheProvider::new(inner);

        let ctx_a = ctx_for("git checkout ma");
        let ctx_b = ctx_for("git checkout fe");

        provider.provide(&ctx_a);
        provider.provide(&ctx_b);

        // call_count は「inner が呼ばれた」瞬間に増えるが、キャッシュへの
        // 書き込みはその後（inner 呼び出し完了後）に起きるため、call_count
        // だけをポーリングすると書き込み前に読みにいってレースする。
        // 両方のキーが実際にキャッシュへ書き込まれたことをポーリングする。
        poll_until(Duration::from_secs(2), || {
            let cache = provider.shared.cache.lock().ok()?;
            let both_present =
                cache.contains_key(&cache_key(&ctx_a)) && cache.contains_key(&cache_key(&ctx_b));
            both_present.then_some(())
        });

        let cache = provider.shared.cache.lock().unwrap();
        assert_eq!(
            cache.len(),
            2,
            "different spans should produce independent cache entries: {} keys",
            cache.len()
        );
        assert_ne!(cache_key(&ctx_a), cache_key(&ctx_b));
    }

    // ── 7. TTL 失効: 古いエントリはミス扱いで再フェッチされる ──

    #[test]
    #[serial]
    fn expired_entry_is_treated_as_miss_and_refetched() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(FakeInner {
            response: Some(vec![candidate("foo")]),
            call_count: Arc::clone(&call_count),
            delay: Duration::from_millis(5),
        });
        // テストで実時間 5 秒を待たずに済むよう、短い TTL を注入する。
        let provider = AsyncCacheProvider::with_ttl(inner, Duration::from_millis(50));

        let ctx = ctx_for("git checkout ma");
        provider.provide(&ctx);
        poll_until(Duration::from_secs(2), || {
            let r = provider.provide(&ctx);
            r.is_some().then_some(())
        });
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        // TTL を過ぎるまで待つ。
        thread::sleep(Duration::from_millis(80));

        // TTL 切れなので再度ミス（None）になり、新規 fetch がトリガーされる。
        let result = provider.provide(&ctx);
        assert_eq!(result, None, "expired entry should be treated as a miss");

        let seen = poll_until(Duration::from_secs(2), || {
            let n = call_count.load(Ordering::SeqCst);
            (n >= 2).then_some(n)
        });
        assert_eq!(
            seen,
            Some(2),
            "expired entry should trigger exactly one refetch"
        );
    }

    // ── 8. cwd を含めたキー: 同じ spans でも別ディレクトリなら別キー ──

    #[test]
    #[serial]
    fn cache_key_distinguishes_different_cwd() {
        let tmp_a = tempfile::tempdir().unwrap();
        let tmp_b = tempfile::tempdir().unwrap();
        let original = std::env::current_dir().unwrap();

        std::env::set_current_dir(tmp_a.path()).unwrap();
        let ctx_a = ctx_for("git checkout ma");
        let key_a = cache_key(&ctx_a);

        std::env::set_current_dir(tmp_b.path()).unwrap();
        let ctx_b = ctx_for("git checkout ma");
        let key_b = cache_key(&ctx_b);

        std::env::set_current_dir(&original).unwrap();

        assert_ne!(
            key_a, key_b,
            "same spans in two different cwds must produce distinct cache keys"
        );
    }

    // ── 9. 容量上限: 上限を超えても map が無制限に成長しない ──

    #[test]
    #[serial]
    fn capacity_bound_prevents_unbounded_growth() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(FakeInner {
            response: Some(vec![candidate("foo")]),
            call_count: Arc::clone(&call_count),
            delay: Duration::from_millis(1),
        });
        let provider = AsyncCacheProvider::new(inner);

        // MAX_CACHE_ENTRIES を大きく超える数の異なるキーを順番に投入する。
        // 同時実行数の上限に引っかからないよう、1 件ずつ完了を待ってから
        // 次を投入する（in-flight dedup やワーカー上限の影響を避けるため）。
        let total = MAX_CACHE_ENTRIES + 50;
        for i in 0..total {
            let line = format!("git checkout unique-branch-{i}");
            let ctx = ctx_for(&line);
            provider.provide(&ctx);
            poll_until(Duration::from_secs(2), || {
                let cache = provider.shared.cache.lock().unwrap();
                cache.contains_key(&cache_key(&ctx)).then_some(())
            });
        }

        let cache = provider.shared.cache.lock().unwrap();
        assert!(
            cache.len() <= MAX_CACHE_ENTRIES,
            "cache must never exceed MAX_CACHE_ENTRIES: got {} entries",
            cache.len()
        );
    }

    // ── 10. 内部プロバイダが panic してもワーカー枠と in-flight が回収される ──

    /// `provide()` が必ず panic するフェイク内部プロバイダ。
    struct PanickingInner {
        call_count: Arc<AtomicUsize>,
    }

    impl CompletionProvider for PanickingInner {
        fn provide(&self, _ctx: &CompletionContext) -> Option<Vec<Candidate>> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            panic!("simulated inner provider panic");
        }
    }

    /// 内部プロバイダの panic でワーカースレッドが巻き戻っても、
    /// `active_workers` と `in_flight` が [`WorkerGuard`] の `Drop` によって
    /// 回収されることを保証する回帰テスト。
    ///
    /// これを取りこぼすと、panic が [`MAX_CONCURRENT_WORKERS`] 回積み重なった
    /// 時点で以後 spawn が一切行われなくなり、**そのシェルセッションの間
    /// 外部補完が無言で沈黙する**（ユーザーにはエラーすら見えない）。
    /// ワーカー上限（4）より多い回数を試行して、枠が本当に再利用されている
    /// ことまで確認する。
    #[test]
    #[serial]
    fn inner_panic_still_releases_worker_slot_and_in_flight_key() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(PanickingInner {
            call_count: Arc::clone(&call_count),
        });
        let provider = AsyncCacheProvider::new(inner);

        // ワーカー上限を超える回数、別々のキーで試行する。枠が回収されずに
        // リークするなら、5 回目以降は spawn 自体が行われず call_count が
        // MAX_CONCURRENT_WORKERS で頭打ちになる。
        let attempts = MAX_CONCURRENT_WORKERS + 3;
        for i in 0..attempts {
            let line = format!("git checkout panic-probe-{i}");
            let ctx = ctx_for(&line);
            assert_eq!(
                provider.provide(&ctx),
                None,
                "a panicking inner provider must still surface as a plain cache miss"
            );
            // 各試行のワーカーが片付くのを待ってから次へ。
            poll_until(Duration::from_secs(2), || {
                let active = provider.shared.active_workers.lock().ok()?;
                (*active == 0).then_some(())
            });
        }

        let seen = call_count.load(Ordering::SeqCst);
        assert_eq!(
            seen, attempts,
            "every attempt must have spawned a worker; a leaked worker slot would \
             cap this at MAX_CONCURRENT_WORKERS ({MAX_CONCURRENT_WORKERS})"
        );

        let active = *provider.shared.active_workers.lock().unwrap();
        assert_eq!(active, 0, "worker count must return to 0 after panics");

        let in_flight = provider.shared.in_flight.lock().unwrap();
        assert!(
            in_flight.is_empty(),
            "in-flight keys must be cleared even when the inner provider panics: {in_flight:?}"
        );
    }

    // ── 11. 長さプレフィックス方式が NUL 入りの span でも衝突しない ──

    /// span 自体が区切り文字（NUL）を含んでいてもキーが一意であることを
    /// 保証する。素朴な「`\0` 区切りで連結」方式では `["a\0"]` と
    /// `["a", ""]` がどちらも `"a\0\0"` になって衝突していた
    /// （[`cache_key`] のドキュメント参照）。
    ///
    /// `lex_lenient` は NUL を除去しないため、これは机上の空論ではなく
    /// 「NUL を含む文字列を貼り付けた」だけで到達しうる経路。
    #[test]
    fn length_prefixed_key_does_not_collide_on_embedded_nul() {
        let mut with_nul = String::new();
        push_length_prefixed(&mut with_nul, "a\0");

        let mut split = String::new();
        push_length_prefixed(&mut split, "a");
        push_length_prefixed(&mut split, "");

        assert_ne!(
            with_nul, split,
            "spans [\"a\\0\"] and [\"a\", \"\"] must not produce the same cache key"
        );
    }

    // ── 12. ワーカー上限に達したら spawn せず None を返す ──

    /// 同時ワーカー数が [`MAX_CONCURRENT_WORKERS`] に達している間は、
    /// 別キーの新規リクエストでも spawn せずに諦めることを保証する
    /// （Tab 連打によるスレッド砲対策が実際に効いているかの検証）。
    /// 上限テストのため、内部プロバイダは明示的に解放するまでブロックする。
    #[test]
    #[serial]
    fn requests_beyond_worker_cap_do_not_spawn() {
        let call_count = Arc::new(AtomicUsize::new(0));
        // 十分長い delay で「ワーカーが埋まったまま」の状態を作る。
        let inner = Arc::new(FakeInner {
            response: Some(vec![candidate("foo")]),
            call_count: Arc::clone(&call_count),
            delay: Duration::from_millis(400),
        });
        let provider = AsyncCacheProvider::new(inner);

        // 上限ぴったりまで、それぞれ別キーで埋める。
        for i in 0..MAX_CONCURRENT_WORKERS {
            provider.provide(&ctx_for(&format!("git checkout cap-{i}")));
        }

        // 全ワーカーが「実際に内部プロバイダを呼び終える」ところまで待つ。
        //
        // ここで `active_workers == MAX_CONCURRENT_WORKERS` を待つだけでは
        // 不十分だった: カウンタの増加は spawn 側（呼び出しスレッド）が
        // 行うのに対し `call_count` の増加はワーカー側が行うため、
        // 「枠は埋まったがまだ内部プロバイダを呼んでいないワーカー」が
        // 残りうる。その状態で `before` を採ると、比較の途中でそのワーカーが
        // カウントを進めてしまい偽陽性の失敗になる（実際に観測: left=4,
        // right=3）。判定に使う値そのもの（`call_count`）が上限に達するまで
        // 待つことで、この競合を原理的に排除する。
        poll_until(Duration::from_secs(2), || {
            (call_count.load(Ordering::SeqCst) == MAX_CONCURRENT_WORKERS).then_some(())
        })
        .expect("all worker slots should fill up and start their inner calls");

        let before = call_count.load(Ordering::SeqCst);

        // 上限に達した状態でのさらなるリクエストは spawn してはならない。
        let overflow = provider.provide(&ctx_for("git checkout cap-overflow"));
        assert_eq!(overflow, None, "over-cap request must fall through");

        assert_eq!(
            call_count.load(Ordering::SeqCst),
            before,
            "no additional inner call may be spawned once the worker cap is reached"
        );

        let active = *provider.shared.active_workers.lock().unwrap();
        assert_eq!(
            active, MAX_CONCURRENT_WORKERS,
            "the over-cap request must not have incremented the worker count"
        );
    }

    // ── 13. 容量超過時に本当に全消去が起きる ──

    /// `capacity_bound_prevents_unbounded_growth` は「上限を超えない」ことしか
    /// 見ていないため、全消去ブランチが実際に踏まれているかを別途確認する。
    /// 古いキーが消えていることまで主張することで、上限が別の仕組みで
    /// 偶然守られている可能性を排除する。
    #[test]
    #[serial]
    fn exceeding_capacity_actually_clears_old_entries() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(FakeInner {
            response: Some(vec![candidate("foo")]),
            call_count: Arc::clone(&call_count),
            delay: Duration::from_millis(1),
        });
        let provider = AsyncCacheProvider::new(inner);

        let first_ctx = ctx_for("git checkout evict-probe-first");
        let first_key = cache_key(&first_ctx);
        provider.provide(&first_ctx);
        poll_until(Duration::from_secs(2), || {
            let cache = provider.shared.cache.lock().ok()?;
            cache.contains_key(&first_key).then_some(())
        })
        .expect("the first entry should be cached");

        // 上限を踏み越えるまで別キーを投入する。
        for i in 0..MAX_CACHE_ENTRIES {
            let ctx = ctx_for(&format!("git checkout evict-filler-{i}"));
            provider.provide(&ctx);
            poll_until(Duration::from_secs(2), || {
                let cache = provider.shared.cache.lock().ok()?;
                cache.contains_key(&cache_key(&ctx)).then_some(())
            });
        }

        let cache = provider.shared.cache.lock().unwrap();
        assert!(
            !cache.contains_key(&first_key),
            "the clear-all eviction branch must actually drop old entries"
        );
    }
}

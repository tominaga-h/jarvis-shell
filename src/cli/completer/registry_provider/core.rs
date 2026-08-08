//! Registry-backed completion provider core.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::super::carapace::ExternalCompletionSettings;
use super::super::context::CompletionContext;
use super::super::provider::{Candidate, CompletionProvider};
use super::super::registry::{CompletionRegistry, CompletionSpec};
use super::candidates::static_candidates;
use super::conditions::condition_is_active;
use super::filtering::{dedup_candidates, flag_candidates};

/// 動的候補（`$(...)`）実行タイムアウトの下限値。
///
/// `[completion] external_timeout_ms` は 0 や極端に小さい値を設定できて
/// しまうため、Tab 補完のホットパスで実質即タイムアウト（= 常に無効）に
/// なることを避けるための最小フロア。carapace/zsh ブリッジと違い、
/// ユーザー自身が任意のコマンドを登録する機能のため、あまり大きくは
/// 取らず「明らかに短すぎる設定を底上げする」程度に留める。
const MIN_DYNAMIC_TIMEOUT_MS: u64 = 200;

/// ユーザー定義補完（`complete` ビルトイン）プロバイダ。
pub(crate) struct RegistryProvider {
    registry: Arc<RwLock<CompletionRegistry>>,
    /// 動的候補（`-a "$(...)"`）実行時のタイムアウト算出に使う共有設定。
    /// `Shell` / carapace / zsh ブリッジと同じ `Arc<RwLock<_>>` 配管
    /// パターン（`external_timeout_ms` の hot-reload にも追従する）。
    external_completion: Arc<RwLock<ExternalCompletionSettings>>,
}

impl RegistryProvider {
    pub(crate) fn new(
        registry: Arc<RwLock<CompletionRegistry>>,
        external_completion: Arc<RwLock<ExternalCompletionSettings>>,
    ) -> Self {
        Self {
            registry,
            external_completion,
        }
    }

    /// 動的候補実行の実効タイムアウトを求める（共有設定の値と
    /// [`MIN_DYNAMIC_TIMEOUT_MS`] の大きい方）。設定の読み取りに失敗した
    /// 場合（poisoned lock）はフロア値のみを使う。
    fn dynamic_timeout(&self) -> Duration {
        let floor = Duration::from_millis(MIN_DYNAMIC_TIMEOUT_MS);
        match self.external_completion.read() {
            Ok(settings) => settings.timeout.max(floor),
            Err(_) => floor,
        }
    }
}

impl CompletionProvider for RegistryProvider {
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        if ctx.is_first_token {
            return None;
        }

        let head = ctx.head_command()?;

        let registry = self.registry.read().ok()?;
        let specs = registry.specs_for(head);
        if specs.is_empty() {
            return None;
        }

        let active_specs: Vec<&CompletionSpec> = specs
            .iter()
            .filter(|spec| condition_is_active(spec, ctx))
            .collect();
        if active_specs.is_empty() {
            return None;
        }

        // '-' 分岐: フラグ候補の後ろに `-a`（静的/動的）候補も連結する。
        // fish は非ダッシュ分岐では引数のみを出すが、'-' 分岐だけの場合
        // 「フラグに前方一致しない `-a` 語（例: `--custom`）が到達不能」に
        // なる不具合があったため、フラグ候補優先でマージする。非ダッシュ
        // 分岐は従来通り引数のみ（fish parity）。
        let candidates = if ctx.partial.starts_with('-') {
            let mut candidates = flag_candidates(&active_specs, &ctx.partial);
            candidates.extend(static_candidates(
                &active_specs,
                &ctx.partial,
                self.dynamic_timeout(),
            ));
            candidates
        } else {
            static_candidates(&active_specs, &ctx.partial, self.dynamic_timeout())
        };

        let candidates = dedup_candidates(candidates);

        if candidates.is_empty() {
            None
        } else {
            Some(candidates)
        }
    }
}

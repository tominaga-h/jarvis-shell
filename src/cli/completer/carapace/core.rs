//! The carapace completion provider.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::cli::completer::context::CompletionContext;
use crate::cli::completer::external::run_external_capped;
use crate::cli::completer::provider::{Candidate, CompletionProvider};

use super::binary::{gate, ExternalCompletionSettings, ExternalKind};
use super::candidates;
use super::parsing::CarapaceExport;

/// carapace 連携補完プロバイダ。
///
/// 先頭トークン補完（コマンド名自体の補完）は [`super::super::command::CommandProvider`]
/// の担当のため、`ctx.is_first_token` の場合は必ず `None`（担当外）を返す。
pub(crate) struct CarapaceProvider {
    /// `Shell` と共有する外部補完設定（`git_branch_commands` と同じ配管
    /// パターン）。`source` コマンドによる `reload_config` が `which()` の
    /// 再検出込みで更新するため、`provide()` 呼び出しごとに短命な read を行う。
    settings: Arc<RwLock<ExternalCompletionSettings>>,
}

impl CarapaceProvider {
    pub(crate) fn new(settings: Arc<RwLock<ExternalCompletionSettings>>) -> Self {
        Self { settings }
    }
}

impl CarapaceProvider {
    /// `provide()` 冒頭の「そもそも自分の対象か」ガード群だけを切り出した
    /// もの（外部プロセスは一切起動しない、安価な判定）。
    ///
    /// [`CompletionProvider::is_responsible`] と `provide()` 本体の両方から
    /// 呼ぶことで、判定基準が 2 箇所に drift するのを防ぐ（perf/
    /// completion-latency）。返り値が `Some((binary, timeout))` なら
    /// 「carapace が実際に対象コマンドの責任者である」ことを意味し、
    /// `provide()` はこれを使って実行に進む。`is_responsible` は中身
    /// （バイナリパス/timeout）を使わず `is_some()` だけを見る。
    fn responsibility_gate(&self, ctx: &CompletionContext) -> Option<(PathBuf, Duration)> {
        // 短命な read ロック（`gate` 内部で取得・即座に drop する — `mod.rs`
        // の aliases スナップショットと同じ方針）。carapace は起動コストが
        // 低いため `min_timeout` フロアは適用しない（`None`）。
        let gated = gate(&self.settings, ExternalKind::Carapace, None)?;

        if ctx.is_first_token {
            // コマンド名自体の補完は CommandProvider の担当。
            return None;
        }

        if ctx.head_command() == Some("cd") {
            // 防御的ガード: carapace-bin 1.7.3 は cd 用の spec を
            // 同梱しておらず、`values` は事実上 PathProvider の dirs_only
            // フィルタだけを頼りに空になる（=フォールスルー）。しかし将来の
            // carapace/bridge バージョンが cd 補完（ファイルを含みうる）を
            // 発行し始めた場合、ここで通してしまうと dirs-only 契約が
            // 静かに壊れる。cd は常に PathProvider（dirs_only 判定を持つ）
            // に担当させるため、carapace 側は最初から手を引く。
            return None;
        }

        if ctx.spans().len() < 2 {
            // spans[0] (コマンド名) しかない = まだサブコマンド/引数の
            // 補完対象がない。
            return None;
        }

        Some(gated)
    }
}

impl CompletionProvider for CarapaceProvider {
    /// `ctx` が carapace の対象（バイナリ有効・先頭トークンでない・`cd`
    /// でない・spans 十分）かどうかを、実際に carapace プロセスを起動せず
    /// 判定する。`provide()` と同じ [`CarapaceProvider::responsibility_gate`]
    /// を共有するため、判定基準が drift しない（`is_responsible` の
    /// ドキュメントは `provider.rs` 参照）。
    ///
    /// **carapace は「責任者」を名乗らない**（常に `false`）。
    ///
    /// # なぜ常に false なのか（実機で踏んだ不具合）
    /// carapace は内蔵 spec（653 個）を持つコマンドしか答えられず、spec が
    /// 無いコマンドでは**正常終了しつつ空の出力**を返す（実測: `carapace
    /// tmuxinator export tmuxinator ''` は exit 0 かつ出力ゼロ、10〜20ms）。
    /// `provide()` はこれを `None` に畳むが、これは「タイムアウトして
    /// 答えられなかった」ではなく「自分の担当ではないので次に譲る」の意味。
    ///
    /// ここで `responsibility_gate().is_some()` を返していた実装は、
    /// **spec の有無を区別できない**ため、carapace が spec を持たない
    /// コマンド（tmuxinator 等）でも「責任者だが失敗した」と申告していた。
    /// その結果 `dispatch_providers` がチェーンをそこで打ち切り、**本来
    /// 答えられる zsh ブリッジが一度も呼ばれず**、ユーザーには
    /// 「NO RECORDS FOUND」だけが表示された（実機報告）。
    ///
    /// # 責任者になれるのは「後ろに誰もいない」プロバイダだけ
    /// `external = "auto"` では carapace → zsh ブリッジの順に並ぶ。carapace が
    /// 答えられなくても後段の zsh ブリッジが答えられる以上、carapace は
    /// 「このコマンドの最終的な責任者」ではない。誤ったパス補完を抑止する
    /// 役割は、外部補完チェーンの**最後**に位置する zsh ブリッジ
    /// （[`super::super::zsh_bridge::ZshBridgeProvider::is_responsible`]）が担う。
    ///
    /// carapace のみを有効化した構成（`external = "carapace"`）では zsh
    /// ブリッジが存在しないため、carapace が答えられなければ従来どおり
    /// `PathProvider` へフォールバックする。carapace が扱えないコマンドで
    /// パス補完すら出さないより、パス補完に落ちるほうが実害が小さい
    /// （carapace は spec の無いコマンドが多数あるため）。
    fn is_responsible(&self, _ctx: &CompletionContext) -> bool {
        false
    }

    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        let (binary, timeout) = self.responsibility_gate(ctx)?;

        let spans = ctx.spans();
        let mut args = vec![spans[0].clone(), "export".to_string()];
        args.extend(spans.iter().cloned());

        let envs = [("CARAPACE_LENIENT".to_string(), "1".to_string())];
        let stdout = run_external_capped(&binary, &args, &envs, timeout)?;

        let export: CarapaceExport = serde_json::from_str(&stdout).ok()?;
        candidates::convert_export(export)
    }
}

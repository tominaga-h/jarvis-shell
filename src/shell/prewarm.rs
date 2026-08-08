//! 起動時の zsh 補完デーモン prewarm スレッド起動を切り出したサブモジュール。
//!
//! 振る舞いは元の `src/shell/mod.rs` から一切変更していない。

use std::sync::{Arc, RwLock};

use crate::cli::completer::{
    prewarm_zsh_daemon, DaemonGate, ExternalCompletionSettings, SharedDaemonSlot,
};

/// 起動時のバックグラウンド事前ウォームアップスレッドを、対話モードの
/// ときだけ起動する（`Shell::new` から切り出し）。
///
/// `interactive == false`（`-c` 単体実行）では、そもそも Tab 補完が発生
/// しない短命プロセスなので、スレッドを起動すること自体が無駄なうえ、
/// プロセスが数ミリ秒で完走してしまうと prewarm の spawn（PTY + プロセス
/// 起動 + レディマーカー待ち、数百ms かかりうる）がプロセス終了後まで
/// 完了せず、孤児 `/bin/zsh -i` 化のレースを踏みやすい（`DaemonGate` の
/// ドキュメント参照）。`Shell` 全体を構築せずに「スレッドを1本も起動しない」
/// ことを直接観測できるよう、`Shell::new` 本体から切り出している。
///
/// 戻り値は `interactive == true` の場合のみ `Some(Receiver<()>)`
/// （prewarm スレッドが実行を終えた瞬間に送信される完了通知チャネル）を
/// 返す。`Shell::zsh_daemon_prewarm_done` のドキュメント参照 — `main` が
/// `std::process::exit` する前に `shutdown_zsh_daemon` がこのチャネルを
/// 有界時間で待つことで、「gate.close() 後に prewarm がまだ実行中の
/// まま強制終了され、tombstone チェックが一度も走らない」レースを閉じる。
pub(super) fn spawn_prewarm_thread_if_interactive(
    interactive: bool,
    external_completion: &Arc<RwLock<ExternalCompletionSettings>>,
    zsh_daemon: &SharedDaemonSlot,
    zsh_daemon_gate: &Arc<DaemonGate>,
) -> Option<std::sync::mpsc::Receiver<()>> {
    if !interactive {
        return None;
    }
    let settings_for_prewarm = Arc::clone(external_completion);
    let daemon_for_prewarm = Arc::clone(zsh_daemon);
    let gate_for_prewarm = Arc::clone(zsh_daemon_gate);
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        prewarm_zsh_daemon(
            &settings_for_prewarm,
            &daemon_for_prewarm,
            &gate_for_prewarm,
        );
        // prewarm が（スロットへの書き込みの成否に関わらず）完全に終了
        // したことを通知する。送信失敗（受信側が既に drop 済み）は
        // 無視してよい — `shutdown_zsh_daemon` が呼ばれずプロセスが
        // 対話 REPL のまま動き続けているケース（`Receiver` は `Shell` に
        // 保持されたまま）を含め、通知を誰も待っていない状況は正常。
        let _ = done_tx.send(());
    });
    Some(done_rx)
}

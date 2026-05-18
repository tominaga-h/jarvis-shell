//! fzf 外部コマンドラッパ
//!
//! zoxide の `src/util.rs::Fzf` と `src/cmd/query.rs::get_fzf` を踏襲する。
//! `cdj` ビルトインが「複数候補から 1 件選ばせる」用途で利用する。
//!
//! テストは存在しない（ユーザー指示により fzf 起動を伴うテストは不要）。
//! 実装の正しさは zoxide パターンとの一致をコードレビューで担保する。

use std::io::{self, Write};
use std::process::{Child, Command, Stdio};

/// fzf 未インストール時の共通エラーメッセージ。
const ERR_FZF_NOT_FOUND: &str = "could not find fzf, is it installed?";

/// fzf 起動コマンドビルダ。
///
/// zoxide `src/cmd/query.rs::get_fzf` で使われている引数を踏襲する。
pub(crate) struct Fzf(Command);

impl Fzf {
    /// jarvish 向けの引数をセットした fzf ランチャを構築する。
    pub(crate) fn new() -> Self {
        let mut cmd = Command::new("fzf");
        cmd.args([
            "--exact",
            "--no-sort",
            "--bind=ctrl-z:ignore,btab:up,tab:down",
            "--cycle",
            "--keep-right",
            "--border=sharp",
            "--height=45%",
            "--info=inline",
            "--layout=reverse",
            "--tabstop=1",
            "--exit-0",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
        Self(cmd)
    }

    /// プレビューウィンドウを有効化する（UNIX のみ）。
    ///
    /// zoxide `Fzf::enable_preview()` を踏襲。ただし jarvish の stdin 形式が
    /// `<path>\n`（score なし）であるため、プレースホルダは `{2..}` ではなく
    /// `{}` を使う。Windows では何もせず self を返す。
    pub(crate) fn enable_preview(mut self) -> Self {
        if !cfg!(unix) {
            return self;
        }
        let preview_cmd = if cfg!(target_os = "linux") {
            "command -p ls -Cp --color=always --group-directories-first {}"
        } else {
            "command -p ls -Cp {}"
        };
        self.0
            .args([
                format!("--preview={preview_cmd}"),
                "--preview-window=down,30%,sharp".to_string(),
            ])
            .envs([("CLICOLOR", "1"), ("CLICOLOR_FORCE", "1"), ("SHELL", "sh")]);
        self
    }

    /// fzf 子プロセスを起動する。
    ///
    /// fzf がインストールされていない場合は `ERR_FZF_NOT_FOUND` を返す。
    pub(crate) fn spawn(mut self) -> Result<FzfChild, String> {
        match self.0.spawn() {
            Ok(child) => Ok(FzfChild(child)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(ERR_FZF_NOT_FOUND.to_string()),
            Err(e) => Err(format!("failed to spawn fzf: {e}")),
        }
    }
}

/// 起動済み fzf プロセスのハンドル。
pub(crate) struct FzfChild(Child);

impl FzfChild {
    /// `candidates` を fzf の stdin に流し、選択結果を受け取る。
    ///
    /// 戻り値:
    /// - `Ok(Some(line))` — ユーザーが選択した行
    /// - `Ok(None)` — ユーザーキャンセル (exit 130) または no-match (exit 1)
    /// - `Err(_)` — その他の fzf 異常終了
    pub(crate) fn run(mut self, candidates: &[String]) -> Result<Option<String>, String> {
        // stdin に候補を書き込む（fzf は EOF を待っているため drop で閉じる）
        {
            let stdin = self
                .0
                .stdin
                .as_mut()
                .ok_or_else(|| "failed to open fzf stdin".to_string())?;

            for line in candidates {
                if let Err(e) = writeln!(stdin, "{line}") {
                    // EPIPE: fzf が即時終了した場合。後段の wait_with_output で診断する。
                    if e.kind() != io::ErrorKind::BrokenPipe {
                        return Err(format!("failed to write to fzf stdin: {e}"));
                    }
                    break;
                }
            }
        }

        let output = self
            .0
            .wait_with_output()
            .map_err(|e| format!("failed to wait for fzf: {e}"))?;

        match output.status.code() {
            Some(0) => {
                let selection = String::from_utf8_lossy(&output.stdout)
                    .trim_end_matches(['\n', '\r'])
                    .to_string();
                if selection.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(selection))
                }
            }
            // fzf: 1 = no match
            Some(1) => Ok(None),
            // fzf: 130 = ユーザーキャンセル (Ctrl-C / ESC)
            Some(130) => Ok(None),
            Some(code) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!("fzf exited with code {code}: {stderr}"))
            }
            None => Err("fzf terminated by signal".to_string()),
        }
    }
}

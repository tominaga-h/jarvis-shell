//! `complete` ビルトインが操作するユーザー定義補完のデータモデル
//!
//! fish の `complete` コマンドに倣い、コマンド名ごとに 1 個以上の
//! [`CompletionSpec`] を登録できる。登録順は保持され、`complete`（引数なし）
//! による一覧表示は登録順で決定的に出力される。
//!
//! このモジュールはデータ構造のみを持つ（登録・一覧化・消去のロジックは
//! [`crate::engine::builtins::complete`] が持つ — 単一責任の原則に従い、
//! 「何を持つか」と「どう操作するか」を分離している）。`Shell` とは
//! `Arc<RwLock<CompletionRegistry>>` で共有され、`RegistryProvider`
//! （補完プロバイダ側、Task 3.2 で追加）から読み取られる。

use std::collections::BTreeMap;

/// `complete -c CMD [-s X]... [-l LONG]... [-a 'WORDS'] [-d DESC] [-n COND]`
/// 1 回の呼び出しで登録される 1 個の補完仕様。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompletionSpec {
    /// `-s` で指定された単一文字のショートオプション（先頭の `-` は含まない）。
    pub short: Vec<String>,
    /// `-l` で指定されたロングオプション名（先頭の `--` は含まない）。
    pub long: Vec<String>,
    /// `-a` で指定された静的候補の生文字列（空白区切り、クォート可）。
    pub arguments: Option<String>,
    /// `-d` で指定された説明文。
    pub description: Option<String>,
    /// `-n` で指定された条件式（現時点では登録・表示のみで評価はしない）。
    pub condition: Option<String>,
}

/// コマンド名 → 登録された [`CompletionSpec`] 列（登録順）のレジストリ。
///
/// `BTreeMap` を採用しているのはコマンド名の一覧順を決定的にするため
/// （`complete` 引数なし実行時の全コマンド一覧表示に使う）。各コマンド内の
/// spec 列自体は `Vec` で登録順を保持する。
#[derive(Debug, Clone, Default)]
pub struct CompletionRegistry {
    commands: BTreeMap<String, Vec<CompletionSpec>>,
}

impl CompletionRegistry {
    /// 空のレジストリを作成する。
    pub fn new() -> Self {
        Self::default()
    }

    /// `cmd` に対して `spec` を末尾に追加登録する（同名コマンドへの複数回登録は蓄積される）。
    pub(crate) fn register(&mut self, cmd: &str, spec: CompletionSpec) {
        self.commands.entry(cmd.to_string()).or_default().push(spec);
    }

    /// `cmd` に登録済みの全 spec を、登録順で返す。未登録なら空スライス。
    ///
    /// `RegistryProvider`（Task 3.2）が Tab 補完のホットパスで読み取る想定。
    #[allow(dead_code)]
    pub(crate) fn specs_for(&self, cmd: &str) -> &[CompletionSpec] {
        self.commands.get(cmd).map_or(&[], Vec::as_slice)
    }

    /// `cmd` に登録済みの全 spec を消去する。消去した場合 `true`、
    /// 元々未登録だった場合 `false` を返す。
    pub(crate) fn erase(&mut self, cmd: &str) -> bool {
        self.commands.remove(cmd).is_some()
    }

    /// 登録済みの全コマンド名を、コマンド名の昇順（決定的）で列挙する。
    /// 各要素は `(コマンド名, そのコマンドの spec 列（登録順）)`。
    pub(crate) fn iter_sorted(&self) -> impl Iterator<Item = (&str, &[CompletionSpec])> {
        self.commands
            .iter()
            .map(|(cmd, specs)| (cmd.as_str(), specs.as_slice()))
    }

    /// レジストリが空かどうか。
    ///
    /// Task 3.1 時点では未使用（将来のログ出力・空レジストリの早期 return
    /// 最適化等での利用を見込んで公開しておく）。
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_registry_is_empty() {
        let reg = CompletionRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.specs_for("git"), &[] as &[CompletionSpec]);
    }

    #[test]
    fn register_accumulates_in_order() {
        let mut reg = CompletionRegistry::new();
        let spec1 = CompletionSpec {
            short: vec!["v".to_string()],
            ..Default::default()
        };
        let spec2 = CompletionSpec {
            long: vec!["verbose".to_string()],
            ..Default::default()
        };
        reg.register("mycmd", spec1.clone());
        reg.register("mycmd", spec2.clone());

        let specs = reg.specs_for("mycmd");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0], spec1);
        assert_eq!(specs[1], spec2);
    }

    #[test]
    fn erase_removes_only_named_command() {
        let mut reg = CompletionRegistry::new();
        reg.register("mycmd", CompletionSpec::default());
        reg.register("othercmd", CompletionSpec::default());

        assert!(reg.erase("mycmd"));
        assert!(reg.specs_for("mycmd").is_empty());
        assert_eq!(reg.specs_for("othercmd").len(), 1);
    }

    #[test]
    fn erase_unknown_command_returns_false() {
        let mut reg = CompletionRegistry::new();
        assert!(!reg.erase("nonexistent"));
    }

    #[test]
    fn iter_sorted_is_deterministic_by_command_name() {
        let mut reg = CompletionRegistry::new();
        reg.register("zeta", CompletionSpec::default());
        reg.register("alpha", CompletionSpec::default());
        reg.register("mid", CompletionSpec::default());

        let names: Vec<&str> = reg.iter_sorted().map(|(cmd, _)| cmd).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }
}

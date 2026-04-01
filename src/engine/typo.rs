//! タイポ補正
//!
//! zsh の `correct 'gti' to 'git' [nyae]?` に相当する機能。
//! 存在しないコマンドに対して PATH 上の類似コマンドを Damerau-Levenshtein 距離で
//! 探索し、補正候補を返す。
//!
//! PATH 上の全コマンド一覧は TTL キャッシュ（60秒）で保持し、
//! `$PATH` 環境変数の変化を検出した場合も即座に再収集する。

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;
use std::time::Instant;

/// Optimal String Alignment (Damerau-Levenshtein) 距離を計算する。
///
/// 通常の挿入・削除・置換に加え、隣接文字の入れ替え（転置）も 1 操作として扱う。
/// `gti` → `git` のようなタイポを距離 1 で検出できる。
fn damerau_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();

    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, val) in dp[0].iter_mut().enumerate() {
        *val = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
            // 転置（隣接する 2 文字の入れ替え）
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                dp[i][j] = dp[i][j].min(dp[i - 2][j - 2] + cost);
            }
        }
    }
    dp[m][n]
}

/// 入力文字列がコマンド名として妥当な形式かを判定する。
///
/// ASCII 英数字と `-`/`_`/`.` のみで構成され、長さが 2 以上の場合に `true` を返す。
pub fn is_command_like(token: &str) -> bool {
    token.len() >= 2
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// コマンド名の長さに応じた補正距離閾値を返す。
///
/// - 3 文字以下: 距離 ≤ 1
/// - 7 文字以下: 距離 ≤ 2
/// - 8 文字以上: 距離 ≤ 3
fn correction_threshold(cmd_len: usize) -> usize {
    if cmd_len <= 3 {
        1
    } else if cmd_len <= 7 {
        2
    } else {
        3
    }
}

/// PATH コマンド一覧のキャッシュ TTL（秒）。
///
/// `InputClassifier` の PATH ルックアップ TTL（5秒）より長く設定する。
/// フルスキャンは単体 lookup より重いため、60秒のキャッシュで十分な鮮度を保つ。
const COMMANDS_CACHE_TTL_SECS: u64 = 60;

/// TTL キャッシュ付きコマンド一覧
struct CommandCache {
    commands: HashSet<String>,
    cached_at: Instant,
    /// キャッシュ取得時の `$PATH` 値（変更検出用）
    path_env: String,
}

static PATH_COMMANDS_CACHE: Mutex<Option<CommandCache>> = Mutex::new(None);

/// PATH 上の全実行可能ファイル名を収集する（キャッシュなし）。
fn collect_path_commands_raw(path: &str) -> HashSet<String> {
    let mut commands = HashSet::new();
    for dir in path.split(':') {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                continue;
            }
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                commands.insert(name.to_string());
            }
        }
    }
    commands
}

/// PATH 上の全実行可能ファイル名を TTL キャッシュ経由で返す。
///
/// 以下のいずれかの場合にキャッシュを破棄して再収集する:
/// - TTL（60秒）が経過した
/// - `$PATH` 環境変数の値が変化した（`brew install` 等）
///
/// `Mutex` のロック取得に失敗した場合はキャッシュなしで直接収集する。
fn get_path_commands() -> HashSet<String> {
    let current_path = std::env::var("PATH").unwrap_or_default();
    let now = Instant::now();

    let Ok(mut cache) = PATH_COMMANDS_CACHE.lock() else {
        // ロック汚染時のフォールバック
        return collect_path_commands_raw(&current_path);
    };

    if let Some(ref c) = *cache {
        let ttl_valid = now.duration_since(c.cached_at).as_secs() < COMMANDS_CACHE_TTL_SECS;
        let path_unchanged = c.path_env == current_path;
        if ttl_valid && path_unchanged {
            return c.commands.clone();
        }
    }

    // キャッシュミス または 無効: 再収集して保存
    let commands = collect_path_commands_raw(&current_path);
    *cache = Some(CommandCache {
        commands: commands.clone(),
        cached_at: now,
        path_env: current_path,
    });
    commands
}

/// `cmd` に最も近い PATH 上のコマンドを返す。
///
/// 距離が閾値以下の最近傍コマンドが存在する場合に `Some(suggestion)` を返す。
/// 候補が複数ある場合は距離が最小のもの（同距離なら辞書順で最初のもの）を返す。
pub fn find_correction(cmd: &str) -> Option<String> {
    let threshold = correction_threshold(cmd.len());
    let commands = get_path_commands();

    let mut best: Option<(usize, String)> = None;
    for candidate in &commands {
        let dist = damerau_levenshtein(cmd, candidate);
        if dist > threshold {
            continue;
        }
        let better = match &best {
            None => true,
            Some((best_dist, best_name)) => {
                dist < *best_dist || (dist == *best_dist && candidate < best_name)
            }
        };
        if better {
            best = Some((dist, candidate.clone()));
        }
    }

    best.map(|(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_same() {
        assert_eq!(damerau_levenshtein("git", "git"), 0);
    }

    #[test]
    fn distance_transposition() {
        // 隣接文字の転置は距離 1
        assert_eq!(damerau_levenshtein("gti", "git"), 1);
    }

    #[test]
    fn distance_substitution() {
        assert_eq!(damerau_levenshtein("gut", "git"), 1);
    }

    #[test]
    fn distance_insertion() {
        assert_eq!(damerau_levenshtein("gt", "git"), 1);
    }

    #[test]
    fn distance_deletion() {
        // "grpe" → "grep" は p と e の転置なので距離 1
        assert_eq!(damerau_levenshtein("grpe", "grep"), 1);
    }

    #[test]
    fn is_command_like_valid() {
        assert!(is_command_like("git"));
        assert!(is_command_like("ls"));
        assert!(is_command_like("cargo-build"));
        assert!(is_command_like("node.js"));
    }

    #[test]
    fn is_command_like_invalid() {
        assert!(!is_command_like("g"));
        assert!(!is_command_like(""));
        assert!(!is_command_like("エラー"));
        assert!(!is_command_like("hello world"));
    }

    #[test]
    fn find_correction_suggests_command() {
        // "gti" は "git" の転置（距離 1）→ 候補が返る
        if which::which("git").is_ok() {
            let result = find_correction("gti");
            // 距離 1 の候補が存在すれば Some を返す（環境依存で別コマンドの可能性あり）
            assert!(result.is_some());
        }
    }

    #[test]
    fn find_correction_no_match() {
        // 全コマンドと大きく離れた文字列
        assert_eq!(find_correction("zzzjarvishtest"), None);
    }
}

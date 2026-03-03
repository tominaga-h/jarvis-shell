//! 機密情報のマスキング処理
//!
//! コマンド出力（stdout/stderr）に含まれる API キーやトークンを検出し、
//! Black Box への保存前にマスキングすることで AI コンテキストへの流出を防止する。

use regex::Regex;
use std::sync::OnceLock;

const MASKED: &str = "***";

const SENSITIVE_KEYWORDS: &[&str] = &[
    "API_KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AUTH",
    "PRIVATE",
];

const TOKEN_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "AKIA",
];

/// テキストに機密情報が含まれる可能性があるかを軽量に判定する。
///
/// 正規表現を使わず部分文字列チェックのみで高速に判定する事前フィルタ。
/// `mask_secrets()` の呼び出し前にこの関数で判定し、`true` の場合のみ
/// `mask_secrets()` を実行することで不要な正規表現処理を回避する。
///
/// - **false positive は許容**: この関数が `true` でも `mask_secrets` が何も変えない場合がある。
/// - **false negative は不許容**: `mask_secrets` が変更するテキストに対して `false` を返してはならない。
pub fn contains_secrets(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }

    let upper = text.to_ascii_uppercase();
    for kw in SENSITIVE_KEYWORDS {
        if upper.contains(kw) {
            return true;
        }
    }

    for prefix in TOKEN_PREFIXES {
        if text.contains(prefix) {
            return true;
        }
    }

    false
}

/// キー名が機密キーワードを含むかを判定する正規表現を返す。
///
/// キー名全体に対して適用し、アンダーバーまたは先頭/末尾を境界として
/// 機密キーワードが出現するかを判定する。
fn sensitive_key_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:^|_)(?:API_KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|AUTH|PRIVATE)(?:_|$)",
        )
        .unwrap()
    })
}

/// 環境変数代入パターン（`export KEY=VALUE` または `KEY=VALUE`）を検出する正規表現を返す。
///
/// キャプチャグループ:
/// - 1: `export ` プレフィックス（あれば）
/// - 2: キー名
/// - 3: `=` 以降の値部分（クォート含む）
fn assignment_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?m)((?:export\s+)?)([A-Za-z_][A-Za-z_0-9]*)\s*=\s*('[^']*'|"[^"]*"|\S+)"#)
            .unwrap()
    })
}

/// トークン値ベースのマスキング用正規表現を返す。
///
/// テキスト中に現れる既知のトークンフォーマットを検出する:
/// - OpenAI: `sk-` プレフィックス (20文字以上)
/// - GitHub PAT: `ghp_`, `gho_` (36文字以上)
/// - GitHub Fine-grained PAT: `github_pat_` (22文字以上)
/// - Slack: `xoxb-`, `xoxp-` (10文字以上)
/// - AWS Access Key: `AKIA` (16桁の英大文字+数字)
fn token_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:sk-[A-Za-z0-9_-]{20,}|ghp_[A-Za-z0-9]{36,}|gho_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,}|xox[bp]-[0-9A-Za-z-]{10,}|AKIA[0-9A-Z]{16})"
        ).unwrap()
    })
}

/// テキスト中の機密情報をマスキングする。
///
/// 2段階でマスキングを適用する:
/// 1. キー名ベース: 機密キーワードを含む環境変数代入の値をマスク
/// 2. トークン値ベース: 既知のトークンフォーマットをマスク
pub fn mask_secrets(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let sensitive_key_re = sensitive_key_regex();
    let assign_re = assignment_regex();
    let token_re = token_regex();

    // Step 1: キー名ベースのマスキング
    let result = assign_re.replace_all(text, |caps: &regex::Captures| {
        let prefix = caps.get(1).map_or("", |m| m.as_str());
        let key = caps.get(2).map_or("", |m| m.as_str());
        let value = caps.get(3).map_or("", |m| m.as_str());

        if sensitive_key_re.is_match(key) {
            format!("{prefix}{key}={MASKED}")
        } else {
            format!("{prefix}{key}={value}")
        }
    });

    // Step 2: トークン値ベースのマスキング
    let result = token_re.replace_all(&result, MASKED);

    result.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // A. キー名ベース: 全キーワード x 検出テスト
    // ========================================================================

    // --- API_KEY ---

    #[test]
    fn mask_api_key() {
        assert_eq!(
            mask_secrets("export OPENAI_API_KEY=sk-xxx"),
            "export OPENAI_API_KEY=***"
        );
    }

    #[test]
    fn mask_api_key_prefix() {
        assert_eq!(mask_secrets("API_KEY_OPENAI=sk-xxx"), "API_KEY_OPENAI=***");
    }

    #[test]
    fn mask_api_key_middle() {
        assert_eq!(
            mask_secrets("MY_API_KEY_VALUE=sk-xxx"),
            "MY_API_KEY_VALUE=***"
        );
    }

    #[test]
    fn mask_api_key_exact() {
        assert_eq!(mask_secrets("API_KEY=sk-xxx"), "API_KEY=***");
    }

    // --- TOKEN ---

    #[test]
    fn mask_token() {
        assert_eq!(
            mask_secrets("export AUTH_TOKEN=abc123"),
            "export AUTH_TOKEN=***"
        );
    }

    #[test]
    fn mask_token_prefix() {
        assert_eq!(mask_secrets("TOKEN_VALUE=abc123"), "TOKEN_VALUE=***");
    }

    #[test]
    fn mask_token_exact() {
        assert_eq!(mask_secrets("TOKEN=abc123"), "TOKEN=***");
    }

    // --- SECRET ---

    #[test]
    fn mask_secret() {
        assert_eq!(
            mask_secrets("export MY_SECRET=hidden"),
            "export MY_SECRET=***"
        );
    }

    #[test]
    fn mask_secret_prefix() {
        assert_eq!(mask_secrets("SECRET_VALUE=hidden"), "SECRET_VALUE=***");
    }

    #[test]
    fn mask_secret_exact() {
        assert_eq!(mask_secrets("SECRET=hidden"), "SECRET=***");
    }

    // --- PASSWORD ---

    #[test]
    fn mask_password() {
        assert_eq!(
            mask_secrets("export DB_PASSWORD=pass123"),
            "export DB_PASSWORD=***"
        );
    }

    #[test]
    fn mask_password_prefix() {
        assert_eq!(mask_secrets("PASSWORD_HASH=abc"), "PASSWORD_HASH=***");
    }

    #[test]
    fn mask_password_exact() {
        assert_eq!(mask_secrets("PASSWORD=pass123"), "PASSWORD=***");
    }

    // --- PASSWD ---

    #[test]
    fn mask_passwd() {
        assert_eq!(
            mask_secrets("export MYSQL_PASSWD=pass"),
            "export MYSQL_PASSWD=***"
        );
    }

    #[test]
    fn mask_passwd_exact() {
        assert_eq!(mask_secrets("PASSWD=pass"), "PASSWD=***");
    }

    // --- CREDENTIAL ---

    #[test]
    fn mask_credential() {
        assert_eq!(
            mask_secrets("export AWS_CREDENTIAL=xxx"),
            "export AWS_CREDENTIAL=***"
        );
    }

    #[test]
    fn mask_credential_prefix() {
        assert_eq!(mask_secrets("CREDENTIAL_FILE=/path"), "CREDENTIAL_FILE=***");
    }

    #[test]
    fn mask_credential_exact() {
        assert_eq!(mask_secrets("CREDENTIAL=xxx"), "CREDENTIAL=***");
    }

    // --- AUTH ---

    #[test]
    fn mask_auth() {
        assert_eq!(
            mask_secrets("export MY_AUTH=bearer-xxx"),
            "export MY_AUTH=***"
        );
    }

    #[test]
    fn mask_auth_prefix() {
        assert_eq!(mask_secrets("AUTH_HEADER=Bearer"), "AUTH_HEADER=***");
    }

    #[test]
    fn mask_auth_exact() {
        assert_eq!(mask_secrets("AUTH=xxx"), "AUTH=***");
    }

    // --- PRIVATE ---

    #[test]
    fn mask_private() {
        assert_eq!(
            mask_secrets("export PRIVATE_KEY=-----BEGIN"),
            "export PRIVATE_KEY=***"
        );
    }

    #[test]
    fn mask_private_prefix() {
        assert_eq!(mask_secrets("PRIVATE_DATA=xxx"), "PRIVATE_DATA=***");
    }

    #[test]
    fn mask_private_exact() {
        assert_eq!(mask_secrets("PRIVATE=xxx"), "PRIVATE=***");
    }

    // ========================================================================
    // A-2. キー名ベース: クォート付き値
    // ========================================================================

    #[test]
    fn mask_double_quoted() {
        assert_eq!(
            mask_secrets(r#"export API_KEY="sk-xxx""#),
            "export API_KEY=***"
        );
    }

    #[test]
    fn mask_single_quoted() {
        assert_eq!(
            mask_secrets("export API_KEY='sk-xxx'"),
            "export API_KEY=***"
        );
    }

    // ========================================================================
    // B. トークン値ベース: 全パターン x 検出テスト
    // ========================================================================

    #[test]
    fn mask_openai_sk() {
        let token = "sk-proj-abc123def456ghi789";
        assert_eq!(mask_secrets(token), "***");
    }

    #[test]
    fn mask_openai_sk_short_prefix() {
        let token = "sk-abc123def456ghi789jk";
        assert_eq!(mask_secrets(token), "***");
    }

    #[test]
    fn mask_github_ghp() {
        let token = "ghp_ABCDEFghijklmnopqrstuvwxyz0123456789";
        assert_eq!(mask_secrets(token), "***");
    }

    #[test]
    fn mask_github_gho() {
        let token = "gho_ABCDEFghijklmnopqrstuvwxyz0123456789";
        assert_eq!(mask_secrets(token), "***");
    }

    #[test]
    fn mask_github_pat() {
        let token = "github_pat_ABCDEF1234567890abcdefgh";
        assert_eq!(mask_secrets(token), "***");
    }

    #[test]
    fn mask_slack_xoxb() {
        let token = "xoxb-1234-5678-abcdefghij";
        assert_eq!(mask_secrets(token), "***");
    }

    #[test]
    fn mask_slack_xoxp() {
        let token = "xoxp-1234-5678-abcdefghij";
        assert_eq!(mask_secrets(token), "***");
    }

    #[test]
    fn mask_aws_akia() {
        let token = "AKIAIOSFODNN7EXAMPLE";
        assert_eq!(mask_secrets(token), "***");
    }

    // ========================================================================
    // C. 誤検出回避テスト（非マッチ確認）
    // ========================================================================

    #[test]
    fn no_mask_keyboard() {
        let input = "KEYBOARD=us";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_tokenizer() {
        let input = "TOKENIZER=fast";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_monkey() {
        let input = "MONKEY=banana";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_authorize() {
        let input = "AUTHORIZE=true";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_secretary() {
        let input = "SECRETARY=john";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_privately() {
        let input = "PRIVATELY=true";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_passwords() {
        let input = "PASSWORDS_FILE=/etc/shadow";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_normal_text() {
        let input = "Hello, this is normal text.";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_path() {
        let input = "PATH=/usr/local/bin:/usr/bin";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_home() {
        let input = "HOME=/Users/me";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_editor() {
        let input = "EDITOR=vim";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_short_sk() {
        let input = "sk-abc";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn no_mask_sk_word() {
        let input = "I'll skip this task";
        assert_eq!(mask_secrets(input), input);
    }

    // ========================================================================
    // D. 複合・エッジケーステスト
    // ========================================================================

    #[test]
    fn mask_multiple_secrets_in_text() {
        let input = "\
export PATH=/usr/local/bin:$PATH
export OPENAI_API_KEY=sk-proj-abcdef1234567890
export EDITOR=vim
export DB_PASSWORD='super_secret_pass'
export HOME=/Users/me
AUTH_TOKEN=my-secret-token
";
        let expected = "\
export PATH=/usr/local/bin:$PATH
export OPENAI_API_KEY=***
export EDITOR=vim
export DB_PASSWORD=***
export HOME=/Users/me
AUTH_TOKEN=***
";
        assert_eq!(mask_secrets(input), expected);
    }

    #[test]
    fn mask_mixed_patterns() {
        let input =
            "curl -H \"Authorization: Bearer sk-proj-abc123def456ghi789\" https://api.openai.com";
        let result = mask_secrets(input);
        assert!(!result.contains("sk-proj-abc123def456ghi789"));
        assert!(result.contains("***"));
        assert!(result.contains("curl"));
    }

    #[test]
    fn empty_string() {
        assert_eq!(mask_secrets(""), "");
    }

    #[test]
    fn no_equals_sign() {
        let input = "This is just a normal line of text without any assignments";
        assert_eq!(mask_secrets(input), input);
    }

    #[test]
    fn mask_without_export() {
        assert_eq!(mask_secrets("API_KEY=xxx"), "API_KEY=***");
    }

    // ========================================================================
    // E. contains_secrets: true を返すケース
    // ========================================================================

    // --- 全キーワード ---

    #[test]
    fn contains_secrets_api_key() {
        assert!(contains_secrets("OPENAI_API_KEY=sk-xxx"));
    }

    #[test]
    fn contains_secrets_token() {
        assert!(contains_secrets("AUTH_TOKEN=abc123"));
    }

    #[test]
    fn contains_secrets_secret() {
        assert!(contains_secrets("MY_SECRET=hidden"));
    }

    #[test]
    fn contains_secrets_password() {
        assert!(contains_secrets("DB_PASSWORD=pass123"));
    }

    #[test]
    fn contains_secrets_passwd() {
        assert!(contains_secrets("MYSQL_PASSWD=pass"));
    }

    #[test]
    fn contains_secrets_credential() {
        assert!(contains_secrets("AWS_CREDENTIAL=xxx"));
    }

    #[test]
    fn contains_secrets_auth() {
        assert!(contains_secrets("MY_AUTH=bearer-xxx"));
    }

    #[test]
    fn contains_secrets_private() {
        assert!(contains_secrets("PRIVATE_KEY=-----BEGIN"));
    }

    // --- 全トークンプレフィックス ---

    #[test]
    fn contains_secrets_sk_prefix() {
        assert!(contains_secrets("sk-proj-abc123def456ghi789"));
    }

    #[test]
    fn contains_secrets_ghp_prefix() {
        assert!(contains_secrets("ghp_ABCDEFghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn contains_secrets_gho_prefix() {
        assert!(contains_secrets("gho_ABCDEFghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn contains_secrets_github_pat_prefix() {
        assert!(contains_secrets("github_pat_ABCDEF1234567890abcdefgh"));
    }

    #[test]
    fn contains_secrets_xoxb_prefix() {
        assert!(contains_secrets("xoxb-1234-5678-abcdefghij"));
    }

    #[test]
    fn contains_secrets_xoxp_prefix() {
        assert!(contains_secrets("xoxp-1234-5678-abcdefghij"));
    }

    #[test]
    fn contains_secrets_akia_prefix() {
        assert!(contains_secrets("AKIAIOSFODNN7EXAMPLE"));
    }

    // ========================================================================
    // F. contains_secrets: false を返すケース
    // ========================================================================

    #[test]
    fn contains_secrets_false_path() {
        assert!(!contains_secrets("PATH=/usr/local/bin:/usr/bin"));
    }

    #[test]
    fn contains_secrets_false_home() {
        assert!(!contains_secrets("HOME=/Users/me"));
    }

    #[test]
    fn contains_secrets_false_editor() {
        assert!(!contains_secrets("EDITOR=vim"));
    }

    #[test]
    fn contains_secrets_false_normal_text() {
        assert!(!contains_secrets("Hello, this is normal text."));
    }

    #[test]
    fn contains_secrets_false_empty() {
        assert!(!contains_secrets(""));
    }
}

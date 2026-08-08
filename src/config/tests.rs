use super::{default_max_tokens_for, CompletionConfig, ExternalSetting, JarvishConfig};

fn load_from_str(content: &str) -> JarvishConfig {
    toml::from_str(content).unwrap()
}

#[test]
fn default_config_has_expected_values() {
    let config = JarvishConfig::default();
    assert_eq!(config.ai.model, "gpt-4o");
    assert_eq!(config.ai.provider, "openai");
    assert_eq!(config.ai.max_tokens, None);
    assert_eq!(config.ai.max_rounds, 10);
    assert!(config.ai.markdown_rendering);
    assert!(config.ai.ignore_auto_investigation_cmds.is_empty());
    assert!(config.alias.is_empty());
    assert!(config.export.is_empty());
    assert!(config.prompt.nerd_font);
    assert!(!config.prompt.starship);
    assert!(config
        .completion
        .git_branch_commands
        .contains(&"checkout".to_string()));
    assert!(config
        .completion
        .git_branch_commands
        .contains(&"fetch".to_string()));
    assert_eq!(config.completion.git_branch_commands.len(), 11);
    assert_eq!(config.completion.external, "auto");
    assert_eq!(config.completion.external_timeout_ms, 400);
    assert!(config.completion.external_zsh_daemon);
}

#[test]
fn provider_max_tokens_defaults_are_backward_compatible() {
    assert_eq!(default_max_tokens_for("openai"), 8192);
    assert_eq!(default_max_tokens_for("anthropic"), 16384);
    assert_eq!(default_max_tokens_for("opencode-zen"), 8192);
    assert_eq!(default_max_tokens_for("opencode-go"), 8192);
}

#[test]
fn parse_anthropic_ai_config() {
    let config = load_from_str(
        r#"
[ai]
provider = "anthropic"
model = "claude-sonnet-5"
max_tokens = 16384
base_url = "http://localhost:1234"
api_key_env = "TEST_ANTHROPIC_KEY"
"#,
    );
    assert_eq!(config.ai.provider, "anthropic");
    assert_eq!(config.ai.max_tokens, Some(16384));
    assert_eq!(config.ai.base_url.as_deref(), Some("http://localhost:1234"));
    assert_eq!(config.ai.api_key_env.as_deref(), Some("TEST_ANTHROPIC_KEY"));
}

#[test]
fn parse_opencode_provider_config() {
    let config = load_from_str(
        r#"
[ai]
provider = "opencode-zen"
base_url = "http://localhost:8080/v1"
api_key_env = "OPENCODE_TEST_KEY"
"#,
    );
    assert_eq!(config.ai.provider, "opencode-zen");
    assert_eq!(
        config.ai.base_url.as_deref(),
        Some("http://localhost:8080/v1")
    );
    assert_eq!(config.ai.api_key_env.as_deref(), Some("OPENCODE_TEST_KEY"));
}

#[test]
fn parse_full_config() {
    let toml = r#"
[ai]
model = "gpt-4o-mini"
max_rounds = 5
markdown_rendering = false
ignore_auto_investigation_cmds = ["git log", "git diff"]

[alias]
g = "git"
ll = "ls -la"

[export]
EDITOR = "vim"

[prompt]
nerd_font = false
starship = true
"#;
    let config = load_from_str(toml);
    assert_eq!(config.ai.model, "gpt-4o-mini");
    assert_eq!(config.ai.max_rounds, 5);
    assert!(!config.ai.markdown_rendering);
    assert_eq!(
        config.ai.ignore_auto_investigation_cmds,
        vec!["git log", "git diff"]
    );
    assert_eq!(config.alias.get("g").unwrap(), "git");
    assert_eq!(config.alias.get("ll").unwrap(), "ls -la");
    assert_eq!(config.export.get("EDITOR").unwrap(), "vim");
    assert!(!config.prompt.nerd_font);
    assert!(config.prompt.starship);
}

#[test]
fn parse_partial_config_uses_defaults() {
    let toml = r#"
[alias]
g = "git"
"#;
    let config = load_from_str(toml);
    assert_eq!(config.ai.model, "gpt-4o");
    assert_eq!(config.ai.max_rounds, 10);
    assert!(config.ai.markdown_rendering);
    assert!(config.ai.ignore_auto_investigation_cmds.is_empty());
    assert!(config.prompt.nerd_font);
    assert_eq!(config.alias.get("g").unwrap(), "git");
    assert!(config.export.is_empty());
}

#[test]
fn parse_empty_config() {
    let config = load_from_str("");
    assert_eq!(config.ai.model, "gpt-4o");
    assert_eq!(config.ai.max_rounds, 10);
    assert!(config.alias.is_empty());
    assert!(config.export.is_empty());
}

#[test]
fn parse_ignore_auto_investigation_cmds_single_entry() {
    let toml = r#"
[ai]
ignore_auto_investigation_cmds = ["git"]
"#;
    let config = load_from_str(toml);
    assert_eq!(config.ai.ignore_auto_investigation_cmds, vec!["git"]);
}

#[test]
fn config_path_contains_expected_components() {
    let path = JarvishConfig::config_path();
    let path_str = path.to_string_lossy();
    assert!(path_str.contains(".config/jarvish/config.toml"));
}

#[test]
fn load_returns_default_when_file_missing() {
    let config = JarvishConfig::load();
    assert!(!config.ai.model.is_empty());
    assert!(config.ai.max_rounds > 0);
}

#[test]
fn load_from_valid_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("test.toml");
    std::fs::write(
        &path,
        r#"
[alias]
g = "git"

[export]
EDITOR = "vim"
"#,
    )
    .unwrap();

    let config = JarvishConfig::load_from(&path).unwrap();
    assert_eq!(config.alias.get("g").unwrap(), "git");
    assert_eq!(config.export.get("EDITOR").unwrap(), "vim");
    assert_eq!(config.ai.model, "gpt-4o");
}

#[test]
fn load_from_nonexistent_file_returns_error() {
    let result = JarvishConfig::load_from(std::path::Path::new("/nonexistent/config.toml"));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("failed to read"));
}

#[test]
fn load_from_invalid_toml_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("bad.toml");
    std::fs::write(&path, "this is not valid toml [[[").unwrap();

    let result = JarvishConfig::load_from(&path);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("failed to parse"));
}

#[test]
fn create_default_config_creates_file_and_dirs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("sub/dir/config.toml");

    assert!(!path.exists());
    JarvishConfig::create_default_config(&path);

    assert!(path.exists());

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("[ai]"));
    assert!(content.contains("[alias]"));
    assert!(content.contains("[export]"));
    assert!(content.contains("[completion]"));
    assert!(content.contains("external = \"auto\""));
    assert!(content.contains("external_timeout_ms = 400"));
    assert!(content.contains("external_zsh_daemon = true"));

    let config: JarvishConfig = toml::from_str(&content).unwrap();
    assert_eq!(config.ai.model, "gpt-4o");
    assert!(config.alias.is_empty());
}

#[test]
fn parse_completion_config_custom_commands() {
    let toml = r#"
[completion]
git_branch_commands = ["checkout", "fetch", "pull"]
"#;
    let config = load_from_str(toml);
    assert_eq!(
        config.completion.git_branch_commands,
        vec!["checkout", "fetch", "pull"]
    );
}

#[test]
fn parse_completion_config_empty_commands() {
    let toml = r#"
[completion]
git_branch_commands = []
"#;
    let config = load_from_str(toml);
    assert!(config.completion.git_branch_commands.is_empty());
}

#[test]
fn parse_no_completion_section_uses_default() {
    let config = load_from_str("");
    assert_eq!(config.completion.git_branch_commands.len(), 11);
    assert!(config
        .completion
        .git_branch_commands
        .contains(&"fetch".to_string()));
    assert_eq!(config.completion.external, "auto");
    assert_eq!(config.completion.external_timeout_ms, 400);
}

#[test]
fn parse_completion_config_external_carapace() {
    let toml = r#"
[completion]
external = "carapace"
"#;
    let config = load_from_str(toml);
    assert_eq!(config.completion.external, "carapace");
}

#[test]
fn parse_completion_config_external_none() {
    let toml = r#"
[completion]
external = "none"
"#;
    let config = load_from_str(toml);
    assert_eq!(config.completion.external, "none");
}

#[test]
fn parse_completion_config_external_unknown_value_kept_as_is() {
    // TOML パース自体は文字列をそのまま受け入れる。
    // "auto" への読み替えと警告は ExternalCompletionSettings 構築時（実行時）の責務。
    let toml = r#"
[completion]
external = "bogus"
"#;
    let config = load_from_str(toml);
    assert_eq!(config.completion.external, "bogus");
}

#[test]
fn parse_completion_config_external_zsh_string() {
    let toml = r#"
[completion]
external = "zsh"
"#;
    let config = load_from_str(toml);
    assert_eq!(config.completion.external, "zsh");
}

#[test]
fn parse_completion_config_external_array_form_explicit_order() {
    let toml = r#"
[completion]
external = ["zsh", "carapace"]
"#;
    let config = load_from_str(toml);
    assert_eq!(
        config.completion.external,
        ExternalSetting::List(vec!["zsh".to_string(), "carapace".to_string()])
    );
}

#[test]
fn parse_completion_config_external_array_form_single_entry() {
    let toml = r#"
[completion]
external = ["carapace"]
"#;
    let config = load_from_str(toml);
    assert_eq!(
        config.completion.external,
        ExternalSetting::List(vec!["carapace".to_string()])
    );
}

#[test]
fn parse_completion_config_external_array_form_with_invalid_entry() {
    // 不正な要素を含む配列でも TOML パース自体は成功する（要素単位の
    // 妥当性検査・警告は resolve() の責務、raw_entries() 経由）。
    let toml = r#"
[completion]
external = ["zsh", "bogus", "carapace"]
"#;
    let config = load_from_str(toml);
    assert_eq!(
        config.completion.external,
        ExternalSetting::List(vec![
            "zsh".to_string(),
            "bogus".to_string(),
            "carapace".to_string()
        ])
    );
}

#[test]
fn external_setting_default_is_single_auto_string_form() {
    // 既存の文字列形式との後方互換の起点: デフォルトは Single("auto")。
    assert_eq!(
        ExternalSetting::default(),
        ExternalSetting::Single("auto".to_string())
    );
    assert_eq!(ExternalSetting::default(), "auto");
}

#[test]
fn external_setting_raw_entries_single_returns_one_element() {
    let setting = ExternalSetting::Single("carapace".to_string());
    assert_eq!(setting.raw_entries(), vec!["carapace"]);
}

#[test]
fn external_setting_raw_entries_list_returns_all_elements_in_order() {
    let setting = ExternalSetting::List(vec!["zsh".to_string(), "carapace".to_string()]);
    assert_eq!(setting.raw_entries(), vec!["zsh", "carapace"]);
}

#[test]
fn external_setting_display_single_matches_raw_string() {
    assert_eq!(
        ExternalSetting::Single("auto".to_string()).to_string(),
        "auto"
    );
}

#[test]
fn external_setting_display_list_shows_bracketed_order() {
    let setting = ExternalSetting::List(vec!["zsh".to_string(), "carapace".to_string()]);
    assert_eq!(setting.to_string(), r#"["zsh", "carapace"]"#);
}

#[test]
fn parse_completion_config_custom_external_timeout_ms() {
    let toml = r#"
[completion]
external_timeout_ms = 1500
"#;
    let config = load_from_str(toml);
    assert_eq!(config.completion.external_timeout_ms, 1500);
}

#[test]
fn external_zsh_daemon_defaults_to_true() {
    assert!(CompletionConfig::default().external_zsh_daemon);
}

#[test]
fn parse_completion_config_external_zsh_daemon_explicit_false() {
    let toml = r#"
[completion]
external_zsh_daemon = false
"#;
    let config = load_from_str(toml);
    assert!(!config.completion.external_zsh_daemon);
}

#[test]
fn parse_completion_config_external_zsh_daemon_explicit_true() {
    let toml = r#"
[completion]
external_zsh_daemon = true
"#;
    let config = load_from_str(toml);
    assert!(config.completion.external_zsh_daemon);
}

#[test]
fn parse_completion_config_without_external_zsh_daemon_key_defaults_true() {
    // 後方互換: 既存の config.toml（キー未記載）でもパースが失敗せず、
    // デフォルト値 true が使われる。
    let toml = r#"
[completion]
external_timeout_ms = 1500
"#;
    let config = load_from_str(toml);
    assert!(config.completion.external_zsh_daemon);
    assert_eq!(config.completion.external_timeout_ms, 1500);
}

#[test]
fn parse_config_without_completion_section_at_all_defaults_zsh_daemon_true() {
    // completion セクション自体が存在しない設定ファイル（旧バージョン）
    // でもパースが失敗しないことの確認。
    let config = load_from_str("");
    assert!(config.completion.external_zsh_daemon);
}

#[test]
fn parse_startup_commands() {
    let toml = r#"
[startup]
commands = ["echo hello", "cd /tmp"]
"#;
    let config = load_from_str(toml);
    assert_eq!(config.startup.commands.len(), 2);
    assert_eq!(config.startup.commands[0], "echo hello");
    assert_eq!(config.startup.commands[1], "cd /tmp");
}

#[test]
fn parse_startup_commands_empty() {
    let toml = r#"
[startup]
commands = []
"#;
    let config = load_from_str(toml);
    assert!(config.startup.commands.is_empty());
}

#[test]
fn parse_no_startup_section_uses_default() {
    let config = load_from_str("");
    assert!(config.startup.commands.is_empty());
}

#[test]
fn default_config_has_empty_startup_commands() {
    let config = JarvishConfig::default();
    assert!(config.startup.commands.is_empty());
}

#[test]
fn default_template_contains_startup_section() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    JarvishConfig::create_default_config(&path);
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("[startup]"));
}

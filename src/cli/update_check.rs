//! 起動時バージョンチェック通知
//!
//! バックグラウンドで GitHub Releases API をチェックし、
//! 新しいバージョンが利用可能な場合にバナーを表示する。
//! 24時間以内にチェック済みの場合はスキップする。

use std::path::PathBuf;

use tracing::{debug, info, warn};

/// キャッシュファイルのパス: `~/.config/jarvish/update_check.json`
fn cache_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "jarvish")
        .map(|p| p.config_dir().join("update_check.json"))
}

/// キャッシュの有効期間（24時間）
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// キャッシュデータ
#[derive(serde::Serialize, serde::Deserialize)]
struct UpdateCache {
    /// 最後にチェックした時刻（Unix epoch秒）
    checked_at: u64,
    /// 最新バージョン（"1.7.0" 形式、vプレフィックスなし）
    latest_version: String,
}

/// キャッシュを読み込む。有効期限内であれば最新バージョンを返す。
fn read_cache() -> Option<String> {
    let path = cache_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let cache: UpdateCache = serde_json::from_str(&content).ok()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();

    if now - cache.checked_at < CACHE_TTL_SECS {
        debug!(
            latest = %cache.latest_version,
            age_secs = now - cache.checked_at,
            "Using cached update check result"
        );
        Some(cache.latest_version)
    } else {
        debug!("Update check cache expired");
        None
    }
}

/// キャッシュに最新バージョンを書き込む。
fn write_cache(version: &str) {
    let Some(path) = cache_path() else { return };

    // 親ディレクトリが存在しない場合は作成
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let cache = UpdateCache {
        checked_at: now,
        latest_version: version.to_string(),
    };

    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = std::fs::write(&path, json);
        debug!(path = %path.display(), "Update check cache written");
    }
}

/// バックグラウンドでバージョンチェックを行い、新バージョンがあれば通知文字列を返す。
///
/// `Shell::run()` から `tokio::spawn` で呼び出される。
pub async fn check_for_update_notification() -> Option<String> {
    let current = env!("CARGO_PKG_VERSION");

    // キャッシュが有効ならそれを使う
    if let Some(latest) = read_cache() {
        return build_notification(current, &latest);
    }

    // GitHub Releases API にアクセス
    let latest = match tokio::task::spawn_blocking(fetch_latest_version).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            debug!(error = %e, "Background update check failed");
            return None;
        }
        Err(e) => {
            warn!(error = %e, "Background update check task panicked");
            return None;
        }
    };

    // キャッシュに書き込み
    write_cache(&latest);

    build_notification(current, &latest)
}

/// GitHub Releases API で最新バージョンを取得する。
fn fetch_latest_version() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    info!("Checking for updates from GitHub Releases...");

    let release = self_update::backends::github::Update::configure()
        .repo_owner("tominaga-h")
        .repo_name("jarvis-shell")
        .bin_name("jarvish")
        .current_version(self_update::cargo_crate_version!())
        .build()?;

    let latest = release.get_latest_release()?;
    let version = latest.version.trim_start_matches('v').to_string();
    info!(latest_version = %version, "Update check complete");
    Ok(version)
}

/// バージョン比較を行い、通知メッセージを組み立てる。
fn build_notification(current: &str, latest: &str) -> Option<String> {
    let latest_clean = latest.trim_start_matches('v');

    if latest_clean == current {
        return None;
    }

    // semver の簡易比較（major.minor.patch）
    let current_parts: Vec<u32> = current.split('.').filter_map(|s| s.parse().ok()).collect();
    let latest_parts: Vec<u32> = latest_clean
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();

    if latest_parts <= current_parts {
        return None;
    }

    // Homebrew インストールかどうかでメッセージを変える
    let is_homebrew = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .map(|s| s.contains("/Cellar/") || s.contains("/homebrew/"))
        .unwrap_or(false);

    let update_cmd = if is_homebrew {
        "`brew upgrade jarvish`"
    } else {
        "`update`"
    };

    Some(format!(
        "  New version available: v{latest_clean} (current: v{current}). Run {update_cmd} to update."
    ))
}

mod flag_file;
mod homebrew;
mod local_binary;
mod release;
mod version;

#[cfg(test)]
mod tests;

pub use flag_file::check_update_flag;
#[cfg(test)]
pub use flag_file::write_update_flag_for_test;

use clap::Parser;

use crate::engine::CommandResult;

/// update: jarvish を最新バージョンに更新する。
#[derive(Parser)]
#[command(name = "update", about = "Update jarvish to the latest version")]
struct UpdateArgs {
    /// Check for updates without installing
    #[arg(long)]
    check: bool,

    /// Update from a local binary instead of GitHub Releases.
    /// Optionally specify the path to the binary (default: target/release/jarvish).
    #[arg(long)]
    local: Option<Option<String>>,
}

/// update: GitHub Releases またはローカルバイナリから更新する。
///
/// `--local` オプションでローカルビルドのバイナリを使った更新が可能。
/// Homebrew でインストールされている場合は `brew upgrade jarvish` を案内する。
pub(super) fn execute(args: &[&str]) -> CommandResult {
    let parsed = match super::parse_args::<UpdateArgs>("update", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    // --local が指定された場合はローカルバイナリから更新
    if let Some(local_path) = parsed.local {
        let path = local_binary::resolve_local_binary_path(local_path.as_deref());
        if parsed.check {
            return local_binary::check_for_local_updates(&path);
        }
        return local_binary::perform_local_update(&path);
    }

    if homebrew::is_homebrew_install() {
        return homebrew::handle_homebrew_update(parsed.check);
    }

    if parsed.check {
        return release::check_for_updates();
    }

    release::perform_update()
}

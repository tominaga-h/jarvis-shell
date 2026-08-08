//! carapace 連携 — 外部補完プログラム carapace-bin をブリッジする Provider

mod binary;
mod candidates;
mod core;
mod parsing;
#[cfg(test)]
mod tests;

pub use binary::{
    format_external_binaries_display, format_external_summary, ExternalCompletionSettings,
    ExternalKind,
};
#[allow(unused_imports)] // re-exported for cli::completer and zsh_bridge tests
pub(crate) use binary::{gate, ResolvedExternal};
pub(crate) use core::CarapaceProvider;

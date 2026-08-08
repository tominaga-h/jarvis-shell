//! JSON parsing for carapace export output.

use serde::Deserialize;

/// carapace の JSON 出力全体（`#[serde(default)]` で壊れたフィールドがあっても
/// パース自体は失敗させない — resilient parsing）。
#[derive(Debug, Default, Deserialize)]
pub(super) struct CarapaceExport {
    #[serde(default)]
    pub(super) nospace: String,
    #[serde(default)]
    pub(super) values: Vec<CarapaceValue>,
}

/// carapace の `values[]` 内の 1 要素。
#[derive(Debug, Default, Deserialize)]
pub(super) struct CarapaceValue {
    #[serde(default)]
    pub(super) value: String,
    #[serde(default)]
    pub(super) description: String,
}

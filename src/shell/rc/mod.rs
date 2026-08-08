mod execution;
mod file_reading;
mod options;
mod parsing;
mod resolution;
mod template;

pub use options::RcOptions;
pub(super) use options::RcOutcome;

#[cfg(test)]
mod tests;

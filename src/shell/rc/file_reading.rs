use std::io;
use std::path::Path;

/// rc スクリプト読み込みの上限（バイト）。
pub(in crate::shell) const MAX_RC_FILE_SIZE: u64 = 1024 * 1024; // 1 MiB

/// rc スクリプト読み込みで実際に使われるガード付きリーダー。
pub(super) fn read_rc_file_guarded(path: &Path) -> io::Result<String> {
    let metadata = std::fs::metadata(path)?;

    if metadata.is_dir() {
        return Err(io::Error::other(format!(
            "{} is a directory",
            path.display()
        )));
    }
    if !metadata.is_file() {
        // FIFO・ソケット・デバイスファイル等（symlink は metadata() が
        // たどった先の種別で判定されるため、ここには来ない）。
        return Err(io::Error::other(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_RC_FILE_SIZE {
        return Err(io::Error::other(format!(
            "{} is too large ({} bytes, limit is {} bytes)",
            path.display(),
            metadata.len(),
            MAX_RC_FILE_SIZE
        )));
    }

    std::fs::read_to_string(path)
}

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

/// Git のようなコンテンツアドレッサブルストレージ。
/// テキストを SHA-256 でハッシュ化し、zstd 圧縮して保存する。
pub struct BlobStore {
    base_dir: PathBuf,
}

impl BlobStore {
    /// 指定されたベースディレクトリで BlobStore を初期化する。
    /// ディレクトリが存在しない場合は作成する。
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&base_dir)
            .with_context(|| format!("failed to create blob directory: {}", base_dir.display()))?;
        Ok(Self { base_dir })
    }

    /// テキストを SHA-256 ハッシュ化・zstd 圧縮して Blob として保存する。
    /// ハッシュ文字列を返す。空文字列の場合は None を返す。
    pub fn store(&self, content: &str) -> Result<Option<String>> {
        if content.is_empty() {
            return Ok(None);
        }

        let hash = Self::sha256_hex(content);
        let blob_path = self.blob_path(&hash);

        // 同一ハッシュの Blob が既に存在する場合はスキップ（冪等）
        if blob_path.exists() {
            return Ok(Some(hash));
        }

        // ディレクトリ作成（先頭2文字のサブディレクトリ）
        if let Some(parent) = blob_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create blob subdirectory: {}", parent.display())
            })?;
        }

        // zstd 圧縮して書き込み
        let compressed =
            zstd::encode_all(content.as_bytes(), 3).context("failed to compress blob content")?;
        fs::write(&blob_path, &compressed)
            .with_context(|| format!("failed to write blob: {}", blob_path.display()))?;

        Ok(Some(hash))
    }

    /// ハッシュを指定して Blob を読み込み、展開したテキストを返す。
    /// Phase 3 (AI Context Retrieval) で使用する。
    #[allow(dead_code)]
    pub fn load(&self, hash: &str) -> Result<String> {
        let blob_path = self.blob_path(hash);
        let compressed = fs::read(&blob_path)
            .with_context(|| format!("failed to read blob: {}", blob_path.display()))?;

        let mut decoder = zstd::Decoder::new(compressed.as_slice())
            .context("failed to initialize zstd decoder")?;
        let mut decompressed = String::new();
        decoder
            .read_to_string(&mut decompressed)
            .context("failed to decompress blob content")?;

        Ok(decompressed)
    }

    /// SHA-256 ハッシュの16進文字列を計算する。
    fn sha256_hex(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// ハッシュ文字列から Blob ファイルのパスを返す。
    /// Git と同様に先頭2文字をディレクトリ名として使用する。
    /// 例: "abcdef1234..." → "blobs/ab/cdef1234..."
    fn blob_path(&self, hash: &str) -> PathBuf {
        let (prefix, rest) = hash.split_at(2);
        self.base_dir.join(prefix).join(rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn store_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(tmp.path().join("blobs")).unwrap();

        let content = "Hello, Jarvis!\nThis is a test output.";
        let hash = store.store(content).unwrap().expect("should return hash");

        // ハッシュが64文字の16進文字列であることを確認
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        // ラウンドトリップ：保存した内容が復元できることを確認
        let loaded = store.load(&hash).unwrap();
        assert_eq!(loaded, content);
    }

    #[test]
    fn store_empty_returns_none() {
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(tmp.path().join("blobs")).unwrap();

        let result = store.store("").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn store_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(tmp.path().join("blobs")).unwrap();

        let content = "duplicate content";
        let hash1 = store.store(content).unwrap().unwrap();
        let hash2 = store.store(content).unwrap().unwrap();

        // 同一コンテンツは同一ハッシュを返す
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn blob_file_uses_prefix_directory() {
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(tmp.path().join("blobs")).unwrap();

        let content = "test content for path check";
        let hash = store.store(content).unwrap().unwrap();

        // blobs/{先頭2文字}/{残り} のパスにファイルが存在することを確認
        let (prefix, rest) = hash.split_at(2);
        let expected_path = tmp.path().join("blobs").join(prefix).join(rest);
        assert!(expected_path.exists());
    }

    #[test]
    fn load_nonexistent_blob_returns_error() {
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(tmp.path().join("blobs")).unwrap();

        let result = store.load("0000000000000000000000000000000000000000000000000000000000000000");
        assert!(result.is_err());
    }
}

//! Attachment files on disk, named by their SHA-256 (`<dir>/ab/abcdef…`); identical uploads share one file.
use std::path::{Path, PathBuf};

use axum::extract::multipart::Field;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

pub struct Storage {
    root: PathBuf,
    /// Largest file accepted.
    pub max_bytes: u64,
}

/// A file just written to storage.
pub struct Stored {
    pub sha256: String,
    pub size: u64,
    pub content_type: &'static str,
}

impl Storage {
    pub fn new(root: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self { root: root.into(), max_bytes }
    }

    /// Creates the directories; fails early when the volume is missing or read-only.
    pub async fn init(&self) -> std::io::Result<()> {
        tokio::fs::create_dir_all(self.root.join("tmp")).await
    }

    pub fn path(&self, sha256: &str) -> PathBuf {
        self.root.join(&sha256[..2]).join(sha256)
    }

    /// Streams an upload to disk while hashing it, checks its size and type, and moves it into place.
    pub async fn store(&self, field: &mut Field<'_>) -> ApiResult<Stored> {
        let tmp = self.root.join("tmp").join(Uuid::now_v7().to_string());
        let result = self.write(field, &tmp).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        let (sha256, size, head) = result?;
        let Some(content_type) = sniff(&head) else {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(ApiError::UnsupportedMediaType(
                "only images (PNG, JPEG, GIF, WebP, HEIC, AVIF) and videos (MP4, MOV, WebM) are accepted".to_owned(),
            ));
        };
        let path = self.path(&sha256);
        let moved = async {
            tokio::fs::create_dir_all(path.parent().expect("has a parent")).await?;
            tokio::fs::rename(&tmp, &path).await
        };
        if let Err(e) = moved.await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(anyhow::Error::from(e).context("storing an attachment").into());
        }
        Ok(Stored { sha256, size, content_type })
    }

    async fn write(&self, field: &mut Field<'_>, tmp: &Path) -> ApiResult<(String, u64, Vec<u8>)> {
        tokio::fs::create_dir_all(self.root.join("tmp")).await.map_err(anyhow::Error::from)?;
        let mut file = tokio::fs::File::create(tmp).await.map_err(anyhow::Error::from)?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut head = Vec::with_capacity(32);
        while let Some(chunk) = field.chunk().await.map_err(|e| ApiError::bad_request(e.body_text()))? {
            size += chunk.len() as u64;
            if size > self.max_bytes {
                return Err(ApiError::PayloadTooLarge(format!(
                    "files can be at most {} MB",
                    self.max_bytes / 1024 / 1024
                )));
            }
            if head.len() < 32 {
                head.extend(chunk.iter().take(32 - head.len()));
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(anyhow::Error::from)?;
        }
        file.flush().await.map_err(anyhow::Error::from)?;
        if size == 0 {
            return Err(ApiError::bad_request("the file is empty"));
        }
        Ok((hex::encode(hasher.finalize()), size, head))
    }

    /// Deletes the files no attachment refers to any more.
    pub async fn remove_unused(&self, db: &PgPool, hashes: Vec<String>) {
        for sha256 in hashes {
            let used: Result<bool, _> =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM attachments WHERE sha256 = $1)")
                    .bind(&sha256)
                    .fetch_one(db)
                    .await;
            if let Ok(false) = used
                && let Err(e) = tokio::fs::remove_file(self.path(&sha256)).await
            {
                tracing::warn!(error = %e, %sha256, "removing an attachment file failed");
            }
        }
    }
}

/// The media type, from the file's first bytes; `None` for anything but images and videos.
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    Some(match head {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [0x1A, 0x45, 0xDF, 0xA3, ..] => "video/webm",
        [_, _, _, _, b'f', b't', b'y', b'p', brand @ ..] if brand.len() >= 4 => match &brand[..4] {
            b"qt  " => "video/quicktime",
            b"heic" | b"heix" | b"mif1" | b"msf1" | b"heif" => "image/heic",
            b"avif" | b"avis" => "image/avif",
            _ => "video/mp4",
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::sniff;

    #[test]
    fn sniffs_images_and_videos() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(sniff(b"\xff\xd8\xff\xe0...."), Some("image/jpeg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"\0\0\0\x20ftypisom\0\0\x02\0"), Some("video/mp4"));
        assert_eq!(sniff(b"\0\0\0\x14ftypqt  \0\0\0\0"), Some("video/quicktime"));
        assert_eq!(sniff(b"\0\0\0\x18ftypheic\0\0\0\0"), Some("image/heic"));
        assert_eq!(sniff(b"\x1a\x45\xdf\xa3\x01\0\0\0"), Some("video/webm"));
        assert_eq!(sniff(b"%PDF-1.7\n"), None);
        assert_eq!(sniff(b"<svg xmlns"), None);
        assert_eq!(sniff(b""), None);
    }
}

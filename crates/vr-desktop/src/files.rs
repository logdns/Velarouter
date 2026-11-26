//! File transfer state for the desktop viewer.
//!
//! Downloads land in `~/Downloads` through a `.part` file so an interrupted
//! transfer never leaves a truncated file behind; uploads stream from disk in
//! 256 KiB chunks and are verified by the agent, which answers with the SHA-256
//! it actually stored.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vr_proto::FileHeader;

const CHUNK: usize = 256 * 1024;

pub fn downloads_dir() -> PathBuf {
    // `HOME` is usually unset on Windows; without this every download landed in
    // %TEMP% and looked like it "did not work".
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    if let Some(home) = home {
        let downloads = PathBuf::from(&home).join("Downloads");
        if downloads.is_dir() {
            return downloads;
        }
        if PathBuf::from(&home).is_dir() {
            return PathBuf::from(home);
        }
    }
    std::env::temp_dir()
}

#[cfg(test)]
mod roundtrip;

#[cfg(test)]
mod path_tests {
    #[test]
    fn downloads_directory_exists() {
        assert!(super::downloads_dir().is_dir());
    }
}

/// `report.txt` → `report (1).txt` when the name is taken.
fn unique(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.to_string(), String::new()),
    };
    for index in 1..1000 {
        let path = dir.join(format!("{stem} ({index}){ext}"));
        if !path.exists() {
            return path;
        }
    }
    candidate
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or("download")
}

pub struct Download {
    pub id: u32,
    pub label: String,
    pub target: PathBuf,
    part: PathBuf,
    file: tokio::fs::File,
    hasher: Sha256,
    pub written: u64,
    pub reported: u64,
    pub total: u64,
}

impl Download {
    pub async fn start(id: u32, remote: &str, save_dir: Option<&Path>) -> Result<Self> {
        let name = basename(remote).to_string();
        let dir = match save_dir {
            Some(dir) => {
                anyhow::ensure!(dir.is_dir(), "所选本地目录不存在：{}", dir.display());
                dir.to_path_buf()
            }
            None => downloads_dir(),
        };
        let target = unique(&dir, &name);
        let part = target.with_extension(format!(
            "{}part",
            target
                .extension()
                .map(|e| format!("{}.", e.to_string_lossy()))
                .unwrap_or_default()
        ));
        let file = tokio::fs::File::create(&part)
            .await
            .with_context(|| format!("cannot create {}", part.display()))?;
        Ok(Self {
            id,
            label: format!("下载至 {}", target.display()),
            target,
            part,
            file,
            hasher: Sha256::new(),
            written: 0,
            reported: 0,
            total: 0,
        })
    }

    pub async fn write(&mut self, payload: &[u8]) -> Result<u64> {
        self.file.write_all(payload).await?;
        self.hasher.update(payload);
        self.written += payload.len() as u64;
        Ok(self.written)
    }

    /// Verifies the digest the agent reported and moves the `.part` into place.
    pub async fn finish(&mut self, expected: &str) -> bool {
        let _ = self.file.flush().await;
        let digest = std::mem::take(&mut self.hasher).finalize();
        let actual = hex(&digest);
        if !expected.is_empty() && !actual.eq_ignore_ascii_case(expected) {
            let _ = tokio::fs::remove_file(&self.part).await;
            return false;
        }
        tokio::fs::rename(&self.part, &self.target).await.is_ok()
    }

    pub async fn discard(&self) {
        let _ = tokio::fs::remove_file(&self.part).await;
    }
}

pub struct Upload {
    pub id: u32,
    pub name: String,
    file: tokio::fs::File,
    pub total: u64,
    pub sent: u64,
    pub reported: u64,
    seq: u32,
}

impl Upload {
    pub async fn start(id: u32, local: PathBuf) -> Result<Self> {
        let meta = tokio::fs::metadata(&local).await?;
        anyhow::ensure!(meta.is_file(), "只能上传文件，暂不支持文件夹");
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "upload.bin".into());
        let file = tokio::fs::File::open(&local).await?;
        Ok(Self {
            id,
            name,
            file,
            total: meta.len(),
            sent: 0,
            reported: 0,
            seq: 0,
        })
    }

    /// Keep the state after the LAST chunk until the agent confirms its rename.
    pub fn finished_sending(&self) -> bool {
        self.seq > 0 && self.sent == self.total
    }

    /// Returns the next chunk with its header, or `None` at end of file.
    pub async fn next_chunk(&mut self) -> Option<Result<(FileHeader, Vec<u8>, u64)>> {
        if self.finished_sending() {
            return None;
        }
        let mut buf = vec![0u8; CHUNK];
        let read = match self.file.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => return Some(Err(e.into())),
        };
        if read == 0 && self.total != 0 {
            return Some(Err(anyhow::anyhow!("上传期间文件已缩短")));
        }
        buf.truncate(read);
        let offset = self.sent;
        let done = offset + read as u64;
        if done > self.total {
            return Some(Err(anyhow::anyhow!("上传期间文件大小已改变")));
        }
        let last = done == self.total;
        self.seq += 1;
        let header = FileHeader::new(self.id, offset, self.seq, read as u32, last);
        self.sent = done;
        Some(Ok((header, buf, done)))
    }
}

/// `1.4 MB`-style sizes for the transfer UI.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_do_not_clobber() {
        let dir = std::env::temp_dir().join(format!(
            "vr-dl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique(&dir, "report.pdf");
        assert_eq!(first.file_name().unwrap(), "report.pdf");
        std::fs::write(&first, b"x").unwrap();
        let second = unique(&dir, "report.pdf");
        assert_eq!(second.file_name().unwrap(), "report (1).pdf");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn basename_handles_both_separators() {
        assert_eq!(basename("/home/u/a.txt"), "a.txt");
        assert_eq!(basename(r"C:\Users\u\a.txt"), "a.txt");
        assert_eq!(basename("plain.txt"), "plain.txt");
    }

    #[tokio::test]
    async fn upload_streams_and_marks_the_last_chunk() {
        let path = std::env::temp_dir().join(format!(
            "vr-up-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::write(&path, vec![7u8; CHUNK + 10]).unwrap();
        let mut upload = Upload::start(1, path.clone()).await.unwrap();
        let (h1, d1, done1) = upload.next_chunk().await.unwrap().unwrap();
        assert_eq!(d1.len(), CHUNK);
        assert!(!h1.is_last());
        assert_eq!(done1 as usize, CHUNK);
        let (h2, d2, _) = upload.next_chunk().await.unwrap().unwrap();
        assert_eq!(d2.len(), 10);
        assert!(h2.is_last());
        assert!(upload.next_chunk().await.is_none());
        let _ = std::fs::remove_file(path);
    }
}

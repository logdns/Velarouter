//! File transfer, borrowed from the UUYC model but simplified.
//!
//! UUYC 4.48.6 ships `server_ft_task_manager`, `server_ft_sender_task`,
//! `server_ft_receiver_task`, an FTP-style v2 protocol with resumable and
//! "speedy" modes, 13 result codes and a `.downloading` temp suffix. That is a
//! lot of machinery for what is fundamentally: list → stream chunks → verify.
//!
//! Velarouter keeps the useful parts (chunked transfer, resume offset, temp
//! file, SHA-256 verification, cancellation) and drops the rest. The protocol
//! is documented in `docs/PROTOCOL.md` §6.9.
//!
//! Security: every path is resolved inside a single shared root. `..`, absolute
//! paths and symlinks that escape the root are rejected. The root is chosen at
//! startup (`--files`), never by the viewer.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use vr_proto::FileEntry;

/// Owns the shared directory and enforces containment.
#[derive(Debug, Clone)]
pub struct FileService {
    root: PathBuf,
}

impl FileService {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root)
            .with_context(|| format!("cannot create file root {}", root.display()))?;
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot resolve file root {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Rejects anything that is not a clean relative path.
    fn relative(rel: &str) -> Result<PathBuf> {
        if rel.contains('\0') {
            bail!("path contains NUL");
        }
        let mut out = PathBuf::new();
        for component in Path::new(rel.trim_start_matches('/')).components() {
            match component {
                Component::Normal(part) => out.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    bail!("path escapes the shared root");
                }
            }
        }
        Ok(out)
    }

    /// Resolves an existing file or directory and verifies containment
    /// *after* symlink resolution.
    pub fn existing(&self, rel: &str) -> Result<PathBuf> {
        let candidate = self.root.join(Self::relative(rel)?);
        let resolved = candidate
            .canonicalize()
            .with_context(|| format!("no such file: {rel}"))?;
        if !resolved.starts_with(&self.root) {
            bail!("path escapes the shared root");
        }
        Ok(resolved)
    }

    /// Resolves a destination for an upload. The parent must exist inside the
    /// root; the file itself may not, so we canonicalise the parent instead.
    pub fn destination(&self, rel: &str, overwrite: bool) -> Result<(PathBuf, PathBuf)> {
        let rel = Self::relative(rel)?;
        let name = rel
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("upload needs a file name"))?
            .to_owned();
        let parent_rel = rel.parent().unwrap_or(Path::new(""));
        let parent = self.root.join(parent_rel);
        let parent = parent
            .canonicalize()
            .with_context(|| format!("no such directory: {}", parent_rel.display()))?;
        if !parent.starts_with(&self.root) {
            bail!("path escapes the shared root");
        }
        let target = parent.join(&name);
        if target.exists() && !overwrite {
            bail!("refusing to overwrite {}", target.display());
        }
        let part = parent.join(format!(".{}.velarouter-part", name.to_string_lossy()));
        Ok((target, part))
    }

    /// Lists one directory, sorted directories-first then by name.
    pub fn list(&self, rel: &str) -> Result<Vec<FileEntry>> {
        let dir = self.existing(rel)?;
        if !dir.is_dir() {
            bail!("not a directory: {rel}");
        }
        let mut entries = Vec::new();
        for item in std::fs::read_dir(&dir)? {
            let item = item?;
            let meta = match item.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let name = item.file_name().to_string_lossy().to_string();
            if name == "." || name == ".." {
                continue;
            }
            let child_rel = if rel.trim_matches('/').is_empty() {
                name.clone()
            } else {
                format!("{}/{}", rel.trim_end_matches('/'), name)
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::MetadataExt;
                meta.mode()
            };
            #[cfg(not(unix))]
            let mode = 0;
            entries.push(FileEntry {
                name,
                path: child_rel,
                is_dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                mtime,
                mode,
            });
            // Cap the listing so a huge directory cannot blow up a control frame.
            if entries.len() >= 4000 {
                break;
            }
        }
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vr-files-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_inside_root_and_hides_nothing_outside() {
        let root = temp_root("list");
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        let svc = FileService::new(&root).unwrap();
        let entries = svc.list("").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_dir, "directories sort first");
        assert_eq!(entries[1].name, "a.txt");
        assert_eq!(entries[1].size, 5);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        let root = temp_root("traversal");
        let svc = FileService::new(&root).unwrap();
        for bad in ["../etc/passwd", "/etc/passwd", "a/../../b", ".."] {
            assert!(svc.existing(bad).is_err(), "must reject {bad}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_symlink_escape() {
        #[cfg(unix)]
        {
            let root = temp_root("symlink");
            let outside = temp_root("outside");
            std::fs::write(outside.join("secret.txt"), b"nope").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
            let svc = FileService::new(&root).unwrap();
            assert!(svc.existing("link/secret.txt").is_err());
            let _ = std::fs::remove_dir_all(root);
            let _ = std::fs::remove_dir_all(outside);
        }
    }

    #[test]
    fn destination_requires_overwrite_consent() {
        let root = temp_root("dest");
        std::fs::write(root.join("exists.txt"), b"x").unwrap();
        let svc = FileService::new(&root).unwrap();
        assert!(svc.destination("exists.txt", false).is_err());
        assert!(svc.destination("exists.txt", true).is_ok());
        let (target, part) = svc.destination("new.txt", false).unwrap();
        assert!(target.ends_with("new.txt"));
        assert!(part.to_string_lossy().contains("velarouter-part"));
        let _ = std::fs::remove_dir_all(root);
    }
}

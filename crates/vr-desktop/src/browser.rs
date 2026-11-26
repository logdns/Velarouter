//! Local side of the two-pane file transfer window.
//!
//! Deliberately minimal: list one directory, hand back paths. All transfer
//! decisions stay in the UI so the two panes behave the same way.

use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct LocalEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

/// Directories first, then names, case-insensitive.
pub fn list(dir: &Path) -> std::io::Result<Vec<LocalEntry>> {
    let mut entries: Vec<LocalEntry> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let meta = entry.metadata().ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            Some(LocalEntry {
                name,
                path,
                is_dir: meta.is_dir(),
                size: meta.len(),
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Only apply an explicit, readable existing directory. Never silently use the CWD.
pub fn local_folder(text: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err("请输入本机已有目录的完整绝对路径".into());
    }
    std::fs::read_dir(&path).map_err(|e| format!("无法读取 {}：{e}", path.display()))?;
    Ok(path)
}

/// User-facing remote paths are relative to the share, never the viewer's filesystem.
pub fn remote_folder(text: &str) -> Result<String, String> {
    let path = text.replace('\\', "/");
    if path.starts_with('/')
        || path.as_bytes().get(1) == Some(&b':')
        || path.contains('\0')
        || path.split('/').any(|p| p == "..")
    {
        return Err("请输入共享根内的相对路径（如 项目/收件），不能指定盘符、绝对路径或 ..".into());
    }
    Ok(path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect::<Vec<_>>()
        .join("/"))
}

pub fn parent(dir: &Path) -> Option<PathBuf> {
    dir.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_remote_paths_are_relative_to_share() {
        assert_eq!(remote_folder(r"项目\收 件//./").unwrap(), "项目/收 件");
        assert_eq!(remote_folder("").unwrap(), "");
        for bad in ["../other", "a/../b", "/tmp", r"C:\tmp", r"\\host\share"] {
            assert!(remote_folder(bad).is_err());
        }
    }

    #[test]
    fn typed_local_folder_requires_readable_absolute_directory() {
        assert!(local_folder(".").is_err());
        assert!(local_folder(std::env::temp_dir().to_str().unwrap()).is_ok());
        assert!(local_folder(std::env::current_exe().unwrap().to_str().unwrap()).is_err());
    }

    #[test]
    fn missing_directory_is_not_an_empty_listing() {
        assert!(
            list(&std::env::temp_dir().join(format!("missing-{}", rand::random::<u64>()))).is_err()
        );
    }

    #[test]
    fn listing_puts_directories_first() {
        let dir = std::env::temp_dir().join(format!("vr-browse-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("zeta")).unwrap();
        std::fs::write(dir.join("alpha.txt"), b"x").unwrap();
        let entries = list(&dir).unwrap();
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "alpha.txt");
        let _ = std::fs::remove_dir_all(dir);
    }
}

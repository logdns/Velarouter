//! File-transfer state machine shared by the agent session loop.
//!
//! Mirrors the useful half of UUYC's `server_ft_*` tasks: chunked streaming,
//! resume offsets, temp files, SHA-256 verification, progress and cancellation.
//! It deliberately does **not** implement an FTP command channel, plugin
//! installation or a FUSE mount — those are product features, not transport.

use std::collections::HashMap;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use vr_proto::{file_flags, Control, FileHeader, FILE_CHUNK};

use crate::files::FileService;

/// Messages produced by transfer tasks and consumed by the session loop.
#[derive(Debug)]
pub enum FileOut {
    Chunk {
        transfer_id: u32,
        offset: u64,
        seq: u32,
        last: bool,
        data: Vec<u8>,
    },
    Done {
        transfer_id: u32,
        path: String,
        sha256: String,
        total: u64,
    },
    Error {
        transfer_id: u32,
        code: &'static str,
        message: String,
    },
}

struct PushState {
    file: File,
    target: PathBuf,
    part: PathBuf,
    written: u64,
    total: u64,
    last_progress: u64,
    hasher: Sha256,
}

/// Owns in-flight uploads and spawns downloads.
pub struct TransferManager {
    tx: mpsc::Sender<FileOut>,
    pushes: HashMap<u32, PushState>,
}

impl TransferManager {
    pub fn new(tx: mpsc::Sender<FileOut>) -> Self {
        Self {
            tx,
            pushes: HashMap::new(),
        }
    }

    /// Handles file-related control messages; returns replies to send back.
    pub async fn handle_control(&mut self, control: Control, files: &FileService) -> Vec<Control> {
        match control {
            Control::FileListRequest { request_id, path } => {
                let files = files.clone();
                let requested = path.clone();
                let root = files.root().display().to_string();
                match tokio::task::spawn_blocking(move || files.list(&requested)).await {
                    Ok(Ok(entries)) => vec![Control::FileList {
                        request_id,
                        path,
                        root: Some(root),
                        entries,
                    }],
                    Ok(Err(e)) => vec![Control::FileListError {
                        request_id,
                        code: "list".into(),
                        message: e.to_string(),
                    }],
                    Err(e) => vec![Control::FileListError {
                        request_id,
                        code: "internal".into(),
                        message: e.to_string(),
                    }],
                }
            }
            Control::FilePull {
                transfer_id,
                path,
                offset,
            } => {
                let files = files.clone();
                let resolved = tokio::task::spawn_blocking({
                    let path = path.clone();
                    move || files.existing(&path)
                })
                .await;
                let resolved = match resolved {
                    Ok(Ok(p)) => p,
                    Ok(Err(e)) => {
                        return vec![Control::FileError {
                            transfer_id,
                            code: "not_found".into(),
                            message: e.to_string(),
                        }]
                    }
                    Err(e) => {
                        return vec![Control::FileError {
                            transfer_id,
                            code: "internal".into(),
                            message: e.to_string(),
                        }]
                    }
                };
                if resolved.is_dir() {
                    return vec![Control::FileError {
                        transfer_id,
                        code: "is_dir".into(),
                        message: "cannot download a directory".into(),
                    }];
                }
                let tx = self.tx.clone();
                let display_path = path.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        stream_file(resolved, transfer_id, offset, display_path, tx.clone()).await
                    {
                        warn!(%e, transfer = transfer_id, "file pull failed");
                        let _ = tx
                            .send(FileOut::Error {
                                transfer_id,
                                code: "io",
                                message: e.to_string(),
                            })
                            .await;
                    }
                });
                Vec::new()
            }
            Control::FilePush {
                transfer_id,
                name,
                size,
                offset,
                overwrite,
            } => {
                let files = files.clone();
                let resolved = tokio::task::spawn_blocking({
                    let name = name.clone();
                    move || files.destination(&name, overwrite)
                })
                .await;
                let (target, part) = match resolved {
                    Ok(Ok(pair)) => pair,
                    Ok(Err(e)) => {
                        return vec![Control::FileError {
                            transfer_id,
                            code: "refused".into(),
                            message: e.to_string(),
                        }]
                    }
                    Err(e) => {
                        return vec![Control::FileError {
                            transfer_id,
                            code: "internal".into(),
                            message: e.to_string(),
                        }]
                    }
                };

                // Validate/seek the temp file for resume.
                let mut file = match prepare_part(&part, offset).await {
                    Ok(f) => f,
                    Err(e) => {
                        return vec![Control::FileError {
                            transfer_id,
                            code: "io".into(),
                            message: e.to_string(),
                        }]
                    }
                };
                let mut hasher = Sha256::new();
                if offset > 0 {
                    if let Err(e) = hash_prefix(&mut file, offset, &mut hasher).await {
                        return vec![Control::FileError {
                            transfer_id,
                            code: "io".into(),
                            message: format!("cannot hash resume prefix: {e}"),
                        }];
                    }
                }
                self.pushes.insert(
                    transfer_id,
                    PushState {
                        file,
                        target,
                        part,
                        written: offset,
                        total: size,
                        last_progress: offset,
                        hasher,
                    },
                );
                vec![Control::FileProgress {
                    transfer_id,
                    offset,
                    total: size,
                }]
            }
            Control::FileCancel { transfer_id } => {
                if let Some(state) = self.pushes.remove(&transfer_id) {
                    drop(state.file);
                    let _ = tokio::fs::remove_file(&state.part).await;
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Handles one inbound file chunk (an upload from the viewer).
    pub async fn handle_chunk(&mut self, header: FileHeader, payload: &[u8]) -> Vec<Control> {
        let Some(state) = self.pushes.get_mut(&header.transfer_id) else {
            return vec![Control::FileError {
                transfer_id: header.transfer_id,
                code: "no_transfer".into(),
                message: "unknown transfer id".into(),
            }];
        };
        if header.offset != state.written {
            return vec![Control::FileError {
                transfer_id: header.transfer_id,
                code: "offset".into(),
                message: format!("expected offset {}, got {}", state.written, header.offset),
            }];
        }

        if let Err(e) = state.file.write_all(payload).await {
            let part = state.part.clone();
            let id = header.transfer_id;
            self.pushes.remove(&id);
            let _ = tokio::fs::remove_file(part).await;
            return vec![Control::FileError {
                transfer_id: id,
                code: "io".into(),
                message: e.to_string(),
            }];
        }
        state.hasher.update(payload);
        state.written += payload.len() as u64;

        let mut replies = Vec::new();
        if state.written.saturating_sub(state.last_progress) >= 4 * 1024 * 1024 {
            state.last_progress = state.written;
            replies.push(Control::FileProgress {
                transfer_id: header.transfer_id,
                offset: state.written,
                total: state.total,
            });
        }

        if header.flags & file_flags::LAST != 0 {
            let state = self.pushes.remove(&header.transfer_id).unwrap();
            let id = header.transfer_id;
            match finalize(state).await {
                Ok((sha256, path)) => replies.push(Control::FileComplete {
                    transfer_id: id,
                    path,
                    sha256,
                }),
                Err(e) => replies.push(Control::FileError {
                    transfer_id: id,
                    code: "io".into(),
                    message: e.to_string(),
                }),
            }
        }
        replies
    }
}

async fn prepare_part(part: &PathBuf, offset: u64) -> std::io::Result<File> {
    let mut file = if offset > 0 {
        tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(part)
            .await?
    } else {
        tokio::fs::File::create(part).await?
    };
    if offset > 0 {
        let len = file.metadata().await?.len();
        if len < offset {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "resume offset beyond temporary file",
            ));
        }
        file.set_len(offset).await?;
    }
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    Ok(file)
}

async fn hash_prefix(file: &mut File, len: u64, hasher: &mut Sha256) -> std::io::Result<()> {
    file.seek(std::io::SeekFrom::Start(0)).await?;
    let mut remaining = len;
    let mut buf = vec![0u8; FILE_CHUNK];
    while remaining > 0 {
        let take = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..take]).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "resume prefix shorter than expected",
            ));
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    file.seek(std::io::SeekFrom::Start(len)).await?;
    Ok(())
}

async fn finalize(mut state: PushState) -> std::io::Result<(String, String)> {
    state.file.flush().await?;
    state.file.sync_all().await?;
    drop(state.file);
    tokio::fs::rename(&state.part, &state.target).await?;
    let digest = state.hasher.finalize();
    let name = state
        .target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok((hex(&digest), name))
}

async fn stream_file(
    path: PathBuf,
    transfer_id: u32,
    offset: u64,
    display_path: String,
    tx: mpsc::Sender<FileOut>,
) -> anyhow::Result<()> {
    let total = tokio::fs::metadata(&path).await?.len();
    let mut file = File::open(&path).await?;
    let mut hasher = Sha256::new();
    if offset > 0 {
        hash_prefix(&mut file, offset, &mut hasher).await?;
    }
    file.seek(std::io::SeekFrom::Start(offset)).await?;

    let mut seq = 0u32;
    let mut sent = offset;
    let mut buf = vec![0u8; FILE_CHUNK];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            let _ = tx
                .send(FileOut::Chunk {
                    transfer_id,
                    offset: sent,
                    seq,
                    last: true,
                    data: Vec::new(),
                })
                .await;
            break;
        }
        hasher.update(&buf[..n]);
        let _ = tx
            .send(FileOut::Chunk {
                transfer_id,
                offset: sent,
                seq,
                last: false,
                data: buf[..n].to_vec(),
            })
            .await;
        sent += n as u64;
        seq = seq.wrapping_add(1);
    }

    let digest = hasher.finalize();
    debug!(transfer = transfer_id, total, "file pull finished");
    let _ = tx
        .send(FileOut::Done {
            transfer_id,
            path: display_path,
            sha256: hex(&digest),
            total,
        })
        .await;
    Ok(())
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

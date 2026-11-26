//! Opt-in Linux audio backend using the user's PulseAudio-compatible server.
//! No microphone fallback: capture explicitly requests the default sink monitor.

use std::{io, process::Stdio, time::Duration};

use anyhow::{bail, Context, Result};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::mpsc,
    task::JoinHandle,
};
use vr_proto::audio::{CHANNELS, PACKET_BYTES, RATE};

const CAPTURE_BIN: &str = "parec";
const PLAYBACK_BIN: &str = "paplay";
const QUEUE_PACKETS: usize = 4;
const IO_TIMEOUT: Duration = Duration::from_secs(3);

async fn available(binary: &str) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let mut cmd = Command::new(binary);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    matches!(tokio::time::timeout(Duration::from_secs(1), cmd.status()).await, Ok(Ok(status)) if status.success())
}

pub async fn capture_available() -> bool {
    available(CAPTURE_BIN).await
}
pub async fn playback_available() -> bool {
    available(PLAYBACK_BIN).await
}

fn command(binary: &str) -> Command {
    let mut cmd = Command::new(binary);
    cmd.args([
        "--raw",
        "--format=s16le",
        &format!("--rate={RATE}"),
        &format!("--channels={CHANNELS}"),
        "--latency-msec=40",
        "--client-name=Velarouter",
    ])
    .stderr(Stdio::null())
    .kill_on_drop(true);
    cmd
}

/// Resolve the default sink's monitor by name, never the default audio source
/// (which can be a microphone). Check the selected source is a sink monitor.
async fn default_monitor() -> Result<String> {
    let pactl = |args: &[&str]| {
        let mut cmd = Command::new("pactl");
        cmd.args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        cmd
    };
    let sink = tokio::time::timeout(
        Duration::from_secs(1),
        pactl(&["get-default-sink"]).output(),
    )
    .await??;
    anyhow::ensure!(sink.status.success(), "没有可用的系统输出设备");
    let sink = std::str::from_utf8(&sink.stdout)?.trim();
    anyhow::ensure!(
        !sink.is_empty() && sink.len() <= 256 && !sink.chars().any(char::is_whitespace),
        "无效的系统输出设备"
    );
    let name = format!("{sink}.monitor");
    let output = tokio::time::timeout(Duration::from_secs(1), pactl(&["list", "sources"]).output())
        .await??;
    anyhow::ensure!(
        output.status.success()
            && is_monitor(&String::from_utf8_lossy(&output.stdout), &name, sink),
        "没有可用的系统输出监视源"
    );
    Ok(name)
}

fn is_monitor(list: &str, name: &str, sink: &str) -> bool {
    list.split("Source #").any(|source| {
        let lines: Vec<_> = source.lines().map(str::trim).collect();
        lines.iter().any(|line| {
            line.strip_prefix("Name:")
                .is_some_and(|value| value.trim() == name)
        }) && lines.iter().any(|line| {
            line.strip_prefix("Monitor of Sink:")
                .is_some_and(|value| value.trim() == sink)
        })
    })
}

pub struct Capture {
    packets: mpsc::Receiver<Vec<u8>>,
    task: JoinHandle<()>,
}

impl Capture {
    pub async fn start() -> Result<Self> {
        if !cfg!(target_os = "linux") {
            bail!("系统音频当前仅支持 Linux");
        }
        let monitor = default_monitor().await?;
        let mut child = command(CAPTURE_BIN)
            .arg(format!("--device={monitor}"))
            .arg("--stream-name=Velarouter system audio")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .context("无法启动系统音频采集")?;
        let mut output = child.stdout.take().context("音频采集输出不可用")?;
        // Report success only after the monitor actually produced samples.
        let mut first = vec![0; PACKET_BYTES];
        tokio::time::timeout(IO_TIMEOUT, output.read_exact(&mut first)).await??;
        let (tx, packets) = mpsc::channel(QUEUE_PACKETS);
        tx.try_send(first).expect("new audio queue cannot be full");
        let task = tokio::spawn(async move {
            let _child = child;
            loop {
                let mut packet = vec![0; PACKET_BYTES];
                if !matches!(
                    tokio::time::timeout(IO_TIMEOUT, output.read_exact(&mut packet)).await,
                    Ok(Ok(_))
                ) {
                    break;
                }
                if let Err(mpsc::error::TrySendError::Closed(_)) = tx.try_send(packet) {
                    break;
                }
            }
        });
        Ok(Self { packets, task })
    }

    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.packets.recv().await
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct Playback {
    packets: mpsc::Sender<Vec<u8>>,
    task: JoinHandle<io::Result<()>>,
}

impl Playback {
    pub fn start() -> Result<Self> {
        if !cfg!(target_os = "linux") {
            bail!("系统音频当前仅支持 Linux");
        }
        let mut child = command(PLAYBACK_BIN)
            .arg("--stream-name=Velarouter remote audio")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .context("无法启动本地音频播放")?;
        let mut input = child.stdin.take().context("本地音频输入不可用")?;
        let (packets, mut rx) = mpsc::channel::<Vec<u8>>(QUEUE_PACKETS);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    status = child.wait() => return status.map(|_| ()),
                    packet = rx.recv() => {
                        let Some(packet) = packet else { break; };
                        tokio::time::timeout(IO_TIMEOUT, input.write_all(&packet)).await??;
                    }
                }
            }
            let _ = child.kill().await;
            Ok(())
        });
        Ok(Self { packets, task })
    }

    pub fn push(&self, packet: Vec<u8>) {
        if packet.len() == PACKET_BYTES {
            let _ = self.packets.try_send(packet);
        }
    }

    pub async fn finished(&mut self) {
        let _ = (&mut self.task).await;
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires an isolated PulseAudio server with a default test sink"]
    async fn records_and_plays_synthetic_system_audio() {
        assert!(capture_available().await && playback_available().await);
        let monitor = default_monitor().await.unwrap();
        assert_eq!(monitor, "velarouter_test.monitor");
        let mut capture = Capture::start().await.unwrap();
        // A short synthetic waveform, played into the isolated test sink.
        let mut pcm = Vec::with_capacity(PACKET_BYTES);
        for sample in 0..RATE * 20 / 1000 {
            let value = if sample % 96 < 48 {
                4096_i16
            } else {
                -4096_i16
            };
            pcm.extend_from_slice(&value.to_le_bytes());
            pcm.extend_from_slice(&value.to_le_bytes());
        }
        let playback = Playback::start().unwrap();
        let recorded = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                playback.push(pcm.clone());
                let samples = capture.recv().await.expect("capture still running");
                if samples.iter().any(|byte| *byte != 0) {
                    break samples;
                }
            }
        })
        .await
        .expect("monitor must receive the test waveform");
        assert_eq!(recorded.len(), PACKET_BYTES);
    }

    #[test]
    fn accepts_only_the_named_output_monitor_never_a_microphone() {
        let sources = "Source #3\n\tName: input\n\tMonitor of Sink: n/a\nSource #4\n\tName: output.monitor\n\tMonitor of Sink: output\n";
        assert!(is_monitor(sources, "output.monitor", "output"));
        assert!(!is_monitor(sources, "input", "output"));
        assert!(!is_monitor(sources, "output.monitor", "other"));
        assert_eq!(PACKET_BYTES, 3840);
    }
}

//! Wayland input is sent only through the locally approved RemoteDesktop session.
use crate::{input::map_key, AgentEvent};
use anyhow::{ensure, Context, Result};
use ashpd::desktop::{
    remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions},
    screencast::{Screencast, SelectSourcesOptions, SourceType},
    Session,
};
use futures_util::StreamExt;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::{mpsc::UnboundedSender, watch};
use vr_proto::{denormalized, InputEvent, MouseButton, TouchPhase};

pub fn is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}

pub struct Grant {
    proxy: RemoteDesktop,
    session: Session<RemoteDesktop>,
    node: u32,
    size: (i32, i32),
    pub(crate) display: u32,
}
pub type Permission = watch::Receiver<Option<Arc<Grant>>>;

/// Owning a private bus connection ties the portal session (including any open
/// consent dialog) to this hosting lifetime, including cancellation during Start.
pub fn request(
    events: Option<UnboundedSender<AgentEvent>>,
    mut shutdown: Option<watch::Receiver<bool>>,
) -> Permission {
    let (tx, rx) = watch::channel(None);
    tokio::spawn(async move {
        let status = |s: String| {
            if let Some(tx) = &events {
                let _ = tx.send(AgentEvent::InteractionStatus(s));
            }
        };
        status("请在系统「远程桌面」对话框选择要控制的屏幕，打开「允许远程交互」，再点「分享」。未授权时不能控制键鼠。".into());
        let run = async {
            let bus = ashpd::zbus::Connection::session().await?;
            let proxy = RemoteDesktop::with_connection(bus.clone()).await?;
            let cast = Screencast::with_connection(bus).await?;
            let session = proxy.create_session(Default::default()).await?;
            let devices = DeviceType::POINTER | DeviceType::KEYBOARD;
            proxy
                .select_devices(
                    &session,
                    SelectDevicesOptions::default().set_devices(devices),
                )
                .await?
                .response()?;
            cast.select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_sources(SourceType::MONITOR)
                    .set_multiple(false),
            )
            .await?
            .response()?;
            let result = proxy
                .start(&session, None, Default::default())
                .await?
                .response()?;
            ensure!(result.devices().contains(devices), "未允许远程键鼠交互");
            let stream = result.streams().first().context("未选择共享屏幕")?;
            let size = stream
                .size()
                .filter(|(w, h)| *w > 0 && *h > 0)
                .context("桌面未提供所选屏幕的逻辑尺寸")?;
            let monitors = xcap::Monitor::all()?;
            // The portal identifies the chosen output by its logical position
            // (and size when the compositor does not expose a position). Match
            // those against xcap's monitor list so input is normalized against
            // the same screen the viewer is watching.
            let monitor = monitors
                .iter()
                .find(|m| {
                    stream.position().is_some()
                        && m.x().ok().zip(m.y().ok()) == stream.position()
                })
                .or_else(|| {
                    let size = (size.0.max(0) as u32, size.1.max(0) as u32);
                    let mut same_size = monitors.iter().filter(|m| {
                        m.width().ok().zip(m.height().ok()) == Some(size)
                    });
                    let first = same_size.next()?;
                    same_size.next().is_none().then_some(first)
                })
                .or_else(|| (monitors.len() == 1).then(|| &monitors[0]))
                .with_context(|| {
                    format!(
                        "无法匹配所选屏幕（portal 位置 {:?}、尺寸 {size:?}，本机检测到 {} 块屏幕）；请检查多屏布局或重新选择屏幕",
                        stream.position(),
                        monitors.len()
                    )
                })?;
            let grant = Arc::new(Grant {
                proxy,
                session,
                node: stream.pipe_wire_node_id(),
                size,
                display: monitor.id()?,
            });
            let mut closed = grant.session.receive_closed().await?;
            tx.send_replace(Some(grant.clone()));
            status("本机已允许远程键鼠交互（仅所选屏幕）。".into());
            let _ = closed.next().await;
            anyhow::bail!("系统已结束授权")
        };
        tokio::select! {
            result = run => { let result: Result<()> = result; if let Err(e) = result { status(format!("仅观看：{e}。可在「设置 → 权限」重新申请。")); } },
            _ = async { match shutdown.as_mut() {Some(rx) => { let _ = rx.wait_for(|v|*v).await; },None=> std::future::pending().await} } => {},
            _ = tx.closed() => {},
        }
        tx.send_replace(None);
    });
    rx
}

#[derive(Default)]
pub(crate) struct Held {
    keys: HashSet<i32>,
    buttons: HashSet<i32>,
}
fn state(down: bool) -> KeyState {
    if down {
        KeyState::Pressed
    } else {
        KeyState::Released
    }
}
impl Grant {
    async fn point(&self, x: u16, y: u16, display: u32) -> Result<()> {
        ensure!(self.display == display, "请在主控端选择被控端授权的屏幕");
        self.proxy
            .notify_pointer_motion_absolute(
                &self.session,
                self.node,
                denormalized(x, (self.size.0 - 1) as f64),
                denormalized(y, (self.size.1 - 1) as f64),
                Default::default(),
            )
            .await?;
        Ok(())
    }
    async fn key(&self, k: i32, down: bool, h: &mut Held) -> Result<()> {
        self.proxy
            .notify_keyboard_keysym(&self.session, k, state(down), Default::default())
            .await?;
        if down {
            h.keys.insert(k);
        } else {
            h.keys.remove(&k);
        }
        Ok(())
    }
    async fn button(&self, k: i32, down: bool, h: &mut Held) -> Result<()> {
        self.proxy
            .notify_pointer_button(&self.session, k, state(down), Default::default())
            .await?;
        if down {
            h.buttons.insert(k);
        } else {
            h.buttons.remove(&k);
        }
        Ok(())
    }
    pub(crate) async fn release(&self, h: &mut Held) {
        for k in std::mem::take(&mut h.keys) {
            let _ = self.key(k, false, h).await;
        }
        for k in std::mem::take(&mut h.buttons) {
            let _ = self.button(k, false, h).await;
        }
    }
    pub(crate) async fn apply(&self, event: &InputEvent, h: &mut Held) -> Result<()> {
        match event {
            InputEvent::MouseMove { x, y, display } => self.point(*x, *y, *display).await?,
            InputEvent::MouseButton {
                x,
                y,
                button,
                down,
                display,
            } => {
                self.point(*x, *y, *display).await?;
                self.button(
                    match button {
                        MouseButton::Left => 0x110,
                        MouseButton::Right => 0x111,
                        MouseButton::Middle => 0x112,
                        MouseButton::Back => 0x113,
                        MouseButton::Forward => 0x114,
                    },
                    *down,
                    h,
                )
                .await?;
            }
            InputEvent::Wheel {
                x,
                y,
                dx,
                dy,
                display,
            } => {
                self.point(*x, *y, *display).await?;
                for (axis, v) in [(Axis::Horizontal, *dx), (Axis::Vertical, *dy)] {
                    if v != 0 {
                        self.proxy
                            .notify_pointer_axis_discrete(
                                &self.session,
                                axis,
                                v,
                                Default::default(),
                            )
                            .await?;
                    }
                }
            }
            InputEvent::Key { code, down, .. } => {
                if let Some(k) = map_key(code) {
                    self.key(xkeysym::Keysym::from(k).raw() as i32, *down, h)
                        .await?;
                }
            }
            InputEvent::Text { text } => {
                for c in text.chars() {
                    let k = xkeysym::Keysym::from_char(c).raw() as i32;
                    self.key(k, true, h).await?;
                    self.key(k, false, h).await?;
                }
            }
            InputEvent::Touch {
                points,
                display,
                phase,
            } => {
                if let Some(p) = points.first() {
                    self.point(p.x, p.y, *display).await?;
                }
                match phase {
                    TouchPhase::Start => self.button(0x110, true, h).await?,
                    TouchPhase::End | TouchPhase::Cancel => self.button(0x110, false, h).await?,
                    TouchPhase::Move => {}
                }
            }
            InputEvent::ReleaseAll => self.release(h).await,
        }
        Ok(())
    }
}

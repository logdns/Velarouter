//! Velarouter wire protocol (VRP/1).
//!
//! The protocol has exactly two layers:
//!
//! 1. **Control plane** — UTF-8 JSON documents, one per WebSocket text frame or
//!    one per reliable stream message. Described by [`Control`].
//! 2. **Media plane** — binary frames with a fixed 24-byte header, see
//!    [`VideoHeader`]. Never JSON, never base64.
//!
//! Both layers are encrypted after the handshake (see `vr-crypto`); the
//! handshake itself only carries public keys and a proof, never a secret.

use serde::{Deserialize, Serialize};
use std::fmt;

pub mod audio;

/// Wire protocol version. Bumped on any breaking change to [`Control`] or the
/// binary framing. Peers must reject unknown major versions instead of guessing.
pub const PROTOCOL_VERSION: u16 = 1;

/// Viewer app family reserved for the opt-in desktop wall preview.
pub const WALL_APP_PREFIX: &str = "velarouter-wall/";
pub const WALL_WIDTH: u16 = 480;
pub const WALL_HEIGHT: u16 = 270;
pub const WALL_FPS: u8 = 3;

/// Default TCP/UDP port of an agent ("被控端") on the local network.
pub const DEFAULT_AGENT_PORT: u16 = 34789;

/// Default HTTP/WebSocket port of the signal server.
pub const DEFAULT_SIGNAL_PORT: u16 = 34790;

/// Default UDP port used for address reflection (mini-STUN) and hole punching.
pub const DEFAULT_REFLEX_PORT: u16 = 34791;

/// UDP port used by the rendezvous/QUIC path when it is free. A stable port
/// keeps router state warm between sessions; it falls back to an ephemeral one.
pub const DEFAULT_MEET_PORT: u16 = 34793;

/// Default UDP port of the rendezvous service (peer address exchange + punching).
pub const DEFAULT_RENDEZVOUS_PORT: u16 = 34792;

/// Twelve decimal digits distinguish ephemeral desktop IDs from legacy nine-digit IDs.
pub fn is_internet_code(code: &str) -> bool {
    code.len() == 12 && code.bytes().all(|b| b.is_ascii_digit())
}

/// Magic of a media frame header: `V R F 1`.
pub const MEDIA_MAGIC: [u8; 4] = *b"VRF1";

/// Size of [`VideoHeader`] on the wire.
pub const VIDEO_HEADER_LEN: usize = 24;

/// Maximum accepted media payload (a single JPEG/H.26x frame).
pub const MAX_MEDIA_PAYLOAD: usize = 8 * 1024 * 1024;

/// Maximum accepted control message.
pub const MAX_CONTROL_BYTES: usize = 1024 * 1024;

/// Magic of a file-transfer chunk header: `V R F L`.
pub const FILE_MAGIC: [u8; 4] = *b"VRFL";
/// Size of [`FileHeader`] on the wire.
pub const FILE_HEADER_LEN: usize = 28;
/// Payload carried by one file chunk. 64 KiB keeps memory bounded and gives
/// good throughput on both LAN and relay without head-of-line blocking.
pub const FILE_CHUNK: usize = 64 * 1024;

/// Which side of the session a peer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The host that shares its screen and accepts input ("被控端").
    Agent,
    /// The peer that watches and controls ("主控端").
    Viewer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Agent => "agent",
            Role::Viewer => "viewer",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Video encodings understood by VRP/1.
///
/// `Jpeg` is mandatory to implement: it is the baseline that works on every
/// platform without hardware encoders and is what the reference agent uses.
/// The remaining codecs are negotiated, never assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    /// One complete JPEG image per frame (baseline, always available).
    Jpeg,
    /// Annex-B H.264 access unit.
    H264,
    /// Annex-B HEVC access unit.
    H265,
    /// VP8 frame.
    Vp8,
    /// AV1 temporal unit.
    Av1,
}

impl Codec {
    pub fn mime(self) -> &'static str {
        match self {
            Codec::Jpeg => "image/jpeg",
            Codec::H264 => "video/avc",
            Codec::H265 => "video/hevc",
            Codec::Vp8 => "video/vp8",
            Codec::Av1 => "video/av1",
        }
    }

    /// Whether the codec is intra-only, i.e. a decoder can start at any frame.
    pub fn intra_only(self) -> bool {
        matches!(self, Codec::Jpeg)
    }
}

/// Mouse buttons, numbered like the DOM `MouseEvent.button`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

/// Touch phase, mirroring Android `MotionEvent` and the Pointer Events API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchPhase {
    Start,
    Move,
    End,
    Cancel,
}

/// A single touch point in normalised display coordinates (0..=65535).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TouchPoint {
    pub id: u16,
    /// Horizontal position, `0..=65535` of the display width.
    pub x: u16,
    /// Vertical position, `0..=65535` of the display height.
    pub y: u16,
}

/// Modifier bitmask carried next to keyboard events.
pub mod modifiers {
    pub const SHIFT: u8 = 1 << 0;
    pub const CONTROL: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const META: u8 = 1 << 3;
    pub const CAPS_LOCK: u8 = 1 << 4;
    pub const NUM_LOCK: u8 = 1 << 5;
}

/// An input event sent by the viewer and injected by the agent.
///
/// Coordinates are always normalised to `0..=65535` so that the viewer never
/// needs to know the true display resolution, and the agent stays correct when
/// the resolution changes mid-session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum InputEvent {
    MouseMove {
        x: u16,
        y: u16,
        #[serde(default)]
        display: u32,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
        x: u16,
        y: u16,
        #[serde(default)]
        display: u32,
    },
    /// Wheel scrolling in detents; positive `dy` scrolls down.
    Wheel {
        dx: i32,
        dy: i32,
        x: u16,
        y: u16,
        #[serde(default)]
        display: u32,
    },
    /// `code` is a W3C `KeyboardEvent.code` value (`KeyA`, `Enter`, `F11`, …)
    /// so every client platform can map it to its own key table.
    Key {
        code: String,
        down: bool,
        #[serde(default)]
        modifiers: u8,
    },
    /// Committed text from an IME or a soft keyboard.
    Text { text: String },
    Touch {
        phase: TouchPhase,
        points: Vec<TouchPoint>,
        #[serde(default)]
        display: u32,
    },
    /// Release every pressed button/key (sent on focus loss).
    ReleaseAll,
}

/// A monitor exposed by the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub primary: bool,
}

/// Static information about the agent, shown in the client device list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub version: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Codec/feature negotiation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub codecs: Vec<Codec>,
    pub max_width: u16,
    pub max_height: u16,
    pub max_fps: u8,
    /// The agent can inject mouse/keyboard events (view-only agents cannot).
    pub input: bool,
    pub clipboard: bool,
    pub multi_display: bool,
    pub audio: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            codecs: vec![Codec::Jpeg],
            max_width: 3840,
            max_height: 2160,
            max_fps: 60,
            input: true,
            clipboard: true,
            multi_display: true,
            audio: false,
        }
    }
}

impl Capabilities {
    /// Returns the first codec supported by both sides, preferring the viewer's
    /// order (viewers list what they decode fastest first).
    pub fn negotiate<'a>(&'a self, viewer: &'a Capabilities) -> Option<Codec> {
        viewer
            .codecs
            .iter()
            .copied()
            .find(|c| self.codecs.contains(c))
    }
}

/// Requested stream parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoConfig {
    pub codec: Codec,
    /// `1..=100`; interpreted by the encoder (JPEG quality, H.264 QP).
    pub quality: u8,
    pub fps: u8,
    pub max_width: u16,
    pub max_height: u16,
    pub display: u32,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            codec: Codec::Jpeg,
            quality: 70,
            fps: 30,
            max_width: 1920,
            max_height: 1080,
            display: 0,
        }
    }
}

/// Latency/quality telemetry. Sent by both sides once per second.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub fps: f32,
    /// Video bitrate in kbit/s.
    pub kbps: u32,
    /// Round trip time in milliseconds, measured with `Ping`/`Pong`.
    pub rtt_ms: u32,
    /// Time spent capturing one frame.
    pub capture_ms: f32,
    /// Time spent encoding one frame.
    pub encode_ms: f32,
    /// Time the viewer spent decoding one frame.
    pub decode_ms: f32,
    /// Frames dropped by the transport or the decoder since the last report.
    pub dropped: u32,
    /// End-to-end latency estimate in milliseconds.
    pub e2e_ms: u32,
}

/// Clipboard payload, including the direction so the receiver can loop-guard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipboardMsg {
    pub direction: ClipboardDirection,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardDirection {
    ViewerToAgent,
    AgentToViewer,
}

/// One directory entry returned by `file_list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    /// Path relative to the agent's shared root, using `/` separators.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix seconds; 0 when unknown.
    pub mtime: i64,
    /// Unix mode bits; 0 when unknown.
    pub mode: u32,
}

/// Header of one file-transfer chunk.
///
/// ```text
/// 0   4     5     6     8        12       20     24     28
/// +---+-----+-----+-----+--------+--------+------+------+
/// |VRFL| ver|flags|rsv | xfer   | offset | seq  | len  |
/// +---+-----+-----+-----+--------+--------+------+------+
/// ```
/// Widths: `transfer_id u32`, `offset u64`, `seq u32`, `len u32`; big-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileHeader {
    pub version: u8,
    pub flags: u8,
    pub transfer_id: u32,
    pub offset: u64,
    pub seq: u32,
    pub len: u32,
}

pub mod file_flags {
    /// Last chunk of the transfer.
    pub const LAST: u8 = 1 << 0;
}

impl FileHeader {
    pub fn new(transfer_id: u32, offset: u64, seq: u32, len: u32, last: bool) -> Self {
        Self {
            version: PROTOCOL_VERSION as u8,
            flags: if last { file_flags::LAST } else { 0 },
            transfer_id,
            offset,
            seq,
            len,
        }
    }

    pub fn is_last(&self) -> bool {
        self.flags & file_flags::LAST != 0
    }

    pub fn encode_into(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&FILE_MAGIC);
        out.push(self.version);
        out.push(self.flags);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&self.transfer_id.to_be_bytes());
        out.extend_from_slice(&self.offset.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.len.to_be_bytes());
    }

    /// Serialises header + payload in one buffer.
    pub fn frame(self, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(FILE_HEADER_LEN + payload.len());
        let mut hdr = self;
        hdr.len = payload.len() as u32;
        hdr.encode_into(&mut out);
        out.extend_from_slice(payload);
        out
    }

    pub fn parse(buf: &[u8]) -> Result<(Self, &[u8]), ProtoError> {
        if buf.len() < FILE_HEADER_LEN {
            return Err(ProtoError::ShortFrame(buf.len()));
        }
        if buf[0..4] != FILE_MAGIC {
            return Err(ProtoError::BadMagic);
        }
        if buf[4] != PROTOCOL_VERSION as u8 {
            return Err(ProtoError::UnsupportedVersion(buf[4] as u16));
        }
        let flags = buf[5];
        let transfer_id = u32::from_be_bytes(buf[8..12].try_into().unwrap());
        let offset = u64::from_be_bytes(buf[12..20].try_into().unwrap());
        let seq = u32::from_be_bytes(buf[20..24].try_into().unwrap());
        let len = u32::from_be_bytes(buf[24..28].try_into().unwrap()) as usize;
        if len > MAX_MEDIA_PAYLOAD {
            return Err(ProtoError::TooLarge(len));
        }
        if buf.len() < FILE_HEADER_LEN + len {
            return Err(ProtoError::ShortFrame(buf.len()));
        }
        Ok((
            Self {
                version: buf[4],
                flags,
                transfer_id,
                offset,
                seq,
                len: len as u32,
            },
            &buf[FILE_HEADER_LEN..FILE_HEADER_LEN + len],
        ))
    }
}

/// Every control-plane message of VRP/1.
///
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewerIdentityProof {
    pub public_key: String,
    pub signature: String,
}

/// The JSON shape is the contract that all SDKs implement:
/// `{"t":"input","event":{"k":"mouse_move","x":32768,"y":16384}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Control {
    /// First message, plaintext. Announces the protocol version and role.
    Hello {
        version: u16,
        role: Role,
        #[serde(default)]
        device: Option<DeviceInfo>,
        /// Random per-connection id, used to bind the crypto transcript.
        session_id: String,
        /// Human-facing name of the client app.
        app: String,
    },
    /// Viewer → agent. Carries the viewer's ephemeral P-256 public key
    /// (base64, uncompressed SEC1) and a 16-byte random nonce (base64).
    AuthInit {
        viewer_pub: String,
        nonce: String,
    },
    /// Agent → viewer. Carries its ephemeral public key plus
    /// `HMAC-SHA256(session_key, "agent" || session_id)`.
    AuthFinish {
        agent_pub: String,
        proof: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        require_viewer_identity: bool,
    },
    /// Viewer → agent. `HMAC-SHA256(session_key, "viewer" || session_id)`.
    AuthProof {
        proof: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        viewer_identity: Option<ViewerIdentityProof>,
    },
    /// Agent → viewer. Handshake accepted; later frames are encrypted.
    AuthOk,
    /// Either side, first encrypted message. Advertises codecs, limits and the
    /// device identity so the peer can configure itself before media starts.
    Caps {
        capabilities: Capabilities,
        #[serde(default)]
        device: Option<DeviceInfo>,
    },
    /// Either side. Announces the UDP media endpoint and the token that tags its
    /// datagrams. Absent `udp` means "media over the control channel".
    Transport {
        #[serde(default)]
        udp: Option<String>,
        #[serde(default)]
        token: String,
    },
    /// Viewer → agent. List a directory under the agent's shared root.
    FileListRequest {
        request_id: u32,
        path: String,
    },
    /// Agent → viewer. Directory listing.
    FileList {
        request_id: u32,
        path: String,
        /// Display only; requests always use paths relative to this root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<String>,
        entries: Vec<FileEntry>,
    },
    /// Agent → viewer. Directory listing failed.
    FileListError {
        request_id: u32,
        code: String,
        message: String,
    },
    /// Viewer → agent. Start downloading `path`; `offset` allows resuming.
    FilePull {
        transfer_id: u32,
        path: String,
        offset: u64,
    },
    /// Viewer → agent. Announce an upload; chunks follow on the file channel.
    FilePush {
        transfer_id: u32,
        name: String,
        size: u64,
        offset: u64,
        overwrite: bool,
    },
    /// Either side. Progress report, at most once per MiB.
    FileProgress {
        transfer_id: u32,
        offset: u64,
        total: u64,
    },
    /// Either side. Transfer finished; `sha256` is lowercase hex.
    FileComplete {
        transfer_id: u32,
        path: String,
        sha256: String,
    },
    /// Either side. Transfer failed; the receiver should discard partial data.
    FileError {
        transfer_id: u32,
        code: String,
        message: String,
    },
    /// Either side. Cancel an in-flight transfer.
    FileCancel {
        transfer_id: u32,
    },
    /// Either side. Handshake rejected; the connection is closed afterwards.
    AuthFail {
        reason: String,
    },
    /// Viewer → agent, optionally agent → viewer to advertise its own settings.
    VideoConfig(VideoConfig),
    /// Agent → viewer. Confirms the host enforces a view-only session.
    ViewOnly,
    /// Viewer → agent. A per-session opt-in; the host may still refuse.
    AudioRequest {
        enabled: bool,
    },
    /// Agent → viewer. Outcome of a request or asynchronous device failure.
    AudioStatus {
        active: bool,
        message: String,
    },
    /// Agent → viewer. Lists capturable displays.
    DisplayList {
        displays: Vec<DisplayInfo>,
    },
    /// Agent → viewer. Describes the next media frames without decoding them.
    FrameMeta {
        frame_id: u32,
        display: u32,
        width: u32,
        height: u32,
        codec: Codec,
        keyframe: bool,
        /// Number of tiles the frame was split into (JPEG tile mode), else 1.
        tiles: u16,
    },
    /// Viewer → agent. Input to inject.
    Input {
        event: InputEvent,
    },
    /// Either side. Clipboard text.
    Clipboard(ClipboardMsg),
    /// Agent → viewer. Cursor shape/visibility; `shape` is a CSS cursor name.
    Cursor {
        visible: bool,
        shape: String,
        x: u16,
        y: u16,
    },
    /// Either side. Telemetry.
    Stats(Stats),
    /// Either side. RTT probe.
    Ping {
        ts: u64,
    },
    Pong {
        ts: u64,
    },
    /// Agent → viewer. A non fatal problem worth surfacing (e.g. permission).
    Warning {
        code: String,
        message: String,
    },
    /// Either side. Terminal condition for the session.
    Bye {
        reason: String,
    },
    /// Either side. Fatal protocol error.
    Error {
        code: String,
        message: String,
    },
}

impl Control {
    pub fn to_json(&self) -> String {
        // Control messages are small and only sent on events, so a compact
        // encoding is enough; never `to_string_pretty` on the hot path.
        serde_json::to_string(self).expect("Control is always serialisable")
    }

    pub fn from_json(s: &str) -> Result<Self, ProtoError> {
        if s.len() > MAX_CONTROL_BYTES {
            return Err(ProtoError::TooLarge(s.len()));
        }
        serde_json::from_str(s).map_err(ProtoError::Json)
    }

    /// Messages that must never be encrypted-coerced away: the handshake has to
    /// travel in clear text so the peer can derive the key.
    pub fn is_handshake(&self) -> bool {
        matches!(
            self,
            Control::Hello { .. }
                | Control::AuthInit { .. }
                | Control::AuthFinish { .. }
                | Control::AuthProof { .. }
                | Control::AuthOk
                | Control::AuthFail { .. }
        )
    }
}

/// Header prepended to every media payload on both WebSocket and UDP.
///
/// ```text
/// 0   4    5    6     8       12      16        20      24
/// +---+----+----+-----+-------+-------+---------+-------+
/// |magc|ver|cdc|flags|frame  |display| w  | h  | len   |
/// +---+----+----+-----+-------+-------+---------+-------+
/// ```
/// Width/height are `u16`, payload length is `u32`; all big-endian so JS
/// `DataView` can read them without alignment tricks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    pub version: u8,
    pub codec: Codec,
    pub flags: u8,
    pub frame_id: u32,
    pub display: u32,
    pub width: u16,
    pub height: u16,
    pub payload_len: u32,
}

pub mod frame_flags {
    /// The frame is an intra frame / can be decoded standalone.
    pub const KEYFRAME: u8 = 1 << 0;
    /// The frame is split into multiple tiles, each a full image.
    pub const TILED: u8 = 1 << 1;
}

impl VideoHeader {
    pub fn new(codec: Codec, frame_id: u32, display: u32, width: u16, height: u16) -> Self {
        Self {
            version: PROTOCOL_VERSION as u8,
            codec,
            flags: if codec.intra_only() {
                frame_flags::KEYFRAME
            } else {
                0
            },
            frame_id,
            display,
            width,
            height,
            payload_len: 0,
        }
    }

    fn codec_to_u8(codec: Codec) -> u8 {
        match codec {
            Codec::Jpeg => 1,
            Codec::H264 => 2,
            Codec::H265 => 3,
            Codec::Vp8 => 4,
            Codec::Av1 => 5,
        }
    }

    fn codec_from_u8(v: u8) -> Result<Codec, ProtoError> {
        Ok(match v {
            1 => Codec::Jpeg,
            2 => Codec::H264,
            3 => Codec::H265,
            4 => Codec::Vp8,
            5 => Codec::Av1,
            other => return Err(ProtoError::BadCodec(other)),
        })
    }

    /// Serialises header + payload in one buffer, ready to be sent.
    pub fn frame(self, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(VIDEO_HEADER_LEN + payload.len());
        let mut hdr = self;
        hdr.payload_len = payload.len() as u32;
        hdr.encode_into(&mut out);
        out.extend_from_slice(payload);
        out
    }

    pub fn encode_into(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MEDIA_MAGIC);
        out.push(self.version);
        out.push(Self::codec_to_u8(self.codec));
        out.push(self.flags);
        out.push(0); // reserved, keeps the header 4-byte aligned
        out.extend_from_slice(&self.frame_id.to_be_bytes());
        out.extend_from_slice(&self.display.to_be_bytes());
        out.extend_from_slice(&self.width.to_be_bytes());
        out.extend_from_slice(&self.height.to_be_bytes());
        out.extend_from_slice(&self.payload_len.to_be_bytes());
    }

    /// Parses a full frame buffer and returns `(header, payload)`.
    pub fn parse(buf: &[u8]) -> Result<(Self, &[u8]), ProtoError> {
        if buf.len() < VIDEO_HEADER_LEN {
            return Err(ProtoError::ShortFrame(buf.len()));
        }
        if buf[0..4] != MEDIA_MAGIC {
            return Err(ProtoError::BadMagic);
        }
        let version = buf[4];
        if version != PROTOCOL_VERSION as u8 {
            return Err(ProtoError::UnsupportedVersion(version as u16));
        }
        let codec = Self::codec_from_u8(buf[5])?;
        let flags = buf[6];
        let frame_id = u32::from_be_bytes(buf[8..12].try_into().unwrap());
        let display = u32::from_be_bytes(buf[12..16].try_into().unwrap());
        let width = u16::from_be_bytes(buf[16..18].try_into().unwrap());
        let height = u16::from_be_bytes(buf[18..20].try_into().unwrap());
        let payload_len = u32::from_be_bytes(buf[20..24].try_into().unwrap()) as usize;
        if payload_len > MAX_MEDIA_PAYLOAD {
            return Err(ProtoError::TooLarge(payload_len));
        }
        if buf.len() < VIDEO_HEADER_LEN + payload_len {
            return Err(ProtoError::ShortFrame(buf.len()));
        }
        Ok((
            Self {
                version,
                codec,
                flags,
                frame_id,
                display,
                width,
                height,
                payload_len: payload_len as u32,
            },
            &buf[VIDEO_HEADER_LEN..VIDEO_HEADER_LEN + payload_len],
        ))
    }
}

/// Normalises a pixel coordinate into the 0..=65535 protocol space.
pub fn normalized(pos: f64, extent: f64) -> u16 {
    if extent <= 0.0 || !pos.is_finite() {
        return 0;
    }
    ((pos / extent).clamp(0.0, 1.0) * 65535.0).round() as u16
}

/// Turns a protocol coordinate back into pixels for injection.
pub fn denormalized(value: u16, extent: f64) -> f64 {
    (value as f64 / 65535.0) * extent
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("control message too large: {0} bytes")]
    TooLarge(usize),
    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("media frame too short: {0} bytes")]
    ShortFrame(usize),
    #[error("bad media magic")]
    BadMagic,
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("unknown codec id {0}")]
    BadCodec(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_roundtrip_input() {
        let msg = Control::Input {
            event: InputEvent::Key {
                code: "KeyA".into(),
                down: true,
                modifiers: modifiers::SHIFT,
            },
        };
        let json = msg.to_json();
        assert_eq!(
            json,
            r#"{"t":"input","event":{"k":"key","code":"KeyA","down":true,"modifiers":1}}"#
        );
        assert_eq!(Control::from_json(&json).unwrap(), msg);
    }

    #[test]
    fn media_header_roundtrip() {
        let payload = vec![0xABu8; 1234];
        let hdr = VideoHeader::new(Codec::Jpeg, 7, 2, 1920, 1080);
        let buf = hdr.frame(&payload);
        let (got, body) = VideoHeader::parse(&buf).unwrap();
        assert_eq!(got.payload_len, 1234);
        assert_eq!(got.frame_id, 7);
        assert_eq!(got.display, 2);
        assert_eq!(body, &payload[..]);
        assert_eq!(buf.len(), VIDEO_HEADER_LEN + payload.len());
        assert_eq!(got.flags & frame_flags::KEYFRAME, frame_flags::KEYFRAME);
    }

    #[test]
    fn media_header_rejects_truncation() {
        let hdr = VideoHeader::new(Codec::Jpeg, 1, 0, 640, 480);
        let mut buf = hdr.frame(&[1, 2, 3]);
        buf.truncate(VIDEO_HEADER_LEN + 1);
        assert!(matches!(
            VideoHeader::parse(&buf),
            Err(ProtoError::ShortFrame(_))
        ));
    }

    #[test]
    fn codec_negotiation_prefers_viewer_order() {
        let agent = Capabilities {
            codecs: vec![Codec::Jpeg, Codec::H264, Codec::Av1],
            ..Default::default()
        };
        let viewer = Capabilities {
            codecs: vec![Codec::Av1, Codec::Jpeg],
            ..Default::default()
        };
        assert_eq!(agent.negotiate(&viewer), Some(Codec::Av1));
        let viewer_old = Capabilities {
            codecs: vec![Codec::Vp8],
            ..Default::default()
        };
        assert_eq!(agent.negotiate(&viewer_old), None);
    }

    #[test]
    fn coordinates_are_normalized() {
        assert_eq!(normalized(0.0, 1920.0), 0);
        assert_eq!(normalized(1920.0, 1920.0), 65535);
        assert_eq!(normalized(960.0, 1920.0), 32768);
        assert!((denormalized(32768, 1920.0) - 960.0).abs() < 0.05);
        assert_eq!(normalized(f64::NAN, 100.0), 0);
    }

    #[test]
    fn file_header_roundtrip_and_last_flag() {
        let payload = vec![9u8; 777];
        let hdr = FileHeader::new(42, 1 << 20, 7, 0, true);
        let buf = hdr.frame(&payload);
        let (got, body) = FileHeader::parse(&buf).unwrap();
        assert_eq!(got.transfer_id, 42);
        assert_eq!(got.offset, 1 << 20);
        assert_eq!(got.seq, 7);
        assert_eq!(got.len as usize, payload.len());
        assert!(got.is_last());
        assert_eq!(body, &payload[..]);
        assert_eq!(buf.len(), FILE_HEADER_LEN + payload.len());
    }

    #[test]
    fn file_header_rejects_bad_magic_and_truncation() {
        let mut buf = FileHeader::new(1, 0, 0, 0, false).frame(&[1, 2, 3]);
        buf[0] = b'X';
        assert!(matches!(FileHeader::parse(&buf), Err(ProtoError::BadMagic)));
        let mut good = FileHeader::new(1, 0, 0, 0, false).frame(&[1, 2, 3]);
        good.truncate(FILE_HEADER_LEN + 1);
        assert!(matches!(
            FileHeader::parse(&good),
            Err(ProtoError::ShortFrame(_))
        ));
    }

    #[test]
    fn handshake_messages_are_recognised() {
        let hello = Control::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Viewer,
            device: None,
            session_id: "s".into(),
            app: "web".into(),
        };
        assert!(hello.is_handshake());
        assert!(!Control::Ping { ts: 1 }.is_handshake());
    }
}

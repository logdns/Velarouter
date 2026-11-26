//! Fixed-format experimental system audio for VRP/1 (`pcm_s16le_48k_stereo`).

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const PACKET_MS: u32 = 20;
pub const PACKET_BYTES: usize = (RATE * PACKET_MS / 1_000) as usize * CHANNELS * 2;

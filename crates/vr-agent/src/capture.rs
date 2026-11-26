//! Screen capture with JPEG by default and optional software H.264.
//!
//! The reference agent uses a **software JPEG pipeline**. It is not the fastest
//! possible encoder, but it is the only one that works unchanged on Windows,
//! macOS, Linux, Android and iOS, and it lets a browser viewer decode with
//! `createImageBitmap` alone. H.264 is an opt-in desktop experiment.

use std::io::Cursor;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::{imageops, DynamicImage, ImageFormat, RgbaImage};
use vr_proto::{Codec, DisplayInfo};

/// One encoded frame, ready to be sealed and sent.
pub struct EncodedFrame {
    pub generation: u32,
    pub display: u32,
    pub width: u16,
    pub height: u16,
    pub codec: Codec,
    pub keyframe: bool,
    pub payload: Vec<u8>,
    pub capture: Duration,
    pub encode: Duration,
}

/// A frame source. Implementations exist for the desktop (`xcap`), Android
/// (`MediaProjection`), iOS (`ReplayKit`) and for tests.
pub trait FrameSource: Send {
    fn displays(&self) -> Vec<DisplayInfo>;
    /// Captures and encodes one frame. `None` means "identical to the previous
    /// frame", which lets the transport skip it entirely.
    fn next_frame(&mut self) -> Result<Option<EncodedFrame>>;
    fn select_display(&mut self, id: u32);
    fn set_quality(&mut self, quality: u8);
    fn set_limits(&mut self, max_width: u16, max_height: u16, fps: u8);
}

/// Desktop capturer built on `xcap` (GDI / DXGI / ScreenCaptureKit / X11+Wayland).
pub struct ScreenCapturer {
    monitors: Vec<xcap::Monitor>,
    selected: u32,
    quality: u8,
    max_width: u16,
    max_height: u16,
    fps: u8,
    last_signature: Option<u64>,
    last_frame_at: Instant,
    #[cfg(feature = "h264")]
    h264: Option<vr_video::h264::SoftwareEncoder>,
    #[cfg(feature = "h264")]
    h264_requested: bool,
    #[cfg(feature = "h264")]
    force_idr: bool,
}

impl ScreenCapturer {
    pub fn new(
        max_width: u16,
        max_height: u16,
        quality: u8,
        fps: u8,
        display: u32,
    ) -> Result<Self> {
        let monitors = xcap::Monitor::all().context(
            "cannot enumerate monitors; on macOS grant Screen Recording permission \
             (System Settings › Privacy & Security › Screen & System Audio Recording)",
        )?;
        if monitors.is_empty() {
            anyhow::bail!("no monitors found");
        }
        Ok(Self {
            monitors,
            selected: display,
            quality,
            max_width,
            max_height,
            fps,
            last_signature: None,
            last_frame_at: Instant::now(),
            #[cfg(feature = "h264")]
            h264: None,
            #[cfg(feature = "h264")]
            h264_requested: false,
            #[cfg(feature = "h264")]
            force_idr: true,
        })
    }

    #[cfg(feature = "h264")]
    pub fn enable_h264(&mut self) {
        self.h264_requested = true;
        self.reset_encoder();
    }

    #[cfg(feature = "h264")]
    fn reset_encoder(&mut self) {
        match vr_video::h264::SoftwareEncoder::new(self.quality, self.fps) {
            Ok(encoder) => self.h264 = Some(encoder),
            Err(error) => {
                tracing::warn!(%error, "H.264 unavailable; falling back to JPEG");
                self.h264 = None;
                self.h264_requested = false;
            }
        }
        self.last_signature = None;
        self.force_idr = true;
    }

    pub fn frame_dropped(&mut self) {
        self.last_signature = None;
        #[cfg(feature = "h264")]
        {
            self.force_idr = true;
        }
    }

    fn monitor(&self) -> Result<&xcap::Monitor> {
        self.monitors
            .iter()
            .find(|m| m.id().map(|id| id == self.selected).unwrap_or(false))
            .or_else(|| self.monitors.first())
            .context("no monitor selected")
    }

    /// Cheap perceptual signature: 16×16 grayscale, rounded to 4-bit buckets.
    /// It catches static desktops without hashing megabytes.
    fn signature(img: &RgbaImage) -> u64 {
        let small = imageops::resize(img, 16, 16, imageops::FilterType::Nearest);
        // FNV-1a over 4-bit luminance buckets. A plain XOR would cancel out on
        // uniform images (the same shift repeats an even number of times), which
        // is exactly the case a static desktop hits first.
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for p in small.pixels() {
            let lum = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) as u8;
            hash ^= (lum >> 4) as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

// `xcap::Monitor` holds an HMONITOR on Windows, which the `windows` crate marks
// as !Send. The handle is only used from the capture thread after construction
// and never shared, so moving it into that thread is sound.
unsafe impl Send for ScreenCapturer {}

impl FrameSource for ScreenCapturer {
    fn displays(&self) -> Vec<DisplayInfo> {
        self.monitors
            .iter()
            .filter_map(|m| {
                Some(DisplayInfo {
                    id: m.id().ok()?,
                    name: m.name().ok().unwrap_or_else(|| "Display".into()),
                    width: m.width().ok()?,
                    height: m.height().ok()?,
                    scale: m.scale_factor().ok().unwrap_or(1.0),
                    primary: m.is_primary().ok().unwrap_or(false),
                })
            })
            .collect()
    }

    fn next_frame(&mut self) -> Result<Option<EncodedFrame>> {
        let interval = Duration::from_millis(1000 / self.fps.max(1) as u64);
        std::thread::sleep(interval.saturating_sub(self.last_frame_at.elapsed()));

        let capture_started = Instant::now();
        let monitor = self.monitor()?;
        let raw = monitor.capture_image().context("screen capture failed")?;
        let capture = capture_started.elapsed();

        let signature = Self::signature(&raw);
        if self.last_signature == Some(signature) {
            self.last_frame_at = Instant::now();
            return Ok(None);
        }

        // Fit inside the requested box without ever upscaling.
        let (mut w, mut h) = (raw.width(), raw.height());
        let (max_w, max_h) = (
            self.max_width.max(320) as u32,
            self.max_height.max(240) as u32,
        );
        #[cfg(feature = "h264")]
        let (max_w, max_h) = if self.h264.is_some() {
            (
                max_w.min(u32::from(vr_video::h264::MAX_WIDTH)),
                max_h.min(u32::from(vr_video::h264::MAX_HEIGHT)),
            )
        } else {
            (max_w, max_h)
        };
        if w > max_w || h > max_h {
            let scale = (max_w as f32 / w as f32).min(max_h as f32 / h as f32);
            w = ((w as f32 * scale).round() as u32).max(2);
            h = ((h as f32 * scale).round() as u32).max(2);
        }

        #[cfg(feature = "h264")]
        if self.h264.is_some() {
            w = (w & !1).max(2);
            h = (h & !1).max(2);
        }
        let encode_started = Instant::now();
        let resized = if (w, h) == (raw.width(), raw.height()) {
            raw
        } else {
            imageops::resize(&raw, w, h, imageops::FilterType::Triangle)
        };
        #[cfg(feature = "h264")]
        if let Some(encoder) = self.h264.as_mut() {
            match encoder.encode(resized.as_raw(), w as u16, h as u16, self.force_idr) {
                Ok((payload, keyframe)) if !payload.is_empty() => {
                    self.force_idr = false;
                    self.last_signature = Some(signature);
                    self.last_frame_at = Instant::now();
                    return Ok(Some(EncodedFrame {
                        generation: 0,
                        display: self.selected,
                        width: w as u16,
                        height: h as u16,
                        codec: Codec::H264,
                        keyframe,
                        payload,
                        capture,
                        encode: encode_started.elapsed(),
                    }));
                }
                Ok(_) => {
                    self.last_frame_at = Instant::now();
                    return Ok(None);
                }
                Err(error) => {
                    tracing::warn!(%error, "H.264 encode failed; falling back to JPEG");
                    self.h264 = None;
                    self.h264_requested = false;
                }
            }
        }
        let rgb = DynamicImage::ImageRgba8(resized).to_rgb8();
        let mut jpeg = Vec::with_capacity(64 * 1024);
        let mut encoder =
            JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), self.quality.clamp(20, 95));
        encoder
            .encode(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .context("jpeg encode failed")?;
        let encode = encode_started.elapsed();

        self.last_signature = Some(signature);
        self.last_frame_at = Instant::now();
        Ok(Some(EncodedFrame {
            generation: 0,
            display: self.selected,
            width: w as u16,
            height: h as u16,
            codec: Codec::Jpeg,
            keyframe: true,
            payload: jpeg,
            capture,
            encode,
        }))
    }

    fn select_display(&mut self, id: u32) {
        if self.selected != id {
            self.selected = id;
            self.frame_dropped();
        }
    }

    fn set_quality(&mut self, quality: u8) {
        let quality = quality.clamp(20, 95);
        if self.quality != quality {
            self.quality = quality;
            #[cfg(feature = "h264")]
            if self.h264_requested {
                self.reset_encoder();
            }
            self.last_signature = None;
        }
    }

    fn set_limits(&mut self, max_width: u16, max_height: u16, fps: u8) {
        self.max_width = max_width.max(320);
        self.max_height = max_height.max(240);
        self.fps = fps.clamp(1, 120);
    }
}

/// Encodes a PNG/JPEG buffer into a normalised JPEG at the requested size.
/// Used by tests and by the `probe` subcommand.
pub fn reencode(bytes: &[u8], max_width: u32, max_height: u32, quality: u8) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes).context("unsupported image")?;
    let (w, h) = (
        img.width().min(max_width.max(2)),
        img.height().min(max_height.max(2)),
    );
    let resized = img
        .resize_exact(w, h, imageops::FilterType::Triangle)
        .to_rgb8();
    let mut out = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(Cursor::new(&mut out), quality.clamp(20, 95));
    enc.encode(resized.as_raw(), w, h, image::ExtendedColorType::Rgb8)?;
    Ok(out)
}

#[allow(dead_code)]
fn _assert_image_formats() {
    let _ = ImageFormat::Jpeg;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_stable_and_detects_change() {
        let a = RgbaImage::from_pixel(64, 64, image::Rgba([10, 20, 30, 255]));
        let b = RgbaImage::from_pixel(64, 64, image::Rgba([10, 20, 30, 255]));
        let c = RgbaImage::from_pixel(64, 64, image::Rgba([250, 250, 250, 255]));
        assert_eq!(ScreenCapturer::signature(&a), ScreenCapturer::signature(&b));
        assert_ne!(ScreenCapturer::signature(&a), ScreenCapturer::signature(&c));
    }

    #[test]
    fn reencode_produces_jpeg() {
        let mut png = Vec::new();
        let img = image::RgbImage::from_pixel(40, 30, image::Rgb([1, 2, 3]));
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let jpeg = reencode(&png, 20, 20, 60).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "jpeg SOI marker");
    }
}

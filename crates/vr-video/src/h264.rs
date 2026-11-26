//! Experimental, source-built Cisco OpenH264 backend. Not in default packages.
use anyhow::{ensure, Context, Result};
use openh264::{
    decoder::Decoder,
    encoder::{
        BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode,
        UsageType,
    },
    formats::{RgbaSliceU8, YUVBuffer, YUVSource},
    OpenH264API,
};
use vr_proto::{frame_flags, Codec, VideoHeader};

const MAX_PACKET: usize = 8 * 1024 * 1024;
pub const MAX_WIDTH: u16 = 1920;
pub const MAX_HEIGHT: u16 = 1080;
pub const MAX_FPS: u8 = 30;

pub struct SoftwareEncoder {
    encoder: Encoder,
    yuv: Option<YUVBuffer>,
}

impl SoftwareEncoder {
    pub fn new(quality: u8, fps: u8) -> Result<Self> {
        let fps = fps.clamp(1, MAX_FPS);
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(
                500_000 + u32::from(quality.clamp(20, 95)) * 50_000,
            ))
            .max_frame_rate(FrameRate::from_hz(f32::from(fps)))
            .intra_frame_period(IntraFramePeriod::from_num_frames(u32::from(fps)))
            .adaptive_quantization(false)
            .background_detection(false)
            .skip_frames(true)
            .num_threads(1);
        Ok(Self {
            encoder: Encoder::with_api_config(OpenH264API::from_source(), config)?,
            yuv: None,
        })
    }

    pub fn encode(
        &mut self,
        rgba: &[u8],
        width: u16,
        height: u16,
        force_idr: bool,
    ) -> Result<(Vec<u8>, bool)> {
        ensure!(
            (2..=MAX_WIDTH).contains(&width) && (2..=MAX_HEIGHT).contains(&height),
            "invalid H.264 dimensions"
        );
        let size = (usize::from(width), usize::from(height));
        ensure!(
            size.0.is_multiple_of(2)
                && size.1.is_multiple_of(2)
                && rgba.len() == size.0 * size.1 * 4,
            "invalid RGBA input"
        );
        if self.yuv.as_ref().is_none_or(|yuv| yuv.dimensions() != size) {
            self.yuv = Some(YUVBuffer::new(size.0, size.1));
        } else if force_idr {
            // ForceIntraFrame is only valid after OpenH264 was initialized
            // with a frame; new dimensions automatically start with an IDR.
            self.encoder.force_intra_frame();
        }
        let yuv = self.yuv.as_mut().expect("YUV buffer created above");
        yuv.read_rgba8(RgbaSliceU8::new(rgba, size));
        let stream = self.encoder.encode(yuv)?;
        let keyframe = stream.frame_type() == FrameType::IDR;
        Ok((stream.to_vec(), keyframe))
    }
}

/// Stateful receiver. After an interruption, resumes only at an IDR.
#[derive(Default)]
pub struct SoftwareDecoder {
    decoder: Option<Decoder>,
}

impl SoftwareDecoder {
    pub fn reset(&mut self) {
        self.decoder = None;
    }

    pub fn decode(&mut self, header: VideoHeader, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
        ensure!(
            header.codec == Codec::H264
                && (2..=MAX_WIDTH).contains(&header.width)
                && (2..=MAX_HEIGHT).contains(&header.height)
                && header.width.is_multiple_of(2)
                && header.height.is_multiple_of(2)
                && !bytes.is_empty()
                && bytes.len() <= MAX_PACKET
                && header.flags & frame_flags::TILED == 0,
            "invalid H.264 frame metadata"
        );
        if header.flags & frame_flags::KEYFRAME != 0 {
            self.decoder = Some(Decoder::new()?);
        }
        let Some(decoder) = &mut self.decoder else {
            return Ok(None);
        };
        let Some(decoded) = decoder.decode(bytes).context("H.264 decode failed")? else {
            return Ok(None);
        };
        let size = (usize::from(header.width), usize::from(header.height));
        ensure!(
            decoded.dimensions() == size,
            "H.264 dimensions do not match VRP header"
        );
        let mut rgba = vec![0; size.0 * size.1 * 4];
        decoded.write_rgba8(&mut rgba);
        Ok(Some(rgba))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_idr_and_delta_then_recovers_at_idr() {
        let mut encoder = SoftwareEncoder::new(70, 30).unwrap();
        let mut decoder = SoftwareDecoder::default();
        let (width, height) = (128, 96);
        let mut pixels = vec![180; usize::from(width) * usize::from(height) * 4];
        let mut header = VideoHeader::new(Codec::H264, 0, 1, width, height);
        let (first, keyframe) = encoder.encode(&pixels, width, height, false).unwrap();
        assert!(keyframe);
        header.flags |= frame_flags::KEYFRAME;
        assert_eq!(
            decoder.decode(header, &first).unwrap().unwrap().len(),
            pixels.len()
        );

        pixels[..2048].fill(80);
        let (next, keyframe) = encoder.encode(&pixels, width, height, false).unwrap();
        assert!(!keyframe);
        header.flags = 0;
        assert!(decoder.decode(header, &next).unwrap().is_some());
        decoder.reset();
        assert!(decoder.decode(header, &next).unwrap().is_none());

        let (restart, keyframe) = encoder.encode(&pixels, width, height, true).unwrap();
        assert!(keyframe);
        header.flags = frame_flags::KEYFRAME;
        assert!(decoder.decode(header, &restart).unwrap().is_some());
        header.width = MAX_WIDTH + 2;
        assert!(decoder.decode(header, &restart).is_err());
    }
}

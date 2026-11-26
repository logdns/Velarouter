//! Bounded JPEG decoding and an ASCII luminance ramp inside an ANSI screen.
use std::io::Cursor;

use anyhow::{ensure, Context, Result};
use image::{imageops, GrayImage, ImageFormat, ImageReader, Limits};
use vr_proto::{frame_flags, Codec, VideoHeader};

const MAX_SIDE: u32 = 4096;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const RAMP: &[u8] = b" .:-=+*#%@";

pub(super) fn decode(header: VideoHeader, bytes: &[u8]) -> Result<GrayImage> {
    ensure!(
        header.codec == Codec::Jpeg && header.flags & frame_flags::TILED == 0,
        "不支持的画面格式。"
    );
    ensure!(
        bytes.len() <= MAX_FRAME_BYTES
            && u32::from(header.width) <= MAX_SIDE
            && u32::from(header.height) <= MAX_SIDE,
        "画面数据过大。"
    );
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Jpeg);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let frame = reader.decode().context("无法解码远端 JPEG 画面")?;
    ensure!(
        frame.width() == u32::from(header.width) && frame.height() == u32::from(header.height),
        "画面尺寸与协议头不一致。"
    );
    Ok(frame.into_luma8())
}

pub(super) fn render(frame: &GrayImage, size: (u16, u16)) -> String {
    // Leave a column, a header row and the last row unused to avoid wrapping
    // and scrolling on terminals with automatic wrap enabled.
    let max_width = u32::from(size.0.saturating_sub(1).min(240));
    let max_height = u32::from(size.1.saturating_sub(2).min(120));
    if max_width == 0 || max_height == 0 || frame.width() == 0 || frame.height() == 0 {
        return String::new();
    }
    // Most monospace terminal cells are about twice as tall as they are wide.
    let adjusted_height = frame.height().div_ceil(2);
    let width = max_width
        .min(max_height * frame.width() / adjusted_height)
        .max(1);
    let height = (width * adjusted_height / frame.width()).clamp(1, max_height);
    let resized = imageops::resize(frame, width, height, imageops::FilterType::Triangle);
    let mut output = String::with_capacity(((width + 2) * height) as usize);
    for (index, row) in resized.as_raw().chunks_exact(width as usize).enumerate() {
        if index > 0 {
            output.push_str("\r\n");
        }
        for &pixel in row {
            output.push(char::from(
                RAMP[usize::from(pixel) * (RAMP.len() - 1) / 255],
            ));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{codecs::jpeg::JpegEncoder, ExtendedColorType, Luma};

    #[test]
    fn image_stays_inside_terminal_without_control_characters() {
        let frame = GrayImage::from_pixel(1600, 900, Luma([255]));
        let output = render(&frame, (80, 24));
        assert_eq!(output.lines().count(), 21);
        assert!(output
            .lines()
            .all(|s| s.len() <= 79 && s.chars().all(|c| c == '@')));
        assert_eq!(render(&frame, (1, 2)), "");
    }

    #[test]
    fn only_bounded_matching_jpeg_frames_are_decoded() {
        let mut encoded = Vec::new();
        JpegEncoder::new(&mut encoded)
            .encode(&[255, 0], 2, 1, ExtendedColorType::L8)
            .unwrap();
        let header = VideoHeader::new(Codec::Jpeg, 1, 0, 2, 1);
        assert_eq!(decode(header, &encoded).unwrap().dimensions(), (2, 1));
        assert!(decode(
            VideoHeader {
                width: 200,
                ..header
            },
            &encoded
        )
        .is_err());
        assert!(decode(
            VideoHeader {
                codec: Codec::H264,
                ..header
            },
            &encoded
        )
        .is_err());
        assert!(decode(header, b"not a JPEG").is_err());
        encoded.clear();
        let width = MAX_SIDE + 1;
        JpegEncoder::new(&mut encoded)
            .encode(&vec![255; width as usize], width, 1, ExtendedColorType::L8)
            .unwrap();
        assert!(decode(
            VideoHeader {
                width: width as u16,
                ..header
            },
            &encoded
        )
        .is_err());
    }
}

//! Negotiation shared by the software video pipeline. JPEG is the VRP/1 baseline.
use vr_proto::Codec;

#[cfg(feature = "h264")]
pub mod h264;

pub fn capture_codec(requested: Codec, host: &[Codec], viewer: &[Codec]) -> Codec {
    if requested == Codec::H264 && host.contains(&Codec::H264) && viewer.contains(&Codec::H264) {
        Codec::H264
    } else {
        Codec::Jpeg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_requires_an_explicit_request_and_support_at_both_ends() {
        let jpeg = [Codec::Jpeg];
        let both = [Codec::Jpeg, Codec::H264];
        assert_eq!(capture_codec(Codec::H264, &both, &both), Codec::H264);
        for (request, host, viewer) in [
            (Codec::Jpeg, &both[..], &both[..]),
            (Codec::H264, &jpeg[..], &both[..]),
            (Codec::H264, &both[..], &jpeg[..]),
            (Codec::H264, &both[..], &[][..]),
            (Codec::Av1, &both[..], &both[..]),
        ] {
            assert_eq!(capture_codec(request, host, viewer), Codec::Jpeg);
        }
    }
}

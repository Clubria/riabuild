//! The one transform the channel applies.
//!
//! Claude's vision resizes anything above this long edge and discards the
//! excess, so sending a 5K screenshot uncompressed costs several times the
//! transfer time and yields the model no additional pixels. Resizing *to* the
//! ceiling loses nothing the model would have seen; resizing below it would,
//! which is why the constant sits at the ceiling and not under it.
//!
//! This is not a setting. There is no config key, no environment variable, and
//! no dashboard field — riabuild does not ask a developer to pick a resolution
//! any more than it asks them to pick a Node version. Changing the number is a
//! release.
//!
//! It belongs here, on the laptop, rather than in the shim: the shim runs after
//! the bytes have already crossed the wire, so resizing there would save tokens
//! but not transfer time, which is the whole point.

use image::ImageFormat;
use image::imageops::FilterType;

/// The long-edge ceiling Claude's vision applies before it looks at an image.
/// Resizing to this is information-neutral: the detail discarded here is the
/// detail the model was going to discard anyway.
pub const MAX_LONG_EDGE: u32 = 2576;

/// Brings an oversized image down to the ceiling. Everything else is returned
/// untouched.
///
/// Never fails. An image that cannot be decoded is passed through whole — a
/// clipboard bridge that refuses to carry a picture because a decoder did not
/// recognise it is worse than one that carries it at full size.
pub fn to_ceiling(mime: &str, bytes: Vec<u8>) -> Vec<u8> {
    if !mime.starts_with("image/") {
        return bytes;
    }

    let Ok(image) = image::load_from_memory(&bytes) else {
        return bytes;
    };

    let long_edge = image.width().max(image.height());
    if long_edge <= MAX_LONG_EDGE {
        // The common case, and the reason this is a cheap check rather than an
        // unconditional re-encode: a screenshot already under the ceiling
        // crosses the wire as the exact bytes the pasteboard held.
        return bytes;
    }

    // `thumbnail` is faster but visibly soft on small text, and a dense error
    // dialog is the paste this feature exists for. Lanczos3 keeps type legible.
    let scaled = image.resize(MAX_LONG_EDGE, MAX_LONG_EDGE, FilterType::Lanczos3);

    let mut out = std::io::Cursor::new(Vec::new());
    match scaled.write_to(&mut out, ImageFormat::Png) {
        Ok(()) => out.into_inner(),
        Err(_) => bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbaImage};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, image::Rgba([1, 2, 3, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut bytes, ImageFormat::Png)
            .expect("encode png");
        bytes.into_inner()
    }

    fn dimensions(bytes: &[u8]) -> (u32, u32) {
        image::load_from_memory(bytes)
            .map(|image| (image.width(), image.height()))
            .expect("decode png")
    }

    #[test]
    fn the_ceiling_is_the_models_own_long_edge_limit() {
        assert_eq!(MAX_LONG_EDGE, 2576);
    }

    /// The common case. Decoding and re-encoding a screenshot that is already
    /// under the ceiling would change its bytes for no gain.
    #[test]
    fn an_image_under_the_ceiling_is_returned_byte_for_byte() {
        let original = png(800, 600);
        let out = to_ceiling("image/png", original.clone());
        assert_eq!(out, original);
    }

    #[test]
    fn an_image_exactly_at_the_ceiling_is_untouched() {
        let original = png(MAX_LONG_EDGE, 100);
        let out = to_ceiling("image/png", original.clone());
        assert_eq!(out, original);
    }

    // Fixtures stay just over the ceiling with exact 2:1 ratios. Lanczos3 over
    // a 24-megapixel image in a debug build costs half a minute, and these
    // assert the same arithmetic for a tenth of it.

    #[test]
    fn a_wide_image_is_scaled_to_the_ceiling_on_its_long_edge() {
        let out = to_ceiling("image/png", png(2600, 1300));
        assert_eq!(dimensions(&out), (MAX_LONG_EDGE, 1288));
    }

    #[test]
    fn a_tall_image_is_scaled_on_its_long_edge_too() {
        let out = to_ceiling("image/png", png(1300, 2600));
        assert_eq!(dimensions(&out), (1288, MAX_LONG_EDGE));
    }

    #[test]
    fn an_oversized_image_gets_smaller_on_the_wire() {
        let original = png(3000, 2000);
        let out = to_ceiling("image/png", original.clone());
        assert!(
            out.len() < original.len(),
            "{} -> {}",
            original.len(),
            out.len()
        );
    }

    /// Text is never an image, whatever its bytes look like.
    #[test]
    fn non_image_types_are_never_decoded() {
        let text = b"long edge 9999".to_vec();
        assert_eq!(to_ceiling("text/plain;charset=utf-8", text.clone()), text);
        assert_eq!(to_ceiling("text/html", text.clone()), text);
    }

    /// Carrying an image whole is better than refusing to carry it because a
    /// decoder did not recognise it.
    #[test]
    fn an_undecodable_image_is_passed_through_rather_than_dropped() {
        let junk = b"\x89PNG not really a png".to_vec();
        assert_eq!(to_ceiling("image/png", junk.clone()), junk);
    }

    /// An oversized image is re-encoded, and the far side was told it is
    /// getting a PNG.
    #[test]
    fn an_oversized_image_re_encodes_to_something_still_decodable() {
        let out = to_ceiling("image/png", png(2600, 1950));
        assert_eq!(image::guess_format(&out).unwrap(), ImageFormat::Png);
    }
}

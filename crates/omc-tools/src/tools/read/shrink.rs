//! Shrinking images that are too large to hand to the model.
//!
//! An image larger than the byte limit, or with a side longer than
//! [`MAX_SIDE`], is decoded, scaled so its long side fits, turned upright by
//! its EXIF orientation and re-encoded: as PNG when it has transparent
//! pixels and stays small enough, otherwise as JPEG at falling qualities.
//! While nothing fits, the dimensions shrink by a quarter and the encodings
//! are tried again.
//!
//! Decoding is bounded so that a small file cannot unpack into a huge
//! bitmap: images with a side over [`MAX_DECODED_SIDE`] pixels, or whose
//! pixels would take more than [`MAX_DECODED_BYTES`], are refused before any
//! pixel is decoded. Scaling averages blocks of pixels, which needs no
//! buffer larger than its output.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder as _, ImageFormat, ImageReader, Limits, RgbImage};

/// Longest side, in pixels, of an image handed to the model.
pub(super) const MAX_SIDE: u32 = 2000;

/// Longest side, in pixels, of an image decoded to be shrunk.
pub(super) const MAX_DECODED_SIDE: u32 = 12_000;

/// Most bytes the decoded pixels of an image may take.
pub(super) const MAX_DECODED_BYTES: u64 = 256 << 20;

/// JPEG qualities tried in turn at each size.
const JPEG_QUALITIES: [u8; 4] = [80, 70, 55, 40];

/// An image re-encoded to fit the model's limits.
#[derive(Debug)]
pub(super) struct Shrunk {
    /// The encoded image.
    pub(super) bytes: Vec<u8>,
    /// Its MIME type, `image/png` or `image/jpeg`.
    pub(super) media_type: &'static str,
    /// Width and height of the original, upright.
    pub(super) original: (u32, u32),
    /// Width and height of the image as encoded.
    pub(super) shown: (u32, u32),
}

impl Shrunk {
    /// Describes the change for the model: the original and shown sizes and,
    /// when they differ, what to multiply coordinates in the shown image by
    /// to map them back to the original.
    pub(super) fn note(&self) -> String {
        let (ow, oh) = self.original;
        let (sw, sh) = self.shown;
        if self.original == self.shown {
            return format!("re-encoded at {ow}x{oh} to fit the size limit");
        }
        let x = f64::from(ow) / f64::from(sw);
        let y = f64::from(oh) / f64::from(sh);
        let factor = if (x - y).abs() < 0.005 {
            format!("{x:.3}")
        } else {
            format!("{x:.3} (x) and {y:.3} (y)")
        };
        format!(
            "shrunk from {ow}x{oh} to {sw}x{sh}; multiply coordinates in this image by \
             {factor} to map them to the original"
        )
    }
}

/// Shrinks the image `bytes` to at most `max_bytes` bytes with no side longer
/// than [`MAX_SIDE`], or returns `None` when it already fits, or when it is
/// small enough and its dimensions cannot be read, so it is passed on as is.
///
/// # Errors
///
/// Returns a description of the problem when the image must shrink but
/// cannot be decoded, is larger than [`MAX_DECODED_SIDE`] or
/// [`MAX_DECODED_BYTES`] allow, or no size down to one pixel fits
/// `max_bytes`.
pub(super) fn shrink(bytes: &[u8], max_bytes: usize) -> Result<Option<Shrunk>, String> {
    let reader = || {
        ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())
    };
    let fits_sides = reader()?
        .into_dimensions()
        .map(|(w, h)| w.max(h) <= MAX_SIDE)
        .unwrap_or(true);
    if bytes.len() <= max_bytes && fits_sides {
        return Ok(None);
    }
    let mut bounded = reader()?;
    bounded.limits(limits());
    let mut decoder = bounded.into_decoder().map_err(too_large)?;
    check_decoded_size(decoder.dimensions(), decoder.total_bytes())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let image = DynamicImage::from_decoder(decoder).map_err(too_large)?;
    let turned = turns_sideways(orientation);
    let upright = |(width, height): (u32, u32)| {
        if turned {
            (height, width)
        } else {
            (width, height)
        }
    };
    let original = upright((image.width(), image.height()));
    let mut size = scaled(original, MAX_SIDE);
    loop {
        let mut resized = if size == original {
            image.clone()
        } else {
            let (width, height) = upright(size);
            image.thumbnail_exact(width, height)
        };
        resized.apply_orientation(orientation);
        if let Some((bytes, media_type)) = encode(&resized, max_bytes)? {
            return Ok(Some(Shrunk {
                bytes,
                media_type,
                original,
                shown: size,
            }));
        }
        if size == (1, 1) {
            return Err(format!(
                "no size of it fits in {max_bytes} bytes, even at one pixel"
            ));
        }
        size = ((size.0 * 3 / 4).max(1), (size.1 * 3 / 4).max(1));
    }
}

/// The limits images are decoded under: sides of at most
/// [`MAX_DECODED_SIDE`] and at most [`MAX_DECODED_BYTES`] allocated.
fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODED_SIDE);
    limits.max_image_height = Some(MAX_DECODED_SIDE);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    limits
}

/// Checks that an image of `(width, height)` pixels, whose decoded pixels
/// take `bytes`, is small enough to decode.
///
/// # Errors
///
/// Returns a description of the problem when a side is over
/// [`MAX_DECODED_SIDE`] or the pixels take more than [`MAX_DECODED_BYTES`].
fn check_decoded_size((width, height): (u32, u32), bytes: u64) -> Result<(), String> {
    if width > MAX_DECODED_SIDE || height > MAX_DECODED_SIDE || bytes > MAX_DECODED_BYTES {
        return Err(format!(
            "it is {width}x{height} pixels, which would take {} to decode; read decodes images \
             of at most {MAX_DECODED_SIDE} pixels a side and {} of pixels, so make a smaller copy \
             first",
            super::media::size(usize::try_from(bytes).unwrap_or(usize::MAX)),
            super::media::size(usize::try_from(MAX_DECODED_BYTES).unwrap_or(usize::MAX))
        ));
    }
    Ok(())
}

/// Describes a decoding error, saying so plainly when it is a limit of
/// [`limits`] that stopped it.
fn too_large(error: image::ImageError) -> String {
    match error {
        image::ImageError::Limits(_) => format!(
            "it is larger than read decodes ({error}); read decodes images of at most \
             {MAX_DECODED_SIDE} pixels a side and {} of pixels, so make a smaller copy first",
            super::media::size(usize::try_from(MAX_DECODED_BYTES).unwrap_or(usize::MAX))
        ),
        other => other.to_string(),
    }
}

/// Whether turning an image upright by `orientation` swaps its width and
/// height.
fn turns_sideways(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

/// Scales `(width, height)` so the longer side is at most `max_side`,
/// keeping the aspect ratio and every side at least one pixel.
fn scaled((width, height): (u32, u32), max_side: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= max_side {
        return (width, height);
    }
    let scale = |side: u32| {
        u32::try_from(
            (u64::from(side) * u64::from(max_side) + u64::from(long) / 2) / u64::from(long),
        )
        .unwrap_or(max_side)
        .max(1)
    };
    (scale(width), scale(height))
}

/// Encodes `image` in the first form that fits `max_bytes`: PNG when it has
/// transparent pixels, then JPEG at each of [`JPEG_QUALITIES`] over white.
/// Returns `None` when none fits.
///
/// # Errors
///
/// Returns a description of the problem when encoding fails.
fn encode(
    image: &DynamicImage,
    max_bytes: usize,
) -> Result<Option<(Vec<u8>, &'static str)>, String> {
    let transparent =
        image.color().has_alpha() && image.to_rgba8().pixels().any(|pixel| pixel[3] < u8::MAX);
    if transparent {
        let mut png = Cursor::new(Vec::new());
        image
            .write_to(&mut png, ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        if png.get_ref().len() <= max_bytes {
            return Ok(Some((png.into_inner(), "image/png")));
        }
    }
    let rgb = if transparent {
        over_white(image)
    } else {
        image.to_rgb8()
    };
    for quality in JPEG_QUALITIES {
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, quality)
            .encode_image(&rgb)
            .map_err(|e| e.to_string())?;
        if jpeg.len() <= max_bytes {
            return Ok(Some((jpeg, "image/jpeg")));
        }
    }
    Ok(None)
}

/// Flattens `image` onto a white background, for formats without
/// transparency.
fn over_white(image: &DynamicImage) -> RgbImage {
    let rgba = image.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let blend = |channel: u8| {
            let channel = u16::from(channel);
            let alpha = u16::from(a);
            u8::try_from((channel * alpha + 255 * (255 - alpha) + 127) / 255).unwrap_or(u8::MAX)
        };
        image::Rgb([blend(r), blend(g), blend(b)])
    })
}

#[cfg(test)]
pub(super) mod tests {
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    use super::*;

    /// A PNG of `width` by `height` noisy pixels, which compresses badly, so
    /// its size grows with its area.
    pub(in super::super) fn noisy_png(width: u32, height: u32) -> Vec<u8> {
        let mut state = 0x2545_f491_u32;
        let image = RgbImage::from_fn(width, height, |_, _| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let [r, g, b, _] = state.to_le_bytes();
            Rgb([r, g, b])
        });
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// Long sides scale to the limit with the aspect kept; small ones stay.
    #[test]
    fn scales_to_the_long_side() {
        assert_eq!(scaled((4000, 3000), 2000), (2000, 1500));
        assert_eq!(scaled((1000, 5000), 2000), (400, 2000));
        assert_eq!(scaled((9000, 1), 2000), (2000, 1));
        assert_eq!(scaled((800, 600), 2000), (800, 600));
    }

    /// An image within the limits is left alone; one too wide is scaled
    /// down to JPEG, with a note mapping coordinates back.
    #[test]
    fn shrinks_only_what_does_not_fit() {
        let small = noisy_png(40, 30);
        assert!(shrink(&small, 1 << 20).unwrap().is_none());
        let wide = noisy_png(2400, 60);
        let shrunk = shrink(&wide, 1 << 20).unwrap().unwrap();
        assert_eq!((shrunk.original, shrunk.shown), ((2400, 60), (2000, 50)));
        assert_eq!(shrunk.media_type, "image/jpeg");
        assert!(
            shrunk.note().contains("by 1.200 to map"),
            "{}",
            shrunk.note()
        );
    }

    /// An image too large in bytes shrinks further until it fits.
    #[test]
    fn steps_down_until_it_fits() {
        let png = noisy_png(400, 400);
        let shrunk = shrink(&png, 20_000).unwrap().unwrap();
        assert!(shrunk.bytes.len() <= 20_000);
        assert!(shrunk.shown.0 < 400, "{:?}", shrunk.shown);
    }

    /// Transparent images stay PNG when that fits; opaque pixels of a
    /// transparent image land on white when it must become JPEG.
    #[test]
    fn keeps_transparency_as_png() {
        let image = RgbaImage::from_fn(2100, 10, |x, _| Rgba([0, 0, 0, (x % 2 * 255) as u8]));
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, ImageFormat::Png).unwrap();
        let shrunk = shrink(png.get_ref(), 1 << 20).unwrap().unwrap();
        assert_eq!(shrunk.media_type, "image/png");
        let flat = over_white(&DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            1,
            1,
            Rgba([0, 0, 0, 0]),
        )));
        assert_eq!(flat.get_pixel(0, 0).0, [255, 255, 255]);
    }

    /// A JPEG of `width` by `height` whose EXIF data says to turn it a
    /// quarter clockwise to show it upright.
    fn sideways_jpeg(width: u32, height: u32) -> Vec<u8> {
        let image = RgbImage::from_pixel(width, height, Rgb([200, 30, 30]));
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode_image(&image)
            .unwrap();
        let tiff: &[u8] = b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0";
        let payload = [b"Exif\0\0".as_slice(), tiff].concat();
        let length = u16::try_from(payload.len() + 2).unwrap().to_be_bytes();
        [&jpeg[..2], &[0xff, 0xe1], &length, &payload, &jpeg[2..]].concat()
    }

    /// A shrunk image is turned upright by its EXIF orientation first.
    #[test]
    fn applies_exif_orientation() {
        let jpeg = sideways_jpeg(60, 20);
        let shrunk = shrink(&jpeg, jpeg.len() - 1).unwrap().unwrap();
        assert_eq!(shrunk.original, (20, 60));
    }

    /// A one-pixel RGBA PNG whose header claims `width` by `height`
    /// pixels, as a decompression bomb's does, at a few dozen bytes.
    fn png_claiming(width: u32, height: u32) -> Vec<u8> {
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::new(1, 1)
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        let mut bytes = png.into_inner();
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(&bytes[12..29]);
        bytes[29..33].copy_from_slice(&crc.sum().to_be_bytes());
        bytes
    }

    /// A small file that would decode into a huge bitmap is refused before
    /// it is decoded, with a message saying why.
    #[test]
    fn refuses_decompression_bombs() {
        let tall = shrink(&png_claiming(100, MAX_DECODED_SIDE + 1), 1 << 20).unwrap_err();
        assert!(
            tall.contains("larger than read decodes") && tall.contains("12000 pixels a side"),
            "{tall}"
        );
        let heavy = shrink(&png_claiming(9000, 9000), 1 << 20).unwrap_err();
        assert!(
            heavy.contains("9000x9000 pixels, which would take 324.0 MB to decode"),
            "{heavy}"
        );
    }

    /// Undecodable bytes pass when small and fail when they must shrink.
    #[test]
    fn handles_undecodable_bytes() {
        assert!(shrink(b"\x89PNG\r\n\x1a\n", 100).unwrap().is_none());
        assert!(shrink(&[0u8; 200], 100).is_err());
    }
}

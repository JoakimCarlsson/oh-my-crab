//! Images and PDFs: recognizing them by their first bytes, handing images to
//! the model and pulling the text out of PDFs.

use base64::Engine as _;

use super::shrink;
use crate::{Image, ToolError, ToolOutput};

/// Largest image handed to the model, in bytes; base64 makes it a third larger,
/// which keeps it under the 5 MB providers accept. Larger images are shrunk.
pub(super) const MAX_IMAGE_BYTES: usize = 3_750_000;

/// Largest image file `read` decodes to shrink it, in bytes.
pub(super) const MAX_IMAGE_FILE_BYTES: usize = 64 << 20;

/// Largest PDF whose text is extracted, in bytes.
pub(super) const MAX_PDF_BYTES: usize = 64 << 20;

/// A kind of file `read` does not show as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Media {
    /// An image the model can look at, with its MIME type.
    Image(&'static str),
    /// A PDF document.
    Pdf,
}

/// Recognizes an image or PDF from the first bytes of a file, `head`.
///
/// Returns `None` for anything else, text included.
pub(super) fn sniff(head: &[u8]) -> Option<Media> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Media::Image("image/png"))
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(Media::Image("image/jpeg"))
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some(Media::Image("image/gif"))
    } else if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        Some(Media::Image("image/webp"))
    } else if head.starts_with(b"%PDF-") {
        Some(Media::Pdf)
    } else {
        None
    }
}

/// Hands the image `bytes` of type `media_type`, called `label`, to the model,
/// under a line saying what it is.
///
/// An image over [`MAX_IMAGE_BYTES`] or with a side over
/// [`shrink::MAX_SIDE`] pixels is shrunk first, and the line then says how
/// coordinates in it map back to the original; any other image is handed on
/// unchanged.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when the image is larger than
/// [`MAX_IMAGE_FILE_BYTES`], or must shrink but cannot be decoded or made
/// small enough.
pub(super) fn image(bytes: &[u8], media_type: &str, label: &str) -> Result<ToolOutput, ToolError> {
    check_image_size(bytes.len(), label)?;
    let shrunk = shrink::shrink(bytes, MAX_IMAGE_BYTES).map_err(|error| {
        ToolError::Failed(format!("could not shrink the image {label}: {error}"))
    })?;
    let (bytes, media_type, note) = match &shrunk {
        Some(shrunk) => (
            &shrunk.bytes[..],
            shrunk.media_type,
            format!("; {}", shrunk.note()),
        ),
        None => (bytes, media_type, String::new()),
    };
    let text = format!("[image {label}, {media_type}, {}{note}]", size(bytes.len()));
    Ok(ToolOutput::new(text).with_images(vec![Image {
        media_type: media_type.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    }]))
}

/// The image in the file at `path`, shrunk as `read` shrinks images for the model, with the line that says what it is; nothing when the file is not an image `read` hands to the model.
///
/// # Errors
///
/// Returns the reason when the file is an image that cannot be read, is too large or cannot be shrunk enough.
pub fn attached_image(path: &std::path::Path) -> Option<Result<(Image, String), String>> {
    let mut head = [0u8; 16];
    let read = std::io::Read::read(&mut std::fs::File::open(path).ok()?, &mut head).ok()?;
    let Some(Media::Image(media_type)) = sniff(&head[..read]) else {
        return None;
    };
    let label = path.display().to_string();
    let loaded = std::fs::metadata(path)
        .map_err(|error| error.to_string())
        .and_then(|metadata| {
            check_image_size(
                usize::try_from(metadata.len()).unwrap_or(usize::MAX),
                &label,
            )
            .map_err(|error| error.to_string())
        })
        .and_then(|()| std::fs::read(path).map_err(|error| error.to_string()))
        .and_then(|bytes| image(&bytes, media_type, &label).map_err(|error| error.to_string()))
        .and_then(|mut output| {
            output
                .images
                .pop()
                .map(|image| (image, output.text))
                .ok_or_else(|| format!("{label} gave no image"))
        });
    Some(loaded)
}

/// Checks that an image of `len` bytes, called `label`, is small enough to
/// decode and, if need be, shrink for the model.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when it is larger than
/// [`MAX_IMAGE_FILE_BYTES`].
pub(super) fn check_image_size(len: usize, label: &str) -> Result<(), ToolError> {
    if len <= MAX_IMAGE_FILE_BYTES {
        return Ok(());
    }
    Err(ToolError::Failed(format!(
        "{label} is a {} image, larger than the {} read shrinks for the model; make a smaller \
         copy first",
        size(len),
        size(MAX_IMAGE_FILE_BYTES)
    )))
}

/// Checks that a PDF of `len` bytes, called `label`, is small enough to
/// extract text from.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when it is larger than [`MAX_PDF_BYTES`].
pub(super) fn check_pdf_size(len: usize, label: &str) -> Result<(), ToolError> {
    if len <= MAX_PDF_BYTES {
        return Ok(());
    }
    Err(ToolError::Failed(format!(
        "{label} is a {} PDF, larger than the {} read extracts text from",
        size(len),
        size(MAX_PDF_BYTES)
    )))
}

/// Says how many pages a document has, such as `1 page` or `12 pages`.
pub(super) fn pages(count: usize) -> String {
    if count == 1 {
        "1 page".to_string()
    } else {
        format!("{count} pages")
    }
}

/// Extracts the text of the PDF `bytes`, called `label`, with a `[page N]`
/// line before each page's text, and returns it with the number of pages.
///
/// The extractor runs behind a panic guard, since malformed files can make it
/// panic.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when the PDF is too large, cannot be parsed or
/// holds no text, as scanned documents do.
pub(super) fn pdf_text(bytes: &[u8], label: &str) -> Result<(String, usize), ToolError> {
    check_pdf_size(bytes.len(), label)?;
    let extracted = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(bytes));
    let pages = match extracted {
        Ok(Ok(pages)) => pages,
        Ok(Err(error)) => {
            return Err(ToolError::Failed(format!(
                "could not read the PDF {label}: {error}"
            )));
        }
        Err(_) => {
            return Err(ToolError::Failed(format!(
                "could not read the PDF {label}: it looks malformed"
            )));
        }
    };
    if pages.iter().all(|page| page.trim().is_empty()) {
        return Err(ToolError::Failed(format!(
            "{label} is a PDF of {} with no extractable text; it may be scanned images",
            self::pages(pages.len())
        )));
    }
    let text = pages
        .iter()
        .enumerate()
        .map(|(index, page)| format!("[page {}]\n{}", index + 1, tidy(page)))
        .collect::<Vec<_>>()
        .join("\n");
    Ok((text, pages.len()))
}

/// Trims trailing spaces from every line of `text` and squeezes runs of blank
/// lines into one, ending with a single newline.
pub(super) fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = true;
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() {
            if !blank {
                out.push('\n');
            }
            blank = true;
        } else {
            out.push_str(line);
            out.push('\n');
            blank = false;
        }
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// Formats a byte count for people, such as `12.3 KB`.
pub(super) fn size(bytes: usize) -> String {
    let bytes = bytes as f64;
    if bytes < 1000.0 {
        format!("{bytes} B")
    } else if bytes < 1_000_000.0 {
        format!("{:.1} KB", bytes / 1000.0)
    } else {
        format!("{:.1} MB", bytes / 1_000_000.0)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A one-page PDF whose page shows `text` in Helvetica, with a correct
    /// cross-reference table.
    pub(in super::super) fn pdf(text: &str) -> Vec<u8> {
        let content = format!("BT /F1 24 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// Each supported format is recognized by its magic bytes; text is not.
    #[test]
    fn sniffs_images_and_pdfs() {
        assert_eq!(
            sniff(b"\x89PNG\r\n\x1a\n\0\0"),
            Some(Media::Image("image/png"))
        );
        assert_eq!(
            sniff(&[0xff, 0xd8, 0xff, 0xe0]),
            Some(Media::Image("image/jpeg"))
        );
        assert_eq!(sniff(b"GIF89a.."), Some(Media::Image("image/gif")));
        assert_eq!(
            sniff(b"RIFF\x10\0\0\0WEBPVP8 "),
            Some(Media::Image("image/webp"))
        );
        assert_eq!(sniff(b"RIFF\x10\0\0\0WAVEfmt "), None);
        assert_eq!(sniff(b"%PDF-1.7\n"), Some(Media::Pdf));
        assert_eq!(sniff(b"fn main() {}\n"), None);
        assert_eq!(sniff(b""), None);
    }

    /// Images come back base64-encoded with a line describing them.
    #[test]
    fn hands_images_to_the_model() {
        let output = image(b"\x89PNG\r\n\x1a\n", "image/png", "p.png").unwrap();
        assert_eq!(output.text, "[image p.png, image/png, 8 B]");
        assert_eq!(output.images[0].media_type, "image/png");
        assert_eq!(output.images[0].data, "iVBORw0KGgo=");
        let broken = vec![0u8; MAX_IMAGE_BYTES + 1];
        assert!(image(&broken, "image/png", "big.png").is_err());
        let huge = vec![0u8; MAX_IMAGE_FILE_BYTES + 1];
        let refused = image(&huge, "image/png", "huge.png")
            .unwrap_err()
            .to_string();
        assert!(refused.contains("make a smaller copy"), "{refused}");
    }

    /// An image too wide for the model comes back smaller, with the factor
    /// that maps its coordinates back to the original.
    #[test]
    fn shrinks_large_images() {
        let output = image(&shrink::tests::noisy_png(2400, 60), "image/png", "w.png").unwrap();
        assert!(
            output.text.starts_with("[image w.png, image/jpeg, "),
            "{}",
            output.text
        );
        assert!(
            output.text.contains("shrunk from 2400x60 to 2000x50"),
            "{}",
            output.text
        );
        assert_eq!(output.images[0].media_type, "image/jpeg");
    }

    /// The text of a PDF comes out page by page.
    #[test]
    fn extracts_pdf_text() {
        let (text, pages) = pdf_text(&pdf("Hello from a PDF"), "d.pdf").unwrap();
        assert_eq!(pages, 1);
        assert!(text.starts_with("[page 1]\n"), "{text:?}");
        assert!(text.contains("Hello from a PDF"), "{text:?}");
    }

    /// A broken PDF is an error, not a panic.
    #[test]
    fn rejects_broken_pdfs() {
        let result = pdf_text(b"%PDF-1.4\nthis is not a pdf", "bad.pdf");
        assert!(matches!(result, Err(ToolError::Failed(m)) if m.contains("bad.pdf")));
    }

    /// Blank runs collapse and trailing spaces go.
    #[test]
    fn tidies_text() {
        assert_eq!(tidy("\n\na  \n\n\n\nb\n\n"), "a\n\nb\n");
    }

    /// Sizes read naturally.
    #[test]
    fn formats_sizes() {
        assert_eq!(size(8), "8 B");
        assert_eq!(size(12_345), "12.3 KB");
        assert_eq!(size(3_750_000), "3.8 MB");
    }
}

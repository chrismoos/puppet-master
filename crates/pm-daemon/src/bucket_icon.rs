//! Validation for how a bucket shows in the web sidebar rail.

use base64::Engine;
use pm_protocol::domain::BucketIcon;

/// Enough for a few letters or one emoji built from several code points.
pub const ICON_TEXT_MAX_BYTES: usize = 32;
/// The browser shrinks a picture to a small square before sending it, so a
/// larger one is a client sending something the rail never shows.
pub const ICON_IMAGE_MAX_BYTES: usize = 64 * 1024;
/// The largest image file whose base64 data URL still fits the limit.
pub const ICON_IMAGE_FILE_MAX_BYTES: usize =
    (ICON_IMAGE_MAX_BYTES - LONGEST_IMAGE_PREFIX) / BASE64_CHARS_PER_GROUP * BASE64_BYTES_PER_GROUP;
const LONGEST_IMAGE_PREFIX: usize = "data:image/webp;base64,".len();
const BASE64_CHARS_PER_GROUP: usize = 4;
const BASE64_BYTES_PER_GROUP: usize = 3;

const IMAGE_TYPES: [(&str, &str); 3] = [
    ("image/png", "data:image/png;base64,"),
    ("image/webp", "data:image/webp;base64,"),
    ("image/jpeg", "data:image/jpeg;base64,"),
];
const HEX_COLOR_DIGITS: usize = 6;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG_SIGNATURE: &[u8] = b"\xff\xd8\xff";
const RIFF_SIGNATURE: &[u8] = b"RIFF";
const WEBP_FORM: &[u8] = b"WEBP";
/// A RIFF file names its form after the four-byte tag and four-byte length.
const RIFF_FORM_OFFSET: usize = 8;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BucketIconError {
    #[error(
        "icon text must be at most {ICON_TEXT_MAX_BYTES} bytes and contain no control characters"
    )]
    Text,
    #[error("icon color must be a #rrggbb hex color")]
    Color,
    #[error("icon image must be a base64 PNG, WebP or JPEG data URL of at most {ICON_IMAGE_MAX_BYTES} bytes")]
    Image,
}

/// Trims blank parts to absent, lowercases the color, and rejects anything
/// the rail cannot show.
pub fn normalize_bucket_icon(icon: BucketIcon) -> Result<BucketIcon, BucketIconError> {
    let text = icon
        .text
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    if let Some(text) = &text {
        if text.len() > ICON_TEXT_MAX_BYTES || text.chars().any(char::is_control) {
            return Err(BucketIconError::Text);
        }
    }
    let color = icon
        .color
        .map(|color| color.trim().to_ascii_lowercase())
        .filter(|color| !color.is_empty());
    if let Some(color) = &color {
        let digits = color.strip_prefix('#').ok_or(BucketIconError::Color)?;
        if digits.len() != HEX_COLOR_DIGITS || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(BucketIconError::Color);
        }
    }
    let image = icon.image.filter(|image| !image.is_empty());
    if let Some(image) = &image {
        if image.len() > ICON_IMAGE_MAX_BYTES {
            return Err(BucketIconError::Image);
        }
        let payload = IMAGE_TYPES
            .iter()
            .find_map(|(_, prefix)| image.strip_prefix(prefix))
            .ok_or(BucketIconError::Image)?;
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|_| BucketIconError::Image)?;
    }
    Ok(BucketIcon { text, color, image })
}

/// Returns the data URL the rail shows for a PNG, JPEG or WebP file, chosen by
/// the file's signature rather than its name.
pub fn image_data_url(bytes: &[u8]) -> Result<String, BucketIconError> {
    let riff_form = bytes.get(RIFF_FORM_OFFSET..RIFF_FORM_OFFSET + WEBP_FORM.len());
    let mime = if bytes.starts_with(PNG_SIGNATURE) {
        "image/png"
    } else if bytes.starts_with(JPEG_SIGNATURE) {
        "image/jpeg"
    } else if bytes.starts_with(RIFF_SIGNATURE) && riff_form == Some(WEBP_FORM) {
        "image/webp"
    } else {
        return Err(BucketIconError::Image);
    };
    let payload = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{payload}"))
}

/// Returns the media type and decoded bytes of a stored icon image.
pub fn split_image_data_url(url: &str) -> Option<(&'static str, Vec<u8>)> {
    IMAGE_TYPES.iter().find_map(|(mime, prefix)| {
        let payload = url.strip_prefix(prefix)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .ok()?;
        Some((*mime, bytes))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon(text: Option<&str>, color: Option<&str>, image: Option<&str>) -> BucketIcon {
        BucketIcon {
            text: text.map(str::to_string),
            color: color.map(str::to_string),
            image: image.map(str::to_string),
        }
    }

    #[test]
    fn trims_blank_parts_to_absent_and_lowercases_the_color() {
        assert_eq!(
            normalize_bucket_icon(icon(Some("  PM "), Some(" #3DD68C "), Some(""))),
            Ok(icon(Some("PM"), Some("#3dd68c"), None))
        );
        assert_eq!(
            normalize_bucket_icon(icon(Some("   "), Some(""), None)),
            Ok(BucketIcon::default())
        );
    }

    #[test]
    fn accepts_an_emoji_built_from_several_code_points() {
        let family = "👩‍👩‍👧‍👦";
        assert_eq!(
            normalize_bucket_icon(icon(Some(family), None, None)),
            Ok(icon(Some(family), None, None))
        );
    }

    #[test]
    fn rejects_text_that_is_too_long_or_holds_control_characters() {
        let long = "x".repeat(ICON_TEXT_MAX_BYTES + 1);
        for text in [long.as_str(), "P\u{7}M", "P\nM"] {
            assert_eq!(
                normalize_bucket_icon(icon(Some(text), None, None)),
                Err(BucketIconError::Text),
                "accepted {text:?}"
            );
        }
    }

    #[test]
    fn rejects_anything_but_a_six_digit_hex_color() {
        for color in ["3dd68c", "#3dd68", "#3dd68cc", "#zzzzzz", "red", "#fff"] {
            assert_eq!(
                normalize_bucket_icon(icon(None, Some(color), None)),
                Err(BucketIconError::Color),
                "accepted {color}"
            );
        }
    }

    #[test]
    fn accepts_small_png_webp_and_jpeg_data_urls() {
        for (_, prefix) in IMAGE_TYPES {
            let image = format!("{prefix}iVBORw0KGgo=");
            assert_eq!(
                normalize_bucket_icon(icon(None, None, Some(&image))),
                Ok(icon(None, None, Some(&image)))
            );
        }
    }

    #[test]
    fn rejects_other_images_bad_base64_and_oversized_pictures() {
        let oversized = format!("data:image/png;base64,{}", "A".repeat(ICON_IMAGE_MAX_BYTES));
        for image in [
            "data:image/svg+xml;base64,PHN2Zy8+",
            "data:image/gif;base64,R0lGODlh",
            "https://example.invalid/icon.png",
            "data:image/png;base64,not base64!",
            oversized.as_str(),
        ] {
            assert_eq!(
                normalize_bucket_icon(icon(None, None, Some(image))),
                Err(BucketIconError::Image),
                "accepted {}",
                &image[..image.len().min(40)]
            );
        }
    }

    #[test]
    fn image_files_become_data_urls_by_signature() {
        let png = [PNG_SIGNATURE, b"rest"].concat();
        assert_eq!(
            image_data_url(&png).unwrap(),
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&png)
            )
        );
        assert!(image_data_url(&[JPEG_SIGNATURE, b"x"].concat())
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
        let webp = [RIFF_SIGNATURE, b"\x10\0\0\0", WEBP_FORM, b"VP8 "].concat();
        assert!(image_data_url(&webp)
            .unwrap()
            .starts_with("data:image/webp;base64,"));
        let wave = [RIFF_SIGNATURE, b"\x10\0\0\0", b"WAVE"].concat();
        assert_eq!(image_data_url(&wave), Err(BucketIconError::Image));
        assert_eq!(image_data_url(b"GIF89a"), Err(BucketIconError::Image));
        assert_eq!(image_data_url(b""), Err(BucketIconError::Image));
        assert_eq!(
            split_image_data_url(&image_data_url(&png).unwrap()),
            Some(("image/png", png))
        );
        assert_eq!(split_image_data_url("data:image/gif;base64,R0lG"), None);
    }

    #[test]
    fn the_largest_accepted_image_file_fits_the_data_url_limit() {
        let largest = [
            PNG_SIGNATURE,
            &vec![0; ICON_IMAGE_FILE_MAX_BYTES - PNG_SIGNATURE.len()],
        ]
        .concat();
        let url = image_data_url(&largest).unwrap();
        assert!(normalize_bucket_icon(icon(None, None, Some(&url))).is_ok());
        let webp = [
            RIFF_SIGNATURE,
            b"\x10\0\0\0",
            WEBP_FORM,
            &vec![0; ICON_IMAGE_FILE_MAX_BYTES - 12],
        ]
        .concat();
        assert!(
            normalize_bucket_icon(icon(None, None, Some(&image_data_url(&webp).unwrap()))).is_ok()
        );
    }
}

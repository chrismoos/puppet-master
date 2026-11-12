//! Validation for how a bucket shows in the web sidebar rail.

use base64::Engine;
use pm_protocol::domain::BucketIcon;

/// Enough for a few letters or one emoji built from several code points.
pub const ICON_TEXT_MAX_BYTES: usize = 32;
/// The browser shrinks a picture to a small square before sending it, so a
/// larger one is a client sending something the rail never shows.
pub const ICON_IMAGE_MAX_BYTES: usize = 64 * 1024;

const IMAGE_PREFIXES: [&str; 3] = [
    "data:image/png;base64,",
    "data:image/webp;base64,",
    "data:image/jpeg;base64,",
];
const HEX_COLOR_DIGITS: usize = 6;

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
        let payload = IMAGE_PREFIXES
            .iter()
            .find_map(|prefix| image.strip_prefix(prefix))
            .ok_or(BucketIconError::Image)?;
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|_| BucketIconError::Image)?;
    }
    Ok(BucketIcon { text, color, image })
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
        for prefix in IMAGE_PREFIXES {
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
}

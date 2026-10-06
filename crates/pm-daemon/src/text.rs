//! Text processing helpers for user-facing strings.

const ENTITY_MAX_SCAN_BYTES: usize = 32;
const HEX_MAX_DIGITS: usize = 6;
const DEC_MAX_DIGITS: usize = 7;

/// Decodes common HTML and XML entities into plain text.
pub fn unescape_html_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }

    let mut out = String::with_capacity(s.len());
    let mut remainder = s;

    while let Some(amp_idx) = remainder.find('&') {
        out.push_str(&remainder[..amp_idx]);
        let candidate_slice = &remainder[amp_idx..];

        let mut semi_idx = None;
        for (i, b) in candidate_slice
            .bytes()
            .enumerate()
            .skip(1)
            .take(ENTITY_MAX_SCAN_BYTES)
        {
            if b == b';' {
                semi_idx = Some(i);
                break;
            }
            if b == b'&' || b.is_ascii_whitespace() {
                break;
            }
        }

        if let Some(end) = semi_idx {
            let entity = &candidate_slice[1..end];
            if let Some(c) = decode_entity(entity) {
                out.push(c);
                remainder = &candidate_slice[end + 1..];
                continue;
            }
        }

        out.push('&');
        remainder = &candidate_slice[1..];
    }

    out.push_str(remainder);
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    let mut lower_buf = [0u8; ENTITY_MAX_SCAN_BYTES];
    let lower = if entity.len() <= ENTITY_MAX_SCAN_BYTES {
        for (i, b) in entity.bytes().enumerate() {
            lower_buf[i] = b.to_ascii_lowercase();
        }
        std::str::from_utf8(&lower_buf[..entity.len()]).unwrap_or("")
    } else {
        ""
    };

    match lower {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        "hellip" => Some('…'),
        "lsquo" => Some('‘'),
        "rsquo" => Some('’'),
        "ldquo" => Some('“'),
        "rdquo" => Some('”'),
        "copy" => Some('©'),
        "reg" => Some('®'),
        "trade" => Some('™'),
        "bull" => Some('•'),
        "middot" => Some('·'),
        "sect" => Some('§'),
        "deg" => Some('°'),
        "plusmn" => Some('±'),
        "times" => Some('×'),
        "divide" => Some('÷'),
        "para" => Some('¶'),
        "cent" => Some('¢'),
        "pound" => Some('£'),
        "yen" => Some('¥'),
        "euro" => Some('€'),
        _ => {
            if let Some(hex) = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
            {
                if !hex.is_empty() && hex.len() <= HEX_MAX_DIGITS {
                    if let Ok(cp) = u32::from_str_radix(hex, 16) {
                        if cp != 0 {
                            return char::from_u32(cp);
                        }
                    }
                }
            } else if let Some(dec) = entity.strip_prefix('#') {
                if !dec.is_empty() && dec.len() <= DEC_MAX_DIGITS {
                    if let Ok(cp) = dec.parse::<u32>() {
                        if cp != 0 {
                            return char::from_u32(cp);
                        }
                    }
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_plain_strings_without_entities_untouched() {
        assert_eq!(unescape_html_entities("hello world"), "hello world");
        assert_eq!(unescape_html_entities("AT&T and 5 < 10"), "AT&T and 5 < 10");
        assert_eq!(unescape_html_entities(""), "");
    }

    #[test]
    fn decodes_standard_named_entities() {
        assert_eq!(unescape_html_entities("&amp;"), "&");
        assert_eq!(unescape_html_entities("&lt;"), "<");
        assert_eq!(unescape_html_entities("&gt;"), ">");
        assert_eq!(unescape_html_entities("&quot;"), "\"");
        assert_eq!(unescape_html_entities("&apos;"), "'");
        assert_eq!(unescape_html_entities("&AMP;"), "&");
        assert_eq!(unescape_html_entities("&QUOT;"), "\"");
    }

    #[test]
    fn decodes_common_typographic_entities() {
        assert_eq!(
            unescape_html_entities("step 1&nbsp;&mdash;&nbsp;done"),
            "step 1 — done"
        );
        assert_eq!(unescape_html_entities("&hellip;"), "…");
        assert_eq!(unescape_html_entities("&ndash;"), "–");
        assert_eq!(unescape_html_entities("&lsquo;hi&rsquo;"), "‘hi’");
        assert_eq!(unescape_html_entities("&ldquo;hi&rdquo;"), "“hi”");
    }

    #[test]
    fn decodes_numeric_decimal_entities() {
        assert_eq!(unescape_html_entities("&#38;"), "&");
        assert_eq!(unescape_html_entities("&#39;"), "'");
        assert_eq!(unescape_html_entities("&#34;"), "\"");
        assert_eq!(unescape_html_entities("&#60;"), "<");
        assert_eq!(unescape_html_entities("&#62;"), ">");
    }

    #[test]
    fn decodes_numeric_hex_entities() {
        assert_eq!(unescape_html_entities("&#x26;"), "&");
        assert_eq!(unescape_html_entities("&#X26;"), "&");
        assert_eq!(unescape_html_entities("&#x27;"), "'");
        assert_eq!(unescape_html_entities("&#x22;"), "\"");
        assert_eq!(unescape_html_entities("&#x3c;"), "<");
        assert_eq!(unescape_html_entities("&#x3e;"), ">");
    }

    #[test]
    fn decodes_mixed_embedded_entities() {
        assert_eq!(
            unescape_html_entities("Building &amp; testing &lt;div class=&quot;box&quot;&gt;"),
            "Building & testing <div class=\"box\">"
        );
        assert_eq!(
            unescape_html_entities("Reviewing &apos;core&apos; &amp; &apos;cli&apos; &#8212; 5/5"),
            "Reviewing 'core' & 'cli' — 5/5"
        );
    }

    #[test]
    fn preserves_malformed_or_unknown_entities() {
        assert_eq!(unescape_html_entities("&unknown;"), "&unknown;");
        assert_eq!(unescape_html_entities("foo & bar"), "foo & bar");
        assert_eq!(unescape_html_entities("&&amp;"), "&&");
        assert_eq!(unescape_html_entities("&#;"), "&#;");
        assert_eq!(unescape_html_entities("&#x;"), "&#x;");
        assert_eq!(unescape_html_entities("&#9999999999;"), "&#9999999999;");
        assert_eq!(unescape_html_entities("&#xD800;"), "&#xD800;");
    }
}

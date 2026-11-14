//! The terminal face, JetBrains Mono, rasterized with swash.

use std::collections::HashMap;
use std::sync::Arc;

use swash::scale::{image::Image, Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;
use swash::FontRef;

const REGULAR: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../fonts/JetBrainsMono-Bold.ttf");
const ITALIC: &[u8] = include_bytes!("../fonts/JetBrainsMono-Italic.ttf");
const BOLD_ITALIC: &[u8] = include_bytes!("../fonts/JetBrainsMono-BoldItalic.ttf");
/// Prompt and powerline icons, the same symbols font the web terminal uses.
const SYMBOLS: &[u8] = include_bytes!("../fonts/SymbolsNerdFontMono-Regular.ttf");

/// Which of the four faces a cell is drawn in.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Face {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

impl Face {
    pub fn of(bold: bool, italic: bool) -> Face {
        match (bold, italic) {
            (false, false) => Face::Regular,
            (true, false) => Face::Bold,
            (false, true) => Face::Italic,
            (true, true) => Face::BoldItalic,
        }
    }

    fn data(self) -> &'static [u8] {
        match self {
            Face::Regular => REGULAR,
            Face::Bold => BOLD,
            Face::Italic => ITALIC,
            Face::BoldItalic => BOLD_ITALIC,
        }
    }

    fn font(self) -> FontRef<'static> {
        FontRef::from_index(self.data(), 0).expect("the bundled face is a valid font")
    }
}

/// A cell's size and where the baseline sits in it, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    pub baseline: f32,
}

/// One rasterized glyph: an alpha coverage bitmap and where it sits
/// relative to the pen position on the baseline.
pub struct Glyph {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub coverage: Vec<u8>,
}

/// A font file a fallback glyph comes from, and the face within it.
#[derive(Clone)]
struct FallbackFace {
    data: Arc<Vec<u8>>,
    index: u32,
}

impl FallbackFace {
    fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index as usize)
    }
}

/// Families tried before scanning every installed font, covering CJK and
/// symbols on macOS and common Linux installs.
const PREFERRED_FALLBACKS: &[&str] = &[
    "PingFang SC",
    "Hiragino Sans",
    "Apple SD Gothic Neo",
    "Apple Symbols",
    "Noto Sans CJK SC",
    "Noto Sans Symbols 2",
    "DejaVu Sans",
];

/// Where a character the bundled faces lack is drawn from: the bundled
/// symbols font first, then a preferred installed family, then any installed
/// font that has it. Each character is looked up once, and only fonts that
/// supplied a glyph stay in memory.
struct Fallbacks {
    symbols: FallbackFace,
    system: Option<fontdb::Database>,
    files: HashMap<fontdb::ID, Arc<Vec<u8>>>,
    by_char: HashMap<char, Option<FallbackFace>>,
}

impl Fallbacks {
    fn new() -> Self {
        Fallbacks {
            symbols: FallbackFace {
                data: Arc::new(SYMBOLS.to_vec()),
                index: 0,
            },
            system: None,
            files: HashMap::new(),
            by_char: HashMap::new(),
        }
    }

    fn face_for(&mut self, ch: char) -> Option<FallbackFace> {
        if let Some(found) = self.by_char.get(&ch) {
            return found.clone();
        }
        let found = self.search(ch);
        self.by_char.insert(ch, found.clone());
        found
    }

    fn search(&mut self, ch: char) -> Option<FallbackFace> {
        if self
            .symbols
            .font()
            .is_some_and(|font| font.charmap().map(ch) != 0)
        {
            return Some(self.symbols.clone());
        }
        let db = self.system.get_or_insert_with(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            db
        });
        let preferred = PREFERRED_FALLBACKS.iter().filter_map(|family| {
            db.query(&fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                ..fontdb::Query::default()
            })
        });
        let mut candidates: Vec<fontdb::ID> = preferred.collect();
        candidates.extend(db.faces().map(|face| face.id));
        for id in candidates {
            if let Some(data) = self.files.get(&id) {
                let index = db.face(id).map_or(0, |face| face.index);
                let face = FallbackFace {
                    data: data.clone(),
                    index,
                };
                if face.font().is_some_and(|font| font.charmap().map(ch) != 0) {
                    return Some(face);
                }
                continue;
            }
            let covers = db.with_face_data(id, |data, index| {
                FontRef::from_index(data, index as usize)
                    .is_some_and(|font| font.charmap().map(ch) != 0)
            });
            if covers != Some(true) {
                continue;
            }
            let (data, index) =
                db.with_face_data(id, |data, index| (Arc::new(data.to_vec()), index))?;
            self.files.insert(id, data.clone());
            return Some(FallbackFace { data, index });
        }
        None
    }
}

pub struct Fonts {
    px: f32,
    context: ScaleContext,
    fallbacks: Fallbacks,
}

impl Fonts {
    /// `px` is the font size in physical pixels: points times the display scale.
    pub fn new(px: f32) -> Self {
        Fonts {
            px,
            context: ScaleContext::new(),
            fallbacks: Fallbacks::new(),
        }
    }

    pub fn px(&self) -> f32 {
        self.px
    }

    pub fn metrics(&self) -> CellMetrics {
        let font = Face::Regular.font();
        let metrics = font.metrics(&[]).scale(self.px);
        let advance = font
            .glyph_metrics(&[])
            .scale(self.px)
            .advance_width(font.charmap().map('M'));
        let height = (metrics.ascent + metrics.descent + metrics.leading).ceil();
        CellMetrics {
            width: advance.round().max(1.0),
            height: height.max(1.0),
            baseline: metrics.ascent.round(),
        }
    }

    /// The glyph for `ch` in `face`, from a fallback font when the face has
    /// none, or `None` when no font has it or it draws nothing, such as a space.
    pub fn rasterize(&mut self, face: Face, ch: char) -> Option<Glyph> {
        let font = face.font();
        let id = font.charmap().map(ch);
        if id != 0 {
            return Self::draw(&mut self.context, self.px, font, id);
        }
        let fallback = self.fallbacks.face_for(ch)?;
        let font = fallback.font()?;
        let id = font.charmap().map(ch);
        Self::draw(&mut self.context, self.px, font, id)
    }

    fn draw(context: &mut ScaleContext, px: f32, font: FontRef<'_>, id: u16) -> Option<Glyph> {
        let mut scaler = context.builder(font).size(px).hint(true).build();
        let image: Image = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ])
        .format(Format::Alpha)
        .render(&mut scaler, id)?;
        if image.placement.width == 0 || image.placement.height == 0 {
            return None;
        }
        Some(Glyph {
            width: image.placement.width,
            height: image.placement.height,
            left: image.placement.left,
            top: image.placement.top,
            coverage: image.data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_is_taller_than_it_is_wide_and_grows_with_the_size() {
        let small = Fonts::new(13.0).metrics();
        let large = Fonts::new(26.0).metrics();
        assert!(small.height > small.width);
        assert!(large.width > small.width && large.height > small.height);
        assert!(small.baseline > 0.0 && small.baseline < small.height);
    }

    #[test]
    fn letters_have_ink_and_a_space_has_none() {
        let mut fonts = Fonts::new(16.0);
        let m = fonts.rasterize(Face::Regular, 'M').expect("M has a glyph");
        assert!(m.coverage.iter().any(|&a| a > 200));
        assert_eq!(m.coverage.len(), (m.width * m.height) as usize);
        assert!(fonts.rasterize(Face::Regular, ' ').is_none());
        assert!(fonts.rasterize(Face::Bold, 'M').is_some());
    }
}

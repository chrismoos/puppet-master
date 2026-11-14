//! Glyph bitmaps packed into one grayscale texture, each uploaded once.

use std::collections::HashMap;

use crate::font::{Face, Fonts};

/// The atlas texture's side, in pixels. A full one is cleared and refilled
/// with the glyphs still on screen.
pub const ATLAS_SIZE: u32 = 2048;
/// Space between packed glyphs, so sampling never bleeds into a neighbor.
const GUTTER: u32 = 1;

/// Where a glyph is in the atlas and how it sits on the baseline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
}

/// Packs glyphs in rows ("shelves") from the top down.
pub struct Atlas {
    texture: wgpu::Texture,
    glyphs: HashMap<(Face, char), Option<Placed>>,
    cursor_x: u32,
    cursor_y: u32,
    shelf_height: u32,
}

impl Atlas {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        Atlas {
            texture,
            glyphs: HashMap::new(),
            cursor_x: 0,
            cursor_y: 0,
            shelf_height: 0,
        }
    }

    pub fn view(&self) -> wgpu::TextureView {
        self.texture
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// Drops every glyph, for a font size change or a full atlas.
    pub fn clear(&mut self) {
        self.glyphs.clear();
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.shelf_height = 0;
    }

    /// The glyph's place in the atlas, rasterizing and uploading it the first
    /// time. `None` for a character with nothing to draw.
    pub fn glyph(
        &mut self,
        queue: &wgpu::Queue,
        fonts: &mut Fonts,
        face: Face,
        ch: char,
    ) -> Option<Placed> {
        if let Some(found) = self.glyphs.get(&(face, ch)) {
            return *found;
        }
        let placed = fonts.rasterize(face, ch).and_then(|glyph| {
            let (x, y) = self.allocate(glyph.width, glyph.height)?;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &glyph.coverage,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(glyph.width),
                    rows_per_image: Some(glyph.height),
                },
                wgpu::Extent3d {
                    width: glyph.width,
                    height: glyph.height,
                    depth_or_array_layers: 1,
                },
            );
            Some(Placed {
                x,
                y,
                width: glyph.width,
                height: glyph.height,
                left: glyph.left,
                top: glyph.top,
            })
        });
        self.glyphs.insert((face, ch), placed);
        placed
    }

    /// Whether the atlas ran out of room and must be cleared before more
    /// glyphs fit.
    pub fn is_full(&self) -> bool {
        self.cursor_y >= ATLAS_SIZE
    }

    fn allocate(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if width + GUTTER > ATLAS_SIZE || height + GUTTER > ATLAS_SIZE {
            return None;
        }
        if self.cursor_x + width + GUTTER > ATLAS_SIZE {
            self.cursor_x = 0;
            self.cursor_y += self.shelf_height + GUTTER;
            self.shelf_height = 0;
        }
        if self.cursor_y + height + GUTTER > ATLAS_SIZE {
            self.cursor_y = ATLAS_SIZE;
            return None;
        }
        let at = (self.cursor_x, self.cursor_y);
        self.cursor_x += width + GUTTER;
        self.shelf_height = self.shelf_height.max(height);
        Some(at)
    }
}

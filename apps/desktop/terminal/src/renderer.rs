//! Draws a frame of cells on the GPU: one pass of rectangles for
//! backgrounds, one of glyphs from the atlas, one of rectangles for
//! underlines, strikethrough and the cursor.
//!
//! Colors are written as given, so a target should use a non-sRGB format
//! (such as `Bgra8Unorm`) to show the terminal's colors exactly.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::atlas::{Atlas, ATLAS_SIZE};
use crate::font::{CellMetrics, Face, Fonts};
use crate::vt::{style, CursorShape, Frame, Rgb};

/// Rectangle and glyph instances a buffer holds before it is grown.
const INITIAL_INSTANCES: usize = 4096;
const FAINT_ALPHA: f32 = 0.5;
const OPAQUE: f32 = 1.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Viewport {
    size: [f32; 2],
    pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RectInstance {
    origin: [f32; 2],
    size: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphInstance {
    origin: [f32; 2],
    size: [f32; 2],
    uv_origin: [f32; 2],
    uv_size: [f32; 2],
    color: [f32; 4],
}

fn color(rgb: Rgb, alpha: f32) -> [f32; 4] {
    [
        f32::from(rgb.r) / 255.0,
        f32::from(rgb.g) / 255.0,
        f32::from(rgb.b) / 255.0,
        alpha,
    ]
}

/// A growable GPU buffer of instances.
struct Instances {
    buffer: wgpu::Buffer,
    capacity: usize,
    label: &'static str,
}

impl Instances {
    fn new<T: Pod>(device: &wgpu::Device, label: &'static str) -> Self {
        Instances {
            buffer: Self::allocate::<T>(device, label, INITIAL_INSTANCES),
            capacity: INITIAL_INSTANCES,
            label,
        }
    }

    fn allocate<T: Pod>(
        device: &wgpu::Device,
        label: &'static str,
        capacity: usize,
    ) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (capacity * std::mem::size_of::<T>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn upload<T: Pod>(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, items: &[T]) {
        if items.len() > self.capacity {
            self.capacity = items.len().next_power_of_two();
            self.buffer = Self::allocate::<T>(device, self.label, self.capacity);
        }
        if !items.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(items));
        }
    }
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    fonts: Fonts,
    metrics: CellMetrics,
    atlas: Atlas,
    viewport: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    rect_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    cut_pipeline: wgpu::RenderPipeline,
    holes: Instances,
    hole_items: Vec<RectInstance>,
    backgrounds: Instances,
    glyphs: Instances,
    overlays: Instances,
    background_items: Vec<RectInstance>,
    glyph_items: Vec<GlyphInstance>,
    overlay_items: Vec<RectInstance>,
}

impl Renderer {
    /// `font_px` is the font size in physical pixels: points times the
    /// display's scale factor.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        font_px: f32,
    ) -> Self {
        let fonts = Fonts::new(font_px);
        let metrics = fonts.metrics();
        let atlas = Atlas::new(&device);
        let viewport = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("terminal viewport"),
            contents: bytemuck::bytes_of(&Viewport {
                size: [1.0, 1.0],
                pad: [0.0; 2],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terminal bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = Self::bind(&device, &bind_layout, &viewport, &atlas, &sampler);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terminal shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terminal pipeline layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let rect_attributes =
            wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];
        let glyph_attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2, 4 => Float32x4];
        let pipeline = |label, vs, fs, stride, attributes: &[wgpu::VertexAttribute], blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: stride,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes,
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let rect_pipeline = pipeline(
            "terminal rects",
            "rect_vs",
            "rect_fs",
            std::mem::size_of::<RectInstance>() as u64,
            &rect_attributes,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );
        let glyph_pipeline = pipeline(
            "terminal glyphs",
            "glyph_vs",
            "glyph_fs",
            std::mem::size_of::<GlyphInstance>() as u64,
            &glyph_attributes,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );
        // Writes its color as is, so a hole is fully transparent.
        let cut_pipeline = pipeline(
            "terminal holes",
            "rect_vs",
            "rect_fs",
            std::mem::size_of::<RectInstance>() as u64,
            &rect_attributes,
            None,
        );
        Renderer {
            backgrounds: Instances::new::<RectInstance>(&device, "terminal backgrounds"),
            glyphs: Instances::new::<GlyphInstance>(&device, "terminal glyphs"),
            overlays: Instances::new::<RectInstance>(&device, "terminal overlays"),
            holes: Instances::new::<RectInstance>(&device, "terminal holes"),
            cut_pipeline,
            hole_items: Vec::new(),
            device,
            queue,
            fonts,
            metrics,
            atlas,
            viewport,
            bind_group,
            bind_layout,
            sampler,
            rect_pipeline,
            glyph_pipeline,
            background_items: Vec::new(),
            glyph_items: Vec::new(),
            overlay_items: Vec::new(),
        }
    }

    fn bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        viewport: &wgpu::Buffer,
        atlas: &Atlas,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        let view = atlas.view();
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terminal bind group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: viewport.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    /// Lays cells out at exactly `width` by `height` physical pixels, such
    /// as the page's own cell size, so this grid lines up with the browser's.
    /// The glyphs stay at the font size, vertically centered in the cell.
    pub fn set_cell_size(&mut self, width: f32, height: f32) {
        let natural = self.fonts.metrics();
        let slack = (height - natural.height).max(0.0);
        self.metrics = CellMetrics {
            width: width.max(1.0),
            height: height.max(1.0),
            baseline: (natural.baseline + slack / 2.0).round(),
        };
    }

    /// Leaves these rectangles, `[x, y, width, height]` in physical pixels,
    /// fully transparent, so page content over the terminal shows through
    /// when the surface composites with alpha.
    pub fn set_holes(&mut self, holes: &[[f32; 4]]) {
        self.hole_items = holes
            .iter()
            .map(|&[x, y, width, height]| RectInstance {
                origin: [x, y],
                size: [width, height],
                color: [0.0; 4],
            })
            .collect();
    }

    /// Changes the font size, for a zoom or a display with another scale.
    pub fn set_font_px(&mut self, px: f32) {
        if (px - self.fonts.px()).abs() < f32::EPSILON {
            return;
        }
        self.fonts = Fonts::new(px);
        self.metrics = self.fonts.metrics();
        self.atlas.clear();
    }

    /// The grid that fits `width` by `height` physical pixels inside
    /// `padding` on every side.
    pub fn grid_for(&self, width: f32, height: f32, padding: f32) -> (u16, u16) {
        let cols = ((width - 2.0 * padding) / self.metrics.width)
            .floor()
            .max(1.0);
        let rows = ((height - 2.0 * padding) / self.metrics.height)
            .floor()
            .max(1.0);
        (
            cols.min(f32::from(u16::MAX)) as u16,
            rows.min(f32::from(u16::MAX)) as u16,
        )
    }

    /// Draws `frame` onto `target`, a `width` by `height` pixel surface,
    /// with the grid inset by `padding` pixels.
    pub fn render(
        &mut self,
        frame: &Frame,
        target: &wgpu::TextureView,
        width: u32,
        height: u32,
        padding: f32,
    ) {
        self.build(frame, padding);
        if self.atlas.is_full() {
            // Rebuilt from an empty atlas: only the glyphs this frame uses come back.
            self.atlas.clear();
            self.build(frame, padding);
        }
        self.queue.write_buffer(
            &self.viewport,
            0,
            bytemuck::bytes_of(&Viewport {
                size: [width as f32, height as f32],
                pad: [0.0; 2],
            }),
        );
        self.backgrounds
            .upload(&self.device, &self.queue, &self.background_items);
        self.glyphs
            .upload(&self.device, &self.queue, &self.glyph_items);
        self.overlays
            .upload(&self.device, &self.queue, &self.overlay_items);
        self.holes
            .upload(&self.device, &self.queue, &self.hole_items);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("terminal frame"),
            });
        {
            let clear = color(frame.background, OPAQUE);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(clear[0]),
                            g: f64::from(clear[1]),
                            b: f64::from(clear[2]),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_pipeline(&self.rect_pipeline);
            if !self.background_items.is_empty() {
                pass.set_vertex_buffer(0, self.backgrounds.buffer.slice(..));
                pass.draw(0..4, 0..self.background_items.len() as u32);
            }
            if !self.glyph_items.is_empty() {
                pass.set_pipeline(&self.glyph_pipeline);
                pass.set_vertex_buffer(0, self.glyphs.buffer.slice(..));
                pass.draw(0..4, 0..self.glyph_items.len() as u32);
            }
            if !self.overlay_items.is_empty() {
                pass.set_pipeline(&self.rect_pipeline);
                pass.set_vertex_buffer(0, self.overlays.buffer.slice(..));
                pass.draw(0..4, 0..self.overlay_items.len() as u32);
            }
            if !self.hole_items.is_empty() {
                pass.set_pipeline(&self.cut_pipeline);
                pass.set_vertex_buffer(0, self.holes.buffer.slice(..));
                pass.draw(0..4, 0..self.hole_items.len() as u32);
            }
        }
        self.queue.submit([encoder.finish()]);
    }

    fn build(&mut self, frame: &Frame, padding: f32) {
        self.background_items.clear();
        self.glyph_items.clear();
        self.overlay_items.clear();
        let CellMetrics {
            width: cw,
            height: ch,
            baseline,
        } = self.metrics;
        let line = (self.fonts.px() / 14.0).round().max(1.0);
        let cursor_cell = frame
            .cursor
            .filter(|c| c.x < frame.cols && c.y < frame.rows);
        for y in 0..frame.rows {
            for x in 0..frame.cols {
                let cell = frame.cell(x, y);
                let (mut fg, mut bg) = (cell.fg, cell.bg);
                if cell.flags & style::INVERSE != 0 {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.selected {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let left = padding + f32::from(x) * cw;
                let top = padding + f32::from(y) * ch;
                let span = if cell.wide { 2.0 } else { 1.0 };
                let block_cursor = cursor_cell
                    .is_some_and(|c| c.x == x && c.y == y && c.shape == CursorShape::Block);
                if block_cursor {
                    bg = frame.cursor_color;
                    fg = frame.background;
                }
                if bg != frame.background {
                    self.background_items.push(RectInstance {
                        origin: [left, top],
                        size: [cw * span, ch],
                        color: color(bg, OPAQUE),
                    });
                }
                let alpha = if cell.flags & style::FAINT != 0 {
                    FAINT_ALPHA
                } else {
                    OPAQUE
                };
                if !cell.text.is_empty() && cell.flags & style::INVISIBLE == 0 {
                    let face = Face::of(
                        cell.flags & style::BOLD != 0,
                        cell.flags & style::ITALIC != 0,
                    );
                    if let Some(first) = cell.text.chars().next() {
                        if let Some(placed) =
                            self.atlas.glyph(&self.queue, &mut self.fonts, face, first)
                        {
                            let size = ATLAS_SIZE as f32;
                            self.glyph_items.push(GlyphInstance {
                                origin: [
                                    left + placed.left as f32,
                                    top + baseline - placed.top as f32,
                                ],
                                size: [placed.width as f32, placed.height as f32],
                                uv_origin: [placed.x as f32 / size, placed.y as f32 / size],
                                uv_size: [placed.width as f32 / size, placed.height as f32 / size],
                                color: color(fg, alpha),
                            });
                        }
                    }
                }
                if cell.flags & style::UNDERLINE_MASK != 0 {
                    self.overlay_items.push(RectInstance {
                        origin: [left, top + baseline + line],
                        size: [cw * span, line],
                        color: color(fg, alpha),
                    });
                }
                if cell.flags & style::STRIKETHROUGH != 0 {
                    self.overlay_items.push(RectInstance {
                        origin: [left, top + (baseline * 0.62).round()],
                        size: [cw * span, line],
                        color: color(fg, alpha),
                    });
                }
            }
        }
        if let Some(cursor) = cursor_cell {
            let left = padding + f32::from(cursor.x) * cw;
            let top = padding + f32::from(cursor.y) * ch;
            let tint = color(frame.cursor_color, OPAQUE);
            let thick = (line * 2.0).max(2.0);
            let rects: &[([f32; 2], [f32; 2])] = match cursor.shape {
                CursorShape::Block => &[],
                CursorShape::Bar => &[([left, top], [thick, ch])],
                CursorShape::Underline => &[([left, top + ch - thick], [cw, thick])],
                CursorShape::HollowBlock => &[
                    ([left, top], [cw, line]),
                    ([left, top + ch - line], [cw, line]),
                    ([left, top], [line, ch]),
                    ([left + cw - line, top], [line, ch]),
                ],
            };
            for &(origin, size) in rects {
                self.overlay_items.push(RectInstance {
                    origin,
                    size,
                    color: tint,
                });
            }
        }
    }

    /// Recreates the bind group after the atlas texture changes.
    pub fn rebind(&mut self) {
        self.bind_group = Self::bind(
            &self.device,
            &self.bind_layout,
            &self.viewport,
            &self.atlas,
            &self.sampler,
        );
    }
}

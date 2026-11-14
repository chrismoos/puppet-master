//! Draws real frames offscreen on whatever GPU the host has (Metal on a
//! Mac, a software Vulkan driver on a Linux build host) and reads the pixels
//! back.

use pm_native_terminal::renderer::Renderer;
use pm_native_terminal::vt::{Rgb, Vt};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 96;
const FONT_PX: f32 = 18.0;
const PADDING: f32 = 4.0;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const BYTES_PER_PIXEL: u32 = 4;

fn gpu() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .expect("a GPU adapter, even a software one");
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .expect("a device on the adapter")
    })
}

struct Pixels {
    data: Vec<u8>,
    stride: usize,
}

impl Pixels {
    fn at(&self, x: u32, y: u32) -> Rgb {
        let i = y as usize * self.stride + (x * BYTES_PER_PIXEL) as usize;
        Rgb {
            r: self.data[i],
            g: self.data[i + 1],
            b: self.data[i + 2],
        }
    }

    fn any_in(&self, x0: u32, y0: u32, x1: u32, y1: u32, test: impl Fn(Rgb) -> bool) -> bool {
        (y0..y1).any(|y| (x0..x1).any(|x| test(self.at(x, y))))
    }

    fn share_in(&self, x0: u32, y0: u32, x1: u32, y1: u32, test: impl Fn(Rgb) -> bool) -> f32 {
        let total = ((x1 - x0) * (y1 - y0)) as f32;
        let hits = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| test(self.at(x, y)))
            .count();
        hits as f32 / total
    }
}

fn draw(renderer: &mut Renderer, vt: &mut Vt) -> Pixels {
    let device = renderer.device().clone();
    let queue = renderer.queue().clone();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let frame = vt.update().unwrap().clone();
    renderer.render(&frame, &view, WIDTH, HEIGHT, PADDING);

    let unpadded = WIDTH * BYTES_PER_PIXEL;
    let stride =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(stride * HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |result| result.expect("readback maps"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("the GPU finishes");
    let data = slice.get_mapped_range().expect("the mapped range").to_vec();
    Pixels {
        data,
        stride: stride as usize,
    }
}

#[test]
fn text_its_colors_and_the_cursor_reach_the_screen() {
    let (device, queue) = gpu();
    let mut renderer = Renderer::new(device, queue, FORMAT, FONT_PX);
    let metrics = renderer.metrics();
    let (cols, rows) = renderer.grid_for(WIDTH as f32, HEIGHT as f32, PADDING);
    let mut vt = Vt::new(cols, rows).unwrap();
    vt.write(b"\x1b[38;2;255;0;0mHH\x1b[0m \x1b[48;2;0;0;255m  \x1b[0m");
    let frame = vt.update().unwrap().clone();
    let pixels = draw(&mut renderer, &mut vt);

    assert_eq!(
        pixels.at(0, 0),
        frame.background,
        "the padding shows the default background"
    );
    assert_eq!(pixels.at(WIDTH - 1, HEIGHT - 1), frame.background);

    let cell = |x: u16| {
        let left = (PADDING + f32::from(x) * metrics.width) as u32;
        let top = PADDING as u32;
        (
            left,
            top,
            left + metrics.width as u32,
            top + metrics.height as u32,
        )
    };
    let (x0, y0, x1, y1) = cell(0);
    assert!(
        pixels.any_in(x0, y0, x1, y1, |p| p.r > 200 && p.g < 80 && p.b < 80),
        "the red H has red ink in its cell"
    );
    let (x0, y0, x1, y1) = cell(3);
    assert!(
        pixels.share_in(x0 + 1, y0 + 1, x1 - 1, y1 - 1, |p| p
            == Rgb { r: 0, g: 0, b: 255 })
            > 0.95,
        "a colored background fills its cell"
    );
    let cursor = frame.cursor.expect("the cursor is on screen");
    let (x0, y0, x1, y1) = cell(cursor.x);
    assert!(
        pixels.share_in(x0 + 1, y0 + 1, x1 - 1, y1 - 1, |p| p == frame.cursor_color) > 0.9,
        "the block cursor is drawn in the cursor color"
    );
}

#[test]
fn a_full_screen_of_text_draws_and_only_used_glyphs_are_rasterized() {
    let (device, queue) = gpu();
    let mut renderer = Renderer::new(device, queue, FORMAT, FONT_PX);
    let (cols, rows) = renderer.grid_for(WIDTH as f32, HEIGHT as f32, PADDING);
    let mut vt = Vt::new(cols, rows).unwrap();
    for line in 0..rows {
        let text: String = (0..cols)
            .map(|i| char::from(b'!' + ((i + line) % 90) as u8))
            .collect();
        vt.write(text.as_bytes());
    }
    let frame = vt.update().unwrap().clone();
    let pixels = draw(&mut renderer, &mut vt);
    let ink = pixels.share_in(0, 0, WIDTH, HEIGHT, |p| p != frame.background);
    assert!(
        ink > 0.08,
        "a screen full of text has plenty of ink, got {ink}"
    );
}

#[test]
fn a_hole_is_fully_transparent_so_the_page_shows_through() {
    let (device, queue) = gpu();
    let mut renderer = Renderer::new(device, queue, FORMAT, FONT_PX);
    let (cols, rows) = renderer.grid_for(WIDTH as f32, HEIGHT as f32, PADDING);
    let mut vt = Vt::new(cols, rows).unwrap();
    vt.write(b"\x1b[48;2;200;0;0m                                        ");
    renderer.set_holes(&[[10.0, 0.0, 40.0, 20.0]]);
    let pixels = draw(&mut renderer, &mut vt);
    let alpha = |x: u32, y: u32| {
        pixels.data[y as usize * pixels.stride + (x * BYTES_PER_PIXEL) as usize + 3]
    };
    assert_eq!(alpha(20, 10), 0, "inside the hole");
    assert_eq!(alpha(70, 10), 255, "outside the hole");
}

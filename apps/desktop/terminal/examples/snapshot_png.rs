//! Renders a sample screen offscreen and writes it as a PPM image, to look
//! at the renderer's output by eye: `cargo run --example snapshot_png -- out.ppm`.

use pm_native_terminal::renderer::Renderer;
use pm_native_terminal::vt::Vt;

const WIDTH: u32 = 900;
const HEIGHT: u32 = 260;
const FONT_PX: f32 = 26.0;
const PADDING: f32 = 8.0;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "terminal.ppm".into());
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap()
    });
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = Renderer::new(device.clone(), queue.clone(), format, FONT_PX);
    let (cols, rows) = renderer.grid_for(WIDTH as f32, HEIGHT as f32, PADDING);
    let mut vt = Vt::new(cols, rows).unwrap();
    vt.write("\x1b[1m● Puppet Master\x1b[0m native terminal, drawn by \x1b[38;2;77;163;255mMetal\x1b[0m\r\n".as_bytes());
    vt.write("\x1b[32m~/project$\x1b[0m cargo test \x1b[2m--release\x1b[0m  \x1b[3mitalic\x1b[0m \x1b[4munder\x1b[0m \x1b[9mstrike\x1b[0m\r\n".as_bytes());
    vt.write("\x1b[30;43m warn \x1b[0m 界面 wide chars, \x1b[7minverse\x1b[0m, \x1b[1;31mbold red\x1b[0m\r\n> ".as_bytes());
    let frame = vt.update().unwrap().clone();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    renderer.render(&frame, &view, WIDTH, HEIGHT, PADDING);
    let stride = (WIDTH * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
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
            buffer: &buffer,
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
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap();
    let mut ppm = format!("P6 {WIDTH} {HEIGHT} 255\n").into_bytes();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let i = (y * stride + x * 4) as usize;
            ppm.extend_from_slice(&data[i..i + 3]);
        }
    }
    std::fs::write(&path, ppm).unwrap();
    println!("{cols}x{rows} grid, wrote {path}");
}

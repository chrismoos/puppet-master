//! Keeps one terminal's window surface drawn: a render thread sleeps until
//! the screen changes or the view resizes, then draws one frame, paced by
//! the display.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::renderer::Renderer;
use crate::vt::{Frame, Vt};

/// The view's layout in physical pixels and the page's cell size, which the
/// grid is laid out with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub font_px: f32,
    pub padding: f32,
    pub visible: bool,
}

struct State {
    layout: Layout,
    dirty: bool,
    alive: bool,
}

/// What the render thread and everyone signalling it share.
pub struct Painter {
    state: Mutex<State>,
    wake: Condvar,
}

impl Painter {
    /// Asks for a frame.
    pub fn redraw(&self) {
        self.state.lock().unwrap().dirty = true;
        self.wake.notify_one();
    }

    pub fn set_layout(&self, layout: Layout) {
        let mut state = self.state.lock().unwrap();
        if state.layout != layout {
            state.layout = layout;
            state.dirty = true;
            self.wake.notify_one();
        }
    }

    fn stop(&self) {
        self.state.lock().unwrap().alive = false;
        self.wake.notify_one();
    }

    /// Waits for work and returns the layout to draw at, or `None` once stopped.
    fn next(&self) -> Option<Layout> {
        let mut state = self.state.lock().unwrap();
        while state.alive && !state.dirty {
            state = self.wake.wait(state).unwrap();
        }
        if !state.alive {
            return None;
        }
        state.dirty = false;
        Some(state.layout)
    }
}

/// A running render thread. Dropping it stops the thread.
pub struct Paint {
    painter: Arc<Painter>,
    thread: Option<JoinHandle<()>>,
}

impl Paint {
    /// Starts drawing `screen` onto `surface`, configured from `config` and
    /// resized to each layout.
    pub fn start(
        surface: wgpu::Surface<'static>,
        mut config: wgpu::SurfaceConfiguration,
        mut renderer: Renderer,
        screen: Arc<Mutex<Option<Vt>>>,
        layout: Layout,
    ) -> Paint {
        let painter = Arc::new(Painter {
            state: Mutex::new(State {
                layout,
                dirty: true,
                alive: true,
            }),
            wake: Condvar::new(),
        });
        let shared = painter.clone();
        let thread = std::thread::Builder::new()
            .name("terminal render".into())
            .spawn(move || {
                let mut font_px = 0.0;
                let mut cell = (0.0, 0.0);
                while let Some(layout) = shared.next() {
                    if !layout.visible || layout.width == 0 || layout.height == 0 {
                        continue;
                    }
                    if (layout.width, layout.height) != (config.width, config.height) {
                        config.width = layout.width;
                        config.height = layout.height;
                        surface.configure(renderer.device(), &config);
                    }
                    if layout.font_px != font_px {
                        font_px = layout.font_px;
                        renderer.set_font_px(font_px);
                        cell = (0.0, 0.0);
                    }
                    if (layout.cell_width, layout.cell_height) != cell {
                        cell = (layout.cell_width, layout.cell_height);
                        renderer.set_cell_size(cell.0, cell.1);
                    }
                    let texture = match surface.get_current_texture() {
                        wgpu::CurrentSurfaceTexture::Success(texture) => texture,
                        wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                            surface.configure(renderer.device(), &config);
                            texture
                        }
                        wgpu::CurrentSurfaceTexture::Outdated
                        | wgpu::CurrentSurfaceTexture::Lost => {
                            surface.configure(renderer.device(), &config);
                            shared.redraw();
                            continue;
                        }
                        _ => continue,
                    };
                    let view = texture
                        .texture
                        .create_view(&wgpu::TextureViewDescriptor::default());
                    {
                        let mut guard = screen.lock().unwrap();
                        match guard.as_mut().map(Vt::update) {
                            Some(Ok(frame)) => renderer.render(
                                frame,
                                &view,
                                config.width,
                                config.height,
                                layout.padding,
                            ),
                            // Before the first replay, or after a failed update: the plain background.
                            _ => renderer.render(
                                &Frame::default(),
                                &view,
                                config.width,
                                config.height,
                                layout.padding,
                            ),
                        }
                    }
                    renderer.queue().present(texture);
                }
            })
            .expect("the render thread starts");
        Paint {
            painter,
            thread: Some(thread),
        }
    }

    pub fn painter(&self) -> Arc<Painter> {
        self.painter.clone()
    }
}

impl Drop for Paint {
    fn drop(&mut self) {
        self.painter.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

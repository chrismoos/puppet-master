//! A native terminal view on macOS: a Metal-backed `NSView` laid over the
//! web page at the place the page keeps for the terminal, taking keys,
//! pastes and scrolling while it has focus.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSEvent, NSEventModifierFlags, NSPasteboard, NSPasteboardTypeString, NSResponder, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use raw_window_handle::{
    AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle,
};

use crate::input::{keys, mods, Key, KeyPress};
use crate::renderer::Renderer;
use crate::stream::{Attach, Event, Stream};
use crate::surface::{Layout, Paint, Painter};
use crate::vt::Vt;

/// Lines one notch of a mouse wheel scrolls; trackpads report pixels and
/// are divided by the cell height instead.
const WHEEL_LINES: f64 = 3.0;

/// What the view hands to its terminal.
enum ViewInput {
    Key(KeyPress),
    Paste(String),
    Scroll {
        lines: f64,
        precise_pixels: Option<f64>,
    },
}

type InputSink = Box<dyn Fn(ViewInput)>;

pub struct ViewIvars {
    sink: RefCell<Option<InputSink>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder))]
    #[thread_kind = MainThreadOnly]
    #[name = "PMNativeTerminalView"]
    #[ivars = ViewIvars]
    pub struct TerminalView;

    impl TerminalView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if let Some(window) = self.window() {
                let responder: &NSResponder = self;
                window.makeFirstResponder(Some(responder));
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if let Some(press) = key_press(event) {
                self.emit(ViewInput::Key(press));
            }
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> Bool {
            let focused = self.window().is_some_and(|window| {
                window.firstResponder().is_some_and(|responder| {
                    let mine: &NSResponder = self;
                    std::ptr::eq(&*responder, mine)
                })
            });
            let flags = event.modifierFlags();
            let command_v = flags.contains(NSEventModifierFlags::Command) && event.keyCode() == KVK_ANSI_V;
            if !focused || !command_v {
                return Bool::NO;
            }
            let text = unsafe { NSPasteboard::generalPasteboard().stringForType(NSPasteboardTypeString) };
            if let Some(text) = text {
                self.emit(ViewInput::Paste(text.to_string()));
            }
            Bool::YES
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let delta = event.scrollingDeltaY();
            let precise = event.hasPreciseScrollingDeltas().then_some(delta);
            self.emit(ViewInput::Scroll { lines: delta.signum() * WHEEL_LINES, precise_pixels: precise });
        }
    }
);

impl TerminalView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars {
            sink: RefCell::new(None),
        });
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0));
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn emit(&self, input: ViewInput) {
        if let Some(sink) = self.ivars().sink.borrow().as_ref() {
            sink(input);
        }
    }
}

// macOS virtual key codes (Carbon's kVK_* values), which name physical keys
// whatever the keyboard layout.
const KVK_ANSI_V: u16 = 0x09;

fn key_for(code: u16) -> Key {
    match code {
        0x00 => keys::A,
        0x01 => keys::S,
        0x02 => keys::D,
        0x03 => keys::F,
        0x04 => keys::H,
        0x05 => keys::G,
        0x06 => keys::Z,
        0x07 => keys::X,
        0x08 => keys::C,
        0x09 => keys::V,
        0x0B => keys::B,
        0x0C => keys::Q,
        0x0D => keys::W,
        0x0E => keys::E,
        0x0F => keys::R,
        0x10 => keys::Y,
        0x11 => keys::T,
        0x12 => keys::DIGIT_1,
        0x13 => keys::DIGIT_2,
        0x14 => keys::DIGIT_3,
        0x15 => keys::DIGIT_4,
        0x16 => keys::DIGIT_6,
        0x17 => keys::DIGIT_5,
        0x18 => keys::EQUAL,
        0x19 => keys::DIGIT_9,
        0x1A => keys::DIGIT_7,
        0x1B => keys::MINUS,
        0x1C => keys::DIGIT_8,
        0x1D => keys::DIGIT_0,
        0x1E => keys::BRACKET_RIGHT,
        0x1F => keys::O,
        0x20 => keys::U,
        0x21 => keys::BRACKET_LEFT,
        0x22 => keys::I,
        0x23 => keys::P,
        0x24 => keys::ENTER,
        0x25 => keys::L,
        0x26 => keys::J,
        0x27 => keys::QUOTE,
        0x28 => keys::K,
        0x29 => keys::SEMICOLON,
        0x2A => keys::BACKSLASH,
        0x2B => keys::COMMA,
        0x2C => keys::SLASH,
        0x2D => keys::N,
        0x2E => keys::M,
        0x2F => keys::PERIOD,
        0x30 => keys::TAB,
        0x31 => keys::SPACE,
        0x32 => keys::BACKQUOTE,
        0x33 => keys::BACKSPACE,
        0x35 => keys::ESCAPE,
        0x4C => keys::NUMPAD_ENTER,
        0x60 => keys::F5,
        0x61 => keys::F6,
        0x62 => keys::F7,
        0x63 => keys::F3,
        0x64 => keys::F8,
        0x65 => keys::F9,
        0x67 => keys::F11,
        0x6D => keys::F10,
        0x6F => keys::F12,
        0x72 => keys::HELP,
        0x73 => keys::HOME,
        0x74 => keys::PAGE_UP,
        0x75 => keys::DELETE,
        0x76 => keys::F4,
        0x77 => keys::END,
        0x78 => keys::F2,
        0x79 => keys::PAGE_DOWN,
        0x7A => keys::F1,
        0x7B => keys::ARROW_LEFT,
        0x7C => keys::ARROW_RIGHT,
        0x7D => keys::ARROW_DOWN,
        0x7E => keys::ARROW_UP,
        _ => keys::UNIDENTIFIED,
    }
}

/// AppKit's private-use range for function keys, which are not text.
const FUNCTION_KEYS: std::ops::RangeInclusive<u32> = 0xF700..=0xF8FF;

fn is_text(ch: char) -> bool {
    let code = u32::from(ch);
    code >= 0x20 && code != 0x7F && !FUNCTION_KEYS.contains(&code)
}

/// The key press for `event`, or `None` for a Command shortcut, which
/// belongs to the app's menus.
fn key_press(event: &NSEvent) -> Option<KeyPress> {
    let flags = event.modifierFlags();
    if flags.contains(NSEventModifierFlags::Command) {
        return None;
    }
    let mut bits = 0;
    if flags.contains(NSEventModifierFlags::Shift) {
        bits |= mods::SHIFT;
    }
    if flags.contains(NSEventModifierFlags::Control) {
        bits |= mods::CTRL;
    }
    if flags.contains(NSEventModifierFlags::Option) {
        bits |= mods::ALT;
    }
    let text: String = if bits & mods::CTRL != 0 {
        String::new()
    } else {
        event
            .characters()
            .map(|s| s.to_string())
            .unwrap_or_default()
            .chars()
            .filter(|&c| is_text(c))
            .collect()
    };
    let unshifted = event
        .charactersIgnoringModifiers()
        .and_then(|s| s.to_string().chars().next())
        .filter(|&c| is_text(c))
        .map(|c| c.to_ascii_lowercase());
    Some(KeyPress {
        key: key_for(event.keyCode()),
        mods: bits,
        text,
        unshifted,
        repeat: event.isARepeat(),
    })
}

/// Where the page keeps the terminal, in CSS pixels from the page's top
/// left, and the page's cell size and font.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub cell_width: f64,
    pub cell_height: f64,
    pub font_size: f64,
    pub padding: f64,
    pub visible: bool,
}

/// A terminal drawn natively over the page. It lives on the main thread.
pub struct NativeTerminal {
    view: Retained<TerminalView>,
    parent: Retained<NSView>,
    stream: Arc<Stream>,
    /// Kept for its drop, which stops the render thread.
    _paint: Paint,
    painter: Arc<Painter>,
    scale: f64,
    /// The grid this view last claimed.
    claimed: Arc<Mutex<(u16, u16)>>,
    /// Whether this viewer owns the terminal's size.
    owner: Arc<AtomicBool>,
}

/// The grid a placement holds, at the page's cell size inside its padding.
fn grid_of(placement: Placement) -> (u16, u16) {
    let cols =
        ((placement.width - 2.0 * placement.padding) / placement.cell_width.max(1.0)).floor();
    let rows =
        ((placement.height - 2.0 * placement.padding) / placement.cell_height.max(1.0)).floor();
    (
        cols.clamp(1.0, f64::from(u16::MAX)) as u16,
        rows.clamp(1.0, f64::from(u16::MAX)) as u16,
    )
}

/// Puts text a program copied on the system clipboard, from the main thread.
fn copy_to_pasteboard(text: String) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let text = NSString::from_str(&text);
        unsafe { pasteboard.setString_forType(&text, NSPasteboardTypeString) };
    });
}

impl NativeTerminal {
    /// Adds a terminal view over `parent`, the window's content view, and
    /// attaches it to the controller. `on_closed` hears when the socket ends,
    /// so the page can mint a new ticket.
    ///
    /// # Safety
    /// `parent` must be a live `NSView` pointer from the same window, and
    /// this must run on the main thread.
    pub unsafe fn open(
        parent: *mut std::ffi::c_void,
        runtime: &tokio::runtime::Handle,
        attach: Attach,
        placement: Placement,
        on_closed: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<NativeTerminal, String> {
        let mtm = MainThreadMarker::new().ok_or("a native terminal opens on the main thread")?;
        let parent: Retained<NSView> = unsafe { Retained::retain(parent.cast::<NSView>()) }
            .ok_or("the window has no content view")?;
        let view = TerminalView::new(mtm);
        parent.addSubview(&view);
        let scale = parent
            .window()
            .map_or(1.0, |window| window.backingScaleFactor());

        let instance = wgpu::Instance::default();
        let handle = AppKitWindowHandle::new(NonNull::from(&*view).cast());
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(RawDisplayHandle::AppKit(AppKitDisplayHandle::new())),
                raw_window_handle: RawWindowHandle::AppKit(handle),
            })
        }
        .map_err(|error| format!("could not create the terminal surface: {error}"))?;
        let (adapter, device, queue) = pollster::block_on(async {
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: Some(&surface),
                    ..Default::default()
                })
                .await
                .map_err(|error| error.to_string())?;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((adapter, device, queue))
        })?;
        let layout = layout_for(placement, scale);
        let mut config = surface
            .get_default_config(&adapter, layout.width.max(1), layout.height.max(1))
            .ok_or("the surface has no usable configuration")?;
        let capabilities = surface.get_capabilities(&adapter);
        if capabilities
            .formats
            .contains(&wgpu::TextureFormat::Bgra8Unorm)
        {
            config.format = wgpu::TextureFormat::Bgra8Unorm;
        }
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);
        let renderer = Renderer::new(device, queue, config.format, layout.font_px);

        let painter_slot: Arc<Mutex<Option<Arc<Painter>>>> = Arc::default();
        let signal = painter_slot.clone();
        let closed = Arc::new(on_closed);
        let owner = Arc::new(AtomicBool::new(true));
        let ownership = owner.clone();
        let grid = grid_of(placement);
        let attach = Attach {
            claim: Some(grid),
            ..attach
        };
        let stream = Arc::new(Stream::open_on(runtime, attach, move |event| match event {
            Event::Closed { reason } => closed(reason),
            Event::Clipboard(text) => copy_to_pasteboard(text),
            Event::Ownership { local, .. } => ownership.store(local, Ordering::Relaxed),
            Event::Changed | Event::PtySize { .. } => {
                if let Some(painter) = signal.lock().unwrap().as_ref() {
                    painter.redraw();
                }
            }
        }));
        let paint = Paint::start(surface, config, renderer, stream.screen(), layout);
        let painter = paint.painter();
        *painter_slot.lock().unwrap() = Some(painter.clone());

        let terminal = NativeTerminal {
            view,
            parent,
            stream,
            _paint: paint,
            painter,
            scale,
            claimed: Arc::new(Mutex::new(grid)),
            owner,
        };
        terminal.place(placement);
        terminal.connect_input(placement.cell_height * scale);
        Ok(terminal)
    }

    fn connect_input(&self, cell_height_px: f64) {
        let stream = self.stream.clone();
        let screen = self.stream.screen();
        let painter = self.painter.clone();
        let claimed = self.claimed.clone();
        let owner = self.owner.clone();
        let cell_height = cell_height_px.max(1.0) / self.scale.max(1.0);
        *self.view.ivars().sink.borrow_mut() = Some(Box::new(move |input| {
            let mut guard = screen.lock().unwrap();
            let Some(vt) = guard.as_mut() else { return };
            match input {
                ViewInput::Key(press) => {
                    let bytes = vt.encode_key(&press);
                    if bytes.is_empty() {
                        return;
                    }
                    // Typing here takes the size back from another viewer, as the browser does.
                    if !owner.load(Ordering::Relaxed) {
                        let (cols, rows) = *claimed.lock().unwrap();
                        stream.claim(cols, rows);
                        stream.resync();
                    }
                    let submitted = press.key == keys::ENTER || press.key == keys::NUMPAD_ENTER;
                    let _ = vt.scroll(isize::MAX);
                    drop(guard);
                    stream.input(bytes, submitted);
                    painter.redraw();
                }
                ViewInput::Paste(text) => {
                    let bytes = vt.encode_paste(&text);
                    drop(guard);
                    stream.input(bytes, false);
                }
                ViewInput::Scroll {
                    lines,
                    precise_pixels,
                } => {
                    let rows = match precise_pixels {
                        Some(pixels) => -(pixels / cell_height).round(),
                        None => -lines,
                    };
                    if rows != 0.0 {
                        let _ = vt.scroll(rows as isize);
                        drop(guard);
                        painter.redraw();
                    }
                }
            }
        }));
    }

    /// Moves the view to the page's current place for the terminal.
    pub fn place(&self, placement: Placement) {
        let bounds = self.parent.bounds();
        let y = if self.parent.isFlipped() {
            placement.y
        } else {
            bounds.size.height - placement.y - placement.height
        };
        self.view.setFrame(NSRect::new(
            NSPoint::new(placement.x, y),
            NSSize::new(placement.width, placement.height),
        ));
        self.view.setHidden(!placement.visible);
        self.painter.set_layout(layout_for(placement, self.scale));
        if placement.visible {
            let grid = grid_of(placement);
            let mut claimed = self.claimed.lock().unwrap();
            if *claimed != grid {
                *claimed = grid;
                self.stream.claim(grid.0, grid.1);
                self.stream.resync();
            }
        }
    }

    /// Gives the terminal the keyboard.
    pub fn focus(&self) {
        if let Some(window) = self.view.window() {
            let responder: &NSResponder = &self.view;
            window.makeFirstResponder(Some(responder));
        }
    }

    /// Claims the terminal's size at this view's grid.
    pub fn claim(&self, cols: u16, rows: u16) {
        self.stream.claim(cols, rows);
    }

    pub fn screen(&self) -> Arc<Mutex<Option<Vt>>> {
        self.stream.screen()
    }
}

impl Drop for NativeTerminal {
    fn drop(&mut self) {
        self.view.ivars().sink.borrow_mut().take();
        self.view.removeFromSuperview();
    }
}

fn layout_for(placement: Placement, scale: f64) -> Layout {
    let px = |value: f64| (value * scale).round().max(0.0);
    Layout {
        width: px(placement.width) as u32,
        height: px(placement.height) as u32,
        cell_width: (placement.cell_width * scale) as f32,
        cell_height: (placement.cell_height * scale) as f32,
        font_px: (placement.font_size * scale) as f32,
        padding: (placement.padding * scale) as f32,
        visible: placement.visible,
    }
}

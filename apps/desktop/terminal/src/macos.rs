//! A native terminal view on macOS: a Metal-backed `NSView` laid over the
//! web page at the place the page keeps for the terminal, taking keys,
//! pastes, scrolling and the mouse while it has focus.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{
    define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSAccessibilityTextAreaRole, NSEvent, NSEventModifierFlags, NSPasteboard,
    NSPasteboardTypeString, NSResponder, NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions,
    NSView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSObjectProtocol, NSPoint, NSRange, NSRect,
    NSSize, NSString,
};
use raw_window_handle::{
    AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle,
};

use crate::input::{keys, mods, Key, KeyPress};
use crate::pointer::{MouseAction, MouseButton, MouseGeometry};
use crate::renderer::Renderer;
use crate::stream::{Attach, Event, Stream};
use crate::surface::{Colors, Layout, Paint, Painter};
use crate::theme::Theme;
use crate::vt::Vt;

/// Lines one notch of a mouse wheel scrolls; trackpads report pixels and
/// are divided by the cell height instead.
const WHEEL_LINES: f64 = 3.0;

/// What the view hands to its terminal.
enum ViewInput {
    Key(KeyPress),
    /// Text an input method committed.
    Text(String),
    /// Text an input method is composing, empty once it ends.
    Preedit(String),
    Paste(String),
    /// ⌘C, handled only when there is a selection to copy.
    Copy,
    /// The pointer moved or ⌘ changed: whether a link is under it decides the cursor.
    Hover {
        point: NSPoint,
        command: bool,
    },
    Scroll {
        lines: f64,
        precise_pixels: Option<f64>,
        point: NSPoint,
        mods: u16,
    },
    Mouse {
        action: MouseAction,
        button: MouseButton,
        /// In the view, in CSS pixels from its top left.
        point: NSPoint,
        clicks: isize,
        mods: u16,
        command: bool,
    },
}

/// Returns whether the input was handled.
type InputSink = Box<dyn Fn(ViewInput) -> bool>;

/// What the terminal tells the page.
#[derive(Clone, Debug, PartialEq)]
pub enum TerminalEvent {
    /// The socket closed; a new ticket is needed to attach again.
    Closed { reason: String, code: Option<u16> },
    /// A full replay arrived: the screen is current.
    Online,
    /// A link was ⌘-clicked.
    Link(String),
    /// Whether ⌘ is held over a link, which shows a pointing hand.
    LinkHover(bool),
    /// A right click the program did not take, in page coordinates.
    ContextMenu { x: f64, y: f64 },
    /// The shell reported the program now in the foreground.
    Command(String),
}

pub struct ViewIvars {
    sink: RefCell<Option<InputSink>>,
    /// The cursor's cell in view coordinates, for the input method's window.
    caret: RefCell<Option<Box<dyn Fn() -> NSRect>>>,
    /// The visible screen's text, for VoiceOver.
    screen_text: RefCell<Option<Box<dyn Fn() -> String>>>,
    /// Text the input method is composing.
    marked: RefCell<String>,
    /// Text the input method committed during the current key press.
    inserted: RefCell<Option<String>>,
    in_key_down: Cell<bool>,
    /// The pointer's last place in the view, for a ⌘ press without a move.
    pointer: Cell<NSPoint>,
    /// Page content over the terminal, in view coordinates: holes are drawn
    /// transparent, and the mouse goes to the page in both.
    holes: RefCell<Vec<NSRect>>,
    passthrough: RefCell<Vec<NSRect>>,
    /// The page's web view, which drags over the terminal go to.
    webview: RefCell<Option<Retained<NSView>>>,
}

fn contains(rect: &NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x
        && point.y >= rect.origin.y
        && point.x < rect.origin.x + rect.size.width
        && point.y < rect.origin.y + rect.size.height
}

fn mods_of(flags: NSEventModifierFlags) -> u16 {
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
    bits
}

/// What VoiceOver calls the terminal.
const ACCESSIBILITY_LABEL: &str = "Terminal";

/// `NSNotFound`, which an input method reads as "no range".
const NOT_FOUND: usize = isize::MAX as usize;

/// The text of an `NSString` or `NSAttributedString` an input method passes.
fn text_of(string: &AnyObject) -> String {
    if let Some(attributed) = string.downcast_ref::<NSAttributedString>() {
        return attributed.string().to_string();
    }
    string
        .downcast_ref::<NSString>()
        .map(|text| text.to_string())
        .unwrap_or_default()
}

/// The value AppKit takes as "no drag operation".
const DRAG_NONE: usize = 0;

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

        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            true
        }

        #[unsafe(method(accessibilityRole))]
        fn accessibility_role(&self) -> *mut NSString {
            let role: &NSString = unsafe { NSAccessibilityTextAreaRole };
            Retained::autorelease_return(role.retain())
        }

        #[unsafe(method(accessibilityLabel))]
        fn accessibility_label(&self) -> *mut NSString {
            Retained::autorelease_return(NSString::from_str(ACCESSIBILITY_LABEL))
        }

        #[unsafe(method(accessibilityValue))]
        fn accessibility_value(&self) -> *mut NSString {
            let text = self.ivars().screen_text.borrow().as_ref().map(|read| read()).unwrap_or_default();
            Retained::autorelease_return(NSString::from_str(&text))
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let local = self.convertPoint_fromView(point, unsafe { self.superview() }.as_deref());
            let ivars = self.ivars();
            let page = ivars.holes.borrow().iter().chain(ivars.passthrough.borrow().iter()).any(|rect| contains(rect, local));
            if page {
                return std::ptr::null_mut();
            }
            unsafe { msg_send![super(self), hitTest: point] }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if let Some(window) = self.window() {
                let responder: &NSResponder = self;
                window.makeFirstResponder(Some(responder));
            }
            self.mouse(event, MouseAction::Press, MouseButton::Left);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.hover(self.local(event), event.modifierFlags());
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            self.hover(self.ivars().pointer.get(), event.modifierFlags());
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.mouse(event, MouseAction::Motion, MouseButton::Left);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.mouse(event, MouseAction::Release, MouseButton::Left);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.mouse(event, MouseAction::Press, MouseButton::Right);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.mouse(event, MouseAction::Release, MouseButton::Right);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            self.mouse(event, MouseAction::Press, MouseButton::Middle);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            self.mouse(event, MouseAction::Release, MouseButton::Middle);
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, info: *mut AnyObject) -> usize {
            self.forward_drag(|webview| unsafe { msg_send![webview, draggingEntered: info] }).unwrap_or(DRAG_NONE)
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, info: *mut AnyObject) -> usize {
            self.forward_drag(|webview| unsafe { msg_send![webview, draggingUpdated: info] }).unwrap_or(DRAG_NONE)
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, info: *mut AnyObject) {
            self.forward_drag(|webview| -> () { unsafe { msg_send![webview, draggingExited: info] } });
        }

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag(&self, info: *mut AnyObject) -> Bool {
            self.forward_drag(|webview| unsafe { msg_send![webview, prepareForDragOperation: info] }).unwrap_or(Bool::NO)
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, info: *mut AnyObject) -> Bool {
            self.forward_drag(|webview| unsafe { msg_send![webview, performDragOperation: info] }).unwrap_or(Bool::NO)
        }

        #[unsafe(method(concludeDragOperation:))]
        fn conclude_drag(&self, info: *mut AnyObject) {
            self.forward_drag(|webview| -> () { unsafe { msg_send![webview, concludeDragOperation: info] } });
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let ivars = self.ivars();
            let flags = event.modifierFlags();
            if flags.contains(NSEventModifierFlags::Command) {
                return;
            }
            let composing_before = !ivars.marked.borrow().is_empty();
            // Control chords are terminal keys, never input method text.
            if !flags.contains(NSEventModifierFlags::Control) {
                ivars.inserted.replace(None);
                ivars.in_key_down.set(true);
                self.interpretKeyEvents(&NSArray::from_retained_slice(&[event.retain()]));
                ivars.in_key_down.set(false);
            }
            let inserted = ivars.inserted.take();
            let composing = !ivars.marked.borrow().is_empty();
            if composing || (composing_before && inserted.is_none()) {
                return;
            }
            if composing_before {
                if let Some(text) = inserted {
                    self.emit(ViewInput::Text(text));
                }
                return;
            }
            if let Some(mut press) = key_press(event) {
                if let Some(text) = inserted {
                    if press.mods & mods::CTRL == 0 {
                        press.text = text;
                    }
                }
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
            if !focused || !flags.contains(NSEventModifierFlags::Command) {
                return Bool::NO;
            }
            match event.keyCode() {
                KVK_ANSI_V => {
                    let text = unsafe { NSPasteboard::generalPasteboard().stringForType(NSPasteboardTypeString) };
                    if let Some(text) = text {
                        self.emit(ViewInput::Paste(text.to_string()));
                    }
                    Bool::YES
                }
                KVK_ANSI_C => Bool::new(self.emit(ViewInput::Copy)),
                _ => Bool::NO,
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let delta = event.scrollingDeltaY();
            let precise = event.hasPreciseScrollingDeltas().then_some(delta);
            self.emit(ViewInput::Scroll {
                lines: delta.signum() * WHEEL_LINES,
                precise_pixels: precise,
                point: self.local(event),
                mods: mods_of(event.modifierFlags()),
            });
        }
    }

    unsafe impl NSObjectProtocol for TerminalView {}

    unsafe impl NSTextInputClient for TerminalView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text(&self, string: &AnyObject, _replacement: NSRange) {
            let text = text_of(string);
            let ivars = self.ivars();
            if !ivars.marked.borrow().is_empty() {
                self.set_marked(String::new());
            }
            if ivars.in_key_down.get() {
                *ivars.inserted.borrow_mut() = Some(text);
            } else {
                self.emit(ViewInput::Text(text));
            }
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, _selector: Sel) {}

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, string: &AnyObject, _selected: NSRange, _replacement: NSRange) {
            self.set_marked(text_of(string));
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            self.set_marked(String::new());
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(NOT_FOUND, 0)
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            match self.marked_length() {
                0 => NSRange::new(NOT_FOUND, 0),
                length => NSRange::new(0, length),
            }
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            !self.ivars().marked.borrow().is_empty()
        }

        #[unsafe(method(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(&self, _range: NSRange, _actual: *mut NSRange) -> *mut NSAttributedString {
            std::ptr::null_mut()
        }

        #[unsafe(method(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> *mut NSArray<NSAttributedStringKey> {
            Retained::autorelease_return(NSArray::new())
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect_for_character_range(&self, _range: NSRange, _actual: *mut NSRange) -> NSRect {
            let local = self.ivars().caret.borrow().as_ref().map_or(NSRect::ZERO, |caret| caret());
            let in_window = self.convertRect_toView(local, None);
            self.window().map_or(in_window, |window| window.convertRectToScreen(in_window))
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> usize {
            NOT_FOUND
        }

    }
);

impl TerminalView {
    fn set_marked(&self, text: String) {
        *self.ivars().marked.borrow_mut() = text.clone();
        self.emit(ViewInput::Preedit(text));
    }

    fn marked_length(&self) -> usize {
        self.ivars().marked.borrow().encode_utf16().count()
    }

    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars {
            sink: RefCell::new(None),
            caret: RefCell::new(None),
            screen_text: RefCell::new(None),
            marked: RefCell::default(),
            inserted: RefCell::new(None),
            in_key_down: Cell::new(false),
            pointer: Cell::new(NSPoint::new(0.0, 0.0)),
            holes: RefCell::default(),
            passthrough: RefCell::default(),
            webview: RefCell::new(None),
        });
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0));
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                NSTrackingAreaOptions::MouseMoved
                    | NSTrackingAreaOptions::ActiveInKeyWindow
                    | NSTrackingAreaOptions::InVisibleRect,
                Some(&view),
                None,
            )
        };
        view.addTrackingArea(&area);
        view
    }

    fn hover(&self, point: NSPoint, flags: NSEventModifierFlags) {
        self.ivars().pointer.set(point);
        self.emit(ViewInput::Hover {
            point,
            command: flags.contains(NSEventModifierFlags::Command),
        });
    }

    fn emit(&self, input: ViewInput) -> bool {
        self.ivars()
            .sink
            .borrow()
            .as_ref()
            .is_some_and(|sink| sink(input))
    }

    fn local(&self, event: &NSEvent) -> NSPoint {
        self.convertPoint_fromView(event.locationInWindow(), None)
    }

    fn mouse(&self, event: &NSEvent, action: MouseAction, button: MouseButton) {
        let flags = event.modifierFlags();
        self.emit(ViewInput::Mouse {
            action,
            button,
            point: self.local(event),
            clicks: event.clickCount(),
            mods: mods_of(flags),
            command: flags.contains(NSEventModifierFlags::Command),
        });
    }

    fn forward_drag<R>(&self, send: impl FnOnce(&NSView) -> R) -> Option<R> {
        let webview = self.ivars().webview.borrow().clone()?;
        Some(send(&webview))
    }
}

// macOS virtual key codes (Carbon's kVK_* values), which name physical keys
// whatever the keyboard layout.
const KVK_ANSI_V: u16 = 0x09;
const KVK_ANSI_C: u16 = 0x08;

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
    let bits = mods_of(flags);
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
    /// The page's viewport width in CSS pixels, which with the web view's
    /// width in points gives the page zoom; 0 for no zoom.
    pub viewport_width: f64,
}

/// A terminal drawn natively over the page. It lives on the main thread.
pub struct NativeTerminal {
    view: Retained<TerminalView>,
    parent: Retained<NSView>,
    stream: Arc<Stream>,
    /// Kept for its drop, which stops the render thread.
    _paint: Paint,
    painter: Arc<Painter>,
    scale: Cell<f64>,
    /// The grid this view last claimed.
    claimed: Arc<Mutex<(u16, u16)>>,
    /// Whether this viewer owns the terminal's size.
    owner: Arc<AtomicBool>,
    placement: Rc<Cell<Placement>>,
    /// Takes no keyboard input, as a read-only pane.
    read_only: bool,
    /// Whether the view was shown at the last placement.
    shown: Cell<bool>,
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
        read_only: bool,
        on_event: impl Fn(TerminalEvent) + Send + Sync + 'static,
    ) -> Result<NativeTerminal, String> {
        let mtm = MainThreadMarker::new().ok_or("a native terminal opens on the main thread")?;
        let parent: Retained<NSView> = unsafe { Retained::retain(parent.cast::<NSView>()) }
            .ok_or("the window has no content view")?;
        let view = TerminalView::new(mtm);
        let webview = find_webview(&parent);
        if let Some(webview) = &webview {
            view.registerForDraggedTypes(&webview.registeredDraggedTypes());
        }
        *view.ivars().webview.borrow_mut() = webview;
        parent.addSubview(&view);
        let scale = zoom_of(&parent, placement) * backing_of(&parent);

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
        if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
        {
            config.alpha_mode = wgpu::CompositeAlphaMode::PostMultiplied;
        }
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);
        let renderer = Renderer::new(device, queue, config.format, layout.font_px);

        let painter_slot: Arc<Mutex<Option<Arc<Painter>>>> = Arc::default();
        let signal = painter_slot.clone();
        let on_event: Arc<dyn Fn(TerminalEvent) + Send + Sync> = Arc::new(on_event);
        let notify = on_event.clone();
        let owner = Arc::new(AtomicBool::new(true));
        let ownership = owner.clone();
        let grid = grid_of(placement);
        let attach = Attach {
            claim: Some(grid),
            ..attach
        };
        let stream = Arc::new(Stream::open_on(runtime, attach, move |event| match event {
            Event::Closed { reason, code } => notify(TerminalEvent::Closed { reason, code }),
            Event::Online => notify(TerminalEvent::Online),
            Event::Clipboard(text) => copy_to_pasteboard(text),
            Event::Ownership { local, .. } => ownership.store(local, Ordering::Relaxed),
            Event::Command(command) => notify(TerminalEvent::Command(command)),
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
            scale: Cell::new(scale),
            claimed: Arc::new(Mutex::new(grid)),
            owner,
            placement: Rc::new(Cell::new(placement)),
            read_only,
            shown: Cell::new(false),
        };
        terminal.place(placement);
        terminal.connect_input(on_event);
        Ok(terminal)
    }

    fn connect_input(&self, on_event: Arc<dyn Fn(TerminalEvent) + Send + Sync>) {
        let stream = self.stream.clone();
        let screen = self.stream.screen();
        let painter = self.painter.clone();
        let claimed = self.claimed.clone();
        let owner = self.owner.clone();
        let placement = self.placement.clone();
        let read_only = self.read_only;
        // Typing here takes the size back from another viewer, as the browser does.
        let reclaim = {
            let stream = stream.clone();
            move || {
                if !owner.load(Ordering::Relaxed) {
                    let (cols, rows) = *claimed.lock().unwrap();
                    stream.claim(cols, rows);
                    stream.resync();
                }
            }
        };
        *self.view.ivars().screen_text.borrow_mut() = Some(Box::new({
            let screen = screen.clone();
            move || {
                screen
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map_or_else(String::new, |vt| {
                        let frame = vt.frame();
                        (0..frame.rows)
                            .map(|y| frame.row_text(y))
                            .collect::<Vec<_>>()
                            .join("\n")
                            .trim_end()
                            .to_string()
                    })
            }
        }));
        *self.view.ivars().caret.borrow_mut() = Some(Box::new({
            let screen = screen.clone();
            let placement = placement.clone();
            move || {
                let at = placement.get();
                let cursor = screen
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|vt| vt.frame().cursor);
                let (x, y) = cursor.map_or((0.0, 0.0), |c| (f64::from(c.x), f64::from(c.y)));
                NSRect::new(
                    NSPoint::new(
                        at.padding + x * at.cell_width,
                        at.padding + y * at.cell_height,
                    ),
                    NSSize::new(at.cell_width, at.cell_height),
                )
            }
        }));
        // Where a drag that selects text started, and where the last
        // selection started for Shift-click, as screen points.
        let anchor: Cell<Option<(u16, u32)>> = Cell::new(None);
        let last_anchor: Cell<Option<(u16, u32)>> = Cell::new(None);
        let over_link = Cell::new(false);
        *self.view.ivars().sink.borrow_mut() = Some(Box::new(move |input| {
            if let ViewInput::Preedit(text) = &input {
                painter.set_preedit(if read_only { "" } else { text });
                return true;
            }
            let typing = matches!(
                input,
                ViewInput::Key(_) | ViewInput::Text(_) | ViewInput::Paste(_)
            );
            if read_only && typing {
                return false;
            }
            let mut guard = screen.lock().unwrap();
            let Some(vt) = guard.as_mut() else {
                return false;
            };
            let at = placement.get();
            match input {
                ViewInput::Preedit(_) => true,
                ViewInput::Hover { point, command } => {
                    let link = command && {
                        let cell = cell_at(vt, at, point);
                        vt.link_at(cell.0, cell.1).is_some()
                    };
                    drop(guard);
                    if over_link.replace(link) != link {
                        on_event(TerminalEvent::LinkHover(link));
                    }
                    true
                }
                ViewInput::Text(text) => {
                    vt.clear_selection();
                    let _ = vt.scroll(isize::MAX);
                    drop(guard);
                    reclaim();
                    stream.input(text.into_bytes(), false);
                    painter.redraw();
                    true
                }
                ViewInput::Key(press) => {
                    let bytes = vt.encode_key(&press);
                    if bytes.is_empty() {
                        return false;
                    }
                    vt.clear_selection();
                    reclaim();
                    let submitted = press.key == keys::ENTER || press.key == keys::NUMPAD_ENTER;
                    let _ = vt.scroll(isize::MAX);
                    drop(guard);
                    stream.input(bytes, submitted);
                    painter.redraw();
                    true
                }
                ViewInput::Paste(text) => {
                    let bytes = vt.encode_paste(&text);
                    drop(guard);
                    stream.input(bytes, false);
                    true
                }
                ViewInput::Copy => {
                    let text = vt.selected_text();
                    if text.is_empty() {
                        return false;
                    }
                    drop(guard);
                    copy_to_pasteboard(text);
                    true
                }
                ViewInput::Scroll {
                    lines,
                    precise_pixels,
                    point,
                    mods: bits,
                } => {
                    let rows = match precise_pixels {
                        Some(pixels) => -(pixels / at.cell_height.max(1.0)).round(),
                        None => -lines,
                    };
                    if rows == 0.0 {
                        return true;
                    }
                    if vt.mouse_tracking() && bits & mods::SHIFT == 0 && !read_only {
                        let button = if rows < 0.0 {
                            MouseButton::WheelUp
                        } else {
                            MouseButton::WheelDown
                        };
                        let mut bytes = Vec::new();
                        for _ in 0..(rows.abs() as usize) {
                            bytes.extend(vt.encode_mouse(
                                MouseAction::Press,
                                Some(button),
                                position(point),
                                bits,
                                geometry(at),
                            ));
                        }
                        drop(guard);
                        if !bytes.is_empty() {
                            stream.input(bytes, false);
                        }
                        return true;
                    }
                    let _ = vt.scroll(rows as isize);
                    drop(guard);
                    painter.redraw();
                    true
                }
                ViewInput::Mouse {
                    action,
                    button,
                    point,
                    clicks,
                    mods: bits,
                    command,
                } => {
                    if vt.mouse_tracking() && bits & mods::SHIFT == 0 && !command && !read_only {
                        let bytes = vt.encode_mouse(
                            action,
                            Some(button),
                            position(point),
                            bits,
                            geometry(at),
                        );
                        drop(guard);
                        if !bytes.is_empty() {
                            stream.input(bytes, false);
                        }
                        return true;
                    }
                    let cell = cell_at(vt, at, point);
                    match (button, action) {
                        (MouseButton::Left, MouseAction::Press) if command => {
                            let link = vt.link_at(cell.0, cell.1);
                            drop(guard);
                            match link {
                                Some(href) => on_event(TerminalEvent::Link(href)),
                                None => eprintln!(
                                    "native terminal: no link under the ⌘-click at column {} row {}",
                                    cell.0, cell.1
                                ),
                            }
                        }
                        (MouseButton::Left, MouseAction::Press) => {
                            anchor.set(None);
                            let here = vt.screen_point(cell.0, cell.1);
                            match clicks {
                                1 if bits & mods::SHIFT != 0 && last_anchor.get().is_some() => {
                                    let start = last_anchor.get().unwrap_or(here);
                                    vt.select_screen(start, here);
                                    anchor.set(Some(start));
                                }
                                1 => {
                                    vt.clear_selection();
                                    anchor.set(Some(here));
                                    last_anchor.set(Some(here));
                                }
                                2 => {
                                    vt.select_word(cell.0, cell.1);
                                }
                                _ => {
                                    vt.select_line(cell.0, cell.1);
                                }
                            }
                            drop(guard);
                            painter.redraw();
                        }
                        (MouseButton::Left, MouseAction::Motion) => {
                            if let Some(start) = anchor.get() {
                                // Dragging past the top or bottom edge scrolls to extend the selection.
                                if point.y < at.padding {
                                    let _ = vt.scroll(-1);
                                } else if point.y > at.height - at.padding {
                                    let _ = vt.scroll(1);
                                }
                                let cell = cell_at(vt, at, point);
                                let here = vt.screen_point(cell.0, cell.1);
                                vt.select_screen(start, here);
                                drop(guard);
                                painter.redraw();
                            }
                        }
                        (MouseButton::Left, MouseAction::Release) => anchor.set(None),
                        (MouseButton::Right, MouseAction::Press) => {
                            drop(guard);
                            on_event(TerminalEvent::ContextMenu {
                                x: at.x + point.x,
                                y: at.y + point.y,
                            });
                        }
                        _ => {}
                    }
                    true
                }
            }
        }));
    }

    /// Sets the page content over the terminal, as rectangles in page
    /// coordinates: `holes` show the page and take the mouse, and
    /// `passthrough` only takes the mouse.
    pub fn set_regions(&self, holes: &[NSRect], passthrough: &[NSRect]) {
        let at = self.placement.get();
        let local = |rects: &[NSRect]| -> Vec<NSRect> {
            rects
                .iter()
                .map(|rect| {
                    NSRect::new(
                        NSPoint::new(rect.origin.x - at.x, rect.origin.y - at.y),
                        rect.size,
                    )
                })
                .collect()
        };
        let holes = local(holes);
        let scale = self.scale.get() as f32;
        self.painter.set_holes(
            holes
                .iter()
                .map(|rect| {
                    [
                        rect.origin.x as f32 * scale,
                        rect.origin.y as f32 * scale,
                        rect.size.width as f32 * scale,
                        rect.size.height as f32 * scale,
                    ]
                })
                .collect(),
        );
        *self.view.ivars().holes.borrow_mut() = holes;
        *self.view.ivars().passthrough.borrow_mut() = local(passthrough);
    }

    /// Moves the view to the page's current place for the terminal.
    pub fn place(&self, placement: Placement) {
        let bounds = self.parent.bounds();
        let zoom = zoom_of(&self.parent, placement);
        let height = placement.height * zoom;
        let y = if self.parent.isFlipped() {
            placement.y * zoom
        } else {
            bounds.size.height - placement.y * zoom - height
        };
        self.view.setFrame(NSRect::new(
            NSPoint::new(placement.x * zoom, y),
            NSSize::new(placement.width * zoom, height),
        ));
        // The view works in the page's CSS pixels whatever the zoom, so mouse
        // points, the caret and the page's regions need no conversion.
        self.view
            .setBoundsSize(NSSize::new(placement.width, placement.height));
        self.view.setHidden(!placement.visible);
        self.scale.set(zoom * backing_of(&self.parent));
        self.painter
            .set_layout(layout_for(placement, self.scale.get()));
        self.placement.set(placement);
        let was_shown = self.shown.replace(placement.visible);
        let showing = placement.visible && !was_shown;
        if placement.visible {
            let grid = grid_of(placement);
            let mut claimed = self.claimed.lock().unwrap();
            // A warm terminal shown again takes the size back if another viewer took it.
            let lost = showing && !self.owner.load(Ordering::Relaxed);
            if *claimed != grid || lost {
                *claimed = grid;
                self.stream.claim(grid.0, grid.1);
                self.stream.resync();
            }
        }
    }

    /// Draws with the page's terminal theme.
    pub fn set_theme(&self, theme: Theme, colors: Colors) {
        self.stream.set_theme(theme);
        self.painter.set_colors(colors);
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

/// Points per CSS pixel: above 1 when the page is zoomed in.
fn zoom_of(parent: &NSView, placement: Placement) -> f64 {
    let width = parent.bounds().size.width;
    if placement.viewport_width > 0.0 && width > 0.0 {
        width / placement.viewport_width
    } else {
        1.0
    }
}

/// Device pixels per point on the window's display.
fn backing_of(parent: &NSView) -> f64 {
    parent
        .window()
        .map_or(1.0, |window| window.backingScaleFactor())
}

/// The viewport cell under a point in the view, clamped to the grid.
fn cell_at(vt: &Vt, at: Placement, point: NSPoint) -> (u16, u16) {
    let frame = vt.frame();
    let col = ((point.x - at.padding) / at.cell_width.max(1.0)).floor();
    let row = ((point.y - at.padding) / at.cell_height.max(1.0)).floor();
    let last_col = f64::from(frame.cols.saturating_sub(1));
    let last_row = f64::from(frame.rows.saturating_sub(1));
    (
        col.clamp(0.0, last_col) as u16,
        row.clamp(0.0, last_row) as u16,
    )
}

fn position(point: NSPoint) -> (f32, f32) {
    (point.x as f32, point.y as f32)
}

/// The view's grid geometry for mouse reports, in CSS pixels like the points.
fn geometry(at: Placement) -> MouseGeometry {
    MouseGeometry {
        width: at.width.max(0.0) as u32,
        height: at.height.max(0.0) as u32,
        cell_width: at.cell_width.round() as u32,
        cell_height: at.cell_height.round() as u32,
        padding: at.padding.round() as u32,
    }
}

/// The window's web view, found among the content view's subviews.
fn find_webview(parent: &NSView) -> Option<Retained<NSView>> {
    let class = AnyClass::get(c"WKWebView")?;
    parent
        .subviews()
        .iter()
        .find(|view| view.isKindOfClass(class))
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

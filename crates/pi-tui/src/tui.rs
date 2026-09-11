//! Port of packages/tui/src/tui.ts.
//!
//! Minimal TUI implementation with differential rendering.

use crate::components::image::with_fullscreen_image_fallback;
use crate::fullscreen::{FullscreenViewport, ScrollInfo, SelectionScrollDirection};
use crate::keybindings::get_keybindings;
use crate::keys::{is_key_release, matches_key};
use crate::mouse::{is_mouse_sequence, is_wheel_down, is_wheel_up, parse_sgr_mouse_event, MOUSE_BUTTON_LEFT};
use crate::selection_metadata::TableCellSelectionRegion;
use crate::terminal::Terminal;
use crate::terminal_image::{delete_kitty_image, get_capabilities, is_image_line, set_cell_dimensions};
use crate::utils::{
    extract_segments, normalize_terminal_output, slice_by_column, slice_with_width, strip_ansi,
    visible_content_span, visible_width,
};
use once_cell::sync::Lazy;
use regex::Regex;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

const KITTY_SEQUENCE_PREFIX: &str = "\x1b_G";

fn extract_kitty_image_ids(line: &str) -> Vec<u32> {
    let sequence_start = match line.find(KITTY_SEQUENCE_PREFIX) {
        Some(index) => index,
        None => return Vec::new(),
    };

    let params_start = sequence_start + KITTY_SEQUENCE_PREFIX.len();
    let params_end = match line[params_start..].find(';') {
        Some(index) => params_start + index,
        None => return Vec::new(),
    };

    let params = &line[params_start..params_end];
    for param in params.split(',') {
        let mut parts = param.splitn(2, '=');
        let key = parts.next().unwrap_or("");
        let value = match parts.next() {
            Some(value) => value,
            None => continue,
        };
        if key != "i" {
            continue;
        }
        let id: i64 = match value.parse() {
            Ok(id) => id,
            Err(_) => continue,
        };
        if id > 0 && id <= 0xffff_ffff {
            return vec![id as u32];
        }
    }
    Vec::new()
}

/// Port of the `Component` interface - all components must implement this.
pub trait Component {
    /// Render the component to lines for the given viewport width.
    fn render(&mut self, width: f64) -> Vec<String>;

    fn get_selection_regions(&self) -> Vec<TableCellSelectionRegion> {
        Vec::new()
    }

    /// Optional handler for keyboard input when component has focus.
    fn handle_input(&mut self, data: &str) {
        let _ = data;
    }

    /// If true, component receives key release events (Kitty protocol).
    /// Default is false - release events are filtered out.
    fn wants_key_release(&self) -> bool {
        false
    }

    /// Invalidate any cached rendering state.
    fn invalidate(&mut self);

    /// Focus hook used by the `isFocusable` type guard. Components that implement
    /// [`Focusable`] override this so the TUI can drive the `focused` flag.
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        None
    }
}

/// Interface for components that can receive focus and display a hardware cursor.
/// When focused, the component should emit `CURSOR_MARKER` at the cursor position
/// in its render output.
pub trait Focusable {
    /// Set by TUI when focus changes. Component should emit `CURSOR_MARKER` when true.
    fn focused(&self) -> bool;
    fn set_focused(&mut self, focused: bool);
}

/// Type guard to check if a component implements Focusable.
pub fn is_focusable(component: Option<Rc<RefCell<dyn Component>>>) -> bool {
    match component {
        Some(component) => component.borrow_mut().as_focusable().is_some(),
        None => false,
    }
}

/// Cursor position marker - APC (Application Program Command) sequence.
/// This is a zero-width escape sequence that terminals ignore.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

pub use crate::utils::visible_width;

/// Anchor position for overlays
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayAnchor {
    #[default]
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    TopCenter,
    BottomCenter,
    LeftCenter,
    RightCenter,
}

/// Margin configuration for overlays
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayMargin {
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
    pub left: i64,
}

/// Value that can be absolute (number) or percentage (string like "50%").
#[derive(Debug, Clone, PartialEq)]
pub enum SizeValue {
    Number(f64),
    Percent(String),
}

/// Port of the `OverlayMargin | number` union.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarginValue {
    Number(i64),
    Sides(OverlayMargin),
}

static PERCENT_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d+(?:\.\d+)?)%$").unwrap());

/// Parse a SizeValue into an absolute value given a reference size.
fn parse_size_value(value: Option<&SizeValue>, reference_size: f64) -> Option<f64> {
    let value = value?;
    match value {
        SizeValue::Number(number) => Some(*number),
        SizeValue::Percent(text) => {
            let caps = PERCENT_PATTERN.captures(text)?;
            let percent: f64 = caps.get(1)?.as_str().parse().ok()?;
            Some((reference_size * percent / 100.0).floor())
        }
    }
}

fn is_termux_session() -> bool {
    std::env::var("TERMUX_VERSION")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

/// Options for overlay positioning and sizing.
///
/// The TypeScript `showOverlay(component, options)` passes the component
/// separately; the port carries it in `component` so a single options value
/// describes the whole call.
#[derive(Default)]
pub struct OverlayOptions {
    pub component: Option<Rc<RefCell<dyn Component>>>,
    pub width: Option<SizeValue>,
    pub min_width: Option<f64>,
    pub max_height: Option<SizeValue>,
    pub scrollback: bool,
    pub anchor: Option<OverlayAnchor>,
    pub offset_x: Option<i64>,
    pub offset_y: Option<i64>,
    pub row: Option<SizeValue>,
    pub col: Option<SizeValue>,
    pub above_marker: Option<String>,
    pub margin: Option<MarginValue>,
    pub visible: Option<Rc<dyn Fn(usize, usize) -> bool>>,
    pub non_capturing: bool,
    pub suspend_fullscreen_mouse: bool,
}

/// Port of `InputListenerResult`: `{ consume?: boolean; data?: string }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputListenerResult {
    pub consume: bool,
    pub data: Option<String>,
}

pub type InputListener = Box<dyn Fn(&str) -> InputListenerResult>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSelectionRegion {
    pub line: usize,
    pub col: usize,
    pub width: usize,
}

/// Port of the `TuiStopOptions` interface.
#[derive(Debug, Clone, Copy, Default)]
pub struct TuiStopOptions {
    pub preserve_alt_screen: bool,
    pub flush_fullscreen: bool,
}

/// Port of the `FullscreenOptions` interface.
pub struct FullscreenOptions {
    pub scroll: Vec<Rc<RefCell<dyn Component>>>,
    pub dock: Rc<RefCell<dyn Component>>,
    pub mouse: bool,
    pub viewport_controls: bool,
}

#[derive(Debug, Clone, Copy)]
struct ExitFullscreenOptions {
    flush: bool,
    leave_alt_screen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorPosition {
    row: usize,
    col: usize,
}
/// Container - a component that contains other components
#[derive(Default)]
pub struct Container {
    pub children: Vec<Rc<RefCell<dyn Component>>>,
    selection_regions: Vec<TableCellSelectionRegion>,
}

impl Container {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_child(&mut self, component: Rc<RefCell<dyn Component>>) {
        self.children.push(component);
        self.selection_regions = Vec::new();
    }

    pub fn remove_child(&mut self, component: &Rc<RefCell<dyn Component>>) {
        let before = self.children.len();
        self.children.retain(|child| !Rc::ptr_eq(child, component));
        if self.children.len() != before {
            self.selection_regions = Vec::new();
        }
    }

    pub fn clear(&mut self) {
        self.children = Vec::new();
        self.selection_regions = Vec::new();
    }

    pub fn invalidate(&mut self) {
        self.selection_regions = Vec::new();
        for child in self.children.iter() {
            child.borrow_mut().invalidate();
        }
    }

    pub fn render(&mut self, width: f64) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut selection_regions: Vec<TableCellSelectionRegion> = Vec::new();
        for child in self.children.iter() {
            let line_offset = lines.len();
            let child_lines = child.borrow_mut().render(width);
            for region in child.borrow().get_selection_regions() {
                selection_regions.push(TableCellSelectionRegion {
                    line: region.line + line_offset,
                    table_top: region.table_top + line_offset,
                    table_bottom: region.table_bottom + line_offset,
                    ..region
                });
            }
            lines.extend(child_lines);
        }
        self.selection_regions = selection_regions;
        lines
    }

    pub fn get_selection_regions(&self) -> Vec<TableCellSelectionRegion> {
        self.selection_regions.clone()
    }
}

impl Component for Container {
    fn render(&mut self, width: f64) -> Vec<String> {
        Container::render(self, width)
    }

    fn get_selection_regions(&self) -> Vec<TableCellSelectionRegion> {
        Container::get_selection_regions(self)
    }

    fn invalidate(&mut self) {
        Container::invalidate(self)
    }
}

/// State shared between an [`OverlayHandle`] and the TUI that owns the entry.
struct OverlayShared {
    hidden: Cell<bool>,
    removed: Cell<bool>,
    focused: Cell<bool>,
    focus_requested: Cell<bool>,
    unfocus_requested: Cell<bool>,
    focus_order: Cell<u64>,
}

/// Handle returned by `showOverlay` for controlling the overlay.
///
/// The TypeScript handle closes over the TUI and mutates the stack directly.
/// Rust cannot alias the owner, so the handle records intent in shared cells and
/// the TUI applies it in [`TUI::sync_overlays`], which runs before input handling
/// and before every render.
pub struct OverlayHandle {
    shared: Rc<OverlayShared>,
}

impl OverlayHandle {
    /// Permanently remove the overlay (cannot be shown again)
    pub fn hide(&self) {
        self.shared.removed.set(true);
    }

    /// Temporarily hide or show the overlay
    pub fn set_hidden(&self, hidden: bool) {
        self.shared.hidden.set(hidden);
    }

    /// Check if overlay is temporarily hidden
    pub fn is_hidden(&self) -> bool {
        self.shared.hidden.get()
    }

    /// Focus this overlay and bring it to the visual front
    pub fn focus(&self) {
        self.shared.focus_requested.set(true);
    }

    /// Release focus to the previous target
    pub fn unfocus(&self) {
        self.shared.unfocus_requested.set(true);
    }

    /// Check if this overlay currently has focus
    pub fn is_focused(&self) -> bool {
        self.shared.focused.get()
    }
}

struct OverlayEntry {
    component: Rc<RefCell<dyn Component>>,
    options: Option<OverlayOptions>,
    pre_focus: Option<Rc<RefCell<dyn Component>>>,
    hidden: bool,
    focus_order: u64,
    shared: Rc<OverlayShared>,
}

/// Snapshot of the inline differ's bookkeeping, frozen while fullscreen is active.
struct InlineState {
    previous_lines: Vec<String>,
    previous_kitty_image_ids: HashSet<u32>,
    previous_width: i64,
    previous_height: usize,
    cursor_row: usize,
    hardware_cursor_row: usize,
    max_lines_rendered: usize,
    previous_viewport_top: usize,
}

struct FullscreenState {
    viewport: FullscreenViewport,
    scroll: Vec<Rc<RefCell<dyn Component>>>,
    dock: Rc<RefCell<dyn Component>>,
    mouse: bool,
    viewport_controls: bool,
    inline_state: InlineState,
}

struct OverlayRender {
    component: Rc<RefCell<dyn Component>>,
    overlay_lines: Vec<String>,
    row: i64,
    col: i64,
    width: usize,
    scrollback: bool,
    above_marker: Option<AboveMarker>,
}

#[derive(Debug, Clone, Copy)]
struct AboveMarker {
    line: usize,
    col: usize,
    offset_y: i64,
}

/// Resolved overlay geometry.
#[derive(Debug, Clone, Copy)]
struct OverlayLayout {
    width: i64,
    row: i64,
    col: i64,
    max_height: Option<i64>,
}

/// Port of `resolveOverlayLayout`'s return value for the width/maxHeight pre-pass.
#[derive(Debug, Clone, Copy)]
struct OverlaySize {
    width: i64,
    max_height: Option<i64>,
}
/// TUI - Main class for managing terminal UI with differential rendering
pub struct TUI {
    container: Container,
    pub terminal: Box<dyn Terminal>,
    previous_lines: Vec<String>,
    previous_kitty_image_ids: HashSet<u32>,
    previous_width: i64,
    previous_height: usize,
    focused_component: Option<Rc<RefCell<dyn Component>>>,
    input_listeners: Vec<usize>,
    input_listener_slots: Vec<(usize, InputListener)>,
    next_input_listener_id: usize,
    /// Global callback for debug key (Shift+Ctrl+D). Called before input is
    /// forwarded to the focused component.
    pub on_debug: Option<Box<dyn FnMut()>>,
    /// Copies fullscreen mouse selections; when unset, OSC 52 is written directly.
    pub on_copy: Option<Box<dyn FnMut(&str)>>,
    /// Opens hyperlinks clicked in the fullscreen viewport; when unset, the
    /// platform opener is used.
    pub on_open_url: Option<Box<dyn FnMut(&str)>>,
    render_requested: bool,
    render_timer_active: bool,
    last_render_at_ms: f64,
    /// Logical cursor row (end of rendered content)
    cursor_row: usize,
    /// Actual terminal cursor row (may differ due to IME positioning)
    hardware_cursor_row: usize,
    show_hardware_cursor: bool,
    /// Clear empty rows when content shrinks (default: off)
    clear_on_shrink: bool,
    /// Track terminal's working area (max lines ever rendered)
    max_lines_rendered: usize,
    /// Track previous viewport top for resize-aware cursor moves
    previous_viewport_top: usize,
    full_redraw_count: usize,
    /// One-shot: repaint visible viewport in place instead of replaying scrollback
    preserve_viewport_on_next_render: bool,
    stopped: bool,
    fullscreen_left_mouse_dragged: bool,
    fullscreen_pressed_hyperlink: Option<String>,
    overlay_selection_regions: Vec<FrameSelectionRegion>,
    fullscreen: Option<FullscreenState>,
    selection_auto_scroll_timer_active: bool,
    selection_auto_scroll_direction: Option<SelectionScrollDirection>,
    selection_auto_scroll_row: i64,
    selection_auto_scroll_column: i64,
    focus_order_counter: u64,
    overlay_stack: Vec<OverlayEntry>,
}

impl TUI {
    pub const MIN_RENDER_INTERVAL_MS: f64 = 16.0;
    pub const WHEEL_SCROLL_LINES: i64 = 3;
    pub const SELECTION_AUTO_SCROLL_DELAY_MS: u64 = 150;
    pub const SELECTION_AUTO_SCROLL_INTERVAL_MS: u64 = 50;
    const SEGMENT_RESET: &'static str = "\x1b[0m\x1b]8;;\x07";

    pub fn new(terminal: Box<dyn Terminal>, show_hardware_cursor: Option<bool>) -> Self {
        let show_hardware_cursor = show_hardware_cursor.unwrap_or_else(|| {
            std::env::var("PI_HARDWARE_CURSOR")
                .map(|value| value == "1")
                .unwrap_or(false)
        });
        Self {
            container: Container::new(),
            terminal,
            previous_lines: Vec::new(),
            previous_kitty_image_ids: HashSet::new(),
            previous_width: 0,
            previous_height: 0,
            focused_component: None,
            input_listeners: Vec::new(),
            input_listener_slots: Vec::new(),
            next_input_listener_id: 1,
            on_debug: None,
            on_copy: None,
            on_open_url: None,
            render_requested: false,
            render_timer_active: false,
            last_render_at_ms: 0.0,
            cursor_row: 0,
            hardware_cursor_row: 0,
            show_hardware_cursor,
            clear_on_shrink: std::env::var("PI_CLEAR_ON_SHRINK")
                .map(|value| value == "1")
                .unwrap_or(false),
            max_lines_rendered: 0,
            previous_viewport_top: 0,
            full_redraw_count: 0,
            preserve_viewport_on_next_render: false,
            stopped: false,
            fullscreen_left_mouse_dragged: false,
            fullscreen_pressed_hyperlink: None,
            overlay_selection_regions: Vec::new(),
            fullscreen: None,
            selection_auto_scroll_timer_active: false,
            selection_auto_scroll_direction: None,
            selection_auto_scroll_row: 0,
            selection_auto_scroll_column: 0,
            focus_order_counter: 0,
            overlay_stack: Vec::new(),
        }
    }

    pub fn add_child(&mut self, component: Rc<RefCell<dyn Component>>) {
        self.container.add_child(component);
    }

    pub fn remove_child(&mut self, component: &Rc<RefCell<dyn Component>>) {
        self.container.remove_child(component);
    }

    pub fn clear(&mut self) {
        self.container.clear();
    }

    pub fn full_redraws(&self) -> usize {
        self.full_redraw_count
    }

    pub fn get_show_hardware_cursor(&self) -> bool {
        self.show_hardware_cursor
    }

    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        if self.show_hardware_cursor == enabled {
            return;
        }
        self.show_hardware_cursor = enabled;
        if !enabled {
            self.terminal.hide_cursor();
        }
        self.request_render(false);
    }

    pub fn get_clear_on_shrink(&self) -> bool {
        self.clear_on_shrink
    }

    /// Set whether to trigger full re-render when content shrinks.
    /// When true (default), empty rows are cleared when content shrinks.
    /// When false, empty rows remain (reduces redraws on slower terminals).
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.clear_on_shrink = enabled;
    }

    pub fn set_focus(&mut self, component: Option<Rc<RefCell<dyn Component>>>) {
        // Clear focused flag on old component
        if let Some(focused) = self.focused_component.clone() {
            if let Some(focusable) = focused.borrow_mut().as_focusable() {
                focusable.set_focused(false);
            }
        }

        self.focused_component = component.clone();

        // Set focused flag on new component
        if let Some(component) = component {
            if let Some(focusable) = component.borrow_mut().as_focusable() {
                focusable.set_focused(true);
            }
        }
    }

    pub fn focused_component(&self) -> Option<Rc<RefCell<dyn Component>>> {
        self.focused_component.clone()
    }

    /// Show an overlay component with configurable positioning and sizing.
    /// Returns a handle to control the overlay's visibility.
    pub fn show_overlay(&mut self, options: OverlayOptions) -> OverlayHandle {
        self.focus_order_counter += 1;
        let shared = Rc::new(OverlayShared {
            hidden: Cell::new(false),
            removed: Cell::new(false),
            focused: Cell::new(false),
            focus_requested: Cell::new(false),
            unfocus_requested: Cell::new(false),
            focus_order: Cell::new(self.focus_order_counter),
        });
        let component = match options.component.clone() {
            Some(component) => component,
            None => panic!("showOverlay requires a component"),
        };
        let non_capturing = options.non_capturing;
        let entry = OverlayEntry {
            component: component.clone(),
            options: Some(options),
            pre_focus: self.focused_component.clone(),
            hidden: false,
            focus_order: self.focus_order_counter,
            shared: shared.clone(),
        };
        let index = self.overlay_stack.len();
        self.overlay_stack.push(entry);
        // Only focus if overlay is actually visible
        if !non_capturing && self.is_overlay_visible(index) {
            self.set_focus(Some(component));
        }
        self.sync_fullscreen_mouse_tracking();
        self.terminal.hide_cursor();
        self.request_render(false);

        OverlayHandle { shared }
    }

    /// Apply the pending requests recorded by overlay handles. Runs before input
    /// handling and before every render so the stack stays consistent.
    pub fn sync_overlays(&mut self) {
        if self.overlay_stack.is_empty() {
            return;
        }
        let mut removed_any = false;
        let mut index = 0usize;
        while index < self.overlay_stack.len() {
            let shared = self.overlay_stack[index].shared.clone();
            if shared.removed.get() {
                let entry = self.overlay_stack.remove(index);
                removed_any = true;
                let is_focused = self.is_focused_component(&entry.component);
                if is_focused {
                    let top_visible = self
                        .get_topmost_visible_overlay_index()
                        .map(|top| self.overlay_stack[top].component.clone());
                    let next = top_visible.or(entry.pre_focus.clone());
                    self.set_focus(next);
                }
                continue;
            }
            let hidden = shared.hidden.get();
            if self.overlay_stack[index].hidden != hidden {
                self.overlay_stack[index].hidden = hidden;
                let component = self.overlay_stack[index].component.clone();
                if hidden {
                    // If this overlay had focus, move focus to next visible or preFocus
                    if self.is_focused_component(&component) {
                        let top_visible = self
                            .get_topmost_visible_overlay_index()
                            .map(|top| self.overlay_stack[top].component.clone());
                        let next = top_visible.or(self.overlay_stack[index].pre_focus.clone());
                        self.set_focus(next);
                    }
                } else {
                    // Restore focus to this overlay when showing (if it's actually visible)
                    let non_capturing = self.overlay_stack[index]
                        .options
                        .as_ref()
                        .map(|options| options.non_capturing)
                        .unwrap_or(false);
                    if !non_capturing && self.is_overlay_visible(index) {
                        self.focus_order_counter += 1;
                        shared.focus_order.set(self.focus_order_counter);
                        self.overlay_stack[index].focus_order = self.focus_order_counter;
                        self.set_focus(Some(component));
                    }
                }
            }
            if shared.focus_requested.get() {
                shared.focus_requested.set(false);
                let component = self.overlay_stack[index].component.clone();
                if self.is_overlay_visible(index) {
                    if !self.is_focused_component(&component) {
                        self.set_focus(Some(component));
                    }
                    self.focus_order_counter += 1;
                    shared.focus_order.set(self.focus_order_counter);
                    self.overlay_stack[index].focus_order = self.focus_order_counter;
                }
            }
            if shared.unfocus_requested.get() {
                shared.unfocus_requested.set(false);
                let component = self.overlay_stack[index].component.clone();
                if self.is_focused_component(&component) {
                    let top_visible = self.get_topmost_visible_overlay_index();
                    let next = match top_visible {
                        Some(top) if top != index => Some(self.overlay_stack[top].component.clone()),
                        _ => self.overlay_stack[index].pre_focus.clone(),
                    };
                    self.set_focus(next);
                }
            }
            index += 1;
        }
        for entry in self.overlay_stack.iter() {
            entry.shared.focused.set(self.is_focused_component(&entry.component));
        }
        if removed_any {
            if self.overlay_stack.is_empty() {
                self.terminal.hide_cursor();
            }
            self.sync_fullscreen_mouse_tracking();
            self.request_render(false);
        }
    }

    /// Hide the topmost overlay and restore previous focus.
    pub fn hide_overlay(&mut self) {
        let overlay = match self.overlay_stack.pop() {
            Some(overlay) => overlay,
            None => return,
        };
        if self.is_focused_component(&overlay.component) {
            // Find topmost visible overlay, or fall back to preFocus
            let top_visible = self
                .get_topmost_visible_overlay_index()
                .map(|top| self.overlay_stack[top].component.clone());
            let next = top_visible.or(overlay.pre_focus.clone());
            self.set_focus(next);
        }
        if self.overlay_stack.is_empty() {
            self.terminal.hide_cursor();
        }
        self.sync_fullscreen_mouse_tracking();
        self.request_render(false);
    }

    /// Check if there are any visible overlays
    pub fn has_overlay(&self) -> bool {
        (0..self.overlay_stack.len()).any(|index| self.is_overlay_visible(index))
    }

    fn is_focused_component(&self, component: &Rc<RefCell<dyn Component>>) -> bool {
        match &self.focused_component {
            Some(focused) => Rc::ptr_eq(focused, component),
            None => false,
        }
    }

    /// Check if an overlay entry is currently visible
    fn is_overlay_visible(&self, index: usize) -> bool {
        let entry = &self.overlay_stack[index];
        if entry.hidden {
            return false;
        }
        match entry.options.as_ref().and_then(|options| options.visible.clone()) {
            Some(visible) => visible(self.terminal.columns(), self.terminal.rows()),
            None => true,
        }
    }

    /// Find the topmost visible capturing overlay, if any
    fn get_topmost_visible_overlay_index(&self) -> Option<usize> {
        for index in (0..self.overlay_stack.len()).rev() {
            let entry = &self.overlay_stack[index];
            if entry
                .options
                .as_ref()
                .map(|options| options.non_capturing)
                .unwrap_or(false)
            {
                continue;
            }
            if self.is_overlay_visible(index) {
                return Some(index);
            }
        }
        None
    }

    fn should_enable_fullscreen_mouse_tracking(&self) -> bool {
        let fullscreen_mouse = match &self.fullscreen {
            Some(fullscreen) => fullscreen.mouse,
            None => false,
        };
        if !fullscreen_mouse {
            return false;
        }
        !(0..self.overlay_stack.len()).any(|index| {
            self.overlay_stack[index]
                .options
                .as_ref()
                .map(|options| options.suspend_fullscreen_mouse)
                .unwrap_or(false)
                && self.is_overlay_visible(index)
        })
    }

    fn is_fullscreen_overlay_focused(&self) -> bool {
        match &self.focused_component {
            Some(focused) => self
                .overlay_stack
                .iter()
                .any(|entry| Rc::ptr_eq(&entry.component, focused)),
            None => false,
        }
    }

    fn sync_fullscreen_mouse_tracking(&mut self) {
        let enabled = self.should_enable_fullscreen_mouse_tracking();
        if !enabled {
            self.stop_selection_auto_scroll();
            self.fullscreen_left_mouse_dragged = false;
            self.fullscreen_pressed_hyperlink = None;
            if let Some(fullscreen) = self.fullscreen.as_mut() {
                fullscreen.viewport.clear_selection();
            }
        } else if self.is_fullscreen_overlay_focused() {
            self.stop_selection_auto_scroll();
        }
        self.terminal.set_mouse_tracking(enabled);
    }

    pub fn invalidate(&mut self) {
        self.container.invalidate();
        for entry in self.overlay_stack.iter() {
            entry.component.borrow_mut().invalidate();
        }
    }

    pub fn start(&mut self) {
        self.stopped = false;
        // The terminal delivers input through a queue; `drain_input()` hands the
        // queued sequences to `handle_input`, which keeps the original ordering
        // without borrowing the TUI inside the callback.
        self.terminal.start(
            Box::new(|data| {
                PENDING_INPUT.with(|queue| queue.borrow_mut().push(data));
            }),
            Box::new(|| {
                PENDING_RESIZE.with(|flag| flag.set(true));
            }),
        );
        self.terminal.hide_cursor();
        self.query_cell_size();
        self.request_render(false);
    }

    /// Deliver queued terminal input and resize notifications to the TUI.
    pub fn drain_input(&mut self) {
        let pending: Vec<String> = PENDING_INPUT.with(|queue| std::mem::take(&mut *queue.borrow_mut()));
        for data in pending {
            self.handle_input(&data);
        }
        if PENDING_RESIZE.with(|flag| flag.replace(false)) {
            self.request_render(false);
        }
    }

    pub fn add_input_listener(&mut self, listener: InputListener) -> usize {
        let id = self.next_input_listener_id;
        self.next_input_listener_id += 1;
        self.input_listeners.push(id);
        self.input_listener_slots.push((id, listener));
        id
    }

    pub fn remove_input_listener(&mut self, id: usize) {
        self.input_listeners.retain(|existing| *existing != id);
        self.input_listener_slots.retain(|(existing, _)| *existing != id);
    }

    fn query_cell_size(&mut self) {
        // Only query if terminal supports images (cell size is only used for image rendering)
        if get_capabilities().images.is_none() {
            return;
        }
        // Query terminal for cell size in pixels: CSI 16 t
        // Response format: CSI 6 ; height ; width t
        self.terminal.write("\x1b[16t");
    }
}

thread_local! {
    static PENDING_INPUT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static PENDING_RESIZE: Cell<bool> = const { Cell::new(false) };
}
impl TUI {
    pub fn stop(&mut self, options: TuiStopOptions) {
        let preserve_alt_screen = options.preserve_alt_screen && self.terminal.alt_screen_active();
        let flush_fullscreen = options.flush_fullscreen || !preserve_alt_screen;
        self.exit_fullscreen(ExitFullscreenOptions {
            flush: flush_fullscreen,
            leave_alt_screen: !preserve_alt_screen,
        });
        self.stopped = true;
        if self.render_timer_active {
            self.render_timer_active = false;
        }
        // Move cursor to the end of the content to prevent overwriting/artifacts on exit
        if !preserve_alt_screen && !self.previous_lines.is_empty() {
            let target_row = self.previous_lines.len(); // Line after the last content
            let line_diff = target_row as i64 - self.hardware_cursor_row as i64;
            if line_diff > 0 {
                self.terminal.write(&format!("\x1b[{line_diff}B"));
            } else if line_diff < 0 {
                self.terminal.write(&format!("\x1b[{}A", -line_diff));
            }
            self.terminal.write("\r\n");
        }

        if preserve_alt_screen {
            self.terminal.hide_cursor();
        } else {
            self.terminal.show_cursor();
        }
        self.terminal.stop(crate::terminal::TerminalStopOptions {
            preserve_alt_screen,
        });
    }

    pub fn request_render(&mut self, force: bool) {
        if force {
            if let Some(fullscreen) = self.fullscreen.as_mut() {
                fullscreen.viewport.reset();
            }
            // Keep the previous frame metadata so the forced full repaint can
            // clean up only the visible viewport and avoid touching scrollback.
            self.previous_width = -1; // -1 triggers widthChanged, forcing a full clear
            self.cursor_row = 0;
            self.hardware_cursor_row = 0;
            self.max_lines_rendered = 0;
            self.render_timer_active = false;
            self.render_requested = true;
            return;
        }
        if self.render_requested {
            return;
        }
        self.render_requested = true;
    }

    /// Request a render that keeps the user anchored at their current scroll
    /// position. Normally, when content above the visible viewport changes, the
    /// renderer may fall back to a full screen redraw that replays the entire
    /// transcript from the top. For deliberate toggles (e.g. expanding all tool
    /// output) that is jarring: it scrolls to the top and reprints everything.
    /// This instead repaints only the visible viewport in place, leaving
    /// scrollback untouched.
    pub fn request_render_preserving_viewport(&mut self) {
        self.preserve_viewport_on_next_render = true;
        self.request_render(false);
    }

    /// True while a render is queued; the owner loop consumes it with
    /// [`TUI::run_pending_render`] after `MIN_RENDER_INTERVAL_MS`.
    pub fn render_requested(&self) -> bool {
        self.render_requested
    }

    /// Port of the `process.nextTick` + `scheduleRender` + `setTimeout` chain.
    /// Returns the delay in milliseconds the owner must wait before calling
    /// [`TUI::run_pending_render`], or `None` when nothing is queued.
    pub fn schedule_render(&mut self, now_ms: f64) -> Option<u64> {
        if self.stopped || self.render_timer_active || !self.render_requested {
            return None;
        }
        let elapsed = now_ms - self.last_render_at_ms;
        let delay = (Self::MIN_RENDER_INTERVAL_MS - elapsed).max(0.0) as u64;
        self.render_timer_active = true;
        Some(delay)
    }

    /// Port of the render timer body. Renders when work is queued.
    pub fn run_pending_render(&mut self, now_ms: f64) {
        self.render_timer_active = false;
        if self.stopped || !self.render_requested {
            return;
        }
        self.render_requested = false;
        self.last_render_at_ms = now_ms;
        self.do_render();
    }

    /// Port of `enterFullscreen`.
    pub fn enter_fullscreen(&mut self, options: FullscreenOptions) {
        if self.fullscreen.is_some() {
            return;
        }
        self.fullscreen_left_mouse_dragged = false;
        self.fullscreen_pressed_hyperlink = None;
        self.fullscreen = Some(FullscreenState {
            viewport: FullscreenViewport::new(),
            scroll: options.scroll,
            dock: options.dock,
            mouse: options.mouse,
            viewport_controls: options.viewport_controls,
            inline_state: InlineState {
                previous_lines: std::mem::take(&mut self.previous_lines),
                previous_kitty_image_ids: std::mem::take(&mut self.previous_kitty_image_ids),
                previous_width: self.previous_width,
                previous_height: self.previous_height,
                cursor_row: self.cursor_row,
                hardware_cursor_row: self.hardware_cursor_row,
                max_lines_rendered: self.max_lines_rendered,
                previous_viewport_top: self.previous_viewport_top,
            },
        });
        self.terminal.enter_alt_screen();
        self.terminal.hide_cursor();
        self.sync_fullscreen_mouse_tracking();
        self.request_render(false);
    }

    /// Leave fullscreen. The inline differ resumes against the entry snapshot,
    /// so content produced while fullscreen flows into native scrollback.
    pub fn exit_fullscreen(&mut self, options: ExitFullscreenOptions) {
        self.stop_selection_auto_scroll();
        let fullscreen = match self.fullscreen.take() {
            Some(fullscreen) => fullscreen,
            None => return,
        };
        let inline_state = fullscreen.inline_state;
        self.sync_fullscreen_mouse_tracking();
        if options.leave_alt_screen {
            self.terminal.leave_alt_screen();
        }
        self.previous_lines = inline_state.previous_lines;
        self.previous_kitty_image_ids = inline_state.previous_kitty_image_ids;
        self.previous_width = inline_state.previous_width;
        self.previous_height = inline_state.previous_height;
        self.cursor_row = inline_state.cursor_row;
        self.hardware_cursor_row = inline_state.hardware_cursor_row;
        self.max_lines_rendered = inline_state.max_lines_rendered;
        self.previous_viewport_top = inline_state.previous_viewport_top;
        // synchronous so the flush also happens on shutdown, where a scheduled
        // render never fires
        if options.flush && !self.stopped {
            self.do_render();
        }
    }

    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen.is_some()
    }

    /// Scroll the fullscreen transcript window (negative = up).
    pub fn scroll_by(&mut self, lines: i64) {
        match self.fullscreen.as_mut() {
            Some(fullscreen) => fullscreen.viewport.scroll_by(lines),
            None => return,
        }
        self.request_render(false);
    }

    pub fn scroll_to_top(&mut self) {
        match self.fullscreen.as_mut() {
            Some(fullscreen) => fullscreen.viewport.scroll_to_top(),
            None => return,
        }
        self.request_render(false);
    }

    pub fn scroll_to_bottom(&mut self) {
        match self.fullscreen.as_mut() {
            Some(fullscreen) => fullscreen.viewport.scroll_to_bottom(),
            None => return,
        }
        self.request_render(false);
    }

    /// Scroll state of the fullscreen window, or null when not fullscreen.
    pub fn get_scroll_info(&self) -> Option<ScrollInfo> {
        self.fullscreen
            .as_ref()
            .map(|fullscreen| fullscreen.viewport.scroll_info())
    }

    // Terminals gate their native link handling while mouse reporting is active
    // (Ghostty only refreshes link hover when reporting is off or shift is held),
    // so clicks the TUI consumes must open OSC 8 hyperlinks itself.
    fn open_hyperlink(&mut self, url: &str) {
        if url.chars().any(|ch| ch.is_control()) {
            return;
        }
        let parsed = match url::Url::parse(url) {
            Ok(parsed) => parsed,
            Err(_) => return,
        };
        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" && scheme != "file" {
            return;
        }
        let href = parsed.to_string();
        if let Some(handler) = self.on_open_url.as_mut() {
            handler(&href);
            return;
        }
        let mut command = if cfg!(target_os = "macos") {
            std::process::Command::new("open")
        } else if cfg!(windows) {
            let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
            let mut command = std::process::Command::new(
                std::path::Path::new(&system_root).join("System32").join("rundll32.exe"),
            );
            command.arg("url.dll,FileProtocolHandler");
            command
        } else {
            std::process::Command::new("xdg-open")
        };
        let _ = command
            .arg(&href)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    fn copy_selection(&mut self, text: &str) {
        if let Some(handler) = self.on_copy.as_mut() {
            handler(text);
            return;
        }
        // fallback: OSC 52 works locally, over SSH, and through tmux (set-clipboard)
        use base64::Engine;
        let base64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        self.terminal.write(&format!("\x1b]52;c;{base64}\x07"));
    }

    fn update_selection_auto_scroll(&mut self, screen_row: i64, screen_column: i64) {
        let direction = self
            .fullscreen
            .as_ref()
            .and_then(|fullscreen| fullscreen.viewport.selection_auto_scroll_direction(screen_row));
        self.selection_auto_scroll_row = screen_row;
        self.selection_auto_scroll_column = screen_column;
        if direction == self.selection_auto_scroll_direction && self.selection_auto_scroll_timer_active {
            return;
        }
        self.stop_selection_auto_scroll();
        let direction = match direction {
            Some(direction) => direction,
            None => return,
        };
        self.selection_auto_scroll_direction = Some(direction);
        self.selection_auto_scroll_timer_active = true;
    }

    /// Port of the selection auto-scroll timer body. Returns the next delay when
    /// the timer reschedules itself, mirroring `setTimeout(..., INTERVAL)`.
    pub fn run_selection_auto_scroll(&mut self) -> Option<u64> {
        self.selection_auto_scroll_timer_active = false;
        let direction = match self.selection_auto_scroll_direction {
            Some(direction) => direction,
            None => return None,
        };
        let row = self.selection_auto_scroll_row;
        let column = self.selection_auto_scroll_column;
        let can_continue = match self.fullscreen.as_mut() {
            Some(fullscreen) => {
                fullscreen.viewport.selection_auto_scroll_direction(row) == Some(direction)
                    && fullscreen.viewport.scroll_selection(direction, column)
            }
            None => false,
        };
        if self.is_fullscreen_overlay_focused() || !can_continue {
            self.stop_selection_auto_scroll();
            return None;
        }
        self.request_render(false);
        self.selection_auto_scroll_timer_active = true;
        Some(Self::SELECTION_AUTO_SCROLL_INTERVAL_MS)
    }

    fn stop_selection_auto_scroll(&mut self) {
        self.selection_auto_scroll_timer_active = false;
        self.selection_auto_scroll_direction = None;
    }
}
impl TUI {
    fn handle_input(&mut self, data: &str) {
        self.sync_overlays();
        let mut data = data.to_string();
        if !self.input_listeners.is_empty() {
            let mut current = data.clone();
            for id in self.input_listeners.clone() {
                let result = match self.input_listener_slots.iter().find(|(slot_id, _)| *slot_id == id) {
                    Some((_, listener)) => listener(&current),
                    None => continue,
                };
                if result.consume {
                    return;
                }
                if let Some(next) = result.data {
                    current = next;
                }
            }
            if current.is_empty() {
                return;
            }
            data = current;
        }

        // Consume terminal cell size responses without blocking unrelated input.
        if self.consume_cell_size_response(&data) {
            return;
        }

        // Global debug key handler (Shift+Ctrl+D)
        if matches_key(&data, "shift+ctrl+d") {
            if let Some(handler) = self.on_debug.as_mut() {
                handler();
                return;
            }
        }

        if self.fullscreen.is_some() && self.handle_fullscreen_input(&data) {
            return;
        }

        // If focused component is an overlay, verify it's still visible
        // (visibility can change due to terminal resize or visible() callback)
        let focused_overlay_index = match &self.focused_component {
            Some(focused) => self
                .overlay_stack
                .iter()
                .position(|entry| Rc::ptr_eq(&entry.component, focused)),
            None => None,
        };
        if let Some(index) = focused_overlay_index {
            if !self.is_overlay_visible(index) {
                // Focused overlay is no longer visible, redirect to topmost visible overlay
                let top_visible = self
                    .get_topmost_visible_overlay_index()
                    .map(|top| self.overlay_stack[top].component.clone());
                match top_visible {
                    Some(component) => self.set_focus(Some(component)),
                    // No visible overlays, restore to preFocus
                    None => {
                        let pre_focus = self.overlay_stack[index].pre_focus.clone();
                        self.set_focus(pre_focus);
                    }
                }
            }
        }

        // Pass input to focused component (including Ctrl+C)
        // The focused component can decide how to handle Ctrl+C
        let focused = self.focused_component.clone();
        if let Some(component) = focused {
            // Filter out key release events unless component opts in
            let wants_key_release = component.borrow().wants_key_release();
            if is_key_release(&data) && !wants_key_release {
                return;
            }
            component.borrow_mut().handle_input(&data);
            self.request_render(false);
        }
    }

    /// Mouse reports are always consumed (nothing downstream understands them);
    /// viewport keys are skipped while an overlay has focus so selectors keep
    /// their own pageUp/pageDown.
    fn handle_fullscreen_input(&mut self, data: &str) -> bool {
        let overlay_focused = self.is_fullscreen_overlay_focused();

        if is_mouse_sequence(data) {
            // consumed even when disabled - mouse reports are garbage downstream
            let event = if self.terminal.mouse_tracking_active() {
                parse_sgr_mouse_event(data)
            } else {
                None
            };
            let left_release_was_drag = match &event {
                Some(event) if event.button == MOUSE_BUTTON_LEFT && !event.press => {
                    self.fullscreen_left_mouse_dragged
                }
                _ => false,
            };
            if let Some(event) = &event {
                if event.button == MOUSE_BUTTON_LEFT && event.press {
                    self.fullscreen_left_mouse_dragged = event.motion;
                    if !event.motion {
                        let (row, col) = (event.y as i64 - 1, event.x as i64 - 1);
                        self.fullscreen_pressed_hyperlink = self
                            .fullscreen
                            .as_ref()
                            .and_then(|fullscreen| fullscreen.viewport.hyperlink_at(row, col));
                    }
                }
            }
            if let Some(event) = &event {
                if !overlay_focused {
                    if is_wheel_up(event) {
                        self.stop_selection_auto_scroll();
                        self.scroll_by(-Self::WHEEL_SCROLL_LINES);
                    } else if is_wheel_down(event) {
                        self.stop_selection_auto_scroll();
                        self.scroll_by(Self::WHEEL_SCROLL_LINES);
                    } else if event.button == MOUSE_BUTTON_LEFT && event.press && !event.motion {
                        self.stop_selection_auto_scroll();
                        let (row, col) = (event.y as i64 - 1, event.x as i64 - 1);
                        let began = match self.fullscreen.as_mut() {
                            Some(fullscreen) => {
                                fullscreen.viewport.begin_selection(row, col)
                                    || fullscreen.viewport.begin_frame_selection(row, col)
                            }
                            None => false,
                        };
                        let _ = began;
                        self.request_render(false);
                    } else if event.button == MOUSE_BUTTON_LEFT && event.press && event.motion {
                        let (row, col) = (event.y as i64 - 1, event.x as i64 - 1);
                        if let Some(fullscreen) = self.fullscreen.as_mut() {
                            fullscreen.viewport.extend_active_selection(row, col);
                        }
                        self.update_selection_auto_scroll(row, col);
                        self.request_render(false);
                    } else if !event.press {
                        let has_selection = self
                            .fullscreen
                            .as_ref()
                            .map(|fullscreen| fullscreen.viewport.has_selection())
                            .unwrap_or(false);
                        if has_selection {
                            self.stop_selection_auto_scroll();
                            let text = self
                                .fullscreen
                                .as_mut()
                                .and_then(|fullscreen| fullscreen.viewport.end_active_selection());
                            if let Some(text) = text {
                                self.copy_selection(&text);
                            }
                            self.request_render(false);
                        } else {
                            self.stop_selection_auto_scroll();
                            if let Some(fullscreen) = self.fullscreen.as_mut() {
                                fullscreen.viewport.clear_selection();
                            }
                            if event.button == MOUSE_BUTTON_LEFT && !event.motion && !left_release_was_drag {
                                let (row, col) = (event.y as i64 - 1, event.x as i64 - 1);
                                let url = self.fullscreen_pressed_hyperlink.clone().or_else(|| {
                                    self.fullscreen.as_ref().and_then(|fullscreen| {
                                        fullscreen.viewport.hyperlink_at(row, col)
                                    })
                                });
                                if let Some(url) = url {
                                    self.open_hyperlink(&url);
                                }
                            }
                        }
                    }
                } else {
                    self.stop_selection_auto_scroll();
                    if event.button == MOUSE_BUTTON_LEFT && event.press && !event.motion {
                        let (row, col) = (event.y as i64 - 1, event.x as i64 - 1);
                        if let Some(fullscreen) = self.fullscreen.as_mut() {
                            if !fullscreen.viewport.begin_frame_selection(row, col) {
                                fullscreen.viewport.begin_selection(row, col);
                            }
                        }
                        self.request_render(false);
                    } else if event.button == MOUSE_BUTTON_LEFT && event.press && event.motion {
                        let (row, col) = (event.y as i64 - 1, event.x as i64 - 1);
                        if let Some(fullscreen) = self.fullscreen.as_mut() {
                            fullscreen.viewport.extend_active_selection(row, col);
                        }
                        self.request_render(false);
                    } else if !event.press {
                        let has_selection = self
                            .fullscreen
                            .as_ref()
                            .map(|fullscreen| fullscreen.viewport.has_selection())
                            .unwrap_or(false);
                        if has_selection {
                            let text = self
                                .fullscreen
                                .as_mut()
                                .and_then(|fullscreen| fullscreen.viewport.end_active_selection());
                            if let Some(text) = text {
                                self.copy_selection(&text);
                            }
                            self.request_render(false);
                        } else {
                            if let Some(fullscreen) = self.fullscreen.as_mut() {
                                fullscreen.viewport.clear_selection();
                            }
                            if event.button == MOUSE_BUTTON_LEFT && !event.motion && !left_release_was_drag {
                                let (row, col) = (event.y as i64 - 1, event.x as i64 - 1);
                                let url = self.fullscreen_pressed_hyperlink.clone().or_else(|| {
                                    self.fullscreen.as_ref().and_then(|fullscreen| {
                                        fullscreen.viewport.hyperlink_at(row, col)
                                    })
                                });
                                if let Some(url) = url {
                                    self.open_hyperlink(&url);
                                }
                            }
                        }
                    }
                }
            }
            if let Some(event) = &event {
                if event.button == MOUSE_BUTTON_LEFT && !event.press {
                    self.fullscreen_left_mouse_dragged = false;
                    self.fullscreen_pressed_hyperlink = None;
                }
            }
            return true;
        }

        self.stop_selection_auto_scroll();

        let viewport_controls = match &self.fullscreen {
            Some(fullscreen) => fullscreen.viewport_controls,
            None => return false,
        };
        if overlay_focused || !viewport_controls {
            return false;
        }

        let keybindings = get_keybindings();
        if keybindings.matches(data, "tui.viewport.pageUp") {
            let page = self.page_size();
            self.scroll_by(-page);
            return true;
        }
        if keybindings.matches(data, "tui.viewport.pageDown") {
            let page = self.page_size();
            self.scroll_by(page);
            return true;
        }
        if keybindings.matches(data, "tui.viewport.top") {
            self.scroll_to_top();
            return true;
        }
        if keybindings.matches(data, "tui.viewport.follow") {
            self.scroll_to_bottom();
            return true;
        }
        false
    }

    fn page_size(&self) -> i64 {
        match &self.fullscreen {
            Some(fullscreen) => fullscreen.viewport.page_size() as i64,
            None => 0,
        }
    }

    /// Port of `consumeCellSizeResponse`.
    fn consume_cell_size_response(&mut self, data: &str) -> bool {
        // Response format: ESC [ 6 ; height ; width t
        let caps = match CELL_SIZE_RESPONSE.captures(data) {
            Some(caps) => caps,
            None => return false,
        };

        let height_px: i64 = caps.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        let width_px: i64 = caps.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        if height_px <= 0 || width_px <= 0 {
            return true;
        }

        set_cell_dimensions(width_px as u32, height_px as u32);
        // Invalidate all components so images re-render with correct dimensions.
        self.invalidate();
        self.request_render(false);
        true
    }
}

static CELL_SIZE_RESPONSE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\x1b\[6;(\d+);(\d+)t$").unwrap());
impl TUI {
    /// Resolve overlay layout from options.
    fn resolve_overlay_layout(
        &self,
        options: Option<&OverlayOptions>,
        overlay_height: i64,
        term_width: i64,
        term_height: i64,
    ) -> OverlayLayout {
        let opt = match options {
            Some(options) => options,
            None => &DEFAULT_OVERLAY_OPTIONS,
        };

        // Parse margin (clamp to non-negative)
        let margin = match opt.margin {
            Some(MarginValue::Number(value)) => OverlayMargin {
                top: value,
                right: value,
                bottom: value,
                left: value,
            },
            Some(MarginValue::Sides(sides)) => sides,
            None => OverlayMargin::default(),
        };
        let margin_top = margin.top.max(0);
        let margin_right = margin.right.max(0);
        let margin_bottom = margin.bottom.max(0);
        let margin_left = margin.left.max(0);

        // Available space after margins
        let avail_width = (term_width - margin_left - margin_right).max(1);
        let avail_height = (term_height - margin_top - margin_bottom).max(1);

        // === Resolve width ===
        let mut width = parse_size_value(opt.width.as_ref(), term_width as f64)
            .map(|value| value as i64)
            .unwrap_or_else(|| 80.min(avail_width));
        // Apply minWidth
        if let Some(min_width) = opt.min_width {
            width = width.max(min_width as i64);
        }
        // Clamp to available space
        width = width.max(1).min(avail_width);

        // === Resolve maxHeight ===
        let mut max_height = parse_size_value(opt.max_height.as_ref(), term_height as f64).map(|value| value as i64);
        // Clamp to available space
        if let Some(height) = max_height {
            max_height = Some(height.max(1).min(avail_height));
        }

        // Effective overlay height (may be clamped by maxHeight)
        let effective_height = match max_height {
            Some(height) => overlay_height.min(height),
            None => overlay_height,
        };

        // === Resolve position ===
        let row: i64;
        let col: i64;

        match opt.row.as_ref() {
            Some(SizeValue::Percent(text)) => match PERCENT_PATTERN.captures(text) {
                Some(caps) => {
                    // Percentage: 0% = top, 100% = bottom (overlay stays within bounds)
                    let max_row = (avail_height - effective_height).max(0);
                    let percent: f64 = caps.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(0.0);
                    row = margin_top + ((max_row as f64 * percent / 100.0).floor() as i64);
                }
                // Invalid format, fall back to center
                None => row = resolve_anchor_row(OverlayAnchor::Center, effective_height, avail_height, margin_top),
            },
            // Absolute row position
            Some(SizeValue::Number(value)) => row = *value as i64,
            // Anchor-based (default: center)
            None => {
                let anchor = opt.anchor.unwrap_or_default();
                row = resolve_anchor_row(anchor, effective_height, avail_height, margin_top);
            }
        }

        match opt.col.as_ref() {
            Some(SizeValue::Percent(text)) => match PERCENT_PATTERN.captures(text) {
                Some(caps) => {
                    // Percentage: 0% = left, 100% = right (overlay stays within bounds)
                    let max_col = (avail_width - width).max(0);
                    let percent: f64 = caps.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(0.0);
                    col = margin_left + ((max_col as f64 * percent / 100.0).floor() as i64);
                }
                // Invalid format, fall back to center
                None => col = resolve_anchor_col(OverlayAnchor::Center, width, avail_width, margin_left),
            },
            // Absolute column position
            Some(SizeValue::Number(value)) => col = *value as i64,
            // Anchor-based (default: center)
            None => {
                let anchor = opt.anchor.unwrap_or_default();
                col = resolve_anchor_col(anchor, width, avail_width, margin_left);
            }
        }

        let mut row = row;
        let mut col = col;

        // Apply offsets
        if let Some(offset_y) = opt.offset_y {
            row += offset_y;
        }
        if let Some(offset_x) = opt.offset_x {
            col += offset_x;
        }

        // Clamp to terminal bounds (respecting margins)
        row = row.max(margin_top).min(term_height - margin_bottom - effective_height);
        col = col.max(margin_left).min(term_width - margin_right - width);

        OverlayLayout {
            width,
            row,
            col,
            max_height,
        }
    }

    /// Composite all overlays into content lines (sorted by focusOrder, higher = on top).
    fn composite_overlays(&mut self, lines: &[String], term_width: usize, term_height: usize) -> Vec<String> {
        if self.overlay_stack.is_empty() {
            return lines.to_vec();
        }
        let mut result: Vec<String> = lines.to_vec();
        let mut overlay_selection_regions: Vec<FrameSelectionRegion> = self.overlay_selection_regions.clone();

        // Pre-render all visible overlays and calculate positions
        let mut rendered: Vec<OverlayRender> = Vec::new();
        let mut min_lines_needed = result.len();

        let mut visible_entries: Vec<usize> = (0..self.overlay_stack.len())
            .filter(|index| self.is_overlay_visible(*index))
            .collect();
        visible_entries.sort_by_key(|index| self.overlay_stack[*index].focus_order);

        for index in visible_entries {
            let component = self.overlay_stack[index].component.clone();
            let options = self.overlay_stack[index].options.clone();
            let scrollback = options.as_ref().map(|o| o.scrollback).unwrap_or(false);
            let mut above_marker: Option<AboveMarker> = None;
            if let Some(marker) = options.as_ref().and_then(|o| o.above_marker.clone()) {
                for line in (0..result.len()).rev() {
                    let marker_index = match result[line].find(&marker) {
                        Some(marker_index) => marker_index,
                        None => continue,
                    };
                    let prefix: String = result[line].chars().take(marker_index).collect();
                    above_marker = Some(AboveMarker {
                        line,
                        col: visible_width(&prefix),
                        offset_y: options.as_ref().and_then(|o| o.offset_y).unwrap_or(0),
                    });
                    let suffix: String = result[line].chars().skip(marker_index + marker.chars().count()).collect();
                    result[line] = format!("{prefix}{suffix}");
                    break;
                }
                if above_marker.is_none() {
                    continue;
                }
            }

            // Get layout with height=0 first to determine width and maxHeight
            // (width and maxHeight don't depend on overlay height)
            let layout = self.resolve_overlay_layout(options.as_ref(), 0, term_width as i64, term_height as i64);

            // Render component at calculated width
            let mut overlay_lines = component.borrow_mut().render(layout.width as f64);

            // Apply maxHeight if specified
            if let Some(max_height) = layout.max_height {
                if overlay_lines.len() as i64 > max_height {
                    overlay_lines.truncate(max_height.max(0) as usize);
                }
            }

            // Get final row/col with actual overlay height
            let final_layout = self.resolve_overlay_layout(
                options.as_ref(),
                overlay_lines.len() as i64,
                term_width as i64,
                term_height as i64,
            );

            rendered.push(OverlayRender {
                component,
                overlay_lines,
                row: final_layout.row,
                col: final_layout.col,
                width: layout.width.max(0) as usize,
                scrollback,
                above_marker,
            });
            if above_marker.is_none() {
                min_lines_needed = min_lines_needed.max(final_layout.row.max(0) as usize + rendered.last().unwrap().overlay_lines.len());
            }
        }

        // Pad to at least terminal height so overlays have screen-relative positions.
        // Excludes maxLinesRendered: the historical high-water mark caused
        // self-reinforcing inflation that pushed content into scrollback on
        // terminal widen.
        let working_height = result.len().max(term_height).max(min_lines_needed);

        // Extend result with empty lines if content is too short for overlay
        // placement or working area
        while result.len() < working_height {
            result.push(String::new());
        }

        let viewport_start = working_height.saturating_sub(term_height);

        // Composite each overlay
        for rendered_overlay in rendered.iter_mut() {
            let component = rendered_overlay.component.clone();
            let width = rendered_overlay.width;
            let scrollback = rendered_overlay.scrollback;
            let above_marker = rendered_overlay.above_marker;
            let mut overlay_lines = rendered_overlay.overlay_lines.clone();
            let mut row = rendered_overlay.row;
            let mut col = rendered_overlay.col;
            if let Some(marker) = above_marker {
                let marker_row = (marker.line as i64 - viewport_start as i64 + marker.offset_y).max(1);
                if marker_row >= term_height as i64 {
                    continue;
                }
                let marker_row = marker_row as usize;
                while overlay_lines.len() > marker_row && strip_ansi(&overlay_lines[0]).trim().is_empty() {
                    overlay_lines.remove(0);
                }
                while overlay_lines.len() > marker_row
                    && strip_ansi(overlay_lines.last().unwrap()).trim().is_empty()
                {
                    overlay_lines.pop();
                }
                if overlay_lines.len() > marker_row {
                    overlay_lines = overlay_lines.split_off(overlay_lines.len() - marker_row);
                }
                row = marker_row as i64 - overlay_lines.len() as i64;
                col = marker.col.min(term_width.saturating_sub(width)) as i64;
                col = col.max(0);
            }

            let overlay_start = if scrollback && above_marker.is_none() {
                working_height.saturating_sub((row.max(0) as usize) + overlay_lines.len())
            } else {
                viewport_start
            };
            for i in 0..overlay_lines.len() {
                let idx = overlay_start as i64 + row + i as i64;
                if idx >= 0 && (idx as usize) < result.len() {
                    // Defensive: truncate overlay line to declared width before
                    // compositing (components should already respect width, but
                    // this ensures it)
                    let truncated_overlay_line = if visible_width(&overlay_lines[i]) > width {
                        slice_by_column(&overlay_lines[i], 0, width, true)
                    } else {
                        overlay_lines[i].clone()
                    };
                    let base = result[idx as usize].clone();
                    result[idx as usize] = self.composite_line_at(&base, &truncated_overlay_line, col, width as i64, term_width as i64);
                    let idx_usize = idx as usize;
                    let cover_start = col.max(0) as usize;
                    let cover_end = (col + width as i64).max(0) as usize;
                    subtract_selection_coverage(&mut overlay_selection_regions, idx_usize, cover_start, cover_end);
                    let span = if self.is_focused_component(&component) {
                        self.selectable_span(&truncated_overlay_line, width)
                    } else {
                        None
                    };
                    if let Some((from, to)) = span {
                        overlay_selection_regions.push(FrameSelectionRegion {
                            line: idx_usize,
                            col: col.max(0) as usize + from,
                            width: to - from,
                        });
                    }
                }
            }
        }

        self.overlay_selection_regions = overlay_selection_regions;
        result
    }

    fn selectable_span(&self, line: &str, max_width: usize) -> Option<(usize, usize)> {
        visible_content_span(line, max_width as f64)
    }

    fn create_dock_selection_regions(
        &self,
        frame: &[String],
        transcript_window_height: usize,
        width: usize,
    ) -> Vec<FrameSelectionRegion> {
        let mut regions: Vec<FrameSelectionRegion> = Vec::new();
        for row in transcript_window_height..frame.len() {
            let span = self.selectable_span(frame.get(row).cloned().unwrap_or_default().as_str(), width);
            if let Some((from, to)) = span {
                regions.push(FrameSelectionRegion {
                    line: row,
                    col: from,
                    width: to - from,
                });
            }
        }
        regions
    }
}

fn resolve_anchor_row(anchor: OverlayAnchor, height: i64, avail_height: i64, margin_top: i64) -> i64 {
    match anchor {
        OverlayAnchor::TopLeft | OverlayAnchor::TopCenter | OverlayAnchor::TopRight => margin_top,
        OverlayAnchor::BottomLeft | OverlayAnchor::BottomCenter | OverlayAnchor::BottomRight => {
            margin_top + avail_height - height
        }
        OverlayAnchor::LeftCenter | OverlayAnchor::Center | OverlayAnchor::RightCenter => {
            margin_top + (avail_height - height) / 2
        }
    }
}

fn resolve_anchor_col(anchor: OverlayAnchor, width: i64, avail_width: i64, margin_left: i64) -> i64 {
    match anchor {
        OverlayAnchor::TopLeft | OverlayAnchor::LeftCenter | OverlayAnchor::BottomLeft => margin_left,
        OverlayAnchor::TopRight | OverlayAnchor::RightCenter | OverlayAnchor::BottomRight => {
            margin_left + avail_width - width
        }
        OverlayAnchor::TopCenter | OverlayAnchor::Center | OverlayAnchor::BottomCenter => {
            margin_left + (avail_width - width) / 2
        }
    }
}

fn subtract_selection_coverage(
    regions: &mut Vec<FrameSelectionRegion>,
    line: usize,
    cover_start: usize,
    cover_end: usize,
) {
    for i in (0..regions.len()).rev() {
        let region = regions[i];
        if region.line != line {
            continue;
        }
        let region_start = region.col;
        let region_end = region.col + region.width;
        if cover_end <= region_start || cover_start >= region_end {
            continue;
        }

        let mut replacements: Vec<FrameSelectionRegion> = Vec::new();
        if region_start < cover_start {
            replacements.push(FrameSelectionRegion {
                line,
                col: region_start,
                width: cover_start - region_start,
            });
        }
        if cover_end < region_end {
            replacements.push(FrameSelectionRegion {
                line,
                col: cover_end,
                width: region_end - cover_end,
            });
        }
        regions.splice(i..i + 1, replacements);
    }
}

thread_local! {
    static DEFAULT_OVERLAY_OPTIONS: OverlayOptions = OverlayOptions::default();
}

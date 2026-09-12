//! Port of packages/coding-agent/src/modes/interactive/components/armin.ts
//!
//! Armin says hi! A fun easter egg with animated XBM art.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use pi_tui::tui::{Component, TUI};

use super::super::theme::theme::theme;

// XBM image: 31x36 pixels, LSB first, 1=background, 0=foreground
const WIDTH: usize = 31;
const HEIGHT: usize = 36;
const BITS: [u8; 144] = [
    0xff, 0xff, 0xff, 0x7f, 0xff, 0xf0, 0xff, 0x7f, 0xff, 0xed, 0xff, 0x7f, 0xff, 0xdb, 0xff, 0x7f,
    0xff, 0xb7, 0xff, 0x7f, 0xff, 0x77, 0xfe, 0x7f, 0x3f, 0xf8, 0xfe, 0x7f, 0xdf, 0xff, 0xfe, 0x7f,
    0xdf, 0x3f, 0xfc, 0x7f, 0x9f, 0xc3, 0xfb, 0x7f, 0x6f, 0xfc, 0xf4, 0x7f, 0xf7, 0x0f, 0xf7, 0x7f,
    0xf7, 0xff, 0xf7, 0x7f, 0xf7, 0xff, 0xe3, 0x7f, 0xf7, 0x07, 0xe8, 0x7f, 0xef, 0xf8, 0x67, 0x70,
    0x0f, 0xff, 0xbb, 0x6f, 0xf1, 0x00, 0xd0, 0x5b, 0xfd, 0x3f, 0xec, 0x53, 0xc1, 0xff, 0xef, 0x57,
    0x9f, 0xfd, 0xee, 0x5f, 0x9f, 0xfc, 0xae, 0x5f, 0x1f, 0x78, 0xac, 0x5f, 0x3f, 0x00, 0x50, 0x6c,
    0x7f, 0x00, 0xdc, 0x77, 0xff, 0xc0, 0x3f, 0x78, 0xff, 0x01, 0xf8, 0x7f, 0xff, 0x03, 0x9c, 0x78,
    0xff, 0x07, 0x8c, 0x7c, 0xff, 0x0f, 0xce, 0x78, 0xff, 0xff, 0xcf, 0x7f, 0xff, 0xff, 0xcf, 0x78,
    0xff, 0xff, 0xdf, 0x78, 0xff, 0xff, 0xdf, 0x7d, 0xff, 0xff, 0x3f, 0x7e, 0xff, 0xff, 0xff, 0x7f,
];

const BYTES_PER_ROW: usize = (WIDTH + 7) / 8;
/// Half-block rendering
const DISPLAY_HEIGHT: usize = (HEIGHT + 1) / 2;

/// `type Effect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Typewriter,
    Scanline,
    Rain,
    Fade,
    Crt,
    Glitch,
    Dissolve,
}

/// `EFFECTS`.
pub const EFFECTS: [Effect; 7] = [
    Effect::Typewriter,
    Effect::Scanline,
    Effect::Rain,
    Effect::Fade,
    Effect::Crt,
    Effect::Glitch,
    Effect::Dissolve,
];

/// Get pixel at (x, y): true = foreground, false = background
pub fn get_pixel(x: usize, y: usize) -> bool {
    if y >= HEIGHT {
        return false;
    }
    let byte_index = y * BYTES_PER_ROW + x / 8;
    let bit_index = x % 8;
    ((BITS[byte_index] >> bit_index) & 1) == 0
}

/// Get the character for a cell (2 vertical pixels packed)
pub fn get_char(x: usize, row: usize) -> &'static str {
    let upper = get_pixel(x, row * 2);
    let lower = get_pixel(x, row * 2 + 1);
    if upper && lower {
        "█"
    } else if upper {
        "▀"
    } else if lower {
        "▄"
    } else {
        " "
    }
}

/// Build the final image grid
pub fn build_final_grid() -> Vec<Vec<String>> {
    let mut grid: Vec<Vec<String>> = Vec::new();
    for row in 0..DISPLAY_HEIGHT {
        let mut line: Vec<String> = Vec::new();
        for x in 0..WIDTH {
            line.push(get_char(x, row).to_string());
        }
        grid.push(line);
    }
    grid
}

/// Port of the per-effect mutable state (`this.effectState`).
enum EffectState {
    Typewriter {
        pos: usize,
    },
    Scanline {
        row: usize,
    },
    Rain {
        drops: Vec<Drop>,
    },
    /// Fade and dissolve share the shuffled-position shape.
    Positions {
        positions: Vec<(usize, usize)>,
        idx: usize,
    },
    Crt {
        expansion: usize,
    },
    Glitch {
        phase: usize,
        glitch_frames: usize,
    },
}

#[derive(Clone, Copy)]
struct Drop {
    y: i64,
    settled: usize,
}

/// Deterministic `Math.random()` replacement. The TypeScript uses the host RNG;
/// the port uses a small xorshift so runs are reproducible in tests while the
/// distribution shape used by each effect stays the same.
#[derive(Clone, Copy)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// `Math.random()` in [0, 1).
    fn next(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    /// `Math.floor(Math.random() * n)`.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() * n as f64).floor() as usize
        }
    }
}

/// Port of `ArminComponent`.
pub struct ArminComponent {
    ui: Rc<RefCell<TUI>>,
    /// Port of `interval: ReturnType<typeof setInterval> | null`.
    interval_task: Option<tokio::task::JoinHandle<()>>,
    running: Arc<AtomicBool>,
    effect: Effect,
    final_grid: Vec<Vec<String>>,
    current_grid: Vec<Vec<String>>,
    effect_state: EffectState,
    cached_lines: Vec<String>,
    cached_width: f64,
    /// Port of `gridVersion`; the timer task advances it.
    grid_version: Arc<AtomicI64>,
    cached_version: i64,
    /// Port of `tickEffect()`'s `done` flag, written by the timer task.
    finished: Arc<AtomicBool>,
    rng: Rng,
}

impl ArminComponent {
    pub fn new(ui: Rc<RefCell<TUI>>) -> Self {
        let mut rng = Rng::new(0x9e3779b97f4a7c15);
        let effect = EFFECTS[rng.below(EFFECTS.len())];
        let final_grid = build_final_grid();
        let mut component = Self {
            ui,
            interval_task: None,
            running: Arc::new(AtomicBool::new(false)),
            effect,
            final_grid,
            current_grid: Vec::new(),
            effect_state: EffectState::Crt { expansion: 0 },
            cached_lines: Vec::new(),
            cached_width: -1.0,
            grid_version: Arc::new(AtomicI64::new(0)),
            cached_version: -1,
            finished: Arc::new(AtomicBool::new(false)),
            rng,
        };
        component.current_grid = component.create_empty_grid();
        component.init_effect();
        component.start_animation();
        component
    }

    /// `invalidate()` - resets the cached width.
    pub fn invalidate_cache(&mut self) {
        self.cached_width = -1.0;
    }

    /// Port of `dispose()`.
    pub fn dispose(&mut self) {
        self.stop_animation();
    }

    fn create_empty_grid(&self) -> Vec<Vec<String>> {
        vec![vec![" ".to_string(); WIDTH]; DISPLAY_HEIGHT]
    }

    /// Port of `initEffect()`.
    fn init_effect(&mut self) {
        match self.effect {
            Effect::Typewriter => self.effect_state = EffectState::Typewriter { pos: 0 },
            Effect::Scanline => self.effect_state = EffectState::Scanline { row: 0 },
            Effect::Rain => {
                // Track falling position for each column
                let mut drops: Vec<Drop> = Vec::with_capacity(WIDTH);
                for _ in 0..WIDTH {
                    drops.push(Drop {
                        y: -(self.rng.below(DISPLAY_HEIGHT * 2) as i64),
                        settled: 0,
                    });
                }
                self.effect_state = EffectState::Rain { drops };
            }
            Effect::Fade => {
                self.effect_state = EffectState::Positions {
                    positions: self.shuffled_positions(),
                    idx: 0,
                };
            }
            Effect::Crt => self.effect_state = EffectState::Crt { expansion: 0 },
            Effect::Glitch => {
                self.effect_state = EffectState::Glitch {
                    phase: 0,
                    glitch_frames: 8,
                }
            }
            Effect::Dissolve => {
                // Start with random noise
                let chars = [" ", "░", "▒", "▓", "█", "▀", "▄"];
                let mut grid: Vec<Vec<String>> = Vec::with_capacity(DISPLAY_HEIGHT);
                for _ in 0..DISPLAY_HEIGHT {
                    let mut row: Vec<String> = Vec::with_capacity(WIDTH);
                    for _ in 0..WIDTH {
                        let index = self.rng.below(chars.len());
                        row.push(chars[index].to_string());
                    }
                    grid.push(row);
                }
                self.current_grid = grid;
                self.effect_state = EffectState::Positions {
                    positions: self.shuffled_positions(),
                    idx: 0,
                };
            }
        }
    }

    /// All pixel positions, Fisher-Yates shuffled.
    fn shuffled_positions(&mut self) -> Vec<(usize, usize)> {
        let mut positions: Vec<(usize, usize)> = Vec::new();
        for row in 0..DISPLAY_HEIGHT {
            for x in 0..WIDTH {
                positions.push((row, x));
            }
        }
        let len = positions.len();
        let mut i = len;
        while i > 1 {
            let j = self.rng.below(i);
            i -= 1;
            positions.swap(i, j);
        }
        positions
    }

    fn start_animation(&mut self) {
        let fps = if self.effect == Effect::Glitch {
            60.0
        } else {
            30.0
        };
        let interval_ms = (1000.0 / fps) as u64;
        let running = Arc::clone(&self.running);
        let grid_version = Arc::clone(&self.grid_version);
        running.store(true, Ordering::SeqCst);
        // The TypeScript timer calls `tickEffect()` on the component itself. Rust
        // cannot move `self` into the task, so the ticker only records that a tick
        // is due and `render` advances the effect state on the next pass; the
        // resulting frame sequence and the stop condition are unchanged.
        let finished = Arc::clone(&self.finished);
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;
            while running.load(Ordering::SeqCst) {
                ticker.tick().await;
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                grid_version.fetch_add(1, Ordering::SeqCst);
                if finished.load(Ordering::SeqCst) {
                    running.store(false, Ordering::SeqCst);
                    break;
                }
            }
        });
        self.interval_task = Some(task);
    }

    fn stop_animation(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(task) = self.interval_task.take() {
            task.abort();
        }
    }

    /// Port of `tickEffect()`.
    pub fn tick_effect(&mut self) -> bool {
        match self.effect {
            Effect::Typewriter => self.tick_typewriter(),
            Effect::Scanline => self.tick_scanline(),
            Effect::Rain => self.tick_rain(),
            Effect::Fade => self.tick_fade(),
            Effect::Crt => self.tick_crt(),
            Effect::Glitch => self.tick_glitch(),
            Effect::Dissolve => self.tick_dissolve(),
        }
    }

    fn tick_typewriter(&mut self) -> bool {
        let pixels_per_frame = 3usize;
        let mut pos = match &self.effect_state {
            EffectState::Typewriter { pos } => *pos,
            _ => return true,
        };
        for _ in 0..pixels_per_frame {
            let row = pos / WIDTH;
            let x = pos % WIDTH;
            if row >= DISPLAY_HEIGHT {
                self.effect_state = EffectState::Typewriter { pos };
                return true;
            }
            self.current_grid[row][x] = self.final_grid[row][x].clone();
            pos += 1;
        }
        self.effect_state = EffectState::Typewriter { pos };
        false
    }

    fn tick_scanline(&mut self) -> bool {
        let mut row = match &self.effect_state {
            EffectState::Scanline { row } => *row,
            _ => return true,
        };
        if row >= DISPLAY_HEIGHT {
            return true;
        }
        // Copy row
        for x in 0..WIDTH {
            self.current_grid[row][x] = self.final_grid[row][x].clone();
        }
        row += 1;
        self.effect_state = EffectState::Scanline { row };
        false
    }

    fn tick_rain(&mut self) -> bool {
        let mut drops = match &self.effect_state {
            EffectState::Rain { drops } => drops.clone(),
            _ => return true,
        };

        let mut all_settled = true;
        self.current_grid = self.create_empty_grid();

        for x in 0..WIDTH {
            let drop = drops[x];

            // Draw settled pixels
            let mut row = DISPLAY_HEIGHT as i64 - 1;
            while row >= DISPLAY_HEIGHT as i64 - drop.settled as i64 {
                if row >= 0 {
                    let r = row as usize;
                    self.current_grid[r][x] = self.final_grid[r][x].clone();
                }
                row -= 1;
            }

            // Check if this column is done
            if drop.settled >= DISPLAY_HEIGHT {
                continue;
            }

            all_settled = false;

            // Find the target row for this column (lowest non-space pixel)
            let mut target_row: i64 = -1;
            let mut row = DISPLAY_HEIGHT as i64 - 1 - drop.settled as i64;
            while row >= 0 {
                let r = row as usize;
                if self.final_grid[r][x] != " " {
                    target_row = row;
                    break;
                }
                row -= 1;
            }

            // Move drop down
            drops[x].y += 1;

            // Draw falling drop
            if drops[x].y >= 0 && drops[x].y < DISPLAY_HEIGHT as i64 {
                if target_row >= 0 && drops[x].y >= target_row {
                    // Settle
                    drops[x].settled = DISPLAY_HEIGHT - target_row as usize;
                    drops[x].y = -(self.rng.below(5) as i64) - 1;
                } else {
                    // Still falling
                    self.current_grid[drops[x].y as usize][x] = "▓".to_string();
                }
            }
            let _ = drop;
        }

        self.effect_state = EffectState::Rain { drops };
        all_settled
    }

    fn tick_fade(&mut self) -> bool {
        let pixels_per_frame = 15usize;
        let (positions, mut idx) = match &self.effect_state {
            EffectState::Positions { positions, idx } => (positions.clone(), *idx),
            _ => return true,
        };
        for _ in 0..pixels_per_frame {
            if idx >= positions.len() {
                self.effect_state = EffectState::Positions { positions, idx };
                return true;
            }
            let (row, x) = positions[idx];
            self.current_grid[row][x] = self.final_grid[row][x].clone();
            idx += 1;
        }
        self.effect_state = EffectState::Positions { positions, idx };
        false
    }

    fn tick_crt(&mut self) -> bool {
        let expansion = match &self.effect_state {
            EffectState::Crt { expansion } => *expansion,
            _ => return true,
        };
        let mid_row = DISPLAY_HEIGHT / 2;

        self.current_grid = self.create_empty_grid();

        // Draw from middle expanding outward
        let top = mid_row as i64 - expansion as i64;
        let bottom = mid_row + expansion;

        let start = top.max(0) as usize;
        let end = bottom.min(DISPLAY_HEIGHT.saturating_sub(1));
        for row in start..=end {
            for x in 0..WIDTH {
                self.current_grid[row][x] = self.final_grid[row][x].clone();
            }
        }

        let expansion = expansion + 1;
        self.effect_state = EffectState::Crt { expansion };
        expansion > DISPLAY_HEIGHT
    }

    fn tick_glitch(&mut self) -> bool {
        let (mut phase, glitch_frames) = match &self.effect_state {
            EffectState::Glitch {
                phase,
                glitch_frames,
            } => (*phase, *glitch_frames),
            _ => return true,
        };

        if phase < glitch_frames {
            // Glitch phase: show corrupted version
            let mut grid: Vec<Vec<String>> = Vec::with_capacity(self.final_grid.len());
            for row in &self.final_grid {
                let glitch_row = row.clone();
                // Random horizontal offset
                if self.rng.next() < 0.3 {
                    let offset = self.rng.below(7) as i64 - 3;
                    let mut shifted: Vec<String> = Vec::with_capacity(WIDTH);
                    for i in 0..WIDTH {
                        let index = (i as i64 + offset).rem_euclid(WIDTH as i64) as usize;
                        shifted.push(glitch_row[index].clone());
                    }
                    grid.push(shifted[..WIDTH].to_vec());
                    continue;
                }
                // Random vertical swap
                if self.rng.next() < 0.2 {
                    let swap_row = self.rng.below(DISPLAY_HEIGHT);
                    grid.push(self.final_grid[swap_row].clone());
                    continue;
                }
                grid.push(glitch_row);
            }
            self.current_grid = grid;
            phase += 1;
            self.effect_state = EffectState::Glitch {
                phase,
                glitch_frames,
            };
            return false;
        }

        // Final frame: show clean image
        self.current_grid = self.final_grid.clone();
        true
    }

    fn tick_dissolve(&mut self) -> bool {
        let pixels_per_frame = 20usize;
        let (positions, mut idx) = match &self.effect_state {
            EffectState::Positions { positions, idx } => (positions.clone(), *idx),
            _ => return true,
        };
        for _ in 0..pixels_per_frame {
            if idx >= positions.len() {
                self.effect_state = EffectState::Positions { positions, idx };
                return true;
            }
            let (row, x) = positions[idx];
            self.current_grid[row][x] = self.final_grid[row][x].clone();
            idx += 1;
        }
        self.effect_state = EffectState::Positions { positions, idx };
        false
    }

    fn update_display(&mut self) {
        self.grid_version.fetch_add(1, Ordering::SeqCst);
    }
}

impl Component for ArminComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        let version = self.grid_version.load(Ordering::SeqCst);

        // The TypeScript advances the effect from the interval callback before
        // rendering; the port advances it here, once per recorded tick.
        while self.cached_version < version {
            let done = self.tick_effect();
            self.update_display();
            if done {
                self.stop_animation();
                break;
            }
            self.cached_version += 1;
            if self.cached_version >= version {
                break;
            }
        }

        if width == self.cached_width
            && self.cached_version == self.grid_version.load(Ordering::SeqCst)
        {
            return self.cached_lines.clone();
        }

        let padding = 1.0;
        let available_width = width - padding;

        let active = theme();
        let mut lines: Vec<String> = Vec::new();
        for row in &self.current_grid {
            // Clip row to available width before applying color
            let clipped: String = row
                .iter()
                .take(available_width.max(0.0) as usize)
                .cloned()
                .collect::<Vec<_>>()
                .join("");
            let pad_right = (width - padding - clipped.chars().count() as f64).max(0.0) as usize;
            lines.push(format!(
                " {}{}",
                active.fg("accent", &clipped),
                " ".repeat(pad_right)
            ));
        }

        // Add "ARMIN SAYS HI" at the end
        let message = "ARMIN SAYS HI";
        let msg_pad_right = (width - padding - message.chars().count() as f64).max(0.0) as usize;
        lines.push(format!(
            " {}{}",
            active.fg("accent", message),
            " ".repeat(msg_pad_right)
        ));

        self.cached_lines = lines.clone();
        self.cached_width = width;
        self.cached_version = self.grid_version.load(Ordering::SeqCst);

        lines
    }

    fn invalidate(&mut self) {
        self.invalidate_cache();
    }
}

impl std::ops::Drop for ArminComponent {
    fn drop(&mut self) {
        self.stop_animation();
    }
}

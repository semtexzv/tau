//! GIF-to-ANSI load test — plays a GIF as colored block art through the TUI.
//!
//! Measures rendering performance (frame time, bytes/frame, FPS) to verify
//! the differential rendering engine is fast enough for real-world use.
//!
//! Run with: `cargo run -p tau-tui --example loadtest -- doom.gif`

use std::cell::{Cell, RefCell};
use std::env;
use std::fs::File;
use std::io::{self, BufReader, Write as IoWrite};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, ImageDecoder};

use tau_tui::component::Component;
use tau_tui::terminal::{CrosstermTerminal, Terminal};
use tau_tui::tui::{Anchor, Event, OverlayOptions, TUI};
use tau_tui::utils::visible_width;

// ── Events ──────────────────────────────────────────────────────────

#[derive(Debug)]
enum LoadTestEvent {
    Frame(usize),
}

// ── Pre-rendered frame data ─────────────────────────────────────────

struct PreRenderedFrame {
    lines: Vec<String>,
    delay: Duration,
}

// ── Timing Terminal Wrapper ─────────────────────────────────────────
//
// Wraps CrosstermTerminal to measure write/flush timing and byte counts.
// Shares stats via Rc<RefCell<PerfStats>> so the stats overlay can read them.

struct PerfStats {
    frame_count: usize,
    render_times: Vec<Duration>,
    bytes_per_frame: Vec<usize>,
    start_time: Instant,
}

impl PerfStats {
    fn new() -> Self {
        Self {
            frame_count: 0,
            render_times: Vec::new(),
            bytes_per_frame: Vec::new(),
            start_time: Instant::now(),
        }
    }

    fn current_fps(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.frame_count as f64 / elapsed
        } else {
            0.0
        }
    }

    fn avg_frame_time_ms(&self) -> f64 {
        if self.render_times.is_empty() {
            return 0.0;
        }
        let total: Duration = self.render_times.iter().sum();
        total.as_secs_f64() * 1000.0 / self.render_times.len() as f64
    }

    fn avg_bytes(&self) -> usize {
        if self.bytes_per_frame.is_empty() {
            return 0;
        }
        self.bytes_per_frame.iter().sum::<usize>() / self.bytes_per_frame.len()
    }

    fn p95_frame_time_ms(&self) -> f64 {
        if self.render_times.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<f64> = self
            .render_times
            .iter()
            .map(|d| d.as_secs_f64() * 1000.0)
            .collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = ((sorted.len() as f64) * 0.95).ceil() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn max_frame_time_ms(&self) -> f64 {
        self.render_times
            .iter()
            .map(|d| d.as_secs_f64() * 1000.0)
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap_or(0.0)
    }
}

struct TimingTerminal {
    inner: CrosstermTerminal,
    stats: Rc<RefCell<PerfStats>>,
    write_start: Option<Instant>,
    pending_bytes: usize,
}

impl TimingTerminal {
    fn new(stats: Rc<RefCell<PerfStats>>) -> Self {
        Self {
            inner: CrosstermTerminal::new(),
            stats,
            write_start: None,
            pending_bytes: 0,
        }
    }
}

impl Terminal for TimingTerminal {
    fn start(&mut self) {
        self.inner.start();
    }

    fn stop(&mut self) {
        self.inner.stop();
    }

    fn write(&mut self, data: &str) {
        if self.write_start.is_none() {
            self.write_start = Some(Instant::now());
        }
        self.pending_bytes += data.len();
        self.inner.write(data);
    }

    fn flush(&mut self) {
        self.inner.flush();
        // Record timing: from write start to flush complete
        if let Some(start) = self.write_start.take() {
            let duration = start.elapsed();
            let bytes = self.pending_bytes;
            self.pending_bytes = 0;
            let mut stats = self.stats.borrow_mut();
            stats.frame_count += 1;
            stats.render_times.push(duration);
            stats.bytes_per_frame.push(bytes);
        }
    }

    fn size(&self) -> (u16, u16) {
        self.inner.size()
    }

    fn hide_cursor(&mut self) {
        self.inner.hide_cursor();
    }

    fn show_cursor(&mut self) {
        self.inner.show_cursor();
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ── GIF → ANSI Conversion ───────────────────────────────────────────

/// Convert a GIF frame's RGBA pixels to ANSI-colored half-block lines.
///
/// Uses `▀` (upper half block) with truecolor foreground (top pixel) and
/// background (bottom pixel) to pack 2 vertical pixels per terminal cell.
/// Accounts for ~2:1 cell height:width ratio when scaling.
fn frame_to_ansi(
    rgba: &[u8],
    src_width: u32,
    src_height: u32,
    term_cols: u16,
    term_rows: u16,
) -> Vec<String> {
    // Reserve 2 rows for stats overlay margin
    let max_cols = term_cols as u32;
    let max_rows = (term_rows.saturating_sub(2)) as u32;

    // Each terminal cell is ~2x taller than wide, and we pack 2 pixels per cell vertically.
    // So effective pixel rows per terminal row = 2.
    // Scale factor: fit (src_width x src_height) into (max_cols x max_rows*2) pixel space.
    let scale_x = max_cols as f64 / src_width as f64;
    let scale_y = (max_rows as f64 * 2.0) / src_height as f64;
    let scale = scale_x.min(scale_y);

    let dst_px_w = ((src_width as f64 * scale).round() as u32).max(1).min(max_cols);
    let dst_px_h = ((src_height as f64 * scale).round() as u32).max(2);

    // Ensure even height for pixel-pair packing
    let dst_px_h = if dst_px_h % 2 == 1 {
        dst_px_h + 1
    } else {
        dst_px_h
    };

    let cell_rows = dst_px_h / 2;

    // Nearest-neighbor sampling helper
    let sample = |px_x: u32, px_y: u32| -> (u8, u8, u8) {
        let sx = ((px_x as f64 / scale).floor() as u32).min(src_width - 1);
        let sy = ((px_y as f64 / scale).floor() as u32).min(src_height - 1);
        let idx = (sy * src_width + sx) as usize * 4;
        (rgba[idx], rgba[idx + 1], rgba[idx + 2])
    };

    let mut lines = Vec::with_capacity(cell_rows as usize);

    for row in 0..cell_rows {
        let mut line = String::with_capacity(dst_px_w as usize * 30);
        let top_y = row * 2;
        let bot_y = row * 2 + 1;

        for col in 0..dst_px_w {
            let (tr, tg, tb) = sample(col, top_y);
            let (br, bg, bb) = sample(col, bot_y);
            // Foreground = top pixel, Background = bottom pixel
            line.push_str(&format!(
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▀",
                tr, tg, tb, br, bg, bb
            ));
        }
        line.push_str("\x1b[0m");
        lines.push(line);
    }

    lines
}

// ── Frame Component ─────────────────────────────────────────────────

/// Displays a pre-rendered ANSI frame. Reads current frame index from shared state.
struct FrameComponent {
    frames: Rc<Vec<PreRenderedFrame>>,
    current: Rc<Cell<usize>>,
}

impl FrameComponent {
    fn new(frames: Rc<Vec<PreRenderedFrame>>, current: Rc<Cell<usize>>) -> Self {
        Self { frames, current }
    }
}

impl Component for FrameComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let idx = self.current.get();
        if idx >= self.frames.len() {
            return vec![];
        }
        self.frames[idx]
            .lines
            .iter()
            .map(|line| {
                let vis = visible_width(line);
                let w = width as usize;
                if vis < w {
                    format!("{}{}", line, " ".repeat(w - vis))
                } else {
                    line.clone()
                }
            })
            .collect()
    }
}

// ── Stats Overlay Component ─────────────────────────────────────────

/// Displays live FPS, avg frame time, and avg bytes/frame.
struct StatsComponent {
    stats: Rc<RefCell<PerfStats>>,
}

impl StatsComponent {
    fn new(stats: Rc<RefCell<PerfStats>>) -> Self {
        Self { stats }
    }
}

impl Component for StatsComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let stats = self.stats.borrow();
        let w = width as usize;

        let lines = vec![
            format!(" FPS: {:.1} ", stats.current_fps()),
            format!(" Avg: {:.1}ms ", stats.avg_frame_time_ms()),
            format!(" Bytes: {} ", stats.avg_bytes()),
        ];

        lines
            .into_iter()
            .map(|line| {
                let vis = visible_width(&line);
                if vis < w {
                    format!("\x1b[43m\x1b[30m{}{}\x1b[0m", line, " ".repeat(w - vis))
                } else {
                    format!("\x1b[43m\x1b[30m{}\x1b[0m", line)
                }
            })
            .collect()
    }
}

// ── Main ────────────────────────────────────────────────────────────

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: cargo run -p tau-tui --example loadtest -- <gif-path>");
        eprintln!("Example: cargo run -p tau-tui --example loadtest -- doom.gif");
        std::process::exit(1);
    }
    let gif_path = &args[1];

    // ── Load and pre-render GIF frames ──────────────────────────────
    eprint!("Loading {}...", gif_path);
    io::stderr().flush().ok();

    let file = BufReader::new(File::open(gif_path).unwrap_or_else(|e| {
        eprintln!("\nError opening {}: {}", gif_path, e);
        std::process::exit(1);
    }));

    let decoder = GifDecoder::new(file).unwrap_or_else(|e| {
        eprintln!("\nError decoding GIF: {}", e);
        std::process::exit(1);
    });

    let (gif_width, gif_height) = decoder.dimensions();

    // Get terminal size for scaling
    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));

    // Pre-render all frames
    let default_delay = Duration::from_millis(33); // ~30fps fallback
    let mut frames: Vec<PreRenderedFrame> = Vec::new();

    for frame_result in decoder.into_frames() {
        let frame = frame_result.unwrap_or_else(|e| {
            eprintln!("\nError decoding frame {}: {}", frames.len(), e);
            std::process::exit(1);
        });

        let delay: Duration = frame.delay().into();
        let delay = if delay.is_zero() { default_delay } else { delay };

        let rgba = frame.buffer();
        let lines = frame_to_ansi(rgba.as_raw(), gif_width, gif_height, term_cols, term_rows);

        frames.push(PreRenderedFrame { lines, delay });
    }

    let total_frames = frames.len();
    eprintln!(
        " loaded {} frames ({}x{} → {}x{} cells)",
        total_frames,
        gif_width,
        gif_height,
        term_cols,
        term_rows,
    );

    if total_frames == 0 {
        eprintln!("No frames found in GIF.");
        std::process::exit(1);
    }

    // ── Set up TUI with timing terminal ─────────────────────────────
    let stats = Rc::new(RefCell::new(PerfStats::new()));
    let terminal = TimingTerminal::new(stats.clone());
    let mut tui: TUI<LoadTestEvent> = TUI::new(Box::new(terminal));

    // Shared state
    let frames = Rc::new(frames);
    let current_frame = Rc::new(Cell::new(0usize));

    // Add frame component as root child
    tui.root().add_child(Box::new(FrameComponent::new(
        frames.clone(),
        current_frame.clone(),
    )));

    // Show stats overlay (top-right corner)
    tui.show_overlay(
        Box::new(StatsComponent::new(stats.clone())),
        OverlayOptions {
            width: 22,
            max_height: Some(3),
            anchor: Anchor::TopRight,
            offset_x: -1,
            offset_y: 0,
        },
    );

    // ── Spawn frame playback task ───────────────────────────────────
    // Extract delays (Vec<Duration>) for the playback thread — Rc can't cross threads.
    let delays: Vec<Duration> = frames.iter().map(|f| f.delay).collect();
    let tx = tui.event_tx();
    std::thread::spawn(move || {
        let mut frame_idx = 0usize;
        loop {
            let delay = delays[frame_idx];
            std::thread::sleep(delay);
            frame_idx = (frame_idx + 1) % delays.len();
            if tx.send(LoadTestEvent::Frame(frame_idx)).is_err() {
                break;
            }
        }
    });

    // Reset start time just before the event loop
    stats.borrow_mut().start_time = Instant::now();

    // ── Event loop ──────────────────────────────────────────────────
    tui.run(|event, tui| {
        match event {
            Event::User(LoadTestEvent::Frame(idx)) => {
                current_frame.set(idx);
            }
            Event::Key(key) => match (key.code, key.modifiers) {
                (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
                    tui.quit();
                }
                _ => {}
            },
            Event::Resize(_, _) => {
                // Resize triggers full redraw automatically
            }
        }
    })
    .await;

    // ── Print summary to stderr ─────────────────────────────────────
    let stats = stats.borrow();
    eprintln!("\n═══ Load Test Summary ═══");
    eprintln!("Total frames rendered: {}", stats.frame_count);
    eprintln!("Average FPS:           {:.1}", stats.current_fps());
    eprintln!(
        "Avg frame time:        {:.2}ms",
        stats.avg_frame_time_ms()
    );
    eprintln!("P95 frame time:        {:.2}ms", stats.p95_frame_time_ms());
    eprintln!("Max frame time:        {:.2}ms", stats.max_frame_time_ms());
    eprintln!("Avg bytes/frame:       {}", stats.avg_bytes());
    eprintln!("═════════════════════════");
}

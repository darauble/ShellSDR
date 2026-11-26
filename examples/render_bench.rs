//! Time one full-screen draw with a full waterfall (the UI thread's work per
//! frame). `cargo run --release --example render_bench -- [cols] [rows]`

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use shellsdr::app::{AppState, RunState};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let w: u16 = a.first().and_then(|s| s.parse().ok()).unwrap_or(200);
    let h: u16 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    let mut s = AppState::with_mock_source();
    s.run = RunState::Running;
    let frame: Vec<f32> = (0..4096).map(|i| -80.0 + (i % 97) as f32 * 0.5).collect();
    for _ in 0..512 {
        s.waterfall.push(frame.clone());
    }
    s.spectrum = frame;
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    let n = 50;
    let t0 = std::time::Instant::now();
    for _ in 0..n {
        t.draw(|f| {
            shellsdr::ui::render::render_with_hits(f, &s);
        })
        .unwrap();
    }
    println!(
        "{w}x{h}: {:.2} ms per draw",
        t0.elapsed().as_secs_f64() * 1e3 / n as f64
    );
}

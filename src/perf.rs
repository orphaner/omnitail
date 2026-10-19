//! Performance checks, run on demand: `cargo test --release perf -- --ignored
//! --nocapture --test-threads=1` (see perf/README.md). Each one prints a
//! `PERF` line and fails when far off its budget, so CI can catch
//! regressions; budgets leave room for slow shared runners.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::App;
use crate::color::{Choice, Library, PaneColor};
use crate::source::SourceEvent;
use crate::spec::K8sPod;

/// Lines of the mix perf/gen-logs.py writes (JSON, logfmt, access logs,
/// text, ANSI, stack traces, long JSON), from a simple deterministic
/// generator so these checks need nothing but cargo.
fn lines(n: usize) -> Vec<String> {
    let mut seed: u64 = 42;
    let mut next = move |m: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    let levels = ["debug", "info", "info", "info", "warn", "error"];
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let level = levels[next(6) as usize];
        let k = next(1000);
        match next(100) {
            0..30 => out.push(format!(
                r#"{{"time":"2026-10-19T12:00:{:02}.{k:03}Z","level":"{level}","msg":"order job","user_id":{},"duration_ms":{},"ok":true}}"#,
                next(60),
                next(100000),
                next(2000)
            )),
            30..50 => out.push(format!(
                r#"time=2026-10-19T12:00:00Z level={level} msg="job processed" job={} attempt=2 duration={}ms"#,
                next(100000),
                next(900)
            )),
            50..70 => out.push(format!(
                r#"10.0.{}.{} - - [19/Oct/2026:12:00:00 +0000] "GET /api/items/{k} HTTP/1.1" {} {} "-" "Mozilla/5.0""#,
                next(256),
                next(256),
                [200, 200, 304, 404, 500][next(5) as usize],
                next(50000)
            )),
            70..85 => out.push(format!(
                "2026-10-19 12:00:00.{k:03} {:5} [worker-{}] c.e.shop.OrderService - order {} handled in {}ms",
                level.to_uppercase(),
                next(16),
                next(10000),
                next(500)
            )),
            85..90 => out.push(format!("\x1b[3{}m{}\x1b[0m checkpoint {k}", 1 + next(6), level.to_uppercase())),
            90..99 => out.push(format!("plain line {k} without anything special about it")),
            _ => {
                out.push(format!("2026-10-19 12:00:00.000 ERROR [worker-1] c.e.shop.OrderService - order {k} failed"));
                out.push("java.lang.IllegalStateException: payment declined".into());
                for i in 0..15 {
                    out.push(format!("\tat com.example.shop.Service{i}.handle(Service{i}.java:{})", next(900)));
                }
                if next(5) == 0 {
                    let items: Vec<String> = (0..300).map(|i| format!(r#"{{"id":{i},"price":{}}}"#, next(10000))).collect();
                    out.push(format!(r#"{{"level":"debug","msg":"cart dump","items":[{}]}}"#, items.join(",")));
                }
            }
        }
    }
    out.truncate(n);
    out
}

/// An app with `names` panes keeping `history` lines each, colors guessed.
fn app(names: &[&str], history: usize) -> App {
    let library = Arc::new(Library::new(&Default::default()).unwrap());
    let panes = names
        .iter()
        .map(|n| (n.to_string(), PaneColor::new(Choice::Auto, &library, true).unwrap()))
        .collect();
    App::new(panes, library, history, false)
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_text(app: &mut App, text: &str) {
    text.chars().for_each(|c| press(app, KeyCode::Char(c)));
}

/// Draws `frames` frames on a 200×60 screen; time per frame.
fn draw(app: &mut App, frames: u32) -> Duration {
    let mut terminal = Terminal::new(TestBackend::new(200, 60)).unwrap();
    let started = Instant::now();
    for _ in 0..frames {
        terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    }
    started.elapsed() / frames
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Prints the result and checks it against its budget.
fn report(name: &str, value: f64, unit: &str, budget: f64, higher_is_better: bool) {
    let ok = if higher_is_better { value >= budget } else { value <= budget };
    let bound = if higher_is_better { "≥" } else { "≤" };
    println!("PERF {name:<32} {value:>12.1} {unit:<8} budget {bound} {budget} {}", if ok { "ok" } else { "OVER" });
    assert!(ok, "{name}: {value:.1} {unit}, budget {bound} {budget}");
}

/// Lines per second through `App::ingest`.
fn ingest(app: &mut App, source: usize, lines: Vec<String>) -> f64 {
    let n = lines.len();
    let started = Instant::now();
    for line in lines {
        app.ingest(source, SourceEvent::Line(line));
    }
    n as f64 / started.elapsed().as_secs_f64()
}

fn shop_pods() -> Vec<K8sPod> {
    let containers = vec!["app".to_string(), "proxy".into(), "shipper".into()];
    (0..3).map(|i| K8sPod { name: format!("shop-5b5c4ff46c-pod{i}a"), containers: containers.clone() }).collect()
}

/// `n` lines spread over 3 pods × 3 containers, as `kubectl logs --prefix` writes them.
fn k8s_lines(n: usize) -> Vec<String> {
    let containers = ["app", "proxy", "shipper"];
    lines(n).into_iter().enumerate().map(|(i, l)| format!("[pod/shop-5b5c4ff46c-pod{}a/{}] {l}", i % 3, containers[i / 3 % 3])).collect()
}

#[test]
#[ignore]
fn perf_level_detection() {
    let lines = lines(200_000);
    let started = Instant::now();
    let found = lines.iter().filter(|l| crate::level::detect(l).is_some()).count();
    let rate = lines.len() as f64 / started.elapsed().as_secs_f64();
    assert!(found > 0);
    report("level detection", rate / 1000.0, "k lines/s", 300.0, true);
}

#[test]
#[ignore]
fn perf_ingest_one_pane() {
    let mut app = app(&["big.log"], 200_000);
    let rate = ingest(&mut app, 0, lines(200_000));
    report("ingest, one pane", rate / 1000.0, "k lines/s", 100.0, true);
}

#[test]
#[ignore]
fn perf_ingest_k8s_split() {
    // Split into pods: each line goes to its pod's pane and to the source's
    // pane kept on the stack.
    let mut app = app(&["k8s: shop > deploy/shop"], 200_000);
    app.set_pods(0, shop_pods());
    app.start();
    let rate = ingest(&mut app, 0, k8s_lines(200_000));
    report("ingest, k8s split in pods", rate / 1000.0, "k lines/s", 50.0, true);
}

#[test]
#[ignore]
fn perf_frame_four_panes() {
    let mut app = app(&["a", "b", "c", "d"], 50_000);
    for (i, chunk) in lines(200_000).chunks(50_000).enumerate() {
        ingest(&mut app, i, chunk.to_vec());
    }
    report("frame, 4 panes wrapped", ms(draw(&mut app, 50)), "ms", 16.0, false);
}

#[test]
#[ignore]
fn perf_frame_sparse_filter() {
    // Almost nothing passes the filter: the pane looks through its whole
    // history at every frame to fill itself.
    let mut app = app(&["big.log"], 100_000);
    ingest(&mut app, 0, lines(100_000));
    press(&mut app, KeyCode::Char('e'));
    type_text(&mut app, "user_id\":4242,");
    press(&mut app, KeyCode::Enter);
    report("frame, filter keeping ~0", ms(draw(&mut app, 20)), "ms", 50.0, false);
}

#[test]
#[ignore]
fn perf_history_search_filter_merge() {
    let mut app = app(&["a", "b"], 100_000);
    for (i, chunk) in lines(200_000).chunks(100_000).enumerate() {
        ingest(&mut app, i, chunk.to_vec());
    }
    press(&mut app, KeyCode::Char('b'));
    press(&mut app, KeyCode::Char('1'));
    draw(&mut app, 1);
    let started = Instant::now();
    press(&mut app, KeyCode::Char('?'));
    type_text(&mut app, "payment declined");
    press(&mut app, KeyCode::Enter);
    draw(&mut app, 1);
    report("history search, 100k", ms(started.elapsed()), "ms", 100.0, false);

    let started = Instant::now();
    press(&mut app, KeyCode::Char('&'));
    type_text(&mut app, "level=error");
    press(&mut app, KeyCode::Enter);
    draw(&mut app, 1);
    report("history filter, 100k", ms(started.elapsed()), "ms", 100.0, false);

    press(&mut app, KeyCode::Esc);
    let started = Instant::now();
    press(&mut app, KeyCode::Char('B'));
    draw(&mut app, 1);
    report("merged history (B), 2×100k", ms(started.elapsed()), "ms", 200.0, false);
}

#[test]
#[ignore]
fn perf_frame_new_lines_below_cursor() {
    // The cursor left the end, then a flood arrived: the title counts it.
    let mut app = app(&["big.log"], 200_000);
    ingest(&mut app, 0, lines(50_000));
    press(&mut app, KeyCode::Char('b'));
    press(&mut app, KeyCode::Up);
    draw(&mut app, 1);
    ingest(&mut app, 0, lines(100_000));
    report("frame, 100k new below cursor", ms(draw(&mut app, 20)), "ms", 16.0, false);
}

#[test]
#[ignore]
fn perf_k8s_split() {
    let mut app = app(&["k8s: shop > deploy/shop"], 100_000);
    app.set_pods(0, shop_pods());
    ingest(&mut app, 0, k8s_lines(100_000));
    let started = Instant::now();
    press(&mut app, KeyCode::Enter);
    draw(&mut app, 1);
    report("split into pods, 100k lines", ms(started.elapsed()), "ms", 100.0, false);
    let started = Instant::now();
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('1'));
    draw(&mut app, 1);
    report("split a pod into containers", ms(started.elapsed()), "ms", 100.0, false);
}

#[tokio::test]
#[ignore]
async fn perf_file_tail_at_start() {
    // A 1M-line file, opened with --tail 100000.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    std::fs::write(&path, lines(1_000_000).join("\n") + "\n").unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8192);
    let started = Instant::now();
    use crate::source::Source;
    let task = Box::new(crate::source::FileSource::new(path, 100_000)).spawn(crate::source::Emitter::new(0, tx));
    let mut got = 0;
    while got < 100_000 {
        if let Some((_, SourceEvent::Line(_))) = rx.recv().await {
            got += 1;
        }
    }
    task.abort();
    report("file tail 100k of 1M at start", ms(started.elapsed()), "ms", 1000.0, false);
}

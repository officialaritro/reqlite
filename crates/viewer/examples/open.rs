//! Opens a file as a response body and reports the cost. Used by the budget check.
//! Usage: cargo run --release -p reqlite-viewer --example open -- FILE

use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: open FILE")?;
    let start = Instant::now();
    let doc = reqlite_viewer::Document::build(std::fs::File::open(&path)?)?;
    let built = start.elapsed();
    let middle = doc.line_count() / 2;
    let window = Instant::now();
    let lines = doc.lines(middle, 60)?;
    println!(
        "lines={} pretty={} build_ms={} window_ms={} first={:?}",
        doc.line_count(),
        doc.is_pretty(),
        built.as_millis(),
        window.elapsed().as_millis(),
        lines
            .first()
            .map(|l| l.chars().take(40).collect::<String>())
    );
    Ok(())
}

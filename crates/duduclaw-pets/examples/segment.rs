//! Live smoke test for the ONNX background-removal path (WP-P4 partial).
//!
//! Usage:
//!   cargo run -p duduclaw-pets --features onnx --example segment -- \
//!     <input.(png|jpg)> <output.png> [birefnet|silueta]
//!
//! Downloads the model into ~/.duduclaw/models/ on first run and prints the
//! inference latency.

#[cfg(feature = "onnx")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use duduclaw_pets::segmentation::{
        download_model, BackgroundRemover, OnnxRemover, BIREFNET_LITE, SILUETA,
    };

    let mut args = std::env::args().skip(1);
    let input = args.next().ok_or("missing <input>")?;
    let output = args.next().ok_or("missing <output>")?;
    let spec = match args.next().as_deref() {
        Some("silueta") => &SILUETA,
        _ => &BIREFNET_LITE,
    };

    if !spec.local_path().is_file() {
        eprintln!("downloading {} …", spec.filename);
        let t = std::time::Instant::now();
        download_model(spec)?;
        eprintln!("downloaded in {:.1}s", t.elapsed().as_secs_f32());
    }

    let remover = OnnxRemover::load(spec)?;
    let bytes = std::fs::read(&input)?;
    let t = std::time::Instant::now();
    let out = remover.remove_background(&bytes)?;
    let dt = t.elapsed();
    std::fs::write(&output, out)?;
    println!(
        "ok: {} -> {} via {} in {:.2}s",
        input,
        output,
        remover.label(),
        dt.as_secs_f32()
    );
    Ok(())
}

#[cfg(not(feature = "onnx"))]
fn main() {
    eprintln!("rebuild with --features onnx");
}

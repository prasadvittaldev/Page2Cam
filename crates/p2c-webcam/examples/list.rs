//! Print the cameras Page2Cam can see: `cargo run -p p2c-webcam --example list`
fn main() -> anyhow::Result<()> {
    for c in p2c_webcam::list_cameras()? {
        println!(
            "{:>2}  {:<40} {}",
            c.index,
            c.name,
            c.path.map(|p| p.display().to_string()).unwrap_or_default()
        );
    }
    Ok(())
}

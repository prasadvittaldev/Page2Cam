//! Print the cameras Page2Cam can see and the modes each one offers:
//! `cargo run -p p2c-webcam --example list`
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};

fn main() -> anyhow::Result<()> {
    for c in p2c_webcam::list_cameras()? {
        println!(
            "{:>2}  {:<40} {}",
            c.index,
            c.name,
            c.path.map(|p| p.display().to_string()).unwrap_or_default()
        );
        let req = RequestedFormat::new::<RgbFormat>(RequestedFormatType::None);
        match nokhwa::Camera::new(CameraIndex::Index(c.index), req)
            .and_then(|mut cam| cam.compatible_camera_formats())
        {
            Ok(modes) => {
                println!("    modes: {}", p2c_webcam::describe_modes(&modes));
                match p2c_webcam::choose_format(&modes) {
                    Some(m) => println!("    Page2Cam would pick: {m}"),
                    None => println!("    Page2Cam would pick: nothing usable"),
                }
            }
            Err(e) => println!("    modes: unavailable ({e})"),
        }
    }
    Ok(())
}

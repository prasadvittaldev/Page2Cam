//! Real capture of the primary monitor (Windows; `--ignored`).
use std::sync::mpsc;
use std::time::Duration;

#[test]
#[ignore = "captures the real screen"]
fn grabs_a_frame_from_the_primary_monitor() {
    let screens = p2c_screen::list().expect("list");
    println!("screens: {screens:?}");
    let primary = p2c_screen::pick(&screens, "").expect("a screen").clone();
    let (tx, rx) = mpsc::channel();
    let cap = p2c_screen::Capture::start(
        "",
        30,
        Box::new(move |f| {
            let _ = tx.send(f);
        }),
    )
    .expect("start");
    assert_eq!(cap.screen().unwrap().id, primary.id);
    assert_eq!(cap.window_status(), p2c_screen::WindowStatus::Ok);
    let f = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("a frame within 5 s");
    println!("frame {}x{}", f.width, f.height);
    assert!(f.width > 0 && f.height > 0);
    assert_eq!(
        (f.width, f.height),
        (primary.width, primary.height),
        "physical size listed"
    );
    assert!(f.data.chunks(4).all(|p| p[3] == 255), "opaque");
    let t = std::time::Instant::now();
    drop(cap);
    assert!(t.elapsed() < Duration::from_secs(2), "stops quickly");
}

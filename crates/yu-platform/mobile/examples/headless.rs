//! Drives a game through the C interface without a view, as the app does:
//! open, step at 60 Hz, click, pull sound. For checking the interface on a
//! desktop: `cargo run --release -p yuris-mobile --example headless -- <game dir> [seconds] [x,y@ms ...]`.

use std::ffi::CString;
use std::time::{Duration, Instant};

use yuris_mobile::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let game = CString::new(args.get(1).expect("game folder").as_str()).unwrap();
    let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let mut clicks: Vec<(f32, f32, u64)> = args[3.min(args.len())..]
        .iter()
        .filter_map(|a| {
            let (xy, ms) = a.split_once('@')?;
            let (x, y) = xy.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?, ms.parse().ok()?))
        })
        .collect();
    let save = CString::new(std::env::temp_dir().join("yuris-mobile-save").to_string_lossy().as_bytes()).unwrap();
    let config = YurisConfig {
        struct_size: std::mem::size_of::<YurisConfig>() as u32,
        game_dir: game.as_ptr(),
        launcher: std::ptr::null(),
        save_dir: save.as_ptr(),
        encoding: 3,
        utc_offset_ms: 9 * 3_600_000,
    };
    let session = unsafe { yuris_open(&config) };
    if session.is_null() {
        let e = unsafe { std::ffi::CStr::from_ptr(yuris_last_error()) };
        eprintln!("open failed: {}", e.to_string_lossy());
        std::process::exit(1);
    }
    let (mut w, mut h) = (0, 0);
    unsafe { yuris_screen_size(session, &mut w, &mut h) };
    println!("screen {w}x{h}");
    let audio = unsafe { yuris_audio(session) };
    let start = Instant::now();
    let mut frames = 0u64;
    let mut loud = 0.0f32;
    let mut buf = vec![0.0f32; 2 * 735];
    while start.elapsed() < Duration::from_secs(seconds) {
        let now = start.elapsed().as_millis() as u64;
        clicks.retain(|&(x, y, at)| {
            if now >= at {
                unsafe {
                    yuris_pointer(session, 1, x, y);
                    yuris_pointer(session, 2, x, y);
                }
                false
            } else {
                true
            }
        });
        let status = unsafe { yuris_step(session, 16) };
        frames += 1;
        unsafe { yuris_audio_render(audio, buf.as_mut_ptr(), 735) };
        loud = buf.iter().fold(loud, |m, s| m.max(s.abs()));
        if status != 0 {
            let e = unsafe { std::ffi::CStr::from_ptr(yuris_last_error()) };
            println!("status {status} after {frames} frames: {}", e.to_string_lossy());
            break;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    println!("{frames} frames, peak sound {loud:.3}");
    unsafe {
        yuris_audio_release(audio);
        yuris_close(session);
    }
}

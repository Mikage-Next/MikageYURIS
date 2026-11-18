//! Drives a game headless at a fixed 60 fps with scripted clicks and keys,
//! writing screenshots on the way. For checking games after a change.
//!
//! ```text
//! cargo run --release -p yu-engine --example drive -- <game dir> [options]
//!   --frames N            frames to run (default 600)
//!   --backend soft|wgpu   renderer (default wgpu)
//!   --patch MODE          auto|original|gbk|big5|tunnel (default auto)
//!   --launcher EXE        a patch's Enigma Virtual Box packed exe
//!   --font PATH           host font file (first face)
//!   --save DIR            save folder (default: a temp folder)
//!   --click X,Y@F         left click at frame F (repeatable)
//!   --rclick X,Y@F        right click at frame F
//!   --key NAME@F          press KEY_NAME at frame F (ENTER, ESCAPE, CTRL...)
//!   --every X,Y@F/S       left click at F, F+S, F+2S... until the end
//!   --shot PATH@F         screenshot after frame F (repeatable)
//! ```

use std::path::PathBuf;

use yu_backend_wgpu::{DeviceOptions, GpuContext};
use yu_engine::engine::{Backend, Engine, EngineOptions, FontChoice};
use yu_engine::patch::{PatchKind, PatchSelection};
use yu_engine::project::Project;

enum Act {
    Click(i64, i64, &'static str),
    Key(String),
    Shot(PathBuf),
}

fn at(spec: &str) -> (&str, u64) {
    let (what, frame) = spec.rsplit_once('@').expect("WHAT@FRAME");
    (what, frame.parse().expect("frame number"))
}

fn xy(s: &str) -> (i64, i64) {
    let (x, y) = s.split_once(',').expect("X,Y");
    (x.parse().expect("x"), y.parse().expect("y"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let game = PathBuf::from(args.first().expect("game folder"));
    let mut frames = 600u64;
    let mut backend = Backend::Wgpu;
    let mut patch = PatchKind::Auto;
    let mut launcher: Option<PathBuf> = None;
    let mut font: Option<PathBuf> = None;
    let mut save: Option<PathBuf> = None;
    let mut acts: Vec<(u64, Act)> = Vec::new();
    let mut every: Vec<(i64, i64, u64, u64)> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let value = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--frames" => frames = value.parse().expect("frames"),
            "--backend" => {
                backend = if value == "soft" { Backend::Soft } else { Backend::Wgpu }
            }
            "--patch" => patch = PatchKind::parse(&value).expect("patch"),
            "--launcher" => launcher = Some(value.into()),
            "--font" => font = Some(value.into()),
            "--save" => save = Some(value.into()),
            "--click" | "--rclick" => {
                let (p, f) = at(&value);
                let (x, y) = xy(p);
                let button = if args[i] == "--click" { "MOUSE_L" } else { "MOUSE_R" };
                acts.push((f, Act::Click(x, y, button)));
            }
            "--key" => {
                let (k, f) = at(&value);
                acts.push((f, Act::Key(format!("KEY_{k}"))));
            }
            "--every" => {
                let (p, rest) = value.split_once('@').expect("X,Y@F/S");
                let (f, s) = rest.split_once('/').expect("F/S");
                let (x, y) = xy(p);
                every.push((x, y, f.parse().unwrap(), s.parse().unwrap()));
            }
            "--shot" => {
                let (p, f) = at(&value);
                acts.push((f, Act::Shot(p.into())));
            }
            other => panic!("unknown option {other}"),
        }
        i += 2;
    }

    let mut project = Project::discover(&game, None).expect("project");
    if let Some(exe) = &launcher {
        project.set_launcher(exe).expect("launcher");
    }
    let save = save.unwrap_or_else(|| std::env::temp_dir().join("yu-drive-save"));
    std::fs::create_dir_all(&save).unwrap();
    let mut options = EngineOptions {
        backend,
        save_root: Some(save),
        patch: PatchSelection::new(patch),
        ..EngineOptions::default()
    };
    if let Some(path) = font {
        options.font_bytes.push(std::fs::read(&path).expect("font"));
        options.font = FontChoice::HostFont(0);
    }
    if matches!(backend, Backend::Wgpu) {
        let gpu = GpuContext::new(&DeviceOptions::default()).expect("GPU");
        options.gpu_context = Some(gpu);
    }
    let mut engine = Engine::new(project, options).expect("boot");
    let vp = engine.viewport();
    println!("screen {}x{}", vp.width, vp.height);

    let mut release: Vec<(u64, String)> = Vec::new();
    for frame in 0..frames {
        release.retain(|(f, button)| {
            if *f == frame {
                engine.set_input(button, false);
                false
            } else {
                true
            }
        });
        for (x, y, first, step) in &every {
            if frame >= *first && (frame - first) % step == 0 {
                acts.push((frame, Act::Click(*x, *y, "MOUSE_L")));
            }
        }
        for (_, act) in acts.iter().filter(|(f, _)| *f == frame) {
            match act {
                Act::Click(x, y, button) => {
                    engine.set_cursor(*x, *y);
                    engine.set_input(button, true);
                    release.push((frame + 3, (*button).to_string()));
                }
                Act::Key(key) => {
                    engine.set_input(key, true);
                    release.push((frame + 3, key.clone()));
                }
                Act::Shot(_) => {}
            }
        }
        engine.tick().expect("tick");
        if engine.main_finished() {
            println!("main script ended at frame {frame}");
            break;
        }
        for (_, act) in acts.iter().filter(|(f, _)| *f == frame) {
            if let Act::Shot(path) = act {
                engine.refresh_screen_surface().expect("read back");
                match engine.write_png(path) {
                    Ok(_) => println!("frame {frame}: {}", path.display()),
                    Err(e) => println!("frame {frame}: no shot: {e}"),
                }
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for d in engine.take_diags() {
        let line = d.to_string();
        if seen.insert(line.clone()) && seen.len() <= 40 {
            println!("diag: {line}");
        }
    }
    for e in engine.script_errors() {
        println!("script error: {e}");
    }
}

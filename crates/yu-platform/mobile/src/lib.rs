//! The C interface for app hosts: `platform/ios/include/yuris_mobile.h`.
//!
//! A session is an [`Engine`] on the wgpu backend plus a [`Presenter`] on
//! the host's view. The host steps it once per display frame; the step runs
//! the game's frame and draws the screen texture into the view without a
//! readback. Sound is pulled by the host from its audio callback through a
//! [`YurisAudio`], fed by a thread of its own (`audio.rs`).
//!
//! The session is opened on a background thread and then used on the main
//! thread. The engine holds `Rc`s, so it is not `Send`; it is only ever used
//! by one thread at a time and moves as a whole, which the host guarantees
//! (see the header).
//!
//! Every entry point catches panics and reports them as errors.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use yu_backend_wgpu::{DeviceOptions, GpuContext};
use yu_engine::engine::{Backend, Engine, EngineOptions, FontChoice};
use yu_engine::patch::{PatchKind, PatchSelection};
use yu_engine::project::Project;

mod audio;
mod present;

use present::Presenter;

/// `YURIS_ABI_VERSION`.
pub const ABI_VERSION: u32 = 1;

/// `YurisConfig`.
#[repr(C)]
pub struct YurisConfig {
    pub struct_size: u32,
    pub game_dir: *const c_char,
    pub launcher: *const c_char,
    pub save_dir: *const c_char,
    pub encoding: u32,
    pub utc_offset_ms: i64,
}

/// `YurisSession`.
pub struct YurisSession {
    engine: Engine,
    gpu: GpuContext,
    presenter: Option<Presenter>,
    /// The engine's sound state, for `yuris_audio`.
    sound: audio::Slot,
    /// Screen size in pixels.
    screen: (u32, u32),
    /// Pointer position last sent, in screen pixels.
    cursor: (i64, i64),
}

/// `YurisAudio`: the sound output, fed from the session's sound state.
pub struct YurisAudio(audio::AudioOutput);

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(msg: impl Into<String>) {
    let msg = msg.into().replace('\0', " ");
    log::error!(target: "yuris-mobile", "{msg}");
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::new(msg).unwrap_or_default());
}

fn clear_error() {
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::default());
}

/// Runs `f`, turning a panic into an error and `fallback`.
fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".into());
            set_error(format!("panic: {msg}"));
            fallback
        }
    }
}

/// A borrowed C string, None for NULL or empty.
///
/// # Safety
/// `p` is NULL or a NUL-terminated string.
unsafe fn string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = CStr::from_ptr(p).to_string_lossy().into_owned();
    (!s.is_empty()).then_some(s)
}

#[no_mangle]
pub extern "C" fn yuris_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
pub extern "C" fn yuris_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

// ---- logging ----

type LogCallback = extern "C" fn(level: i32, target: *const c_char, message: *const c_char, context: *mut c_void);

struct LogSink {
    callback: LogCallback,
    context: usize,
    max_level: i32,
}

static LOG: Mutex<Option<LogSink>> = Mutex::new(None);

/// Forwards `log` records to the host's callback.
struct CallbackLogger;

fn level_number(level: log::Level) -> i32 {
    match level {
        log::Level::Error => 1,
        log::Level::Warn => 2,
        log::Level::Info => 3,
        log::Level::Debug => 4,
        log::Level::Trace => 5,
    }
}

impl log::Log for CallbackLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        // wgpu and naga log a lot through `log`; only the engine's records
        // are wanted.
        !metadata.target().starts_with("wgpu") && !metadata.target().starts_with("naga")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = level_number(record.level());
        // Copied out so the host is called without the lock held: a
        // callback that calls back into the runtime must not deadlock.
        let (callback, context) = match LOG.lock().ok().as_deref() {
            Some(Some(sink)) if level <= sink.max_level => (sink.callback, sink.context),
            _ => return,
        };
        let target = CString::new(record.target().replace('\0', " ")).unwrap_or_default();
        let msg = CString::new(record.args().to_string().replace('\0', " ")).unwrap_or_default();
        callback(level, target.as_ptr(), msg.as_ptr(), context as *mut c_void);
    }

    fn flush(&self) {}
}

static LOGGER: CallbackLogger = CallbackLogger;

#[no_mangle]
pub extern "C" fn yuris_set_log_callback(callback: Option<LogCallback>, context: *mut c_void, max_level: i32) {
    guard((), || {
        if let Ok(mut sink) = LOG.lock() {
            *sink = callback.map(|callback| LogSink { callback, context: context as usize, max_level });
        }
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Trace);
        }
        let filter = match max_level {
            i32::MIN..=0 => log::LevelFilter::Off,
            1 => log::LevelFilter::Error,
            2 => log::LevelFilter::Warn,
            3 => log::LevelFilter::Info,
            4 => log::LevelFilter::Debug,
            _ => log::LevelFilter::Trace,
        };
        log::set_max_level(if callback.is_some() { filter } else { log::LevelFilter::Off });
    })
}

// ---- fonts ----

/// Fonts for the sessions opened from now on: file and face name.
static FONTS: Mutex<Vec<(PathBuf, Option<String>)>> = Mutex::new(Vec::new());

/// # Safety
/// `path` and `face` are NULL or NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn yuris_register_font(path: *const c_char, face: *const c_char) -> i32 {
    guard(0, || {
        let Some(path) = string(path) else { return 0 };
        if !std::path::Path::new(&path).is_file() {
            set_error(format!("no font file {path}"));
            return 0;
        }
        if let Ok(mut fonts) = FONTS.lock() {
            fonts.push((PathBuf::from(path), string(face)));
        }
        1
    })
}

#[no_mangle]
pub extern "C" fn yuris_clear_fonts() {
    guard((), || {
        if let Ok(mut fonts) = FONTS.lock() {
            fonts.clear();
        }
    })
}

// ---- sessions ----

/// The local date and time `utc_offset_ms` from UTC now, for `@_TIME`.
fn local_time(utc_offset_ms: i64) -> Option<yu_rt::HostLocalTime> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?;
    let ms = now.as_millis() as i64 + utc_offset_ms;
    let (days, ms_of_day) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    yu_rt::HostLocalTime::new(
        year,
        month,
        day,
        ms_of_day / 3_600_000,
        ms_of_day / 60_000 % 60,
        ms_of_day / 1000 % 60,
        ms_of_day % 1000,
    )
}

fn create_gpu() -> Result<GpuContext, String> {
    let options = DeviceOptions {
        #[cfg(target_vendor = "apple")]
        backends: Some(wgpu::Backends::METAL),
        ..DeviceOptions::default()
    };
    GpuContext::new(&options).map_err(|d| format!("no GPU: {} {}", d.code, d.detail))
}

fn open(c: &YurisConfig) -> Result<YurisSession, String> {
    let game_dir = PathBuf::from(unsafe { string(c.game_dir) }.ok_or("no game folder")?);
    let save_dir = unsafe { string(c.save_dir) }.map(PathBuf::from).unwrap_or_else(|| game_dir.join("save"));
    std::fs::create_dir_all(&save_dir).map_err(|e| format!("cannot make {}: {e}", save_dir.display()))?;

    let mut font_bytes = Vec::new();
    let mut first_face = None;
    for (path, face) in FONTS.lock().map(|f| f.clone()).unwrap_or_default() {
        let bytes = std::fs::read(&path).map_err(|e| format!("cannot read font {}: {e}", path.display()))?;
        if font_bytes.is_empty() {
            first_face = Some(face);
        }
        font_bytes.push(bytes);
    }
    // The first registered font draws text whose font nobody registered.
    let font = match first_face {
        Some(Some(face)) => FontChoice::Family(face),
        Some(None) => FontChoice::HostFont(0),
        None => FontChoice::Script,
    };
    let patch = match c.encoding {
        0 | 2 => PatchKind::Original,
        1 => PatchKind::Gbk,
        _ => PatchKind::Auto,
    };

    let mut project = Project::discover(&game_dir, None).map_err(|f| format!("no YU-RIS data: {f}"))?;
    if let Some(exe) = unsafe { string(c.launcher) }.filter(|s| !s.is_empty()) {
        let notes = project.set_launcher(std::path::Path::new(&exe)).map_err(|f| format!("launcher {exe}: {f}"))?;
        for note in notes {
            log::warn!(target: "yuris-mobile", "launcher {exe}: {note}");
        }
    }
    let gpu = create_gpu()?;
    let options = EngineOptions {
        backend: Backend::Wgpu,
        gpu_context: Some(gpu.clone()),
        save_root: Some(save_dir),
        font_bytes,
        font,
        patch: PatchSelection::new(patch),
        local_time: local_time(c.utc_offset_ms),
        ..EngineOptions::default()
    };
    let mut engine = Engine::new(project, options).map_err(|f| format!("boot failed: {f}"))?;
    let sound: audio::Slot = Arc::new(Mutex::new(None));
    engine.attach_audio(Box::new(audio::Attach(sound.clone())));
    let viewport = engine.viewport();
    let screen = (viewport.width, viewport.height);
    log::info!(target: "yuris-mobile", "opened {} ({}x{})", game_dir.display(), screen.0, screen.1);
    Ok(YurisSession { engine, gpu, presenter: None, sound, screen, cursor: (-1, -1) })
}

/// # Safety
/// `config` points to a valid `YurisConfig` whose strings are valid for the call.
#[no_mangle]
pub unsafe extern "C" fn yuris_open(config: *const YurisConfig) -> *mut YurisSession {
    guard(std::ptr::null_mut(), || {
        clear_error();
        let Some(c) = config.as_ref() else {
            set_error("no config");
            return std::ptr::null_mut();
        };
        if (c.struct_size as usize) < std::mem::size_of::<YurisConfig>() {
            set_error("config struct_size too small");
            return std::ptr::null_mut();
        }
        match open(c) {
            Ok(session) => Box::into_raw(Box::new(session)),
            Err(e) => {
                set_error(e);
                std::ptr::null_mut()
            }
        }
    })
}

/// # Safety
/// `session` is a session from `yuris_open`; the out pointers are NULL or valid.
#[no_mangle]
pub unsafe extern "C" fn yuris_screen_size(session: *const YurisSession, width: *mut u32, height: *mut u32) {
    guard((), || {
        let Some(s) = session.as_ref() else { return };
        if let Some(p) = width.as_mut() {
            *p = s.screen.0;
        }
        if let Some(p) = height.as_mut() {
            *p = s.screen.1;
        }
    })
}

impl YurisSession {
    fn attach(&mut self, view: *mut c_void, width: u32, height: u32) -> Result<(), String> {
        if view.is_null() || width == 0 || height == 0 {
            return Err("no view or an empty size".into());
        }
        match self.presenter.as_mut() {
            Some(p) if p.view == view => p.resize(width, height),
            _ => {
                self.presenter = None;
                // SAFETY: the host keeps the view alive while attached.
                self.presenter = Some(unsafe { Presenter::new(&self.gpu, view, width, height) }?);
            }
        }
        Ok(())
    }

    fn draw(&mut self) {
        let (Some(presenter), Some((id, w, h, texture))) = (self.presenter.as_mut(), self.engine.screen_texture())
        else {
            return;
        };
        if let Err(e) = presenter.present(texture, id, w, h) {
            log::warn!(target: "yuris-mobile", "{e}");
        }
    }

    /// View pixels to screen pixels (the frame is fitted with black bars).
    fn to_screen(&self, x: f32, y: f32) -> (i64, i64) {
        let (sw, sh) = (self.screen.0.max(1) as f32, self.screen.1.max(1) as f32);
        let (vw, vh) = self.presenter.as_ref().map_or((sw, sh), |p| {
            let (w, h) = p.size();
            (w as f32, h as f32)
        });
        let scale = (vw / sw).min(vh / sh).max(f32::MIN_POSITIVE);
        let (ox, oy) = ((vw - sw * scale) / 2.0, (vh - sh * scale) / 2.0);
        (((x - ox) / scale).floor() as i64, ((y - oy) / scale).floor() as i64)
    }
}

/// # Safety
/// `session` is a session from `yuris_open`; `ui_view` a live UIView.
#[no_mangle]
pub unsafe extern "C" fn yuris_set_surface(session: *mut YurisSession, ui_view: *mut c_void, width_px: u32, height_px: u32) -> i32 {
    guard(-1, || {
        let Some(s) = session.as_mut() else {
            set_error("no session");
            return -1;
        };
        match s.attach(ui_view, width_px, height_px) {
            Ok(()) => 0,
            Err(e) => {
                set_error(e);
                -1
            }
        }
    })
}

/// # Safety
/// `session` is a session from `yuris_open`.
#[no_mangle]
pub unsafe extern "C" fn yuris_detach_surface(session: *mut YurisSession) {
    guard((), || {
        if let Some(s) = session.as_mut() {
            s.presenter = None;
        }
    })
}

/// # Safety
/// `session` is a session from `yuris_open`.
#[no_mangle]
pub unsafe extern "C" fn yuris_step(session: *mut YurisSession, _dt_ms: u32) -> i32 {
    guard(2, || {
        let Some(s) = session.as_mut() else {
            set_error("no session");
            return 2;
        };
        // The engine keeps its own wall clock (script time is real time,
        // as on Windows); the host's frame time is not needed.
        if let Err(e) = s.engine.tick_realtime() {
            set_error(e.to_string());
            return 2;
        }
        s.draw();
        if s.engine.main_finished() {
            1
        } else {
            0
        }
    })
}

/// # Safety
/// `session` is a session from `yuris_open`.
#[no_mangle]
pub unsafe extern "C" fn yuris_pointer(session: *mut YurisSession, phase: i32, x: f32, y: f32) {
    guard((), || {
        let Some(s) = session.as_mut() else { return };
        let cursor = s.to_screen(x, y);
        if cursor != s.cursor {
            s.cursor = cursor;
            s.engine.set_cursor(cursor.0, cursor.1);
        }
        match phase {
            1 => s.engine.set_input("MOUSE_L", true),
            2 => s.engine.set_input("MOUSE_L", false),
            3 => s.engine.set_input("MOUSE_R", true),
            4 => s.engine.set_input("MOUSE_R", false),
            _ => {}
        }
    })
}

/// # Safety
/// `session` is a session from `yuris_open`.
#[no_mangle]
pub unsafe extern "C" fn yuris_wheel(session: *mut YurisSession, notches: i32) {
    guard((), || {
        let Some(s) = session.as_mut() else { return };
        let button = if notches > 0 { "MOUSE_U" } else { "MOUSE_D" };
        // One pulse per frame is what a script sees; extra notches are
        // the same scroll.
        if notches != 0 {
            s.engine.pulse_input(button);
        }
    })
}

/// The engine's name for a Windows virtual-key code.
fn key_name(vk: i32) -> Option<String> {
    let name = match vk {
        0x08 => "BS".to_string(),
        0x09 => "TAB".to_string(),
        0x0D => "ENTER".to_string(),
        0x10 => "SHIFT".to_string(),
        0x11 => "CTRL".to_string(),
        0x12 => "ALT".to_string(),
        0x1B => "ESC".to_string(),
        0x20 => "SPACE".to_string(),
        0x21 => "PAGEUP".to_string(),
        0x22 => "PAGEDOWN".to_string(),
        0x23 => "END".to_string(),
        0x24 => "HOME".to_string(),
        0x25 => "LEFT".to_string(),
        0x26 => "UP".to_string(),
        0x27 => "RIGHT".to_string(),
        0x28 => "DOWN".to_string(),
        0x2D => "INSERT".to_string(),
        0x2E => "DELETE".to_string(),
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x60..=0x69 => format!("NUM{}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => return None,
    };
    yu_rt::input::button_code(&name)?;
    Some(format!("KEY_{name}"))
}

/// # Safety
/// `session` is a session from `yuris_open`.
#[no_mangle]
pub unsafe extern "C" fn yuris_key(session: *mut YurisSession, vk: i32, down: i32) {
    guard((), || {
        let Some(s) = session.as_mut() else { return };
        if let Some(name) = key_name(vk) {
            s.engine.set_input(&name, down != 0);
        }
    })
}

/// # Safety
/// `session` is a session from `yuris_open`.
#[no_mangle]
pub unsafe extern "C" fn yuris_audio(session: *const YurisSession) -> *mut YurisAudio {
    guard(std::ptr::null_mut(), || match session.as_ref() {
        Some(s) => match audio::AudioOutput::new(s.sound.clone()) {
            Ok(out) => Box::into_raw(Box::new(YurisAudio(out))),
            Err(e) => {
                set_error(format!("no audio thread: {e}"));
                std::ptr::null_mut()
            }
        },
        None => std::ptr::null_mut(),
    })
}

#[no_mangle]
pub extern "C" fn yuris_audio_sample_rate() -> u32 {
    audio::RATE
}

/// # Safety
/// `audio` is from `yuris_audio`, used by one thread at a time; `out`
/// holds `2 * frames` floats.
#[no_mangle]
pub unsafe extern "C" fn yuris_audio_render(audio: *mut YurisAudio, out: *mut f32, frames: u32) {
    if out.is_null() {
        return;
    }
    let buf = std::slice::from_raw_parts_mut(out, 2 * frames as usize);
    let ok = catch_unwind(AssertUnwindSafe(|| match audio.as_mut() {
        Some(a) => {
            a.0.render(buf);
            true
        }
        None => false,
    }));
    if !matches!(ok, Ok(true)) {
        buf.fill(0.0);
    }
}

/// # Safety
/// `audio` is from `yuris_audio`, released once.
#[no_mangle]
pub unsafe extern "C" fn yuris_audio_release(audio: *mut YurisAudio) {
    if !audio.is_null() {
        drop(Box::from_raw(audio));
    }
}

/// # Safety
/// `session` is from `yuris_open`, closed once.
#[no_mangle]
pub unsafe extern "C" fn yuris_close(session: *mut YurisSession) {
    guard((), || {
        if !session.is_null() {
            let mut s = Box::from_raw(session);
            // The surface goes before the device it was made from.
            s.presenter = None;
            drop(s);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_time_is_a_real_date() {
        let t = local_time(9 * 3_600_000);
        assert!(t.is_some());
    }

    #[test]
    fn keys_map_to_engine_names() {
        assert_eq!(key_name(0x0D).as_deref(), Some("KEY_ENTER"));
        assert_eq!(key_name(0x11).as_deref(), Some("KEY_CTRL"));
        assert_eq!(key_name(0x41).as_deref(), Some("KEY_A"));
        assert_eq!(key_name(0x0F), None);
    }
}

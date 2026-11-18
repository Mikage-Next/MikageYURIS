//! Sound for hosts that pull it from a real-time audio callback.
//!
//! The callback must not wait on a lock or allocate: the game thread holds
//! the sound state's lock while it opens sounds, and mixing decodes Vorbis,
//! which allocates. So a feeder thread of our own mixes ahead into a
//! lock-free ring and the callback only copies out of it. While the host's
//! audio is stopped the ring stays full and the feeder waits, so the
//! play positions wait with it (the sound state runs on an external clock:
//! positions advance by what was mixed).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use yu_engine::engine::AudioOut;
use yu_sound::SoundState;

/// Output rate (stereo float).
pub const RATE: u32 = 44_100;
/// Frames mixed per lock of the sound state.
const CHUNK: usize = 256;
/// Frames held ahead of the callback: about 93 ms. Sound positions the
/// game reads run this far ahead of what is heard.
const AHEAD: usize = 4096;

/// Where the engine puts its sound state; swapped on a restart.
pub type Slot = Arc<Mutex<Option<Arc<Mutex<SoundState>>>>>;

/// The engine's end of the slot.
pub struct Attach(pub Slot);

impl AudioOut for Attach {
    fn attach(&mut self, state: Arc<Mutex<SoundState>>) {
        if let Ok(mut guard) = state.lock() {
            guard.set_external_clock(true);
        }
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(state);
        }
    }

    fn detach(&mut self) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = None;
        }
    }
}

/// The feeder thread and the ring's reading end.
pub struct AudioOutput {
    ring: rtrb::Consumer<f32>,
    stop: Arc<AtomicBool>,
    feeder: Option<JoinHandle<()>>,
}

impl AudioOutput {
    /// Starts feeding from whatever state is in `slot`.
    pub fn new(slot: Slot) -> std::io::Result<Self> {
        let (mut producer, ring) = rtrb::RingBuffer::<f32>::new(2 * AHEAD);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let feeder = std::thread::Builder::new().name("yuris-audio".into()).spawn(move || {
            let mut buf = vec![0.0f32; 2 * CHUNK];
            while !stopping.load(Ordering::Relaxed) {
                if producer.slots() < buf.len() {
                    std::thread::sleep(Duration::from_millis(3));
                    continue;
                }
                let state = slot.lock().ok().and_then(|s| s.clone());
                match state {
                    Some(state) => match state.lock() {
                        Ok(mut s) => s.render_and_advance(&mut buf, RATE, 2),
                        Err(p) => p.into_inner().render_and_advance(&mut buf, RATE, 2),
                    },
                    None => buf.fill(0.0),
                }
                if let Ok(chunk) = producer.write_chunk_uninit(buf.len()) {
                    chunk.fill_from_iter(buf.iter().copied());
                }
            }
        })?;
        Ok(Self { ring, stop, feeder: Some(feeder) })
    }

    /// Fills `out` (interleaved stereo) from the ring; what the feeder has
    /// not mixed yet is silence. Never blocks or allocates.
    pub fn render(&mut self, out: &mut [f32]) {
        let n = self.ring.slots().min(out.len()) & !1;
        let mut done = 0;
        if let Ok(chunk) = self.ring.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            out[..a.len()].copy_from_slice(a);
            out[a.len()..a.len() + b.len()].copy_from_slice(b);
            done = a.len() + b.len();
            chunk.commit_all();
        }
        out[done..].fill(0.0);
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.feeder.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_without_state_and_no_waiting() {
        let slot: Slot = Arc::new(Mutex::new(None));
        let mut out = AudioOutput::new(slot.clone()).unwrap();
        let mut buf = vec![1.0f32; 512];
        for _ in 0..20 {
            out.render(&mut buf);
            assert!(buf.iter().all(|&s| s == 0.0));
            std::thread::sleep(Duration::from_millis(5));
        }
        // The callback never waits on the slot's lock.
        let held = slot.lock().unwrap();
        let t = std::time::Instant::now();
        out.render(&mut buf);
        assert!(t.elapsed() < Duration::from_millis(50));
        drop(held);
        drop(out);
    }
}

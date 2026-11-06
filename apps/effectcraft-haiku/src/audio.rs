//! Audio preview output through the Media Kit: a `BSoundPlayer` node (`native/audio.cpp`) playing
//! into the system mixer, which converts to the output device's rate and format. It replaces the
//! desktop app's cpal output (`apps/effectcraft/src/audio_out.rs`); the mixer and the Media
//! preferences pick the device, so Settings ▸ Audio lists the one output it has. The node's
//! callback pulls interleaved stereo from the frontend's [`AudioFeed`] and maps it onto the
//! node's channels (mono: L+R average; more than two: left and right where Settings ▸ Audio puts
//! them, silence elsewhere).
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_void};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use effectcraft_ui_egui::audio::{AudioDevice, AudioFeed, AudioOutput};

/// The one output: the Media Kit's system mixer.
pub const DEVICE: &str = "Media Kit (system mixer)";

type Fill = unsafe extern "C" fn(*mut c_void, *mut f32, usize, usize, u32);

unsafe extern "C" {
    fn ec_audio_open(rate: u32, name: *const c_char, cookie: *mut c_void, fill: Fill) -> *mut c_void;
    fn ec_audio_rate(handle: *mut c_void) -> u32;
    fn ec_audio_start(handle: *mut c_void) -> i32;
    fn ec_audio_latency(handle: *mut c_void) -> i64;
    fn ec_audio_close(handle: *mut c_void);
}

/// What the node's callback reads. Lives in a `Box` the node points at until it is closed.
struct Cookie {
    feed: Mutex<Option<Arc<AudioFeed>>>,
    /// Device channels (0-based) for left and right.
    map: (usize, usize),
    stereo: Mutex<Vec<f32>>,
    panicked: AtomicBool,
    /// Frames played and the loudest sample since the node started (logged when it stops).
    played: std::sync::atomic::AtomicU64,
    peak: std::sync::atomic::AtomicU32,
}

/// The node's callback (Media Kit thread): fill `count` floats of `channels`-channel audio.
unsafe extern "C" fn fill(cookie: *mut c_void, samples: *mut f32, count: usize, channels: usize, _rate: u32) {
    if cookie.is_null() || samples.is_null() || channels == 0 {
        return;
    }
    // SAFETY: the cookie is the `Box<Cookie>` this node was opened with, alive until
    // `ec_audio_close` has joined this thread; the buffer holds `count` floats (zeroed).
    let (cookie, out) = unsafe { (&*cookie.cast::<Cookie>(), std::slice::from_raw_parts_mut(samples, count)) };
    // A panic must not unwind into the Media Kit.
    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mix(cookie, out, channels))).is_ok();
    if !ok && !cookie.panicked.swap(true, Ordering::Relaxed) {
        log::error!("audio output: the mixing callback failed; preview audio is silent");
    }
}

fn mix(cookie: &Cookie, out: &mut [f32], ch: usize) {
    let Some(feed) = cookie.feed.lock().unwrap_or_else(PoisonError::into_inner).clone() else { return };
    let frames = out.len() / ch;
    let mut stereo = cookie.stereo.lock().unwrap_or_else(PoisonError::into_inner);
    stereo.resize(frames * 2, 0.0);
    feed.pull(&mut stereo);
    cookie.played.fetch_add(frames as u64, Ordering::Relaxed);
    let peak = stereo.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    cookie.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
    let (left, right) = if cookie.map.0 < ch && cookie.map.1 < ch { cookie.map } else { (0, 1.min(ch - 1)) };
    for (f, s) in out.chunks_mut(ch).zip(stereo.as_chunks::<2>().0.iter()) {
        for (c, o) in f.iter_mut().enumerate() {
            let v = if ch == 1 || (c == left && c == right) {
                (s[0] + s[1]) * 0.5
            } else if c == left {
                s[0]
            } else if c == right {
                s[1]
            } else {
                0.0
            };
            *o = v.clamp(-1.0, 1.0);
        }
    }
}

pub struct MediaKitOut {
    rate: u32,
    cookie: Box<Cookie>,
    handle: Option<NonNull<c_void>>,
    /// Started (the node is closed on stop and opened again for the next start).
    playing: bool,
}

// SAFETY: the handle is only used through this value (the frontend moves it to its audio thread);
// the Media Kit node itself is thread-safe and its callback only touches the `Mutex`es in the
// cookie.
unsafe impl Send for MediaKitOut {}

/// The output list of Settings ▸ Audio.
pub fn devices() -> Vec<String> {
    vec![DEVICE.into()]
}

fn open_node(rate: u32, cookie: &Cookie) -> Option<NonNull<c_void>> {
    let name = CString::new("EffectCraft preview").unwrap_or_default();
    // SAFETY: a valid C string (copied by the node) and a cookie that outlives the node
    // (`MediaKitOut` closes it before the box goes).
    NonNull::new(unsafe { ec_audio_open(rate, name.as_ptr(), std::ptr::from_ref(cookie).cast_mut().cast(), fill) })
}

/// Opens the output (`None` without the media server or an output).
pub fn open(out: &AudioOutput) -> Option<Box<dyn AudioDevice>> {
    let map = ((out.left.max(1) - 1) as usize, (out.right.max(1) - 1) as usize);
    let cookie = Box::new(Cookie {
        feed: Mutex::new(None),
        map,
        stereo: Mutex::new(Vec::new()),
        panicked: AtomicBool::new(false),
        played: Default::default(),
        peak: Default::default(),
    });
    let handle = open_node(if out.rate > 0 { out.rate } else { 48_000 }, &cookie);
    let Some(handle) = handle else {
        log::warn!("audio output: the Media Kit has no output (is the media server running?)");
        return None;
    };
    // SAFETY: an open node.
    let rate = unsafe { ec_audio_rate(handle.as_ptr()) };
    let rate = if rate > 0 { rate } else { 48_000 };
    log::info!("audio output: Media Kit sound player, {rate} Hz");
    Some(Box::new(MediaKitOut { rate, cookie, handle: Some(handle), playing: false }))
}

impl AudioDevice for MediaKitOut {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn start(&mut self, feed: Arc<AudioFeed>) -> Result<(), String> {
        if self.playing {
            self.stop();
        }
        let handle = match self.handle {
            Some(h) => h,
            None => open_node(self.rate, &self.cookie).ok_or("the Media Kit sound player couldn't open")?,
        };
        self.handle = Some(handle);
        *self.cookie.feed.lock().unwrap_or_else(PoisonError::into_inner) = Some(feed);
        // SAFETY: an open node.
        if unsafe { ec_audio_start(handle.as_ptr()) } != 0 {
            self.stop();
            return Err("the Media Kit sound player couldn't start".into());
        }
        self.playing = true;
        Ok(())
    }

    fn stop(&mut self) {
        self.playing = false;
        if let Some(h) = self.handle.take() {
            // SAFETY: an open node; this joins its callback thread before the cookie is touched.
            unsafe { ec_audio_close(h.as_ptr()) };
            let played = self.cookie.played.swap(0, Ordering::Relaxed);
            let peak = f32::from_bits(self.cookie.peak.swap(0, Ordering::Relaxed));
            if played > 0 {
                log::info!("audio output: played {played} frames, peak {:.1} dBFS", 20.0 * peak.max(1e-6).log10());
            }
        }
        *self.cookie.feed.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn latency_frames(&self) -> u64 {
        let Some(h) = self.handle else { return 0 };
        // SAFETY: an open node.
        let us = unsafe { ec_audio_latency(h.as_ptr()) };
        u64::try_from(us).unwrap_or(0).saturating_mul(u64::from(self.rate)) / 1_000_000
    }
}

impl Drop for MediaKitOut {
    fn drop(&mut self) {
        self.stop();
    }
}

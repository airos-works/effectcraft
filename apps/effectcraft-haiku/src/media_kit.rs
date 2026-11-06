//! Footage the Media Kit decodes (`native/media.cpp`): movies and sounds EffectCraft's own
//! decoders (FilmCraft's codecs) can't read, such as MPEG-1/2, MPEG-4 Part 2, Windows Media,
//! Theora, VP8, Vorbis or FLAC, through the system's codec plugins (the ffmpeg plugin, and the
//! hardware decoders air/OS has for NVIDIA, AMD and Rockchip video where they are installed).
//!
//! - **Import** ([`KitImporter`]): the app's own probe first; a file it refuses is probed by the
//!   Media Kit, and its footage's codec says so (`… · Media Kit`), which is how it is recognised
//!   again when a saved project is opened.
//! - **Frames and sound** ([`KitFootage`]): footage of that kind is decoded here (8-bit RGB
//!   frames, the audio track once into memory); everything else goes to the app's own
//!   footage source unchanged.
//! - `EFFECTCRAFT_MEDIA_KIT=0` turns it off; `EFFECTCRAFT_MEDIA_KIT=prefer` sends every movie
//!   the Media Kit can decode through it first (a hardware decoder plugin then decodes H.264 or
//!   HEVC), at the cost of what the app's decoders read and the Media Kit doesn't: high bit
//!   depths, colour profiles, timecode, alpha.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};

use effectcraft_engine::Importer;
use effectcraft_project::{AlphaMode, Footage, FootageKind, ItemId};
use effectcraft_raster::Image;
use effectcraft_render::{FloatCurve, FootageSource};
use effectcraft_time::{FrameRate, Tick};

/// What a Media Kit footage's codec ends with.
pub const MARK: &str = " · Media Kit";
/// The most audio kept in memory per file: 64 Mi samples (256 MB of floats; 11.6 minutes of
/// 48 kHz stereo).
const MAX_SAMPLES: i64 = 64 << 20;
/// The most frames decoded forward from a key frame to reach the one asked for.
const MAX_FORWARD: usize = 1200;

#[repr(C)]
struct RawInfo {
    container: [c_char; 32],
    duration_us: i64,
    has_video: i32,
    video_decodable: i32,
    width: i32,
    height: i32,
    fps: f32,
    frames: i64,
    video_codec: [c_char; 96],
    has_audio: i32,
    audio_decodable: i32,
    channels: i32,
    rate: f32,
    audio_codec: [c_char; 96],
}

unsafe extern "C" {
    fn ec_media_probe(path: *const c_char, info: *mut RawInfo) -> i32;
    fn ec_video_open(path: *const c_char, width: *mut i32, height: *mut i32) -> *mut c_void;
    fn ec_video_next(handle: *mut c_void, rgba: *mut u8, length: usize, time: *mut i64) -> i32;
    fn ec_video_seek(handle: *mut c_void, time: i64) -> i64;
    fn ec_video_close(handle: *mut c_void);
    fn ec_audio_decode(path: *const c_char, max_frames: i64, samples: *mut *mut f32, channels: *mut i32, rate: *mut f32) -> i64;
    fn free(p: *mut c_void);
}

/// How the Media Kit is used (`EFFECTCRAFT_MEDIA_KIT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    Off,
    /// For what the app's decoders refuse (the default).
    Fallback,
    /// For every movie it decodes, before the app's decoders.
    Prefer,
}

impl Use {
    pub fn from_env(v: Option<&str>) -> Use {
        match v.map(str::trim) {
            Some("0" | "off" | "no") => Use::Off,
            Some("prefer") => Use::Prefer,
            _ => Use::Fallback,
        }
    }
}

fn c_path(path: &str) -> Option<CString> {
    CString::new(std::path::Path::new(path).as_os_str().as_bytes()).ok()
}

fn text(field: &[c_char]) -> String {
    if !field.contains(&0) {
        return String::new();
    }
    // SAFETY: NUL-terminated within the array (checked above).
    unsafe { CStr::from_ptr(field.as_ptr()) }.to_string_lossy().into_owned()
}

pub fn is_kit(footage: &Footage) -> bool {
    footage.codec.ends_with(MARK)
}

/// The footage the Media Kit makes of `path`, if it decodes its picture or its sound.
pub fn probe(path: &str) -> Option<Footage> {
    let p = c_path(path)?;
    // SAFETY: an all-integer C struct, which the host fills in.
    let mut info: RawInfo = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a writable info.
    if unsafe { ec_media_probe(p.as_ptr(), &mut info) } != 0 {
        return None;
    }
    let video = info.has_video != 0 && info.video_decodable != 0 && info.width > 0 && info.height > 0;
    let audio = info.has_audio != 0 && info.audio_decodable != 0;
    if !video && !audio {
        return None;
    }
    let codec = match (video, text(&info.video_codec), text(&info.audio_codec)) {
        (true, v, _) if !v.is_empty() => v,
        (false, _, a) if !a.is_empty() => a,
        _ => "Media Kit codec".into(),
    };
    let container = text(&info.container);
    let fps = f64::from(info.fps);
    let rate = if fps.is_finite() && fps > 0.5 && fps < 1000.0 { FrameRate::from_f64(fps) } else { FrameRate::FPS_30 };
    let seconds = info.duration_us.max(0) as f64 / 1e6;
    Some(Footage {
        path: path.to_string(),
        kind: if video { FootageKind::Video } else { FootageKind::Audio },
        width: if video { info.width.unsigned_abs() } else { 0 },
        height: if video { info.height.unsigned_abs() } else { 0 },
        pixel_aspect: 1.0,
        frame_rate: rate,
        duration: Tick::from_seconds_f64(seconds),
        has_video: video,
        has_audio: audio,
        alpha: AlphaMode::Ignore,
        loop_count: 1,
        codec: if container.is_empty() { format!("{codec}{MARK}") } else { format!("{codec} ({container}){MARK}") },
        ..Default::default()
    })
}

/// The app's importer, with the Media Kit for what it refuses (see the module docs).
pub struct KitImporter {
    pub inner: Arc<dyn Importer>,
    pub mode: Use,
}

impl Importer for KitImporter {
    fn probe(&self, path: &str) -> Result<Footage, String> {
        if self.mode == Use::Prefer
            && let Some(f) = probe(path).filter(|f| f.has_video)
        {
            return Ok(f);
        }
        match self.inner.probe(path) {
            Ok(f) => Ok(f),
            Err(e) if self.mode != Use::Off => match probe(path) {
                Some(f) => {
                    log::info!(target: "effectcraft_haiku", "{path}: {e}; the Media Kit reads it ({})", f.codec);
                    Ok(f)
                }
                None => Err(e),
            },
            Err(e) => Err(e),
        }
    }

    fn register(&self, path: &str, data: &[u8]) {
        self.inner.register(path, data);
    }

    fn add_frames(&self, paths: &[String]) {
        self.inner.add_frames(paths);
    }
}

/// An open video decoder and the frame it decoded last.
struct Video {
    handle: *mut c_void,
    width: u32,
    height: u32,
    /// The time of the frame decoded last (microseconds) and the frame.
    last: Option<(i64, Arc<Image>)>,
    rgba: Vec<u8>,
}

// SAFETY: the decoder is used by one thread at a time (the `Mutex` around it).
unsafe impl Send for Video {}

impl Drop for Video {
    fn drop(&mut self) {
        // SAFETY: an open handle, closed once.
        unsafe { ec_video_close(self.handle) }
    }
}

impl Video {
    fn open(path: &str) -> Option<Video> {
        let p = c_path(path)?;
        let (mut w, mut h) = (0, 0);
        // SAFETY: a valid C string and two writable ints.
        let handle = unsafe { ec_video_open(p.as_ptr(), &mut w, &mut h) };
        if handle.is_null() {
            return None;
        }
        let (width, height) = (w.unsigned_abs(), h.unsigned_abs());
        let rgba = vec![0; width as usize * height as usize * 4];
        Some(Video { handle, width, height, last: None, rgba })
    }

    /// Decodes the next frame: its time and pixels, `None` at the end or on an error.
    fn next(&mut self) -> Option<(i64, Arc<Image>)> {
        let mut time = 0i64;
        // SAFETY: an open handle and a buffer of width × height × 4 bytes.
        let r = unsafe { ec_video_next(self.handle, self.rgba.as_mut_ptr(), self.rgba.len(), &mut time) };
        if r != 1 {
            return None;
        }
        let frame = (time, Arc::new(Image::from_rgba8(self.width, self.height, &self.rgba)));
        self.last = Some(frame.clone());
        Some(frame)
    }

    /// The frame showing at `at` (microseconds), one frame lasting `frame_us`.
    fn frame(&mut self, at: i64, frame_us: i64, cancel: &dyn Fn() -> bool) -> Option<Arc<Image>> {
        let half = frame_us / 2;
        if let Some((t, img)) = &self.last
            && (at - *t).abs() <= half
        {
            return Some(img.clone());
        }
        // Forward from the last frame when it is close; else from the key frame before.
        let near = self.last.as_ref().is_some_and(|(t, _)| at > *t && at - *t < 2_000_000);
        if !near {
            // SAFETY: an open handle.
            unsafe { ec_video_seek(self.handle, at) };
            self.last = None;
        }
        let mut best: Option<Arc<Image>> = None;
        for _ in 0..MAX_FORWARD {
            if cancel() {
                return None;
            }
            let Some((t, img)) = self.next() else { break };
            if t + half >= at {
                return Some(img);
            }
            best = Some(img);
        }
        best
    }
}

/// A file's decoded sound: interleaved samples, channels, rate.
struct Sound {
    samples: Vec<f32>,
    channels: usize,
    rate: f64,
}

fn decode_sound(path: &str) -> Option<Sound> {
    let p = c_path(path)?;
    let (mut ptr, mut channels, mut rate) = (std::ptr::null_mut::<f32>(), 0i32, 0f32);
    // SAFETY: a valid C string and writable outputs; the samples come back malloc'd.
    let frames = unsafe { ec_audio_decode(p.as_ptr(), MAX_SAMPLES / 2, &mut ptr, &mut channels, &mut rate) };
    if frames <= 0 || ptr.is_null() || channels <= 0 {
        return None;
    }
    let n = usize::try_from(frames).ok()?.saturating_mul(channels.unsigned_abs() as usize);
    // SAFETY: the host wrote `frames × channels` floats at `ptr`.
    let samples = unsafe { std::slice::from_raw_parts(ptr, n) }.to_vec();
    // SAFETY: allocated by malloc in the host.
    unsafe { free(ptr.cast()) };
    Some(Sound { samples, channels: channels.unsigned_abs() as usize, rate: f64::from(rate) })
}

/// `frames` stereo frames at `rate` from sample `first` (at `rate`) of `sound`.
fn stereo(sound: &Sound, first: i64, frames: usize, rate: u32) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    let ch = sound.channels.max(1);
    let len = sound.samples.len() / ch;
    if len == 0 || rate == 0 || !(sound.rate > 0.0) {
        return out;
    }
    let step = sound.rate / f64::from(rate);
    let at = |i: usize, c: usize| sound.samples.get(i * ch + c.min(ch - 1)).copied().unwrap_or(0.0);
    for (k, o) in out.chunks_exact_mut(2).enumerate() {
        let pos = (first as f64 + k as f64) * step;
        if pos < 0.0 || pos >= len as f64 {
            continue;
        }
        let i = pos as usize;
        let f = (pos - i as f64) as f32;
        let j = (i + 1).min(len - 1);
        for (c, v) in o.iter_mut().enumerate() {
            *v = at(i, c) * (1.0 - f) + at(j, c) * f;
        }
    }
    out
}

/// The app's footage source, with the Media Kit's footage decoded here (see the module docs).
pub struct KitFootage {
    pub inner: Arc<dyn FootageSource>,
    videos: Mutex<HashMap<String, Arc<Mutex<Option<Video>>>>>,
    sounds: Mutex<HashMap<String, Option<Arc<Sound>>>>,
}

thread_local! {
    /// The `cancel` flag of the read this thread is in (`FootageSource::cancellable`).
    static CANCEL: std::cell::RefCell<Option<Arc<AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

impl KitFootage {
    pub fn new(inner: Arc<dyn FootageSource>) -> Self {
        Self { inner, videos: Mutex::default(), sounds: Mutex::default() }
    }

    fn kit_frame(&self, footage: &Footage, t: Tick) -> Option<Arc<Image>> {
        if !footage.has_video {
            return None;
        }
        let slot = self.videos.lock().unwrap_or_else(PoisonError::into_inner).entry(footage.path.clone()).or_default().clone();
        let mut video = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if video.is_none() {
            *video = Video::open(&footage.path);
        }
        let v = video.as_mut()?;
        let rate = footage.native_rate.unwrap_or(footage.frame_rate);
        let frame_us = (1e6 / rate.as_f64().max(1.0)) as i64;
        // The frame's own start, as the app's decoders pick it.
        let n = rate.frame_at(t);
        let at = (n as f64 * 1e6 / rate.as_f64().max(1.0)) as i64;
        let cancel = || CANCEL.with_borrow(|c| c.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)));
        v.frame(at.max(0), frame_us.max(1), &cancel)
    }

    fn kit_audio(&self, footage: &Footage, t: Tick, frames: usize, rate: u32) -> Option<Vec<f32>> {
        if !footage.has_audio {
            return None;
        }
        let sound = {
            let mut sounds = self.sounds.lock().unwrap_or_else(PoisonError::into_inner);
            sounds.entry(footage.path.clone()).or_insert_with(|| decode_sound(&footage.path).map(Arc::new)).clone()
        }?;
        Some(stereo(&sound, t.to_units_floor(i64::from(rate)), frames, rate))
    }
}

impl FootageSource for KitFootage {
    fn frame(&self, item: ItemId, footage: &Footage, t: Tick) -> Option<Arc<Image>> {
        if is_kit(footage) { self.kit_frame(footage, t) } else { self.inner.frame(item, footage, t) }
    }

    fn frame_at_size(&self, item: ItemId, footage: &Footage, t: Tick, width: u32, height: u32) -> Option<Arc<Image>> {
        if is_kit(footage) { self.kit_frame(footage, t) } else { self.inner.frame_at_size(item, footage, t, width, height) }
    }

    fn frame_with(&self, item: ItemId, footage: &Footage, t: Tick, size: Option<(u32, u32)>, float: FloatCurve) -> Option<Arc<Image>> {
        if is_kit(footage) { self.kit_frame(footage, t) } else { self.inner.frame_with(item, footage, t, size, float) }
    }

    fn audio(&self, item: ItemId, footage: &Footage, t: Tick, frames: usize, rate: u32) -> Option<Vec<f32>> {
        if is_kit(footage) { self.kit_audio(footage, t, frames, rate) } else { self.inner.audio(item, footage, t, frames, rate) }
    }

    fn aux(&self, item: ItemId, footage: &Footage, t: Tick) -> Option<Arc<effectcraft_raster::AuxChannels>> {
        self.inner.aux(item, footage, t)
    }

    fn set_aux_footage(&self, paths: &[String]) {
        self.inner.set_aux_footage(paths);
    }

    fn model(&self, item: ItemId, footage: &Footage) -> Option<Arc<effectcraft_model::Model>> {
        self.inner.model(item, footage)
    }

    fn set_cache_budget(&self, bytes: usize) {
        self.inner.set_cache_budget(bytes);
    }

    fn purge(&self) {
        self.videos.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.sounds.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.inner.purge();
    }

    fn forget(&self, path: &str) {
        self.videos.lock().unwrap_or_else(PoisonError::into_inner).remove(path);
        self.sounds.lock().unwrap_or_else(PoisonError::into_inner).remove(path);
        self.inner.forget(path);
    }

    fn set_conform_folder(&self, folder: Option<std::path::PathBuf>) {
        self.inner.set_conform_folder(folder);
    }

    fn cache_budget(&self) -> Option<usize> {
        self.inner.cache_budget()
    }

    fn vector_frame(&self, item: ItemId, footage: &Footage, scale: f64) -> Option<Arc<Image>> {
        self.inner.vector_frame(item, footage, scale)
    }

    fn cancellable(&self, cancel: &Arc<AtomicBool>, f: &mut dyn FnMut()) {
        let before = CANCEL.with_borrow_mut(|c| c.replace(cancel.clone()));
        self.inner.cancellable(cancel, f);
        CANCEL.with_borrow_mut(|c| *c = before);
    }

    fn called_off(&self) -> bool {
        self.inner.called_off()
    }

    fn problem(&self, footage: &Footage) -> Option<String> {
        if is_kit(footage) { None } else { self.inner.problem(footage) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_media_kit_setting() {
        assert_eq!(Use::from_env(None), Use::Fallback);
        assert_eq!(Use::from_env(Some("0")), Use::Off);
        assert_eq!(Use::from_env(Some("prefer")), Use::Prefer);
    }

    #[test]
    fn sound_is_resampled_and_mixed_to_stereo() {
        let mono = Sound { samples: vec![0.0, 1.0, 0.0, -1.0], channels: 1, rate: 100.0 };
        assert_eq!(stereo(&mono, 1, 2, 100), vec![1.0, 1.0, 0.0, 0.0]);
        // Twice the rate: every other output frame falls between two samples.
        assert_eq!(stereo(&mono, 0, 3, 200), vec![0.0, 0.0, 0.5, 0.5, 1.0, 1.0]);
        // Past the end, and before the start: silence.
        assert_eq!(stereo(&mono, 10, 1, 100), vec![0.0, 0.0]);
        assert_eq!(stereo(&mono, -5, 1, 100), vec![0.0, 0.0]);
        let quad = Sound { samples: vec![0.1, 0.2, 0.3, 0.4], channels: 4, rate: 48_000.0 };
        assert_eq!(stereo(&quad, 0, 1, 48_000), vec![0.1, 0.2]);
    }
}

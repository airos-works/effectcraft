//! The interface thread's event loop: Haiku's input goes to the window it happened in
//! (`viewports`, which gives every egui viewport a window of its own: the floating panels' and
//! the video preview's), EffectCraft runs its frame (eframe's `logic` and `ui`) in the root
//! viewport's pass, the main window's, and egui's output goes back to the windows and the system.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::gpu_failure::GpuFailureBridge;
use effectcraft_ui_egui::integration::{App as _, Frame, PointerWarp};
use egui::{ViewportCommand, ViewportId, pos2};

use crate::health::{DeviceHealth, Fault};
use crate::host::{self, RawEvent, kind};
use crate::platform::Haiku;
use crate::services;
use crate::viewports::{Viewports, WindowInput};

const LOG: &str = "effectcraft_haiku";

/// How the windows and the compositor draw (`EFFECTCRAFT_RENDERER`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renderer {
    /// Vulkan on a hardware adapter, else the software rasteriser.
    Auto,
    /// The software rasteriser, and compositing on the processor.
    Cpu,
    /// Vulkan, or the launch fails.
    Vulkan,
}

impl Renderer {
    /// `EFFECTCRAFT_RENDERER`; without it, Settings ▸ Startup & Repair ▸ Window Graphics: its
    /// OpenGL choice (for drivers that crash) means software here, as air/OS has no OpenGL wgpu.
    pub fn choose(env: Option<&str>, window_graphics: &str) -> Result<Renderer, String> {
        match env.map(str::trim) {
            None | Some("") if window_graphics == "gl" => Ok(Renderer::Cpu),
            None | Some("" | "auto") => Ok(Renderer::Auto),
            Some("cpu") => Ok(Renderer::Cpu),
            Some("vulkan") => Ok(Renderer::Vulkan),
            Some(other) => Err(format!("EFFECTCRAFT_RENDERER must be auto, cpu or vulkan, not `{other}`")),
        }
    }
}

/// A document Tracker (or a later launch) asked to open, handed to the interface as a dropped
/// file: a project opens (asking about unsaved changes first), media is imported, as the macOS
/// app does with the Finder's.
#[derive(Debug)]
struct Opened(PathBuf);

impl egui::DroppedFile for Opened {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

pub struct Shell {
    pub app: EffectcraftApp,
    ctx: egui::Context,
    viewports: Viewports<Haiku>,
    /// The main window (the root viewport's), with the menu bar.
    root: i32,
    /// The shared device's health while the windows draw with Vulkan.
    health: Option<DeviceHealth>,
    failures: GpuFailureBridge,
    frame: Frame,
    /// The interface scale (the system font against 12 pt) and device pixels per logical pixel
    /// ([`host::device_scale`]), and when the latter was last asked.
    ui_scale: f32,
    dev: f32,
    dev_checked: Instant,
    quit: bool,
    menu: Option<crate::native_menu::NativeBar<crate::native_menu::HostSink>>,
    /// Documents to open once the interface is ready.
    opened: Vec<PathBuf>,
    /// The project file and revision last saved, to give a new save its file type.
    saved: (Option<String>, u64),
    /// The first frame was drawn (logged once).
    drawn: bool,
    /// `EFFECTCRAFT_TEST_GPU_FAULT=<seconds>`: when to fake a GPU fault, to test the switch to
    /// software rendering without a driver that loses its device.
    test_fault: Option<Instant>,
}

impl Shell {
    /// The shell around `app`, whose main window (the root viewport's) is native window `root`.
    pub fn new(mut app: EffectcraftApp, ctx: egui::Context, root: i32, ui_scale: f32, renderer: Renderer) -> Result<Self, String> {
        let failures = GpuFailureBridge::new(&ctx);
        let mut frame = Frame::default();
        let gpu = match renderer {
            Renderer::Cpu => None,
            Renderer::Auto | Renderer::Vulkan => match crate::guard::guard("GPU startup", crate::gpu::Gpu::new) {
                Ok(gpu) => Some(gpu),
                Err(e) if renderer == Renderer::Vulkan => return Err(e),
                Err(e) => {
                    log::warn!(target: LOG, "GPU startup: {e}; drawing in software");
                    app.session.accel_note = Some(format!("No Vulkan device ({e}); compositing on the CPU"));
                    None
                }
            },
        };
        let health = gpu.as_ref().map(|gpu| {
            let health = DeviceHealth::default();
            // This executable owns the shared device's handlers (as upstream's desktop app does):
            // installed before the compositor's pipelines are built. A fault moves everything to
            // the CPU at the next frame; the project stays open, so the interface isn't told the
            // presentation is lost.
            let (h, f) = (health.clone(), failures.clone());
            gpu.state.device.on_uncaptured_error(Arc::new(move |error| {
                let message = format!("uncaptured GPU error: {error}");
                if h.mark(Fault::Error(message.clone())) {
                    log::error!(target: LOG, "{message}");
                }
                f.report(&message, false);
                host::wake();
            }));
            let (h, f) = (health.clone(), failures.clone());
            gpu.state.device.set_device_lost_callback(move |reason, message| {
                let message = format!("GPU device lost ({reason:?}): {message}");
                h.mark(Fault::Lost(message.clone()));
                f.report(&message, false);
                host::wake();
            });
            health
        });
        // Roto Brush's SAM 2 runs on the same device instead of making a second one (on the CPU
        // when the windows draw in software).
        let sam2 = match &gpu {
            Some(g) => effectcraft_sam2::gpu::Gpu::install(Ok((&g.state.adapter, g.state.device.clone(), g.state.queue.clone()))),
            None => effectcraft_sam2::gpu::Gpu::install(Err("the application draws in software (EFFECTCRAFT_RENDERER=cpu or no Vulkan device)".into())),
        };
        if !sam2 {
            log::debug!(target: LOG, "SAM 2 had its device already");
        }
        frame.wgpu_render_state = gpu.as_ref().map(|g| g.state.clone());
        app.set_gpu_failure_bridge(failures.clone());
        match &gpu {
            Some(g) => log::info!(target: LOG, "air/OS renderer: Vulkan, {} presentation", g.present_mode().name()),
            None => log::info!(target: LOG, "air/OS renderer: CPU"),
        }
        let dev = host::device_scale();
        let viewports = Viewports::new(&ctx, Haiku::new(gpu, health.clone()), root, ui_scale * dev, dev);
        let menu = (std::env::var("EFFECTCRAFT_NATIVE_MENU").as_deref() != Ok("0")).then(|| crate::native_menu::NativeBar::new(crate::native_menu::HostSink));
        let saved = (app.session.path.clone(), app.session.saved_revision);
        Ok(Self {
            app,
            ctx,
            viewports,
            root,
            health,
            failures,
            frame,
            ui_scale,
            dev,
            dev_checked: Instant::now(),
            quit: false,
            menu,
            opened: Vec::new(),
            saved,
            drawn: false,
            test_fault: std::env::var("EFFECTCRAFT_TEST_GPU_FAULT")
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|s| s.is_finite() && *s >= 0.0)
                .map(|s| Instant::now() + Duration::from_secs_f64(s.min(86_400.0))),
        })
    }

    /// Runs until the app quits.
    pub fn run(&mut self) {
        while !self.quit {
            let timeout = match (self.viewports.timeout(), self.test_fault) {
                (t, Some(at)) => Some(t.unwrap_or(Duration::MAX).min(at.saturating_duration_since(Instant::now()))),
                (t, None) => t,
            };
            host::wait(timeout);
            if self.test_fault.is_some_and(|at| Instant::now() >= at) {
                self.viewports.request(ViewportId::ROOT, Duration::ZERO);
            }
            self.drain();
            if self.dev_checked.elapsed() > Duration::from_secs(2) {
                self.check_density();
            }
            for viewport in self.viewports.take_due() {
                if viewport == ViewportId::ROOT {
                    self.pass();
                } else if self.viewports.run_deferred(viewport).close_root {
                    self.quit = true;
                }
            }
            self.viewports.chores();
        }
    }

    /// Asks for the desktop's density again (the monitors may have changed).
    fn check_density(&mut self) {
        self.dev_checked = Instant::now();
        let dev = host::device_scale();
        if (dev - self.dev).abs() > f32::EPSILON {
            log::info!(target: LOG, "display density {} -> {dev}", self.dev);
            self.dev = dev;
            self.viewports.set_density(self.ui_scale * dev, dev);
        }
    }

    /// Hands the queued native events to their windows; the application's own are handled here.
    fn drain(&mut self) {
        while let Some((ev, text)) = services::next_event() {
            if let Some(input) = window_input(&ev, &text) {
                if !self.viewports.input(ev.window, input) {
                    log::debug!(target: LOG, "event {} for window {}, which is gone", ev.kind, ev.window);
                }
                continue;
            }
            match ev.kind {
                // Quit (Command+Q, the Deskbar's Quit) closes the main window, which asks about
                // unsaved changes first.
                kind::QUIT_REQUESTED => {
                    self.viewports.input(self.root, WindowInput::Close);
                }
                // Tracker's Open, Open With and drops on the icon, and the documents of a later
                // launch (one copy runs: B_SINGLE_LAUNCH).
                kind::OPEN_FILES => {
                    self.opened.extend(text.lines().filter(|l| !l.is_empty()).map(PathBuf::from));
                    host::window_command(self.root, &host::NativeCommand::Activate);
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                kind::MENU => {
                    if let Some(menu) = &mut self.menu {
                        menu.click(ev.id);
                    }
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                kind::DIALOG_ANSWER => {
                    services::answer(ev.id, ev.x != 0.0, &text);
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                kind::COLORS => {
                    self.check_density();
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                _ => {}
            }
        }
    }

    /// Back to software for good: the compositor and every window. A device fault (a wgpu
    /// callback, the compositor or a window's frame reported it) waits for this, the start of a
    /// pass: the last pass's GPU textures were drawn with the device.
    fn check_gpu(&mut self) {
        if self.test_fault.is_some_and(|at| Instant::now() >= at)
            && let Some(h) = &self.health
        {
            self.test_fault = None;
            h.mark(Fault::Lost("simulated (EFFECTCRAFT_TEST_GPU_FAULT)".into()));
        }
        let Some(fault) = self.health.as_ref().and_then(DeviceHealth::fault) else { return };
        log::warn!(target: LOG, "{fault}; switching to software rendering");
        // The interface retires its compositor and GPU textures in this pass's logic.
        self.failures.report(&fault.to_string(), false);
        self.frame.wgpu_render_state = None;
        self.viewports.platform(Haiku::drop_gpu);
        self.health = None;
    }

    /// Where the numeric scrub's pointer wrap may move the pointer: the monitor the main window
    /// is on, in its points.
    fn pointer_warp(&self) -> Option<PointerWarp> {
        let info = host::window_info(self.root);
        let k = self.dev / self.ctx.pixels_per_point();
        if !(k.is_finite() && k > 0.0) {
            return None;
        }
        let [left, top, ..] = info.inner;
        let [ml, mt, mr, mb] = info.monitor;
        let bounds = egui::Rect::from_min_max(pos2((ml - left) * k, (mt - top) * k), pos2((mr - left) * k, (mb - top) * k));
        Some(PointerWarp {
            bounds,
            warp: Box::new(move |to| {
                let (x, y) = ((to.x / k + left).round(), (to.y / k + top).round());
                host::set_mouse(x, y).then(|| pos2((x - left) * k, (y - top) * k))
            }),
        })
    }

    /// The root viewport's pass: EffectCraft's frame, which draws the other windows' immediate
    /// viewports on the way.
    fn pass(&mut self) {
        self.check_gpu();
        let Some(mut raw) = self.viewports.root_input() else { return };
        if !self.opened.is_empty() {
            if self.app.is_ready() {
                raw.dropped_files.extend(self.opened.drain(..).map(|p| Arc::new(Opened(p)) as egui::DroppedFileHandle));
            } else {
                self.viewports.request(ViewportId::ROOT, Duration::from_millis(50));
            }
        }
        self.app.raw_input_hook(&self.ctx, &mut raw);
        self.frame.pointer_warp = self.pointer_warp();
        let ctx = self.ctx.clone();
        let t0 = Instant::now();
        let mut closing = false;
        let (app, frame, menu) = (&mut self.app, &mut self.frame, &mut self.menu);
        let out = ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            closing = ctx.input(|i| i.viewport().close_requested());
            if let Some(menu) = menu.as_mut() {
                menu.update(app, &ctx);
            }
            app.logic(&ctx, frame);
            app.ui(ui, frame);
        });
        let t1 = Instant::now();
        // The main window's close button, Close or Quit: the interface asks about unsaved
        // changes and cancels the close while it does (`CancelClose`), as eframe honours.
        let cancelled = out.viewport_output.get(&ViewportId::ROOT).is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, ViewportCommand::CancelClose)));
        if closing && !cancelled {
            self.quit = true;
        }
        if self.viewports.finish(ViewportId::ROOT, out).close_root {
            self.quit = true;
        }
        if self.health.as_ref().is_some_and(|h| h.fault().is_some()) {
            // The next pass draws in software.
            self.viewports.request(ViewportId::ROOT, Duration::ZERO);
        }
        self.type_saved_project();
        if !self.drawn {
            self.drawn = true;
            log::info!(target: LOG, "first frame drawn");
        }
        log::debug!(target: LOG, "frame: interface {:.1} ms, output and drawing {:.1} ms", (t1 - t0).as_secs_f64() * 1e3, t1.elapsed().as_secs_f64() * 1e3);
    }

    /// A project just saved gets its file type (`std::fs` writes none).
    fn type_saved_project(&mut self) {
        let now = (self.app.session.path.clone(), self.app.session.saved_revision);
        if now != self.saved {
            if let Some(path) = &now.0
                && !self.app.session.is_dirty()
            {
                services::type_file(path);
            }
            self.saved = now;
        }
    }

    /// What quitting does besides closing the windows (`on_exit` in the desktop app).
    pub fn finish(&mut self) {
        log::info!(target: LOG, "the window is closing");
        self.app.on_exit();
        effectcraft_host::stop_helpers();
        // Scripts in Scripts/Shutdown run with the project closed.
        let _ = self.app.session.execute("file.runShutdownScripts", serde_json::json!({}));
    }
}

/// A native event that happened in a window, as that window's input.
fn window_input<'a>(ev: &RawEvent, text: &'a str) -> Option<WindowInput<'a>> {
    Some(match ev.kind {
        kind::MOUSE_MOVED | kind::MOUSE_DOWN | kind::MOUSE_UP => WindowInput::Pointer { x: ev.x, y: ev.y, buttons: ev.buttons, modifiers: ev.modifiers },
        kind::POINTER_GONE => WindowInput::PointerGone,
        // A wheel's whole steps, or (`repeat` 1) a touchpad's scroll gesture with its unrounded
        // steps and its phase in `clicks` (`host.cpp`).
        kind::WHEEL => WindowInput::Wheel { dx: ev.dx, dy: ev.dy, modifiers: ev.modifiers, gesture: (ev.repeat != 0).then_some(ev.clicks) },
        kind::KEY_DOWN | kind::KEY_UP => {
            WindowInput::Key { pressed: ev.kind == kind::KEY_DOWN, code: ev.key, raw_char: ev.raw_char, modifiers: ev.modifiers, text }
        }
        kind::MODIFIERS => WindowInput::Modifiers(host::modifiers()),
        kind::FOCUS => WindowInput::Focus(ev.x != 0.0),
        kind::CLOSE => WindowInput::Close,
        // Files dropped on a window go to the interface as the desktop app's do (a project
        // opens, media is imported).
        kind::DROPPED_FILES => WindowInput::Dropped { x: ev.x, y: ev.y, paths: text.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect() },
        // The next frame picks up the new size, position or state.
        kind::RESIZED | kind::MOVED | kind::MINIMIZED => WindowInput::Changed,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::Renderer;

    #[test]
    fn the_renderer_follows_the_variable_then_window_graphics() {
        assert_eq!(Renderer::choose(None, "auto"), Ok(Renderer::Auto));
        assert_eq!(Renderer::choose(None, "gl"), Ok(Renderer::Cpu));
        assert_eq!(Renderer::choose(Some("vulkan"), "gl"), Ok(Renderer::Vulkan));
        assert_eq!(Renderer::choose(Some("cpu"), "auto"), Ok(Renderer::Cpu));
        assert_eq!(Renderer::choose(Some(""), "auto"), Ok(Renderer::Auto));
        assert!(Renderer::choose(Some("gl"), "auto").is_err());
    }
}

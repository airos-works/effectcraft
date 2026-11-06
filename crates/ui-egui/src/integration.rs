//! The host integration the app runs in: eframe's `App` and `Frame` on the desktop and the web,
//! and on air/OS, where the native host drives the app without eframe or winit (neither has a
//! Haiku backend), a frame of the same shape that the host fills in.

#[cfg(not(target_os = "haiku"))]
pub use eframe::{App, Frame, egui_wgpu, wgpu};

#[cfg(target_os = "haiku")]
pub use {egui_wgpu, wgpu};

/// What eframe's `App` gives the air/OS host to call: the same methods, called in eframe's order.
#[cfg(target_os = "haiku")]
pub trait App {
    /// The logic-only tick, before [`App::ui`] in the same pass.
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut Frame) {}
    /// One UI pass.
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame);
    /// The window is closing.
    fn on_exit(&mut self) {}
    /// The pass's input, before it runs.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, _raw_input: &mut egui::RawInput) {}
}

/// Moves the pointer for the numeric scrub's wrap (`pointer_wrap`): to a point in the window's
/// UI points, answering where it went. `bounds` is the monitor the press was on, in the window's
/// UI points.
#[cfg(target_os = "haiku")]
pub struct PointerWarp {
    pub bounds: egui::Rect,
    pub warp: Box<dyn Fn(egui::Pos2) -> Option<egui::Pos2>>,
}

/// The frame the air/OS host hands the app each pass (eframe's `Frame` elsewhere).
#[cfg(target_os = "haiku")]
#[derive(Default)]
pub struct Frame {
    /// The window's wgpu device (Vulkan), shared with the GPU compositor; `None` while the host
    /// draws in software.
    pub wgpu_render_state: Option<egui_wgpu::RenderState>,
    /// How to move the pointer, when the host can.
    pub pointer_warp: Option<PointerWarp>,
}

#[cfg(target_os = "haiku")]
impl Frame {
    /// The window's wgpu device, as eframe's `Frame::wgpu_render_state`.
    pub fn wgpu_render_state(&self) -> Option<&egui_wgpu::RenderState> {
        self.wgpu_render_state.as_ref()
    }

    /// A frame without a device or a window, as eframe's for tests and benchmarks.
    pub fn _new_kittest() -> Self {
        Self::default()
    }
}

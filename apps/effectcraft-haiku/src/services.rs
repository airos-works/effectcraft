//! The Be API services EffectCraft's interface asks its host for (`Hooks` in
//! `crates/ui-egui/src/hooks.rs`): file panels, the clipboard, the system appearance, audio
//! output (`audio.rs`), document types and file types of what it writes.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use effectcraft_engine::project::render_queue::OutputFormat;
use effectcraft_engine::{ExportJob, ExportResult, Exporter};
use effectcraft_ui_egui::{DialogDone, EffectcraftApp, FileDialog};

use crate::host::{self, RawEvent, kind};

/// The application signature (`EffectCraft.rdef`).
pub const SIGNATURE: &str = "application/x-vnd.storytold-EffectCraft";
/// EffectCraft's own projects (`.ecproj`, and the XML copy `.ecprojx`), as the desktop packages
/// declare them.
pub const PROJECT_TYPE: &str = "application/x-effectcraft";
const LOG: &str = "effectcraft_haiku";

/// Makes EffectCraft the preferred application of its project type, and of the media types it
/// imports, where nothing else is (the post-install script registers the project type).
pub fn claim_document_types() {
    let claimed = host::claim_types(SIGNATURE, &[PROJECT_TYPE]);
    if claimed > 0 {
        log::info!(target: LOG, "preferred application of {claimed} document types");
    }
}

pub fn clipboard_text() -> Option<String> {
    host::clipboard_get("text/plain").map(|v| String::from_utf8_lossy(&v).trim_end_matches('\0').to_string())
}

/// Sets a written file's MIME type (`BEOS:TYPE`): files written with `std::fs` have none, and
/// Tracker then can't tell what opens them.
pub fn type_file(path: &str) {
    if !path.is_empty() && Path::new(path).is_file() && !host::type_file(Path::new(path)) {
        log::debug!(target: LOG, "{path}: no file type set");
    }
}

// ---------------------------------------------------------------------------------------------
// File panels

thread_local! {
    /// The commands' panels waiting for their answer, by id ([`answer`]).
    static WAITING: RefCell<HashMap<i32, (FileDialog, DialogDone)>> = RefCell::new(HashMap::new());
    /// Events that arrived while a blocking panel was up ([`pick_blocking`]), for the shell.
    static HELD: RefCell<VecDeque<(RawEvent, String)>> = const { RefCell::new(VecDeque::new()) };
}

static NEXT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);

/// The next native event: those held while a blocking panel was up first.
pub fn next_event() -> Option<(RawEvent, String)> {
    HELD.with_borrow_mut(VecDeque::pop_front).or_else(host::next_event)
}

/// The panel's mode for `host::file_panel` (0 open, 1 save, 2 folder, 3 open several), the
/// suggested name or folder, and the extensions offered.
fn panel(dialog: &FileDialog) -> (i32, String, Vec<String>) {
    match dialog {
        FileDialog::Open { exts, many } => (if *many { 3 } else { 0 }, String::new(), exts.clone()),
        FileDialog::OpenProject => (0, String::new(), vec!["ecproj".into(), "ecprojx".into()]),
        FileDialog::Folder => (2, String::new(), Vec::new()),
        FileDialog::Save { default, .. } => (1, default.clone(), Vec::new()),
    }
}

fn title(dialog: &FileDialog) -> &'static str {
    match dialog {
        FileDialog::Open { .. } => "EffectCraft: Open",
        FileDialog::OpenProject => "EffectCraft: Open Project",
        FileDialog::Folder => "EffectCraft: Choose Folder",
        FileDialog::Save { .. } => "EffectCraft: Save",
    }
}

/// Paths of a panel's answer; a save gets the extension it asked for when the name has none.
fn picked(dialog: &FileDialog, text: &str) -> Vec<String> {
    let mut paths: Vec<String> = text.lines().filter(|s| !s.is_empty()).map(str::to_owned).collect();
    if let FileDialog::Save { ext, .. } = dialog
        && !ext.is_empty()
        && let Some(p) = paths.first_mut()
        && Path::new(p.as_str()).extension().is_none()
    {
        p.push('.');
        p.push_str(ext);
    }
    paths
}

/// Shows a command's panel without blocking the interface (`Hooks::file_dialog_async`); the
/// answer arrives as a `DIALOG_ANSWER` event ([`answer`]). One panel at a time: a newer one
/// cancels the one before.
pub fn show_async(dialog: FileDialog, done: DialogDone) {
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (mode, name, exts) = panel(&dialog);
    let earlier: Vec<DialogDone> = WAITING.with_borrow_mut(|w| w.drain().map(|(_, (_, d))| d).collect());
    for d in earlier {
        d(Vec::new());
    }
    host::file_panel(id, mode, &name, &exts.iter().map(String::as_str).collect::<Vec<_>>(), title(&dialog));
    WAITING.with_borrow_mut(|w| w.insert(id, (dialog, done)));
}

/// A panel's answer (`DIALOG_ANSWER`): to the command that asked, if it still waits.
pub fn answer(id: i32, ok: bool, text: &str) {
    let Some((dialog, done)) = WAITING.with_borrow_mut(|w| w.remove(&id)) else { return };
    done(if ok { picked(&dialog, text) } else { Vec::new() });
}

/// Shows a panel and waits for its answer (the blocking `Hooks::pick_*`, which settings and
/// effect controls call in the middle of a frame). The windows keep their last frame meanwhile;
/// the events that arrive are kept for the shell.
pub fn pick_blocking(dialog: FileDialog) -> Vec<String> {
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (mode, name, exts) = panel(&dialog);
    host::file_panel(id, mode, &name, &exts.iter().map(String::as_str).collect::<Vec<_>>(), title(&dialog));
    loop {
        host::wait(Some(Duration::from_millis(250)));
        while let Some((ev, text)) = host::next_event() {
            if ev.kind == kind::DIALOG_ANSWER && ev.id == id {
                return if ev.x != 0.0 { picked(&dialog, &text) } else { Vec::new() };
            }
            HELD.with_borrow_mut(|h| h.push_back((ev, text)));
        }
    }
}

/// The interface's platform hooks: panels, the clipboard, appearance and audio.
pub fn install(app: &mut EffectcraftApp) {
    let h = &mut app.hooks;
    h.file_dialog_async = Some(Box::new(|dialog, done| {
        show_async(dialog, done);
        host::wake();
    }));
    h.pick_files = Some(Box::new(|exts: &[&str]| pick_blocking(FileDialog::Open { exts: exts.iter().map(|e| (*e).to_string()).collect(), many: true })));
    h.pick_open_project = Some(Box::new(|| pick_blocking(FileDialog::OpenProject).into_iter().next()));
    h.pick_folder = Some(Box::new(|| pick_blocking(FileDialog::Folder).into_iter().next()));
    h.pick_save = Some(Box::new(|name: &str| {
        pick_blocking(FileDialog::Save { default: name.to_string(), ext: "ecproj".into(), project: true }).into_iter().next()
    }));
    h.pick_save_file = Some(Box::new(|name: &str, ext: &str| {
        pick_blocking(FileDialog::Save { default: name.to_string(), ext: ext.to_string(), project: false }).into_iter().next()
    }));
    h.clipboard_text = Some(Box::new(clipboard_text));
    // Settings ▸ Appearance ▸ Sync with System: the panel colour of the Appearance preferences.
    h.system_theme = Some(Box::new(|_| Some(if host::dark() { egui::Theme::Dark } else { egui::Theme::Light })));
    h.audio_device = Some(Box::new(crate::audio::open));
    h.audio_devices = Some(Box::new(crate::audio::devices));
}

// ---------------------------------------------------------------------------------------------
// File types of exports

/// The Render Queue's exporter, giving each finished output its file type.
pub struct TypedExporter(pub Arc<dyn Exporter>);

impl Exporter for TypedExporter {
    fn formats(&self) -> Vec<OutputFormat> {
        self.0.formats()
    }

    fn supports_skip_existing(&self, settings: &effectcraft_engine::project::render_queue::RenderSettings) -> bool {
        self.0.supports_skip_existing(settings)
    }

    fn export(&self, job: &ExportJob, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<ExportResult, String> {
        let r = self.0.export(job, progress)?;
        type_file(&r.path);
        Ok(r)
    }

    fn export_async<'a>(
        &'a self,
        job: &'a ExportJob<'a>,
        progress: &'a mut dyn FnMut(u64, u64) -> bool,
    ) -> effectcraft_engine::render_queue::LocalFuture<'a, Result<ExportResult, String>> {
        Box::pin(async move {
            let r = self.0.export_async(job, progress).await?;
            type_file(&r.path);
            Ok(r)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_gets_its_extension_and_cancel_nothing() {
        let save = FileDialog::Save { default: "a.mov".into(), ext: "mov".into(), project: false };
        assert_eq!(picked(&save, "/boot/home/clip\n"), vec!["/boot/home/clip.mov".to_string()]);
        assert_eq!(picked(&save, "/boot/home/clip.mp4"), vec!["/boot/home/clip.mp4".to_string()]);
        assert!(picked(&save, "").is_empty());
        let open = FileDialog::Open { exts: vec!["png".into()], many: true };
        assert_eq!(picked(&open, "/a.png\n/b.png"), vec!["/a.png".to_string(), "/b.png".to_string()]);
        assert_eq!(panel(&FileDialog::OpenProject).2, vec!["ecproj".to_string(), "ecprojx".to_string()]);
    }
}

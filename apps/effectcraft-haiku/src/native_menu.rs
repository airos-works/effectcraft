//! The native menu bar: a `BMenuBar` at the top of the main window instead of the in-window
//! menus. What the menus hold, and when they change, is upstream's platform-neutral spec
//! (`effectcraft_ui_egui::native_menu`, which the macOS app turns into an `NSMenu`); this file
//! lays it out for `host.cpp` (`pc_menu_build`, `pc_menu_update`) and runs the clicked items
//! the way the in-window bar does (`native_menu::activate`).
//!
//! - Off Mac, upstream's menu tree has no application menu: File ends with Exit, Edit holds
//!   Settings, Help holds About, as Haiku applications keep them.
//! - **Keys stay with the interface**: the window hands every Command key to the view before
//!   `BWindow` looks for a menu shortcut (`DispatchMessage` in host.cpp), so the menus only show
//!   the shortcuts. Those with Command (`Cmd+…`/`Ctrl+…`, Haiku's Command key) and one character
//!   are drawn by `BMenuItem` itself; the others (a function key, a tool's bare letter) as text.
//!   Command+Q still reaches `BWindow`, which quits (with the Save Changes prompt).
//! - A structure change rebuilds the whole bar; labels, enabled and checked are updated in place,
//!   only for the items that changed.
//! - `EFFECTCRAFT_NATIVE_MENU=0` keeps the in-window menus.

use std::fmt::Write as _;

use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::native_menu::{self, NativeMenu, NativeNode};

/// `B_SHIFT_KEY`, `B_COMMAND_KEY`, `B_OPTION_KEY` (`<InterfaceDefs.h>`).
const SHIFT: u32 = 0x1;
const COMMAND: u32 = 0x2;
const OPTION: u32 = 0x40;

/// Where the bar goes: the host in the app, a recorder in tests.
pub trait Sink {
    fn build(&mut self, spec: &str);
    fn update(&mut self, spec: &str);
}

#[cfg(target_os = "haiku")]
pub struct HostSink;

#[cfg(target_os = "haiku")]
impl Sink for HostSink {
    fn build(&mut self, spec: &str) {
        crate::host::menu_build(spec);
    }

    fn update(&mut self, spec: &str) {
        crate::host::menu_update(spec);
    }
}

/// One clickable item of the bar, by its index in the spec.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: String,
    label: String,
    enabled: bool,
    checked: Option<bool>,
}

pub struct NativeBar<S: Sink> {
    sink: S,
    spec: NativeMenu,
    structure: Option<u64>,
    /// Upstream's state key (and whether a text field had the keyboard) of the bar shown.
    state: Option<(u64, bool)>,
    entries: Vec<Entry>,
    /// Clicked items (indexes, from `kind::MENU` events), run at the next frame.
    clicks: Vec<i32>,
}

impl<S: Sink> NativeBar<S> {
    pub fn new(sink: S) -> Self {
        Self { sink, spec: NativeMenu::default(), structure: None, state: None, entries: Vec::new(), clicks: Vec::new() }
    }

    pub fn click(&mut self, index: i32) {
        self.clicks.push(index);
    }

    /// Runs the clicked items, then brings the bar up to date (once per frame, before the
    /// interface's pass, as the macOS app does).
    pub fn update(&mut self, app: &mut EffectcraftApp, ctx: &egui::Context) {
        app.ui.show_menu_bar = false;
        for index in std::mem::take(&mut self.clicks) {
            let Some(id) = usize::try_from(index).ok().and_then(|i| self.entries.get(i)).map(|e| e.id.clone()) else { continue };
            if let Err(e) = native_menu::activate(app, ctx, &self.spec, &id) {
                log::warn!(target: "effectcraft_haiku", "menu: {e}");
            }
            self.state = None;
        }
        let key = (native_menu::state_key(app), native_menu::text_field_window(ctx).is_some());
        if self.state == Some(key) {
            return;
        }
        self.state = Some(key);
        let spec = native_menu::build_now(app, ctx);
        self.show(spec);
    }

    /// Shows `spec`: rebuilt when its structure changed, else updated in place.
    fn show(&mut self, spec: NativeMenu) {
        let structure = spec.structure();
        if self.structure != Some(structure) {
            let (text, entries) = layout(&spec);
            self.sink.build(&text);
            self.entries = entries;
            self.structure = Some(structure);
        } else {
            let mut changes = String::new();
            for (i, (e, it)) in self.entries.iter_mut().zip(spec.items()).enumerate() {
                if e.id != it.id {
                    continue;
                }
                if e.label != it.label || e.enabled != it.enabled || e.checked != it.checked {
                    let _ = writeln!(changes, "{i}\t{}\t{}\t{}", field(&it.label), i32::from(it.enabled), checked(it.checked));
                    e.label.clone_from(&it.label);
                    e.enabled = it.enabled;
                    e.checked = it.checked;
                }
            }
            if !changes.is_empty() {
                self.sink.update(&changes);
            }
        }
        self.spec = spec;
    }
}

/// Text for one field of a spec line.
fn field(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

fn checked(c: Option<bool>) -> i32 {
    c.map_or(-1, i32::from)
}

/// A shortcut (`Cmd+Shift+S`) as the menu shows it: a character and modifiers `BMenuItem` draws
/// (Command shortcuts with one character), or text.
pub fn shortcut(s: &str) -> (u8, u32, String) {
    let parts: Vec<&str> = match s.strip_suffix("++") {
        Some(head) => head.split('+').chain(std::iter::once("+")).collect(),
        None => s.split('+').collect(),
    };
    let Some((key, mods)) = parts.split_last() else { return (0, 0, String::new()) };
    let command = mods.iter().any(|m| matches!(*m, "Cmd" | "Ctrl" | "Command"));
    let ch = match *key {
        k if k.len() == 1 && k.is_ascii() && !k.chars().all(char::is_whitespace) => k.as_bytes()[0].to_ascii_uppercase(),
        "Delete" | "Del" => 0x7f,
        "Backspace" => 0x08,
        "Enter" | "Return" => 0x0a,
        "Tab" => 0x09,
        "Space" => 0x20,
        _ => 0,
    };
    if command && ch != 0 {
        let mut bits = COMMAND;
        for m in mods {
            match *m {
                "Shift" => bits |= SHIFT,
                "Alt" | "Opt" | "Option" => bits |= OPTION,
                _ => {}
            }
        }
        return (ch, bits, String::new());
    }
    (0, 0, s.to_string())
}

/// The host's spec of `menu` (`BuildMenuBar` in host.cpp), and its items by index.
fn layout(menu: &NativeMenu) -> (String, Vec<Entry>) {
    fn walk(out: &mut String, entries: &mut Vec<Entry>, nodes: &[NativeNode]) {
        let mut last_separator = true;
        for n in nodes {
            match n {
                NativeNode::Separator => {
                    if !last_separator {
                        out.push_str("-\n");
                    }
                    last_separator = true;
                    continue;
                }
                NativeNode::Submenu { label, children } => {
                    let _ = writeln!(out, "S\t{}", field(label));
                    walk(out, entries, children);
                    out.push_str("E\n");
                }
                // macOS's own items (Services, Hide…): none off Mac.
                NativeNode::Predefined { .. } => continue,
                NativeNode::Item(it) => {
                    let (ch, mods, text) = it.shortcut.as_deref().map(shortcut).unwrap_or_default();
                    let _ = writeln!(
                        out,
                        "I\t{}\t{}\t{ch}\t{mods}\t{}\t{}\t{}",
                        entries.len(),
                        field(&it.label),
                        field(&text),
                        i32::from(it.enabled),
                        checked(it.checked)
                    );
                    entries.push(Entry { id: it.id.clone(), label: it.label.clone(), enabled: it.enabled, checked: it.checked });
                }
            }
            last_separator = false;
        }
    }
    let mut out = String::new();
    let mut entries = Vec::new();
    for node in &menu.menus {
        if let NativeNode::Submenu { label, children } = node {
            let _ = writeln!(out, "M\t{}", field(label));
            walk(&mut out, &mut entries, children);
            out.push_str("E\n");
        }
    }
    (out, entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use effectcraft_ui_egui::native_menu::NativeItem;
    use serde_json::Value;

    fn item(id: &str, label: &str, shortcut: Option<&str>) -> NativeNode {
        NativeNode::Item(NativeItem {
            id: id.into(),
            label: label.into(),
            command: id.into(),
            params: Value::Null,
            shortcut: shortcut.map(str::to_string),
            accelerator: None,
            enabled: true,
            checked: None,
        })
    }

    #[test]
    fn command_shortcuts_are_native_and_the_others_text() {
        assert_eq!(shortcut("Cmd+S"), (b'S', COMMAND, String::new()));
        assert_eq!(shortcut("Cmd+Shift+S"), (b'S', COMMAND | SHIFT, String::new()));
        assert_eq!(shortcut("Cmd+Alt+N"), (b'N', COMMAND | OPTION, String::new()));
        assert_eq!(shortcut("Cmd++"), (b'+', COMMAND, String::new()));
        assert_eq!(shortcut("F1"), (0, 0, "F1".into()));
        assert_eq!(shortcut("V"), (0, 0, "V".into()));
        assert_eq!(shortcut("Cmd+PageUp"), (0, 0, "Cmd+PageUp".into()));
    }

    #[derive(Default)]
    struct Log(Vec<(bool, String)>);
    impl Sink for &mut Log {
        fn build(&mut self, spec: &str) {
            self.0.push((true, spec.into()));
        }
        fn update(&mut self, spec: &str) {
            self.0.push((false, spec.into()));
        }
    }

    #[test]
    fn builds_once_then_updates_only_what_changed() {
        let menu = |label: &str| NativeMenu {
            menus: vec![NativeNode::Submenu {
                label: "File".into(),
                children: vec![
                    NativeNode::Separator,
                    item("m.0.0", "New Project", Some("Cmd+Alt+N")),
                    NativeNode::Separator,
                    NativeNode::Separator,
                    item("m.0.1", label, None),
                ],
            }],
        };
        let mut log = Log::default();
        {
            let mut bar = NativeBar::new(&mut log);
            bar.show(menu("Save"));
            bar.show(menu("Save"));
            bar.show(menu("Save As"));
        }
        assert_eq!(log.0.len(), 2, "{:?}", log.0);
        assert_eq!(log.0[0], (true, "M\tFile\nI\t0\tNew Project\t78\t66\t\t1\t-1\n-\nI\t1\tSave\t0\t0\t\t1\t-1\nE\n".into()));
        assert_eq!(log.0[1], (false, "1\tSave As\t1\t-1\n".into()));
    }
}

//! The shared GPU device's health, as the host sees it: an uncaptured error or a lost device
//! (wgpu's callbacks, on any thread), or a frame the presenter couldn't draw. The first fault is
//! kept; the shell then moves the app's compositor and every window to the CPU at the start of
//! the next frame (`shell::Shell::check_gpu`), with the project left open.

use std::sync::{Arc, Mutex, PoisonError};

/// Why the device stopped being usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// An uncaptured validation or out-of-memory error, or a frame that failed.
    Error(String),
    /// The driver lost the device.
    Lost(String),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Error(e) => write!(f, "GPU error: {e}"),
            Fault::Lost(e) => write!(f, "GPU device lost: {e}"),
        }
    }
}

#[derive(Clone, Default)]
pub struct DeviceHealth(Arc<Mutex<Option<Fault>>>);

impl DeviceHealth {
    /// Records `fault` unless one is recorded already; true for the first.
    pub fn mark(&self, fault: Fault) -> bool {
        let mut f = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if f.is_some() {
            return false;
        }
        *f = Some(fault);
        true
    }

    pub fn fault(&self) -> Option<Fault> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_fault_is_kept() {
        let h = DeviceHealth::default();
        assert!(h.fault().is_none());
        assert!(h.mark(Fault::Error("a".into())));
        assert!(!h.clone().mark(Fault::Lost("b".into())));
        assert_eq!(h.fault(), Some(Fault::Error("a".into())));
    }
}

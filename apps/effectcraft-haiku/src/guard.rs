//! Last-resort crash guard (AGENTS.md, Never crash): a panic that escapes the host's GPU or
//! window code becomes an error, and the host falls back (software rendering) instead of ending
//! with the project open. The engine logs every panic (`logging::install_panic_hook`).

use std::panic::{AssertUnwindSafe, catch_unwind};

/// Run `f`; a panic inside it becomes an `Err` naming `what`.
pub fn guard<T>(what: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(format!("{what} failed with an internal error (logged); the project is unchanged")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_turns_a_panic_into_an_error() {
        let r: Result<(), String> = guard("Present", || panic!("boom"));
        assert!(r.unwrap_err().contains("Present failed"));
        assert_eq!(guard("Present", || Ok(3)), Ok(3));
        assert_eq!(guard::<()>("Present", || Err("bad".into())), Err("bad".into()));
    }
}

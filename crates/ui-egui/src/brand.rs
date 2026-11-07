//! The Mirai fork's display name for the app.
//!
//! Upstream calls the app GridCraft, in its own strings and in every translation table
//! (`crates/ui-egui/src/i18n/`). The fork shows it as "Mirai Calc" without editing those tables:
//! [`rebrand`] swaps the name in every interface string that goes through
//! [`crate::i18n::Lang::tr`], and the places that draw the name directly (the window title, About,
//! Backstage, message titles) use [`APP_NAME`]. Identifiers stay upstream's: the binary, crate
//! names, the app id, the settings folder and the installer's registry keys, so settings and
//! installs carry over.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The name the fork shows for the app.
pub const APP_NAME: &str = "Mirai Calc";

/// Upstream's name for the app, as its interface strings spell it.
const UPSTREAM_NAME: &str = "GridCraft";

/// Distinct rebranded strings kept. Interface strings are a fixed set (catalog rows and literals),
/// so this is never reached in practice; it guards against unbounded growth all the same: past it,
/// a string not seen before is shown with upstream's name rather than kept.
const CACHE_LIMIT: usize = 8192;

/// `s` with the app's upstream name replaced by [`APP_NAME`]; `s` itself when it doesn't name the
/// app. The rebranded copies are cached for the life of the process, so a string is rebuilt once.
pub fn rebrand(s: &str) -> &str {
    if !s.contains(UPSTREAM_NAME) {
        return s;
    }
    static CACHE: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    // A poisoned lock (a panic elsewhere while holding it) only costs the rename, never a crash.
    let Ok(mut cache) = CACHE.get_or_init(Default::default).lock() else { return s };
    if let Some(&branded) = cache.get(s) {
        return branded;
    }
    if cache.len() >= CACHE_LIMIT {
        return s;
    }
    let branded: &'static str = Box::leak(s.replace(UPSTREAM_NAME, APP_NAME).into_boxed_str());
    cache.insert(s.to_owned(), branded);
    branded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_upstream_name_becomes_the_fork_name() {
        assert_eq!(rebrand("About GridCraft"), "About Mirai Calc");
        assert_eq!(rebrand("GridCraft"), APP_NAME);
        assert_eq!(rebrand("Acerca de GridCraft"), "Acerca de Mirai Calc");
    }

    #[test]
    fn other_strings_are_left_alone() {
        let s = "Save As";
        assert!(std::ptr::eq(rebrand(s), s));
        // The environment variable keeps its name.
        assert_eq!(rebrand("GRIDCRAFT_CONTROL_KEY"), "GRIDCRAFT_CONTROL_KEY");
    }

    #[test]
    fn a_string_is_rebuilt_once() {
        assert!(std::ptr::eq(rebrand("Quit GridCraft"), rebrand("Quit GridCraft")));
    }
}

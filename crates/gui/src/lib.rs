//! The logic behind `reqlite-gui` that needs no window.

pub mod draft;
pub mod present;
pub mod workspace;

/// An error and every cause under it, on one line.
pub fn chain(err: &dyn std::error::Error) -> String {
    let mut line = err.to_string();
    let mut cause = err.source();
    while let Some(c) = cause {
        line.push_str(": ");
        line.push_str(&c.to_string());
        cause = c.source();
    }
    line
}

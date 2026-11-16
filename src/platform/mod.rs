//! Everything that talks to the OS windowing system. Only macOS for now;
//! other platforms would add a sibling module with the same `run`.

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::run;

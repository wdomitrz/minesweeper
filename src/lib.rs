//! Minesweeper: a board that works offline, and nothing else.
//!
//! The split this crate is built on, and the reason it is two thirds Rust:
//!
//! * [`minefield`] — where the mines are, and what every square touches, with
//!   the first click left safe.
//! * [`game`] — one sitting of it: the squares opened and flagged, the clock,
//!   how it ended, and every word the page says about it.
//! * `ui` — the browser layer, compiled only for `wasm32-unknown-unknown`
//!   (so it is absent from a host build, and a doc link to it would not
//!   resolve there). Every `document`, `localStorage` and service-worker call
//!   lives here, and nothing else does.
//!
//! The first two have no `web-sys`, no `js-sys` and no `wasm-bindgen`, so
//! `cargo test` compiles them natively and runs the whole rule set without a
//! browser. That is the point: the moment the rules reach for `document`, their
//! tests stop running and the app becomes a blob.
//!
//! `cargo build --release` writes the whole site to `dist/` through `build.rs`.
//! There is no server, no binary and no run step: the build is the product.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

/// The version of this build.
///
/// `ui.rs` logs it at startup, which is the only reason it survives the
/// optimizer — an unused constant is dropped. A redeploy check looks for this
/// string in the shipped bytes to prove that a new version reaches an installed
/// app rather than a tab keeping the bytes it started with, so it has to be
/// reachable code and it has to be a string literal: a comment is stripped.
pub const VERSION: &str = "1";

pub mod game;
pub mod minefield;

#[cfg(target_arch = "wasm32")]
pub mod ui;

pub use game::{Game, Readout, Report};
pub use minefield::Level;

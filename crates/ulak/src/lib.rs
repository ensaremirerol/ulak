//! ulak operator UI. `model`, `term` and `net` have no window dependency; `view` is the Rune layer.
pub mod app;
pub mod model;
pub mod net;
pub mod scenes;
#[cfg(not(target_arch = "wasm32"))]
pub mod selfupdate;
pub mod term;
pub mod view;
#[cfg(target_arch = "wasm32")]
pub mod web;
pub mod web_login;
pub mod web_view;

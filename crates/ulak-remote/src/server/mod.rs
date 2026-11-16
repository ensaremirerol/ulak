//! Part 2c: the HTTPS hub. mTLS WebSockets for daemons and UIs, `/enroll` and `/mcp` without a client cert.
mod hub;
mod mcp;
mod web;
mod ws;

pub use hub::{Hub, HubConfig, Running};

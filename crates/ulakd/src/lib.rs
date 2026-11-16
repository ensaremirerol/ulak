//! `ulakd`: the per-node daemon. Reports telemetry, guards against gaming, runs agents in tmux,
//! and talks to the hub over mTLS WebSocket.
pub mod config;
pub mod errors;
pub mod guard;
pub mod hub;
pub mod join;
pub mod renew;
pub mod telemetry;
pub mod tmux;

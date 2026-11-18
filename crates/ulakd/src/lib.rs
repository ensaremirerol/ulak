//! `ulakd`: the per-node daemon. Reports telemetry, guards against gaming, runs agents in tmux,
//! and talks to the hub over mTLS WebSocket.
pub mod agents;
pub mod config;
pub mod errors;
pub mod git;
pub mod guard;
pub mod hub;
pub mod join;
pub mod mcp;
pub mod renew;
pub mod repos;
pub mod telemetry;
pub mod tmux;
pub mod update;

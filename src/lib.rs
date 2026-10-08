//! gnerkulfloot: a headless shop backend. The binary in `main.rs` is a thin CLI
//! over this library, so integration tests can build the same router the
//! server runs.

pub mod app;
pub mod auth;
pub mod catalog;
pub mod checkout;
pub mod config;
pub mod db;
pub mod error;
pub mod http;
pub mod money;
pub mod payments;
pub mod ratelimit;
pub mod shipping;
pub mod storage;
pub mod tax;
pub mod worker;

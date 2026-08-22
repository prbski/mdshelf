//! mdshelf serves folders of markdown files as websites.
//!
//! The crate is exposed as a library so the integration suite can drive a real server
//! in-process — the authorization work in particular has to be verified end to end,
//! across every surface that can emit bytes, rather than unit by unit.

pub mod acl;
pub mod auth;
pub mod cli;
pub mod config;
pub mod content;
pub mod export;
pub mod render;
pub mod server;
pub mod service;
pub mod theme;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

//! Core of the `ctx` package manager: package format, version resolution, the registry
//! client and installation. The command-line front end lives in the `ctx-cli` crate.

pub mod archive;
pub mod auth;
pub mod config;
pub mod descriptor;
pub mod error;
pub mod install;
pub mod lockfile;
pub mod manifest;
pub mod project;
pub mod range;
pub mod registry;
pub mod resolve;

pub use error::{Error, Result};
pub use reqwest;

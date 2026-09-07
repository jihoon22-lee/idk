//! Core contracts for the project-centric workspace. User data is never legacy-imported implicitly.
pub mod client;
pub mod doctor;
pub mod git;
pub mod host;
mod local_socket;
pub mod model;
pub mod probe;
mod probe_host;
pub mod project;
pub mod protocol;
pub mod shell;
pub mod store;
pub mod terminal;
pub mod ui;

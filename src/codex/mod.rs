//! Generating codex-local configuration and launching it.

pub mod config;
pub mod launcher;

pub use config::build_document;
pub use config::render;
pub use config::to_cli_overrides;
pub use config::write_generated;
pub use launcher::prepare_delivery;
pub use launcher::resolve_program;
pub use launcher::which;
pub use launcher::ConfigDelivery;
pub use launcher::LaunchPlan;

#[cfg(test)]
mod tests;

//! Spawning, supervising and cleaning up processes llmx owns.

pub mod child;
pub mod manager;

pub use child::OwnedChild;
pub use manager::ProcessManager;

#[cfg(test)]
mod tests;

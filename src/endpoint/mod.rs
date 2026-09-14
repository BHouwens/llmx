//! Health checking and model discovery against an OpenAI-compatible endpoint.

pub mod health;
pub mod openai;

pub use health::WaitError;
pub use health::WaitProgress;
pub use openai::resolve_model;
pub use openai::ModelResolution;
pub use openai::ServedModel;

#[cfg(test)]
mod tests;

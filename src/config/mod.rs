//! Declarative model profiles: schema, loading and validation.

pub mod loader;
pub mod model;
pub mod validation;

pub use loader::load;
pub use model::BackendKind;
pub use model::ConfigFile;
pub use model::Isolation;
pub use model::ModelProfile;
pub use model::ResourceHints;
pub use model::RuntimeConfig;
pub use model::SshConfig;

#[cfg(test)]
mod tests;

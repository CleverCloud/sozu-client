//! # Configuration module
//!
//! This module provides helpers to load Sōzu configuration

use std::path::PathBuf;

use config::{ConfigError, File};
use sozu_command_lib::config::{Config, ConfigBuilder};

// -----------------------------------------------------------------------------
// Error

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to load configuration from path '{0}', {1}")]
    Build(String, ConfigError),
    #[error("failed to deserialize configuration keys-values into internal structure, {0}")]
    Deserialize(ConfigError),
    #[error("failed to convert configuration into internal representation structure, {0}")]
    Convert(Box<dyn std::error::Error + Send>),
    #[error("failed to convert path to utf-8 string, there is incompatibility, {0}")]
    PathIsInvalid(String),
}

// -----------------------------------------------------------------------------
// helpers

/// Try to load Sōzu configuration from path
#[tracing::instrument]
pub fn try_from(path: &PathBuf) -> Result<Config, Error> {
    let file_config = config::Config::builder()
        .add_source(File::from(path.as_path()).required(true))
        .build()
        .map_err(|err| Error::Build(path.display().to_string(), err))?
        .try_deserialize()
        .map_err(Error::Deserialize)?;

    let config_path = path
        .to_str()
        .ok_or_else(|| Error::PathIsInvalid(path.display().to_string()))?;

    ConfigBuilder::new(file_config, config_path)
        .into_config()
        .map_err(|err| Error::Convert(err.into()))
}

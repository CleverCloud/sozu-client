//! # Socket module
//!
//! This module provides helpers to interact with Unix sockets

use std::{io, path::PathBuf, time::Duration};

use mio::net::UnixStream;
use tokio::time::sleep;
use tracing::{debug, trace};

// -----------------------------------------------------------------------------
// Error

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to wait for the socket to be ready, {0}")]
    Wait(io::Error),
}

// -----------------------------------------------------------------------------
// helpers

#[tracing::instrument]
pub async fn connect(path: &PathBuf) -> Result<UnixStream, Error> {
    loop {
        return match UnixStream::connect(path) {
            Ok(stream) => {
                debug!(
                    path = path.display().to_string(),
                    "Successfully connected to socket"
                );
                Ok(stream)
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                trace!(
                    path = path.display().to_string(),
                    "Try to connect to socket"
                );
                sleep(Duration::from_millis(100)).await;
                continue;
            }
            Err(err) => Err(Error::Wait(err)),
        };
    }
}

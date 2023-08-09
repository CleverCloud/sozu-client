//! # Channel module
//!
//! This module provides a connection manager over Sōzu channel

use std::path::PathBuf;

use bb8::ManageConnection;
use sozu_command_lib::{
    channel::{Channel, ChannelError},
    config::Config,
    proto::command::{Request, Response},
};
use tracing::debug;

use crate::socket;

// -----------------------------------------------------------------------------
// Error

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to load configuration, {0}")]
    Load(crate::config::Error),
    #[error("failed to connect to socket, {0}")]
    Connect(socket::Error),
    #[error("socket is unhealthy, {0}")]
    SocketError(std::io::Error),
    #[error("failed to set blocking the socket, {0}")]
    Blocking(ChannelError),
}

// -----------------------------------------------------------------------------
// ConnectionManagerOpts

#[derive(PartialEq, Eq, Clone, Debug)]
pub struct ConnectionProperties {
    pub socket: PathBuf,
    pub buffer_size: usize,
    pub max_buffer_size: usize,
}

impl From<&Config> for ConnectionProperties {
    #[tracing::instrument(skip_all)]
    fn from(config: &Config) -> Self {
        Self {
            socket: PathBuf::from(&config.command_socket),
            buffer_size: config.command_buffer_size,
            max_buffer_size: config.max_command_buffer_size,
        }
    }
}

impl TryFrom<&PathBuf> for ConnectionProperties {
    type Error = Error;

    #[tracing::instrument]
    fn try_from(path: &PathBuf) -> Result<Self, Self::Error> {
        let config = crate::config::try_from(path).map_err(Error::Load)?;

        Ok(Self::from(&config))
    }
}

impl ConnectionProperties {
    #[tracing::instrument]
    fn new(socket: PathBuf, buffer_size: usize, max_buffer_size: usize) -> Self {
        Self {
            socket,
            buffer_size,
            max_buffer_size,
        }
    }
}

// -----------------------------------------------------------------------------
// ConnectionManager

#[derive(Clone, Debug)]
pub struct ConnectionManager {
    opts: ConnectionProperties,
}

#[async_trait::async_trait]
impl ManageConnection for ConnectionManager {
    type Connection = Channel<Request, Response>;
    type Error = Error;

    #[tracing::instrument(skip_all)]
    async fn connect(&self) -> Result<Self::Connection, Self::Error> {
        debug!(
            path = self.opts.socket.display().to_string(),
            "Connect to Sōzu' socket"
        );
        let sock = socket::connect(&self.opts.socket)
            .await
            .map_err(Error::Connect)?;

        let mut channel = Channel::new(sock, self.opts.buffer_size, self.opts.max_buffer_size);

        channel.blocking().map_err(Error::Blocking)?;

        Ok(channel)
    }

    #[tracing::instrument(skip_all)]
    async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Self::Error> {
        // We do not perform a ping-pong message as we know that the unix socket
        // is working if it is present.
        //
        // We only check that the socket behind the connection has no error.
        match conn.sock.take_error() {
            Ok(Some(err)) | Err(err) => Err(Error::SocketError(err)),
            Ok(None) => Ok(()),
        }
    }

    #[tracing::instrument(skip_all)]
    fn has_broken(&self, conn: &mut Self::Connection) -> bool {
        // Check if the connection could be reused by the connection pool.
        //
        // We only check that the socket behind the connection has no error.
        match conn.sock.take_error() {
            Ok(Some(_)) | Err(_) => true,
            Ok(None) => false,
        }
    }
}

impl ConnectionManager {
    #[tracing::instrument(skip_all)]
    pub fn new(opts: ConnectionProperties) -> Self {
        Self { opts }
    }
}

//! # Channel module
//!
//! This module provides a connection manager over Sōzu channel

use std::{
    os::fd::{AsRawFd, RawFd},
    path::PathBuf,
};

use bb8::ManageConnection;
use sozu_command_lib::{
    channel::{Channel, ChannelError},
    config::Config,
    proto::command::{Request, Response},
    ready::Ready,
};
use tokio::io::{Interest, Ready as TokioReady, unix::AsyncFd};
use tracing::{debug, error};

use crate::socket;

// -----------------------------------------------------------------------------
// Error

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to load configuration, {0}")]
    Load(Box<crate::config::Error>),
    #[error("failed to connect to socket, {0}")]
    Connect(socket::Error),
    #[error("socket is unhealthy, {0}")]
    SocketError(std::io::Error),
    #[error("failed to set the socket non-blocking, {0}")]
    NonBlocking(ChannelError),
    #[error("failed to register the socket with Tokio, {0}")]
    Register(std::io::Error),
}

// -----------------------------------------------------------------------------
// ConnectionManagerOpts

#[derive(PartialEq, Eq, Clone, Debug)]
pub struct ConnectionProperties {
    pub socket: PathBuf,
    pub buffer_size: u64,
    pub max_buffer_size: u64,
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
        let config = crate::config::try_from(path).map_err(|error| Error::Load(Box::new(error)))?;

        Ok(Self::from(&config))
    }
}

// -----------------------------------------------------------------------------
// ConnectionManager

#[derive(Debug)]
struct ChannelIo {
    channel: Channel<Request, Response>,
}

impl AsRawFd for ChannelIo {
    fn as_raw_fd(&self) -> RawFd {
        self.channel.fd()
    }
}

/// A Sōzu channel registered with Tokio's I/O driver.
///
/// The inner channel is intentionally private: replacing it while its file
/// descriptor is registered would violate [`AsyncFd`]'s safety contract.
#[derive(Debug)]
pub struct Connection {
    io: AsyncFd<ChannelIo>,
    reusable: bool,
}

impl Connection {
    pub(crate) fn new(mut channel: Channel<Request, Response>) -> Result<Self, Error> {
        channel.nonblocking().map_err(Error::NonBlocking)?;
        let io = ChannelIo { channel };

        // SAFETY: `io` owns the channel and therefore its open socket. Its
        // `AsRawFd` implementation always returns that socket's descriptor,
        // and no API can replace or close the private channel while `AsyncFd`
        // owns it. `AsyncFd` deregisters the descriptor before dropping `io`.
        let io =
            unsafe { AsyncFd::register_with_interest(io, Interest::READABLE | Interest::WRITABLE) }
                .map_err(|error| {
                    let (_, cause) = error.into_parts();
                    Error::Register(cause)
                })?;

        Ok(Self { io, reusable: true })
    }

    pub(crate) async fn write_message(&mut self, request: &Request) -> Result<(), ChannelError> {
        self.reusable = false;
        self.io.get_mut().channel.write_message(request)?;

        while self.io.get_ref().channel.back_buf.available_data() > 0 {
            let mut ready = self
                .io
                .ready_mut(Interest::WRITABLE)
                .await
                .map_err(ChannelError::Write)?;
            let channel = &mut ready.get_inner_mut().channel;
            channel.handle_events(Ready::WRITABLE);
            channel.writable()?;

            if channel.back_buf.available_data() > 0 {
                ready.clear_ready_matching(TokioReady::WRITABLE);
            }
        }

        Ok(())
    }

    pub(crate) async fn read_message(&mut self) -> Result<Response, ChannelError> {
        loop {
            match self.io.get_mut().channel.read_message() {
                Ok(response) => return Ok(response),
                Err(ChannelError::NothingRead) => {}
                Err(error) => return Err(error),
            }

            let mut ready = self
                .io
                .ready_mut(Interest::READABLE)
                .await
                .map_err(ChannelError::Read)?;
            let read_result = {
                let channel = &mut ready.get_inner_mut().channel;
                channel.handle_events(Ready::READABLE);
                channel.readable()
            };
            if ready.get_inner().channel.readiness.is_readable() {
                // `Channel::readable` also stops when its bounded front buffer
                // is full. Preserve Tokio's readiness in that case so the
                // buffered frames can be decoded before reading the socket
                // again, even though no new edge may arrive.
                ready.retain_ready();
            } else {
                ready.clear_ready_matching(TokioReady::READABLE);
            }

            match read_result {
                Ok(_) => {}
                Err(ChannelError::NoByteToRead) => {
                    return match ready.get_inner_mut().channel.read_message() {
                        Ok(response) => Ok(response),
                        Err(ChannelError::NothingRead) => Err(ChannelError::NoByteToRead),
                        Err(error) => Err(error),
                    };
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn mark_reusable(&mut self) -> bool {
        let readiness = self.io.get_ref().channel.readiness;
        self.reusable =
            !(readiness.is_error() || readiness.is_hup() || readiness.is_write_closed());
        self.reusable
    }

    fn socket_error(&mut self) -> Result<Option<std::io::Error>, std::io::Error> {
        self.io.get_mut().channel.sock.take_error()
    }
}

#[derive(Clone, Debug)]
pub struct ConnectionManager {
    opts: ConnectionProperties,
}

impl ManageConnection for ConnectionManager {
    type Connection = Connection;
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

        Connection::new(Channel::new(
            sock,
            self.opts.buffer_size,
            self.opts.max_buffer_size,
        ))
    }

    #[tracing::instrument(skip_all)]
    async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Self::Error> {
        // We do not perform a ping-pong message as we know that the unix socket
        // is working if it is present.
        //
        // We only check that the socket behind the connection has no error.
        match conn.socket_error() {
            Ok(Some(err)) | Err(err) => {
                error!(error = err.to_string(), "connexion to sōzu has errors");
                Err(Error::SocketError(err))
            }
            Ok(None) => Ok(()),
        }
    }

    #[tracing::instrument(skip_all)]
    fn has_broken(&self, conn: &mut Self::Connection) -> bool {
        if !conn.reusable {
            return true;
        }

        // Check if the connection could be reused by the connection pool.
        //
        // We only check that the socket behind the connection has no error.
        match conn.socket_error() {
            Ok(Some(err)) | Err(err) => {
                error!(error = err.to_string(), "connexion to sōzu has errors");
                true
            }
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

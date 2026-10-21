//! # Unpooled module
//!
//! This module provides an unpooled client to interact with Sōzu.
//! It is provided to directly connect to the proxy for debugging purposes.

use std::{future::Future, time::Duration};

use sozu_command_lib::{
    channel::{Channel, ChannelError},
    proto::command::{Request, Response, ResponseStatus, WorkerRequest, request::RequestType},
};
use tokio::{
    fs::File,
    io::{AsyncWriteExt, BufWriter},
    sync::Mutex,
    task::{JoinError, spawn_blocking as blocking},
    time::timeout,
};
use tracing::{debug, trace};

use crate::{
    DEFAULT_REQUEST_TIMEOUT, Sender,
    channel::{Connection, ConnectionProperties},
    socket,
};

// -----------------------------------------------------------------------------
// Error

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to execute blocking task, {0}")]
    Join(JoinError),
    #[error("failed to send request to Sōzu, {0}")]
    Send(ChannelError),
    #[error("failed to read response from Sōzu, {0}")]
    Receive(ChannelError),
    #[error("got an invalid status code, {0}")]
    InvalidStatusCode(i32),
    #[error("failed to execute request on Sōzu")]
    Failure(Box<Response>),
    #[error("failed to create temporary directory, {0}")]
    CreateTempDir(std::io::Error),
    #[error("failed to create temporary file, {0}")]
    CreateTempFile(std::io::Error),
    #[error("failed to serialize worker request, {0}")]
    Serialize(serde_json::Error),
    #[error("failed to write worker request, {0}")]
    Write(std::io::Error),
    #[error("failed to flush worker request buffer, {0}")]
    Flush(std::io::Error),
    #[error("failed to connect to socket, {0}")]
    Connect(socket::Error),
    #[error("failed to prepare the socket for asynchronous I/O, {0}")]
    Prepare(crate::channel::Error),
    #[error("Sōzu operation timed out after {0:?}; a request outcome may be unknown")]
    Timeout(Duration),
    #[error("Sōzu connection is unavailable after an interrupted request")]
    Unavailable,
}

impl From<JoinError> for Error {
    #[tracing::instrument]
    fn from(err: JoinError) -> Self {
        Self::Join(err)
    }
}

// -----------------------------------------------------------------------------
// Client

pub struct Client {
    channel: Mutex<Option<Connection>>,
    request_timeout: Duration,
}

#[async_trait::async_trait]
impl Sender for Client {
    type Error = Error;

    #[tracing::instrument(skip_all)]
    async fn send(&self, request: RequestType) -> Result<Response, Self::Error> {
        self.run_with_timeout(self.send_inner(request)).await
    }

    #[tracing::instrument(skip_all)]
    async fn send_all(&self, requests: &[RequestType]) -> Result<Response, Self::Error> {
        self.run_with_timeout(self.send_all_inner(requests)).await
    }
}

impl Client {
    async fn send_inner(&self, request: RequestType) -> Result<Response, Error> {
        trace!("Retrieve a connection to Sōzu's socket");
        let mut slot = self.channel.lock().await;
        let mut conn = slot.take().ok_or(Error::Unavailable)?;

        trace!("Send request to Sōzu");
        conn.write_message(&Request {
            request_type: Some(request),
        })
        .await
        .map_err(Error::Send)?;

        loop {
            trace!("Read request to Sōzu");
            let response = conn.read_message().await.map_err(Error::Receive)?;

            let status = ResponseStatus::try_from(response.status)
                .map_err(|_| Error::InvalidStatusCode(response.status))?;

            match status {
                ResponseStatus::Processing => {
                    // Buffered responses can otherwise keep this future ready
                    // forever and prevent the outer deadline from being polled.
                    tokio::task::coop::consume_budget().await;
                    continue;
                }
                ResponseStatus::Failure => {
                    if conn.mark_reusable() {
                        *slot = Some(conn);
                    }
                    return Err(Error::Failure(Box::new(response)));
                }
                ResponseStatus::Ok => {
                    if conn.mark_reusable() {
                        *slot = Some(conn);
                    }
                    return Ok(response);
                }
            }
        }
    }

    async fn send_all_inner(&self, requests: &[RequestType]) -> Result<Response, Error> {
        // -------------------------------------------------------------------------
        // Create temporary folder and writer to batch requests
        let tmpdir = blocking(|| tempfile::tempdir().map_err(Error::CreateTempDir)).await??;

        let path = tmpdir.path().join("requests.json");
        let mut writer = BufWriter::new(File::create(&path).await.map_err(Error::CreateTempFile)?);

        for (idx, request) in requests.iter().cloned().enumerate() {
            let worker_request = WorkerRequest {
                id: format!("{}-{idx}", env!("CARGO_PKG_NAME")).to_uppercase(),
                content: Request::from(request),
            };

            let payload =
                blocking(move || serde_json::to_string(&worker_request).map_err(Error::Serialize))
                    .await??;

            writer
                .write_all(format!("{payload}\n\0").as_bytes())
                .await
                .map_err(Error::Write)?;
        }

        writer.flush().await.map_err(Error::Flush)?;

        // -------------------------------------------------------------------------
        // Send a LoadState request with the file that we have created.
        self.send_inner(RequestType::LoadState(path.to_string_lossy().to_string()))
            .await
    }

    async fn run_with_timeout<T, F>(&self, operation: F) -> Result<T, Error>
    where
        F: Future<Output = Result<T, Error>>,
    {
        timeout(self.request_timeout, operation)
            .await
            .map_err(|_| Error::Timeout(self.request_timeout))?
    }
}

impl Client {
    /// Set the absolute deadline applied to every later request.
    #[must_use]
    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    #[tracing::instrument(skip_all)]
    pub async fn connect(props: ConnectionProperties) -> Result<Self, Error> {
        Self::connect_with_timeout(props, DEFAULT_REQUEST_TIMEOUT).await
    }

    /// Connect with an absolute deadline and use it for every later request.
    #[tracing::instrument(skip_all)]
    pub async fn connect_with_timeout(
        props: ConnectionProperties,
        request_timeout: Duration,
    ) -> Result<Self, Error> {
        timeout(request_timeout, Self::connect_inner(props, request_timeout))
            .await
            .map_err(|_| Error::Timeout(request_timeout))?
    }

    async fn connect_inner(
        props: ConnectionProperties,
        request_timeout: Duration,
    ) -> Result<Self, Error> {
        debug!(
            path = props.socket.display().to_string(),
            "Connect to Sōzu' socket"
        );

        let sock = socket::connect(&props.socket)
            .await
            .map_err(Error::Connect)?;

        let channel = Channel::new(sock, props.buffer_size, props.max_buffer_size);
        let connection = Connection::new(channel).map_err(Error::Prepare)?;

        Ok(Self {
            channel: Mutex::new(Some(connection)),
            request_timeout,
        })
    }
}

//! # Sōzu client
//!
//! This library provides a client to interact with Sōzu.
//! The client is able to do one-time request or send batches.

use std::{future::Future, time::Duration};

use bb8::Pool;
use sozu_command_lib::{
    channel::ChannelError,
    proto::command::{Request, Response, ResponseStatus, WorkerRequest, request::RequestType},
};

use tokio::{
    fs::File,
    io::{AsyncWriteExt, BufWriter},
    task::{JoinError, spawn_blocking as blocking},
    time::timeout,
};
use tracing::trace;

use crate::channel::{ConnectionManager, ConnectionProperties};

pub mod channel;
pub mod config;
pub mod socket;
#[cfg(feature = "unpooled")]
pub mod unpooled;

// -----------------------------------------------------------------------------
// Error

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to create connection pool over unix socket, {0}")]
    CreatePool(Box<channel::Error>),
    #[error("failed to execute blocking task, {0}")]
    Join(JoinError),
    #[error("failed to get connection to socket, {0}")]
    GetConnection(Box<bb8::RunError<channel::Error>>),
    #[error("failed to send request, {0}")]
    Send(ChannelError),
    #[error("failed to read response, {0}")]
    Receive(ChannelError),
    #[error("got an invalid status code, {0}")]
    InvalidStatusCode(i32),
    #[error("failed to execute request, got status '{0}', {1}")]
    Failure(String, String, Box<Response>),
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
    #[error("Sōzu request timed out after {0:?}; its outcome is unknown")]
    Timeout(Duration),
}

impl From<JoinError> for Error {
    #[tracing::instrument]
    fn from(err: JoinError) -> Self {
        Self::Join(err)
    }
}

impl Error {
    #[tracing::instrument]
    pub fn is_recoverable(&self) -> bool {
        !matches!(
            self,
            Self::Send(_)
                | Self::Receive(_)
                | Self::CreatePool(_)
                | Self::GetConnection(_)
                | Self::Timeout(_)
        )
    }
}

// -----------------------------------------------------------------------------
// Sender

#[async_trait::async_trait]
pub trait Sender {
    type Error;

    async fn send(&self, request: RequestType) -> Result<Response, Self::Error>;

    async fn send_all(&self, requests: &[RequestType]) -> Result<Response, Self::Error>;
}

// -----------------------------------------------------------------------------
// Client

/// Maximum duration of a request unless the caller configures another value.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct Client {
    pool: Pool<ConnectionManager>,
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
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|error| Error::GetConnection(Box::new(error)))?;

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
                    conn.mark_reusable();
                    return Err(Error::Failure(
                        status.as_str_name().to_string(),
                        response.message.to_string().to_lowercase(),
                        Box::new(response),
                    ));
                }
                ResponseStatus::Ok => {
                    conn.mark_reusable();
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

impl From<Pool<ConnectionManager>> for Client {
    #[tracing::instrument(skip_all)]
    fn from(pool: Pool<ConnectionManager>) -> Self {
        Self {
            pool,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

impl Client {
    /// Set the absolute deadline applied to every later request.
    #[must_use]
    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    #[tracing::instrument]
    pub async fn try_new(opts: ConnectionProperties) -> Result<Self, Error> {
        let pool = Pool::builder()
            .build(ConnectionManager::new(opts))
            .await
            .map_err(|error| Error::CreatePool(Box::new(error)))?;

        Ok(Self::from(pool))
    }
}

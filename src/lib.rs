//! # Sōzu client
//!
//! This library provides a client to interact with Sōzu.
//! The client is able to do one-time request or send batches.

use bb8::Pool;
use sozu_command_lib::{
    channel::ChannelError,
    proto::command::{request::RequestType, Request, Response, ResponseStatus},
    request::WorkerRequest,
};
use tempdir::TempDir;
use tokio::{
    fs::File,
    io::{AsyncWriteExt, BufWriter},
    task::{spawn_blocking as blocking, JoinError},
};
use tracing::trace;

use crate::channel::{ConnectionManager, ConnectionProperties};

pub mod channel;
pub mod config;
pub mod socket;

// -----------------------------------------------------------------------------
// Error

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to create connection pool to Sōzu's socket")]
    CreatePool(channel::Error),
    #[error("failed to execute blocking task, {0}")]
    Join(JoinError),
    #[error("failed to get connection to socket, {0}")]
    GetConnection(bb8::RunError<channel::Error>),
    #[error("failed to send request to Sōzu, {0}")]
    Send(ChannelError),
    #[error("failed to read response from Sōzu, {0}")]
    Receive(ChannelError),
    #[error("got an invalid status code, {0}")]
    InvalidStatusCode(i32),
    #[error("failed to execute request on Sōzu")]
    Failure(Response),
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
}

impl From<JoinError> for Error {
    #[tracing::instrument]
    fn from(err: JoinError) -> Self {
        Self::Join(err)
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

#[derive(Clone, Debug)]
pub struct Client {
    pool: Pool<ConnectionManager>,
}

#[async_trait::async_trait]
impl Sender for Client {
    type Error = Error;

    #[tracing::instrument(skip_all)]
    async fn send(&self, request: RequestType) -> Result<Response, Self::Error> {
        trace!("Retrieve a connection to Sōzu's socket");
        let mut conn = self.pool.get().await.map_err(Error::GetConnection)?;

        trace!("Send request to Sōzu");
        conn.write_message(&Request {
            request_type: Some(request),
        })
        .map_err(Error::Send)?;

        loop {
            trace!("Read request to Sōzu");
            let response = conn.read_message().map_err(Error::Receive)?;

            let status = ResponseStatus::try_from(response.status)
                .map_err(|_| Error::InvalidStatusCode(response.status))?;

            match status {
                ResponseStatus::Processing => continue,
                ResponseStatus::Failure => {
                    return Err(Error::Failure(response));
                }
                ResponseStatus::Ok => {
                    return Ok(response);
                }
            }
        }
    }

    #[tracing::instrument(skip_all)]
    async fn send_all(&self, requests: &[RequestType]) -> Result<Response, Self::Error> {
        // -------------------------------------------------------------------------
        // Create temporary folder and writer to batch requests
        let tmpdir =
            blocking(|| TempDir::new(env!("CARGO_PKG_NAME")).map_err(Error::CreateTempDir))
                .await??;

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
        self.send(RequestType::LoadState(path.to_string_lossy().to_string()))
            .await
    }
}

impl From<Pool<ConnectionManager>> for Client {
    #[tracing::instrument(skip_all)]
    fn from(pool: Pool<ConnectionManager>) -> Self {
        Self { pool }
    }
}

impl Client {
    #[tracing::instrument]
    pub async fn try_new(opts: ConnectionProperties) -> Result<Self, Error> {
        let pool = Pool::builder()
            .build(ConnectionManager::new(opts))
            .await
            .map_err(Error::CreatePool)?;

        Ok(Self::from(pool))
    }
}

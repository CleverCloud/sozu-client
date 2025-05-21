//! # Unpooled module
//!
//! This module provides an unpooled client to interact with Sōzu.
//! It is provided to directly connect to the proxy for debugging purposes.

use sozu_command_lib::{
    channel::{Channel, ChannelError},
    proto::command::{request::RequestType, Request, Response, ResponseStatus, WorkerRequest},
};
use tokio::{
    fs::File,
    io::{AsyncWriteExt, BufWriter},
    sync::Mutex,
    task::{spawn_blocking as blocking, JoinError},
};
use tracing::{debug, trace};

use crate::{channel::ConnectionProperties, socket, Sender};

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
    #[error("failed to connect to socket, {0}")]
    Connect(socket::Error),
    #[error("failed to set blocking the socket, {0}")]
    Blocking(ChannelError),
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
    channel: Mutex<Channel<Request, Response>>,
}

#[async_trait::async_trait]
impl Sender for Client {
    type Error = Error;

    #[tracing::instrument(skip_all)]
    async fn send(&self, request: RequestType) -> Result<Response, Self::Error> {
        trace!("Retrieve a connection to Sōzu's socket");
        let mut conn = self.channel.lock().await;

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
            blocking(|| tempfile::tempdir().map_err(Error::CreateTempDir))
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

impl Client {
    #[tracing::instrument(skip_all)]
    pub async fn connect(props: ConnectionProperties) -> Result<Self, Error> {
        debug!(
            path = props.socket.display().to_string(),
            "Connect to Sōzu' socket"
        );

        let sock = socket::connect(&props.socket)
            .await
            .map_err(Error::Connect)?;

        let mut channel = Channel::new(sock, props.buffer_size, props.max_buffer_size);

        channel.blocking().map_err(Error::Blocking)?;

        Ok(Self {
            channel: Mutex::new(channel),
        })
    }
}

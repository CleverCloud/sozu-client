use std::{
    os::unix::net::UnixListener,
    path::Path,
    thread::{self, JoinHandle},
};

use mio::net::UnixStream;
use sozu_client::{Client, Error, Sender, channel::ConnectionProperties};
use sozu_command_lib::{
    channel::Channel,
    proto::command::{Request, Response, ResponseStatus, Status, request::RequestType},
};

const BUFFER_SIZE: u64 = 16_384;
const MAX_BUFFER_SIZE: u64 = 163_840;
const FAILURE_MESSAGE: &str = "cluster update refused";

fn properties(path: &Path) -> ConnectionProperties {
    ConnectionProperties {
        socket: path.to_owned(),
        buffer_size: BUFFER_SIZE,
        max_buffer_size: MAX_BUFFER_SIZE,
    }
}

fn failure_server(path: &Path) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("failure server socket must bind");

    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("client must connect");
        stream
            .set_nonblocking(true)
            .expect("accepted socket must become non-blocking");
        let mut channel = Channel::<Response, Request>::new(
            UnixStream::from_std(stream),
            BUFFER_SIZE,
            MAX_BUFFER_SIZE,
        );
        channel
            .blocking()
            .expect("failure server channel must become blocking");

        let request = channel.read_message().expect("client request must decode");
        assert!(matches!(request.request_type, Some(RequestType::Status(_))));

        let response = Response {
            status: ResponseStatus::Failure as i32,
            message: FAILURE_MESSAGE.to_owned(),
            ..Response::default()
        };
        channel
            .write_message(&response)
            .expect("failure response must be sent");
    })
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("test runtime must build")
}

#[test]
fn test_pooled_client_failure_preserves_response() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let server = failure_server(&path);

    let error = runtime().block_on(async {
        let client = Client::try_new(properties(&path))
            .await
            .expect("pooled client must be created");
        client
            .send(RequestType::Status(Status {}))
            .await
            .expect_err("failure response must remain an error")
    });
    server.join().expect("failure server must exit cleanly");

    match error {
        Error::Failure(status, message, response) => {
            assert_eq!(status, "FAILURE");
            assert_eq!(message, FAILURE_MESSAGE);
            assert_eq!(response.status, ResponseStatus::Failure as i32);
            assert_eq!(response.message, FAILURE_MESSAGE);
        }
        other => panic!("expected Error::Failure, got {other:?}"),
    }
}

#[cfg(feature = "unpooled")]
#[test]
fn test_unpooled_client_failure_preserves_response() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let server = failure_server(&path);

    let error = runtime().block_on(async {
        let client = sozu_client::unpooled::Client::connect(properties(&path))
            .await
            .expect("unpooled client must connect");
        client
            .send(RequestType::Status(Status {}))
            .await
            .expect_err("failure response must remain an error")
    });
    server.join().expect("failure server must exit cleanly");

    match error {
        sozu_client::unpooled::Error::Failure(response) => {
            assert_eq!(response.status, ResponseStatus::Failure as i32);
            assert_eq!(response.message, FAILURE_MESSAGE);
        }
        other => panic!("expected unpooled Error::Failure, got {other:?}"),
    }
}

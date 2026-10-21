use std::{
    io::ErrorKind,
    os::unix::net::UnixListener,
    path::Path,
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use bb8::Pool;
use mio::net::UnixStream;
use sozu_client::{
    Client, Error, Sender,
    channel::{ConnectionManager, ConnectionProperties},
};
use sozu_command_lib::{
    channel::Channel,
    proto::command::{Request, Response, ResponseStatus, Status, request::RequestType},
    ready::Ready,
};

const BUFFER_SIZE: u64 = 16_384;
const MAX_BUFFER_SIZE: u64 = 163_840;

fn properties(path: &Path) -> ConnectionProperties {
    ConnectionProperties {
        socket: path.to_owned(),
        buffer_size: BUFFER_SIZE,
        max_buffer_size: MAX_BUFFER_SIZE,
    }
}

fn channel(stream: std::os::unix::net::UnixStream) -> Channel<Response, Request> {
    stream
        .set_nonblocking(true)
        .expect("accepted socket must become non-blocking");
    let mut channel = Channel::new(UnixStream::from_std(stream), BUFFER_SIZE, MAX_BUFFER_SIZE);
    channel
        .blocking()
        .expect("server channel must become blocking");
    channel
}

fn accept_before(listener: &UnixListener, timeout: Duration) -> std::os::unix::net::UnixStream {
    listener
        .set_nonblocking(true)
        .expect("listener must become non-blocking");
    let deadline = Instant::now() + timeout;

    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("client must connect before {timeout:?}: {error}"),
        }
    }
}

fn ok_response(message: &str) -> Response {
    Response {
        status: ResponseStatus::Ok as i32,
        message: message.to_owned(),
        ..Response::default()
    }
}

fn progress_gated_server(path: &Path, progress: mpsc::Receiver<()>) -> JoinHandle<bool> {
    let listener = UnixListener::bind(path).expect("server socket must bind");

    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("client must connect");
        let mut channel = channel(stream);

        let request = channel.read_message().expect("client request must decode");
        assert!(matches!(request.request_type, Some(RequestType::Status(_))));

        let executor_progressed = progress.recv_timeout(Duration::from_millis(250)).is_ok();
        channel
            .write_message(&ok_response("executor progressed"))
            .expect("response must be sent");
        executor_progressed
    })
}

fn silent_server(path: &Path, hold: Duration) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("server socket must bind");

    thread::spawn(move || {
        let mut channel = channel(accept_before(&listener, Duration::from_secs(1)));
        channel.read_message().expect("client request must decode");
        thread::sleep(hold);
    })
}

fn non_reading_server(path: &Path, hold: Duration) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("server socket must bind");

    thread::spawn(move || {
        let _stream = accept_before(&listener, Duration::from_secs(1));
        thread::sleep(hold);
    })
}

fn processing_server(path: &Path) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("server socket must bind");

    thread::spawn(move || {
        let mut channel = channel(accept_before(&listener, Duration::from_secs(1)));
        channel.read_message().expect("client request must decode");

        for _ in 0..4 {
            if channel
                .write_message(&Response {
                    status: ResponseStatus::Processing as i32,
                    ..Response::default()
                })
                .is_err()
            {
                return;
            }
            thread::sleep(Duration::from_millis(40));
        }

        let _ = channel.write_message(&ok_response("too late"));
    })
}

fn preloaded_processing_server(
    path: &Path,
    responses_written: mpsc::Sender<()>,
    response_drained: mpsc::Receiver<()>,
) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("server socket must bind");

    thread::spawn(move || {
        let mut channel = channel(accept_before(&listener, Duration::from_secs(1)));
        channel.read_message().expect("first request must decode");
        channel
            .nonblocking()
            .expect("server channel must become non-blocking");
        channel
            .write_message(&ok_response("first response"))
            .expect("first response must be buffered");
        for _ in 0..1_024 {
            channel
                .write_message(&Response {
                    status: ResponseStatus::Processing as i32,
                    ..Response::default()
                })
                .expect("Processing response must be buffered");
        }
        channel
            .write_message(&ok_response("too late"))
            .expect("terminal response must be buffered");
        channel.handle_events(Ready::WRITABLE);
        channel
            .writable()
            .expect("preloaded responses must fit in one socket write");
        assert_eq!(
            channel.back_buf.available_data(),
            0,
            "all responses must be in the socket before the deadline test"
        );
        responses_written
            .send(())
            .expect("test must observe the preloaded responses");

        channel
            .blocking()
            .expect("server channel must become blocking");
        channel.read_message().expect("second request must decode");
        response_drained
            .recv_timeout(Duration::from_secs(1))
            .expect("test must keep the socket open until the request ends");
    })
}

fn late_response_server(path: &Path) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("server socket must bind");

    thread::spawn(move || {
        let mut first = channel(accept_before(&listener, Duration::from_secs(1)));
        first.read_message().expect("first request must decode");
        thread::sleep(Duration::from_millis(150));
        let _ = first.write_message(&ok_response("late response"));

        let mut second = channel(accept_before(&listener, Duration::from_secs(1)));
        second.read_message().expect("second request must decode");
        second
            .write_message(&ok_response("fresh response"))
            .expect("second response must be sent");
    })
}

fn burst_server(
    path: &Path,
    request_received: mpsc::Sender<()>,
    write_gate: mpsc::Receiver<()>,
    burst_written: mpsc::Sender<()>,
    response_drained: mpsc::Receiver<()>,
) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("server socket must bind");

    thread::spawn(move || {
        let mut channel = channel(accept_before(&listener, Duration::from_secs(1)));
        channel.read_message().expect("client request must decode");
        request_received
            .send(())
            .expect("test must wait for the request");
        write_gate
            .recv_timeout(Duration::from_secs(1))
            .expect("test must release the response burst");

        channel
            .nonblocking()
            .expect("server channel must become non-blocking");
        for _ in 0..64 {
            channel
                .write_message(&Response {
                    status: ResponseStatus::Processing as i32,
                    ..Response::default()
                })
                .expect("Processing response must be sent");
        }
        channel
            .write_message(&ok_response("burst complete"))
            .expect("terminal response must be sent");
        channel.handle_events(Ready::WRITABLE);
        channel
            .writable()
            .expect("complete burst must fit in one socket write");
        assert_eq!(
            channel.back_buf.available_data(),
            0,
            "the entire response burst must reach the socket before the client reads"
        );
        burst_written
            .send(())
            .expect("test must wait for the complete response burst");
        response_drained
            .recv_timeout(Duration::from_secs(1))
            .expect("test must keep the server socket open until the burst is drained");
    })
}

fn cancellation_server(
    path: &Path,
    first_request_received: mpsc::Sender<()>,
    late_response_gate: mpsc::Receiver<()>,
) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("server socket must bind");

    thread::spawn(move || {
        let mut first = channel(accept_before(&listener, Duration::from_secs(1)));
        first.read_message().expect("first request must decode");
        first_request_received
            .send(())
            .expect("test must observe the first request");
        late_response_gate
            .recv_timeout(Duration::from_secs(1))
            .expect("test must abort the first request before its response");
        let _ = first.write_message(&ok_response("cancelled response"));

        let mut second = channel(accept_before(&listener, Duration::from_secs(1)));
        second.read_message().expect("second request must decode");
        second
            .write_message(&ok_response("fresh after cancellation"))
            .expect("second response must be sent");
    })
}

fn reuse_server(path: &Path) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("server socket must bind");

    thread::spawn(move || {
        let mut channel = channel(accept_before(&listener, Duration::from_secs(1)));
        for message in ["first response", "second response"] {
            channel.read_message().expect("client request must decode");
            channel
                .write_message(&ok_response(message))
                .expect("response must be sent");
        }
    })
}

fn assert_timeout(error: Error, expected: Duration) {
    assert!(
        !error.is_recoverable(),
        "a timeout has an unknown request outcome and must not be retried blindly"
    );
    assert!(
        matches!(&error, Error::Timeout(actual) if *actual == expected),
        "expected a {expected:?} timeout, got {error:?}"
    );
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("test runtime must build")
}

#[test]
fn test_waiting_for_sozu_does_not_block_the_tokio_executor() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let (progress_tx, progress_rx) = mpsc::channel();
    let server = progress_gated_server(&path, progress_rx);

    runtime().block_on(async {
        let client = Client::try_new(properties(&path))
            .await
            .expect("client must connect");
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            progress_tx
                .send(())
                .expect("server must still wait for executor progress");
        });

        client
            .send(RequestType::Status(Status {}))
            .await
            .expect("request must succeed");
    });

    assert!(
        server.join().expect("server must exit cleanly"),
        "a synchronous channel read blocked every other task on the Tokio executor"
    );
}

#[test]
fn test_silent_server_is_bounded_by_the_request_deadline() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let request_timeout = Duration::from_millis(60);
    let server = silent_server(&path, Duration::from_millis(200));

    let error = runtime().block_on(async {
        Client::try_new(properties(&path))
            .await
            .expect("client must be created")
            .with_request_timeout(request_timeout)
            .send(RequestType::Status(Status {}))
            .await
            .expect_err("silent Sōzu must time out")
    });
    server.join().expect("server must exit cleanly");

    assert_timeout(error, request_timeout);
}

#[test]
fn test_processing_responses_do_not_reset_the_request_deadline() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let request_timeout = Duration::from_millis(90);
    let server = processing_server(&path);

    let error = runtime().block_on(async {
        Client::try_new(properties(&path))
            .await
            .expect("client must be created")
            .with_request_timeout(request_timeout)
            .send(RequestType::Status(Status {}))
            .await
            .expect_err("Processing responses must not extend the absolute deadline")
    });
    server.join().expect("server must exit cleanly");

    assert_timeout(error, request_timeout);
}

#[test]
fn test_preloaded_processing_responses_cannot_starve_the_request_deadline() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let (written_tx, written_rx) = mpsc::channel();
    let (drained_tx, drained_rx) = mpsc::channel();
    let server = preloaded_processing_server(&path, written_tx, drained_rx);

    let error = runtime().block_on(async {
        let client = Client::try_new(properties(&path))
            .await
            .expect("client must be created")
            .with_request_timeout(Duration::from_secs(1));
        client
            .send(RequestType::Status(Status {}))
            .await
            .expect("first request must establish a reusable connection");
        tokio::task::spawn_blocking(move || written_rx.recv_timeout(Duration::from_secs(1)))
            .await
            .expect("response observer must run")
            .expect("server must preload every second response");

        client
            .with_request_timeout(Duration::ZERO)
            .send(RequestType::Status(Status {}))
            .await
            .expect_err("a ready Processing stream must not starve an expired deadline")
    });
    drained_tx
        .send(())
        .expect("server must keep the socket open until the request ends");
    server.join().expect("server must exit cleanly");

    assert_timeout(error, Duration::ZERO);
}

#[test]
fn test_response_burst_larger_than_the_read_buffer_is_fully_drained() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let (request_tx, request_rx) = mpsc::channel();
    let (write_gate_tx, write_gate_rx) = mpsc::channel();
    let (written_tx, written_rx) = mpsc::channel();
    let (drained_tx, drained_rx) = mpsc::channel();
    let server = burst_server(&path, request_tx, write_gate_rx, written_tx, drained_rx);
    let mut props = properties(&path);
    props.buffer_size = 64;
    props.max_buffer_size = 64;

    let response = runtime().block_on(async {
        let client = Client::try_new(props)
            .await
            .expect("client must be created")
            .with_request_timeout(Duration::from_millis(500));
        let request =
            tokio::spawn(async move { client.send(RequestType::Status(Status {})).await });

        tokio::task::spawn_blocking(move || request_rx.recv_timeout(Duration::from_secs(1)))
            .await
            .expect("request observer must run")
            .expect("server must receive the request");
        write_gate_tx
            .send(())
            .expect("server must start the response burst");

        // Intentionally keep the current-thread runtime from reading until the
        // complete burst is queued, making the socket contain more data than
        // the client's bounded front buffer.
        written_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server must queue the complete response burst");

        request
            .await
            .expect("request task must finish")
            .expect("client must drain every response without a new socket edge")
    });
    drained_tx
        .send(())
        .expect("server must keep the socket open until the response is drained");
    server.join().expect("server must exit cleanly");

    assert_eq!(response.message, "burst complete");
}

#[test]
fn test_timed_out_connection_cannot_supply_a_late_response() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let server = late_response_server(&path);

    let response = runtime().block_on(async {
        let client = Client::try_new(properties(&path))
            .await
            .expect("client must be created");
        let short = client
            .clone()
            .with_request_timeout(Duration::from_millis(50));
        let long = client.with_request_timeout(Duration::from_secs(1));

        let error = short
            .send(RequestType::Status(Status {}))
            .await
            .expect_err("first request must time out");
        assert_timeout(error, Duration::from_millis(50));

        long.send(RequestType::Status(Status {}))
            .await
            .expect("second request must use a fresh connection")
    });
    server.join().expect("server must exit cleanly");

    assert_eq!(response.message, "fresh response");
}

#[test]
fn test_cancelled_request_discards_the_connection_before_a_late_response() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let (received_tx, received_rx) = mpsc::channel();
    let (late_gate_tx, late_gate_rx) = mpsc::channel();
    let server = cancellation_server(&path, received_tx, late_gate_rx);

    let response = runtime().block_on(async {
        let client = Client::try_new(properties(&path))
            .await
            .expect("client must be created")
            .with_request_timeout(Duration::from_secs(1));
        let first_client = client.clone();
        let first =
            tokio::spawn(async move { first_client.send(RequestType::Status(Status {})).await });

        tokio::task::spawn_blocking(move || received_rx.recv_timeout(Duration::from_secs(1)))
            .await
            .expect("request observer must run")
            .expect("server must receive the first request");
        first.abort();
        assert!(
            first
                .await
                .expect_err("first request must be cancelled")
                .is_cancelled(),
            "the first request must end through task cancellation"
        );
        late_gate_tx
            .send(())
            .expect("server may now send the cancelled response");

        client
            .send(RequestType::Status(Status {}))
            .await
            .expect("second request must use a fresh connection")
    });
    server.join().expect("server must exit cleanly");

    assert_eq!(response.message, "fresh after cancellation");
}

#[test]
fn test_successful_requests_reuse_the_synchronized_connection() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let server = reuse_server(&path);

    let messages = runtime().block_on(async {
        let client = Client::try_new(properties(&path))
            .await
            .expect("client must be created")
            .with_request_timeout(Duration::from_millis(500));
        let first = client
            .send(RequestType::Status(Status {}))
            .await
            .expect("first request must succeed");
        let second = client
            .send(RequestType::Status(Status {}))
            .await
            .expect("second request must reuse the synchronized connection");
        (first.message, second.message)
    });
    server.join().expect("server must exit cleanly");

    assert_eq!(
        messages,
        ("first response".to_owned(), "second response".to_owned())
    );
}

#[test]
fn test_waiting_for_a_pool_connection_uses_the_request_deadline() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let listener = UnixListener::bind(&path).expect("server socket must bind");
    let server = thread::spawn(move || {
        let _stream = accept_before(&listener, Duration::from_secs(1));
        thread::sleep(Duration::from_millis(200));
    });
    let request_timeout = Duration::from_millis(50);

    let error = runtime().block_on(async {
        let pool = Pool::builder()
            .max_size(1)
            .build(ConnectionManager::new(properties(&path)))
            .await
            .expect("pool must build");
        let held = pool.get().await.expect("one connection must be held");
        let client = Client::from(pool.clone()).with_request_timeout(request_timeout);
        let error = client
            .send(RequestType::Status(Status {}))
            .await
            .expect_err("pool queue wait must time out");
        drop(held);
        error
    });
    server.join().expect("server must exit cleanly");

    assert_timeout(error, request_timeout);
}

#[test]
fn test_connect_retry_uses_the_request_deadline() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("missing-sozu.sock");
    let request_timeout = Duration::from_millis(50);

    let error = runtime().block_on(async {
        Client::try_new(properties(&path))
            .await
            .expect("a lazy pool must be created")
            .with_request_timeout(request_timeout)
            .send(RequestType::Status(Status {}))
            .await
            .expect_err("connect retry must time out")
    });

    assert_timeout(error, request_timeout);
}

#[test]
fn test_write_backpressure_uses_the_request_deadline() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let request_timeout = Duration::from_millis(60);
    let server = non_reading_server(&path, Duration::from_millis(200));
    let mut props = properties(&path);
    props.max_buffer_size = 8 * 1024 * 1024;

    let error = runtime().block_on(async {
        Client::try_new(props)
            .await
            .expect("client must be created")
            .with_request_timeout(request_timeout)
            .send(RequestType::LoadState("x".repeat(4 * 1024 * 1024)))
            .await
            .expect_err("a peer that does not read must trigger the write deadline")
    });
    server.join().expect("server must exit cleanly");

    assert_timeout(error, request_timeout);
}

#[cfg(feature = "unpooled")]
#[test]
fn test_unpooled_timeout_discards_the_ambiguous_connection() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.sock");
    let request_timeout = Duration::from_millis(60);
    let server = silent_server(&path, Duration::from_millis(200));

    runtime().block_on(async {
        let client =
            sozu_client::unpooled::Client::connect_with_timeout(properties(&path), request_timeout)
                .await
                .expect("unpooled client must connect");

        let first = client
            .send(RequestType::Status(Status {}))
            .await
            .expect_err("first request must time out");
        assert!(matches!(
            first,
            sozu_client::unpooled::Error::Timeout(actual) if actual == request_timeout
        ));

        let second = client
            .send(RequestType::Status(Status {}))
            .await
            .expect_err("timed-out connection must be unavailable");
        assert!(matches!(second, sozu_client::unpooled::Error::Unavailable));
    });
    server.join().expect("server must exit cleanly");
}

#[cfg(feature = "unpooled")]
#[test]
fn test_unpooled_connect_retry_uses_its_deadline() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("missing-sozu.sock");
    let request_timeout = Duration::from_millis(50);

    let error = runtime().block_on(async {
        match sozu_client::unpooled::Client::connect_with_timeout(
            properties(&path),
            request_timeout,
        )
        .await
        {
            Ok(_) => panic!("unpooled connect retry must time out"),
            Err(error) => error,
        }
    });

    assert!(matches!(
        error,
        sozu_client::unpooled::Error::Timeout(actual) if actual == request_timeout
    ));
}

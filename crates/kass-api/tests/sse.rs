//! Event streams over a real socket.

mod common;

use std::io::Write;
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use common::{Reply, Server};
use kass_api::types::{ModelProgress, ProgressStatus};
use kass_api::{ApiClient, CaptureEvent, Mac, SseCancel};

const EVENT_STREAM: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n";

/// Writes `head` and `body`, then holds the connection open until the client goes.
fn stream(head: &'static str, body: impl Into<String>) -> Reply {
    let body = body.into();
    Reply::raw(move |mut socket: TcpStream| {
        socket.write_all(head.as_bytes()).unwrap();
        socket.write_all(body.as_bytes()).unwrap();
        socket.flush().unwrap();
        let mut buf = [0; 1];
        let _ = std::io::Read::read(&mut socket, &mut buf);
    })
}

#[test]
fn capture_events_say_which_mac_is_listening() {
    let server = Server::start(vec![stream(
        EVENT_STREAM,
        "event: changed\ndata: {}\n\nevent: learned\ndata: {}\n\n: keepalive\n\n\
         event: peers\ndata: [{\"id\":\"m-1\",\"name\":\"Studio\"},{\"id\":\"m-2\",\"name\":null}]\n\n",
    )]);
    let client = ApiClient::new(
        &server.url,
        Some(Mac::new("m-1", Some("Morgan’s Studio".into()))),
    );
    let cancel = SseCancel::new();
    let mut events = client
        .capture_events(&cancel)
        .unwrap()
        .map(|event| CaptureEvent::from(event.unwrap()));

    assert_eq!(events.next(), Some(CaptureEvent::Changed));
    assert_eq!(events.next(), Some(CaptureEvent::Learned));
    assert_eq!(
        events.next(),
        Some(CaptureEvent::Peers(vec![
            Mac::new("m-1", Some("Studio".into())),
            Mac::new("m-2", None)
        ]))
    );

    let request = server.request();
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.target,
        "/captures/events?mac=m-1&mac_name=Morgan%E2%80%99s+Studio"
    );
    assert_eq!(request.header("Accept"), Some("text/event-stream"));
    assert_eq!(request.header("X-Kass-Mac"), Some("m-1"));
    assert_eq!(
        request.header("X-Kass-Mac-Name"),
        Some("Morgan%E2%80%99s%20Studio")
    );
}

#[test]
fn cancelling_from_another_thread_ends_a_blocked_read() {
    let server = Server::start(vec![stream(EVENT_STREAM, "event: changed\ndata: {}\n\n")]);
    let client = ApiClient::new(&server.url, None);
    let cancel = SseCancel::new();
    let mut events = client.capture_events(&cancel).unwrap();
    assert_eq!(events.next().unwrap().unwrap().event, "changed");
    assert_eq!(server.request().target, "/captures/events");

    let (done, finished) = mpsc::channel();
    let reader = thread::spawn(move || {
        let next = events.next();
        done.send(()).unwrap();
        next.map(|event| event.is_ok())
    });
    thread::sleep(Duration::from_millis(100));
    assert!(
        finished.try_recv().is_err(),
        "the read should block until cancelled"
    );

    let started = Instant::now();
    cancel.cancel();
    finished
        .recv_timeout(Duration::from_secs(2))
        .expect("the read ended");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(reader.join().unwrap(), None);
}

/// A stream to another Mac that went away can block forever, and one to
/// this Mac's server never ends when the other comes back: switching
/// servers ends it, so its reader reconnects to the new one.
#[test]
fn switching_the_server_ends_open_event_streams() {
    let first = Server::start(vec![stream(EVENT_STREAM, "event: changed\ndata: {}\n\n")]);
    let second = Server::start(vec![stream(EVENT_STREAM, "event: changed\ndata: {}\n\n")]);
    let client = ApiClient::new(&first.url, None);
    let cancel = SseCancel::new();
    let mut events = client.capture_events(&cancel).unwrap();
    assert_eq!(events.next().unwrap().unwrap().event, "changed");
    first.request();

    let (done, finished) = mpsc::channel();
    let reader = thread::spawn(move || {
        let next = events.next();
        done.send(()).unwrap();
        // Ended: an error, or the end of the stream.
        next.is_none_or(|event| event.is_err()) && events.next().is_none()
    });
    thread::sleep(Duration::from_millis(100));
    assert!(finished.try_recv().is_err(), "the read should block");

    client.clone().set_base_url(&second.url);
    finished
        .recv_timeout(Duration::from_secs(2))
        .expect("switching servers ended the read");
    assert!(reader.join().unwrap(), "the stream ended");
    assert!(!cancel.is_cancelled(), "the reader may reconnect");

    let mut events = client.capture_events(&cancel).unwrap();
    assert_eq!(events.next().unwrap().unwrap().event, "changed");
    assert_eq!(second.request().target, "/captures/events");
}

#[test]
fn setting_the_same_server_leaves_streams_open() {
    let server = Server::start(vec![stream(EVENT_STREAM, "event: changed\ndata: {}\n\n")]);
    let client = ApiClient::new(&server.url, None);
    let cancel = SseCancel::new();
    let mut events = client.capture_events(&cancel).unwrap();
    assert_eq!(events.next().unwrap().unwrap().event, "changed");

    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let next = events.next().is_some();
        let _ = done.send(next);
    });
    client.set_base_url(format!("{}/", server.url));
    thread::sleep(Duration::from_millis(200));
    assert!(finished.try_recv().is_err(), "the stream should stay open");
    cancel.cancel();
}

#[test]
fn a_stream_cancelled_before_it_opens_is_already_over() {
    let server = Server::start(vec![]);
    let cancel = SseCancel::new();
    cancel.cancel();
    let mut events = ApiClient::new(&server.url, None)
        .capture_events(&cancel)
        .unwrap();
    assert!(events.next().is_none());
}

#[test]
fn reads_chunked_model_progress() {
    // Unnamed events, split across chunks mid-line, one with data on two lines.
    let server = Server::start(vec![stream(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        chunked(&[
            ": heartbeat\n\nda",
            "ta: {\"model_name\":\"qwen3-1.7B\",\"current\":5,",
            "\"total\":10,\"progress\":0.5,\"status\":\"downloading\"}\r\n\r\n",
            "data: first\ndata: ",
            "second\r\n\r\n",
        ]),
    )]);
    let client = ApiClient::new(&server.url, None);
    let mut events = client
        .model_progress("qwen3-1.7B", &SseCancel::new())
        .unwrap();

    let first = events.next().unwrap().unwrap();
    assert_eq!(first.event, "message");
    let progress: ModelProgress = first.json().unwrap();
    assert_eq!(
        (progress.status, progress.progress),
        (ProgressStatus::Downloading, 0.5)
    );

    assert_eq!(events.next().unwrap().unwrap().data, "first\nsecond");
    assert!(events.next().is_none());
    assert_eq!(server.request().target, "/models/progress/qwen3-1.7B");
}

fn chunked(parts: &[&str]) -> String {
    let mut body: String = parts
        .iter()
        .map(|part| format!("{:x}\r\n{part}\r\n", part.len()))
        .collect();
    body.push_str("0\r\n\r\n");
    body
}

#[test]
fn migration_progress_until_the_server_closes() {
    let server = Server::start(vec![Reply::raw(|mut socket: TcpStream| {
        let body = "data: {\"model_name\":\"migration\",\"current\":1,\"total\":1,\"progress\":1.0,\"status\":\"complete\"}\r\n\r\n";
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    })]);
    let client = ApiClient::new(&server.url, None);
    let events: Vec<_> = client
        .migration_progress(&SseCancel::new())
        .unwrap()
        .collect();
    assert_eq!(events.len(), 1);
    assert!(
        events[0]
            .as_ref()
            .unwrap()
            .json::<ModelProgress>()
            .unwrap()
            .is_finished()
    );
    assert_eq!(server.request().target, "/models/migrate/progress");
}

#[test]
fn an_error_response_is_an_error() {
    let server = Server::start(vec![Reply::json(404, r#"{"detail":"No such model"}"#)]);
    let error = ApiClient::new(&server.url, None)
        .model_progress("nope", &SseCancel::new())
        .err()
        .unwrap();
    assert_eq!(
        (error.status(), error.to_string()),
        (Some(404), "No such model".to_string())
    );
}

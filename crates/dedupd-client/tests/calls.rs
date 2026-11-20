//! What a call says when the other end is not a healthy daemon.

use dedupd_client::ClientError;
use std::io::{Read, Write};
use std::time::Duration;

/// Something that listens and answers one request with `answer`, after
/// `wait`.
fn listener(answer: &'static [u8], wait: Duration) -> String {
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = server.accept().unwrap();
        let mut request = [0u8; 2048];
        let _ = stream.read(&mut request);
        std::thread::sleep(wait);
        let _ = stream.write_all(answer);
    });
    address
}

#[test]
fn an_address_is_host_and_port_and_nothing_else() {
    for good in [
        "127.0.0.1:8642",
        "localhost:8642",
        "[::1]:8642",
        "127.0.0.2:1",
    ] {
        assert!(dedupd_client::address_is_valid(good.into()), "{good}");
    }
    for bad in [
        "",
        "127.0.0.1",
        "http://127.0.0.1:8642",
        "127.0.0.1:8642/path",
        "user@127.0.0.1:8642",
        "127.0.0.1:0",
        "127.0.0.1:00",
        "localhost:00",
        "127.0.0.1:99999",
        "127.0.0.1:86 42",
        "::1:8642",
        "[not-v6]:8642",
        // Another machine: the daemon is on this Mac and nowhere else.
        "mac-studio.local:8642",
        "192.168.1.10:8642",
        "localhost.example.com:8642",
        "localhost:+8642",
    ] {
        assert!(!dedupd_client::address_is_valid(bad.into()), "{bad}");
        // Nothing is asked: the call says the address is wrong, not that
        // the daemon is not running.
        match dedupd_client::status(bad.into()) {
            Err(ClientError::BadAddress { address }) => assert_eq!(address, bad),
            other => panic!("{bad}: {other:?}"),
        }
    }
}

#[test]
fn nothing_listening_is_a_daemon_that_is_not_running() {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap().to_string();
    drop(free);
    let said = dedupd_client::status(address).unwrap_err();
    assert!(matches!(said, ClientError::NotRunning { .. }), "{said:?}");
}

#[test]
fn an_error_answer_that_is_not_json_is_told_as_it_came() {
    let answer =
        b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 9\r\nconnection: close\r\n\r\nno daemon";
    let said = dedupd_client::status(listener(answer, Duration::ZERO)).unwrap_err();
    match said {
        ClientError::Failed { message } => assert_eq!(message, "no daemon"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_answer_that_is_not_http_is_not_a_daemon_that_is_away() {
    let said =
        dedupd_client::status(listener(b"SSH-2.0-OpenSSH_9.9\r\n", Duration::ZERO)).unwrap_err();
    assert!(matches!(said, ClientError::Protocol { .. }), "{said:?}");
}

#[test]
fn the_sentences_are_the_ones_the_app_shows() {
    // A time limit that runs out while the body arrives is not tried here:
    // the shortest limit of a call is ten seconds. The head's error and the
    // body's go through one function (`failed`), so they cannot differ.
    assert_eq!(
        dedupd_client::error_message(ClientError::NotRunning {
            address: "a:1".into()
        }),
        "the dedupd daemon is not running at a:1"
    );
    assert_eq!(
        dedupd_client::error_message(ClientError::TimedOut),
        "the daemon did not answer in time"
    );
    assert_eq!(
        dedupd_client::error_message(ClientError::BadAddress {
            address: "a".into()
        }),
        "a is not an address of this Mac of the form host:port, such as 127.0.0.1:8642"
    );
}

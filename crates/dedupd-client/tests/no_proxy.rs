//! The client's calls go straight to the daemon, whatever proxy the
//! environment names. Alone in its file: it changes the environment of its
//! process.

use std::io::{Read, Write};

#[test]
fn a_proxy_in_the_environment_is_not_used_for_the_daemon() {
    // A proxy nothing listens on. A call sent to it cannot be answered.
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
    ] {
        // SAFETY: this test is the only one in its process, and nothing else
        // reads the environment while it is set.
        unsafe { std::env::set_var(name, "http://127.0.0.1:1") };
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let daemon = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 2048];
        let n = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
            .unwrap();
        String::from_utf8_lossy(&request[..n]).into_owned()
    });

    let answer = dedupd_client::status(address.clone());

    // The stand-in daemon was asked directly, in the form a server is asked
    // (a proxy is asked with the whole address on the request line).
    let request = daemon.join().unwrap();
    assert!(request.starts_with("GET /dedupd/v1/status "), "{request}");
    // Its answer is not a status, but it is an answer: the daemon was found.
    assert!(
        !matches!(answer, Err(dedupd_client::ClientError::NotRunning { .. })),
        "{answer:?}"
    );
}

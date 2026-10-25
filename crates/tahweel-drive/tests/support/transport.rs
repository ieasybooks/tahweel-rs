//! A local HTTP server that can close connections before a complete response.
//! Wiremock always completes its response bodies, so these cases use a raw socket.

use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

pub(crate) fn response(status: u16, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

pub(crate) async fn server(responses: Vec<Vec<u8>>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let request = tokio::time::timeout(Duration::from_secs(10), async {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                stream.write_all(&response).await.unwrap();
                stream.shutdown().await.unwrap();
                request
            })
            .await
            .expect("client did not finish the scripted request");
            requests.push(request);
        }
        requests
    });
    (format!("http://{address}"), task)
}

async fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let head_end = loop {
        let mut buffer = [0; 1024];
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0, "client closed before sending request headers");
        request.extend_from_slice(&buffer[..read]);
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8(request[..head_end].to_vec()).unwrap();
    let content_length: usize = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map_or(0, |(_, value)| value.trim().parse().unwrap());
    while request.len() < head_end + content_length {
        let mut buffer = [0; 4096];
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0, "client closed before sending the request body");
        request.extend_from_slice(&buffer[..read]);
    }
    head.lines().next().unwrap().to_owned()
}

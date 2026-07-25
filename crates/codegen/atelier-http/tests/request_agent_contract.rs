use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

fn spawn_header_server(
    requests: usize,
) -> (String, mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 2048];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..read]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            sender.send(String::from_utf8(bytes).unwrap()).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        }
    });
    (format!("http://{address}"), receiver, handle)
}

fn request_user_agent(request: &str) -> Option<&str> {
    request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("user-agent")
            .then(|| value.trim())
    })
}

#[test]
fn configured_request_agent_is_used_as_the_http_user_agent() {
    atelier_http::set_request_agent_identity_with_user_agent(
        "pi".to_owned(),
        Some("0.82.1".to_owned()),
        "pi/0.82.1 (win32; node/v24.18.0; x64)".to_owned(),
    )
    .unwrap();

    let user_agent = atelier_http::process_user_agent_string();
    assert_eq!(user_agent, "pi/0.82.1 (win32; node/v24.18.0; x64)");
    assert!(!user_agent.contains("atelier-shell"), "{user_agent}");

    let (base_url, requests, server) = spawn_header_server(1);
    atelier_http::shared_blocking_client()
        .post(format!("{base_url}/oauth/token"))
        .form(&[("refresh_token", "oauth-secret-must-not-enter-user-agent")])
        .send()
        .unwrap();

    let request = requests.recv().unwrap();
    let actual = request_user_agent(&request).expect("User-Agent header");
    assert_eq!(actual, user_agent);
    assert!(!actual.contains("oauth-secret"));
    server.join().unwrap();
}

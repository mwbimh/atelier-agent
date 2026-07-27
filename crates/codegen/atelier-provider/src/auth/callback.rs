use super::types::OAuthError;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};
use url::Url;

const MAX_CALLBACK_BYTES: usize = 16 * 1024;

pub(super) struct LocalhostCallback {
    listener: TcpListener,
    path: String,
    redirect_uri: Url,
}

pub(super) struct CallbackResult {
    pub(super) code: String,
}

impl LocalhostCallback {
    pub(super) fn bind(port: u16, path: &str, redirect_host: &str) -> Result<Self, OAuthError> {
        validate_callback_path(path)?;
        if !matches!(redirect_host, "127.0.0.1" | "localhost") {
            return Err(OAuthError::InvalidConfig(
                "OAuth callback host must be 127.0.0.1 or localhost".into(),
            ));
        }
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(OAuthError::CallbackBind)?;
        listener
            .set_nonblocking(true)
            .map_err(OAuthError::CallbackBind)?;
        let port = listener
            .local_addr()
            .map_err(OAuthError::CallbackBind)?
            .port();
        let redirect_uri = Url::parse(&format!("http://{redirect_host}:{port}{path}"))
            .map_err(|error| OAuthError::InvalidConfig(error.to_string()))?;
        Ok(Self {
            listener,
            path: path.into(),
            redirect_uri,
        })
    }

    pub(super) fn redirect_uri(&self) -> &Url {
        &self.redirect_uri
    }

    pub(super) fn path(&self) -> &str {
        &self.path
    }

    pub(super) fn port(&self) -> u16 {
        self.redirect_uri
            .port()
            .expect("callback URI always has a port")
    }

    pub(super) fn wait(
        self,
        timeout: Duration,
        expected_state: &str,
    ) -> Result<CallbackResult, OAuthError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => return read_callback(stream, &self.path, expected_state),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(OAuthError::CallbackTimeout);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(OAuthError::CallbackBind(error)),
            }
        }
    }
}

fn validate_callback_path(path: &str) -> Result<(), OAuthError> {
    if !path.starts_with('/') || path.contains(['?', '#', '\r', '\n']) || path.contains("..") {
        return Err(OAuthError::InvalidConfig(
            "callback path must be an absolute path without query, fragment, or traversal".into(),
        ));
    }
    Ok(())
}

fn read_callback(
    mut stream: TcpStream,
    expected_path: &str,
    expected_state: &str,
) -> Result<CallbackResult, OAuthError> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(OAuthError::Io)?;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).map_err(OAuthError::Io)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if request.len() > MAX_CALLBACK_BYTES {
            write_response(&mut stream, 400, "OAuth callback request is too large");
            return Err(OAuthError::CallbackInvalid("request is too large".into()));
        }
    }
    let request = std::str::from_utf8(&request)
        .map_err(|_| OAuthError::CallbackInvalid("request is not UTF-8".into()))?;
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| OAuthError::CallbackInvalid("request line is missing".into()))?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" || target.is_empty() {
        write_response(&mut stream, 405, "OAuth callback requires GET");
        return Err(OAuthError::CallbackInvalid("callback must use GET".into()));
    }
    let callback_url = Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|error| OAuthError::CallbackInvalid(error.to_string()))?;
    if callback_url.path() != expected_path {
        write_response(&mut stream, 404, "OAuth callback path was not found");
        return Err(OAuthError::CallbackInvalid(
            "callback path does not match".into(),
        ));
    }
    let query = callback_url
        .query_pairs()
        .into_owned()
        .collect::<std::collections::BTreeMap<_, _>>();
    if let Some(error) = query.get("error") {
        write_response(&mut stream, 400, "OAuth authorization was not completed");
        return Err(OAuthError::AuthorizationDenied(
            query
                .get("error_description")
                .cloned()
                .unwrap_or_else(|| error.clone()),
        ));
    }
    let code = query
        .get("code")
        .filter(|code| !code.is_empty())
        .cloned()
        .ok_or_else(|| OAuthError::CallbackInvalid("authorization code is missing".into()))?;
    let state = query
        .get("state")
        .filter(|state| !state.is_empty())
        .cloned()
        .ok_or_else(|| OAuthError::CallbackInvalid("state is missing".into()))?;
    if state != expected_state {
        write_response(&mut stream, 400, "OAuth callback state did not match");
        return Err(OAuthError::StateMismatch);
    }
    write_response(
        &mut stream,
        200,
        "OAuth login complete. You can close this window.",
    );
    Ok(CallbackResult { code })
}

fn write_response(stream: &mut TcpStream, status: u16, message: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let body =
        format!("<!doctype html><meta charset=utf-8><title>Atelier OAuth</title><p>{message}</p>");
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

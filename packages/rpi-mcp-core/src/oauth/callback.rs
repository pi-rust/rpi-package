use crate::{AuthError, Control};
use std::{future::Future, time::Instant};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;

pub(super) fn check(control: &Control, deadline: Instant) -> Result<(), AuthError> {
    control.check().map_err(|_| AuthError::Cancelled)?;
    if Instant::now() >= deadline {
        return Err(AuthError::Timeout);
    }
    Ok(())
}
pub(super) async fn bounded<T>(
    control: &Control,
    deadline: Instant,
    future: impl Future<Output = Result<T, AuthError>>,
) -> Result<T, AuthError> {
    check(control, deadline)?;
    tokio::select! {
        biased;
        _ = control.cancelled() => Err(AuthError::Cancelled),
        _ = tokio::time::sleep_until(deadline.into()) => Err(AuthError::Timeout),
        value = future => { check(control, deadline)?; value }
    }
}
/// Owned listener; dropping this value or its receive future closes the socket.
pub(super) struct Callback {
    listener: TcpListener,
    pub redirect: String,
    host: String,
}
impl Callback {
    pub async fn bind() -> Result<Self, AuthError> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| AuthError::Provider)?;
        let host = listener
            .local_addr()
            .map_err(|_| AuthError::Provider)?
            .to_string();
        Ok(Self {
            redirect: format!("http://{host}/oauth/callback"),
            listener,
            host,
        })
    }
    pub async fn receive(&self, state: &str, issuer: &str) -> Result<String, AuthError> {
        let (mut stream, peer) = self
            .listener
            .accept()
            .await
            .map_err(|_| AuthError::InvalidState)?;
        if !peer.ip().is_loopback() {
            return Err(AuthError::InvalidState);
        }
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let n = stream
                .read(&mut chunk)
                .await
                .map_err(|_| AuthError::InvalidState)?;
            if n == 0 || bytes.len() + n > 8192 {
                return Err(AuthError::InvalidState);
            }
            bytes.extend_from_slice(&chunk[..n]);
            if bytes.windows(4).any(|x| x == b"\r\n\r\n") {
                break;
            }
        }
        let header_end = bytes
            .windows(4)
            .position(|x| x == b"\r\n\r\n")
            .ok_or(AuthError::InvalidState)?
            + 4;
        if bytes.len() != header_end {
            return Err(AuthError::InvalidState);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| AuthError::InvalidState)?;
        let mut lines = text.split("\r\n");
        let parts: Vec<_> = lines.next().unwrap_or("").split(' ').collect();
        if parts.len() != 3
            || parts[0] != "GET"
            || parts[2] != "HTTP/1.1"
            || !parts[1].starts_with("/oauth/callback?")
            || parts[1].contains('#')
            || parts[1].chars().any(char::is_control)
        {
            return Err(AuthError::InvalidState);
        }
        let mut hosts = Vec::new();
        for line in lines.take_while(|l| !l.is_empty()) {
            let (name, value) = line.split_once(':').ok_or(AuthError::InvalidState)?;
            if name.eq_ignore_ascii_case("transfer-encoding")
                || (name.eq_ignore_ascii_case("content-length") && value.trim() != "0")
            {
                return Err(AuthError::InvalidState);
            }
            if name.eq_ignore_ascii_case("host") {
                hosts.push(value.trim());
            }
        }
        if hosts != [self.host.as_str()] {
            return Err(AuthError::InvalidState);
        }
        let url = Url::parse(&format!("http://{}{}", self.host, parts[1]))
            .map_err(|_| AuthError::InvalidState)?;
        if url.path() != "/oauth/callback" || url.fragment().is_some() {
            return Err(AuthError::InvalidState);
        }
        let mut seen = std::collections::HashMap::new();
        for (key, value) in url.query_pairs() {
            if key.chars().chain(value.chars()).any(char::is_control) {
                return Err(AuthError::InvalidState);
            }
            if seen.insert(key.into_owned(), value.into_owned()).is_some() {
                return Err(AuthError::InvalidState);
            }
        }
        if seen.get("state").map(String::as_str) != Some(state)
            || seen.get("iss").is_some_and(|s| s != issuer)
            || seen.contains_key("error")
        {
            return Err(AuthError::InvalidState);
        }
        let code = seen
            .remove("code")
            .filter(|s| !s.is_empty() && s.len() <= 4096)
            .ok_or(AuthError::InvalidState)?;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 24\r\nConnection: close\r\nContent-Type: text/plain\r\n\r\nAuthorization received.\n").await.map_err(|_| AuthError::InvalidState)?;
        Ok(code)
    }
}

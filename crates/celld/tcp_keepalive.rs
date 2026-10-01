//! TCP keepalive for outbound connections.
//!
//! Some networks forget a connection after a few idle minutes and then drop
//! its packets without a reset (AWS Nitro v6 hosts track one for 350 s), so a
//! quiet WebSocket, streamed response, or socket hangs until something above
//! it times out. A probe after a minute of silence keeps the connection known
//! to every hop and surfaces a dead peer as an error instead of a hang.

use std::time::Duration;

/// Idle time before the first probe.
pub const IDLE: Duration = Duration::from_secs(60);

/// Enables keepalive probes on `stream` after [`IDLE`] without traffic.
pub fn enable(stream: &tokio::net::TcpStream) -> std::io::Result<()> {
    socket2::SockRef::from(stream).set_tcp_keepalive(&socket2::TcpKeepalive::new().with_time(IDLE))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn enable_turns_on_keepalive() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stream = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        super::enable(&stream).unwrap();
        assert!(socket2::SockRef::from(&stream).keepalive().unwrap());
    }
}

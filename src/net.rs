use std::net::{IpAddr, SocketAddr, UdpSocket};

/// Finds the local IP the system uses to reach `target`.
/// Sends nothing: UDP `connect` only selects the route.
pub fn local_ip_towards(target: SocketAddr) -> std::io::Result<IpAddr> {
    let bind: SocketAddr = if target.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let socket = UdpSocket::bind(bind)?;
    socket.connect(target)?;
    Ok(socket.local_addr()?.ip())
}

pub async fn resolve(server: &str) -> std::io::Result<SocketAddr> {
    tokio::net::lookup_host(server)
        .await?
        .next()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("could not resolve {server}"),
            )
        })
}

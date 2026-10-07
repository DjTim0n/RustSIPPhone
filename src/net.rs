use std::net::{IpAddr, SocketAddr, UdpSocket};

/// Определяет локальный IP, с которого система ходит до `target`.
/// Ничего не отправляет: UDP `connect` лишь выбирает маршрут.
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
    tokio::net::lookup_host(server).await?.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("не удалось определить адрес {server}"),
        )
    })
}

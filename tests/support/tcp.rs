// Reserve the same TCP port on IPv6 loopback, outside the platform's ephemeral
// range. IPv4 loopback has no listener, and automatic client/test port allocation
// cannot reuse this number for an accidental self-connect.
pub fn refused_endpoint() -> (std::net::TcpListener, u16) {
    #[cfg(target_os = "macos")]
    let first = String::from_utf8(
        std::process::Command::new("sysctl")
            .args(["-n", "net.inet.ip.portrange.first"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .parse::<u16>()
    .unwrap();
    #[cfg(target_os = "linux")]
    let first = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let mut ports = (1024..first).rev();
    let listener = ports
        .find_map(|port| std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)).ok())
        .expect("a non-ephemeral loopback port");
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

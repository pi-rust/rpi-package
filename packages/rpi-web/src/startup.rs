//! Plain terminal status page shared by the extension and standalone server.
use crate::AppConfig;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub(crate) fn page(
    address: SocketAddr,
    config: &AppConfig,
    hosts: &[String],
    password: bool,
) -> String {
    // Wildcard addresses describe listeners, not destinations for a browser.
    let browser_ip = match address.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let browser = SocketAddr::new(browser_ip, address.port());
    let clean = |value: &str| {
        value
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
    };
    format!(
        "\n  rpi Web Server v{}\n  ================================================\n\n  Status      Running\n  Listen IP   {}\n  Port        {}\n  Open        http://{}\n  Project     {}\n  Password    {}\n  Host rules  {}\n\n  Open the URL in your browser to use the workspace.\n  Keep this terminal open. Press Ctrl+C to stop.\n",
        env!("CARGO_PKG_VERSION"), address.ip(), address.port(), browser,
        clean(&config.project.display().to_string()),
        if password { "Enabled" } else { "Not configured" },
        clean(&hosts.join(", ")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_uses_the_bound_port_and_a_browser_destination() {
        for (listen, ip, url) in [
            ("127.0.0.1:41234", "127.0.0.1", "http://127.0.0.1:41234"),
            ("0.0.0.0:41234", "0.0.0.0", "http://127.0.0.1:41234"),
            ("[::1]:41234", "::1", "http://[::1]:41234"),
            ("[::]:41234", "::", "http://[::1]:41234"),
        ] {
            let config = AppConfig {
                port: 0,
                project: "workspace\n\u{1b}[31m".into(),
                ..AppConfig::default()
            };
            let text = page(
                listen.parse().unwrap(),
                &config,
                &["localhost:41234".into()],
                true,
            );
            assert!(text.contains(&format!("Listen IP   {ip}")));
            assert!(text.contains("Port        41234"));
            assert!(text.contains(url));
            assert!(text.contains("Status      Running"));
            assert!(text.contains("Password    Enabled"));
            assert!(!text.contains('\u{1b}'));
            assert!(text.contains("Ctrl+C"));
        }
    }
}

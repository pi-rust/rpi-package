//! Suggest a LAN Web origin without opening ports or contacting another host.
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

pub(super) const DEFAULT_BIND: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

/// Native App authorization needs the authenticated API, not browser assets.
/// Keep the Web suggestion separate so an API-only listener is never advertised
/// as a browser UI. The same explicit proxy, private-IP and exact-Host rules apply.
pub(super) fn suggest_app(
    explicit: Option<&str>,
    listener: SocketAddr,
    web_port: Option<u16>,
    routed_ip: Option<Ipv4Addr>,
) -> Result<AddressSuggestion, String> {
    suggest(explicit, listener, true, web_port, routed_ip)
}

#[derive(Default)]
pub(super) struct AddressSuggestion {
    pub url: String,
    pub hint: String,
    /// Only the exact automatic direct-Web authority, never a wildcard.
    pub direct_host: Option<String>,
}

/// UDP connect selects a local route; no packet is sent (there is no send call).
/// This deliberately does not promise reachability or enumerate all adapters.
pub(super) fn routed_lan_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if ip.is_private() => Some(ip),
        _ => None,
    }
}

pub(super) fn suggest(
    explicit: Option<&str>,
    listener: SocketAddr,
    assets: bool,
    web_port: Option<u16>,
    routed_ip: Option<Ipv4Addr>,
) -> Result<AddressSuggestion, String> {
    if let Some(value) = explicit {
        let (url, _) = rpi_http_node::public_address(value)?;
        return Ok(AddressSuggestion {
            url,
            hint: "使用已配置的分享入口；请确认对方网络可达。".into(),
            direct_host: None,
        });
    }
    let direct = web_port.is_none();
    let port = if let Some(port) = web_port {
        port
    } else if !listener.ip().is_loopback() && assets {
        listener.port()
    } else {
        return Ok(AddressSuggestion {
            hint: "未配置局域网 Web 入口，无法自动生成可用二维码。可配置同源 Web 代理端口，或显式监听局域网并提供 Web 资源；也可手动填写同一节点的可达地址。".into(),
            ..AddressSuggestion::default()
        });
    };
    // A proxy has its own listener; never derive its host from the API bind.
    let ip = if direct {
        match listener.ip() {
            IpAddr::V4(ip) if ip.is_private() => Some(ip),
            IpAddr::V4(ip) if ip.is_unspecified() => routed_ip.filter(Ipv4Addr::is_private),
            _ => None,
        }
    } else {
        routed_ip.filter(Ipv4Addr::is_private)
    };
    let Some(ip) = ip.filter(|_| port != 0) else {
        return Ok(AddressSuggestion {
            hint: "未找到可用于分享入口的局域网 IPv4 地址；请手动填写对方可达的同源 Web 地址。"
                .into(),
            ..AddressSuggestion::default()
        });
    };
    let (url, authority) = rpi_http_node::public_address(&format!("http://{ip}:{port}"))?;
    Ok(AddressSuggestion {
        url,
        hint: "已自动选择局域网地址；多网卡/VPN 可改地址后重新生成。请确认 Web 入口已启动、对方在可达网络且防火墙允许访问；未自动修改监听或防火墙。".into(),
        direct_host: direct.then_some(authority),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn listener(value: &str) -> SocketAddr {
        value.parse().unwrap()
    }
    // Match the optional route observation supplied to both suggestion helpers.
    #[allow(clippy::unnecessary_wraps)]
    fn lan() -> Option<Ipv4Addr> {
        Some(Ipv4Addr::new(192, 168, 1, 8))
    }

    #[test]
    fn explicit_origin_wins_without_network_detection() {
        let result = suggest(
            Some("https://work.example"),
            listener("127.0.0.1:3031"),
            false,
            Some(4180),
            None,
        )
        .unwrap();
        assert_eq!(result.url, "https://work.example/");
        assert!(result.direct_host.is_none());
        assert!(
            suggest(
                Some("https://user:secret@work.example"),
                listener("0.0.0.0:3031"),
                true,
                None,
                lan()
            )
            .is_err()
        );
    }

    #[test]
    fn proxy_uses_its_own_port_and_does_not_open_the_api() {
        let result = suggest(None, listener("127.0.0.1:3031"), false, Some(4180), lan()).unwrap();
        assert_eq!(result.url, "http://192.168.1.8:4180/");
        assert!(result.direct_host.is_none());
    }

    #[test]
    fn direct_web_requires_assets_and_non_loopback_listener() {
        for (bind, assets) in [
            ("127.0.0.1:3031", true),
            ("0.0.0.0:3031", false),
            ("127.0.0.1:3031", false),
        ] {
            let result = suggest(None, listener(bind), assets, None, lan()).unwrap();
            assert_eq!(result.url, "");
            assert_ne!(result.hint, "");
        }
        let result = suggest(None, listener("0.0.0.0:3031"), true, None, lan()).unwrap();
        assert_eq!(result.url, "http://192.168.1.8:3031/");
        assert_eq!(result.direct_host.as_deref(), Some("192.168.1.8:3031"));
    }

    #[test]
    fn specific_listener_wins_over_the_default_route_only_for_direct_web() {
        let direct = suggest(None, listener("10.2.3.4:3031"), true, None, lan()).unwrap();
        assert_eq!(direct.url, "http://10.2.3.4:3031/");
        let proxy = suggest(None, listener("10.2.3.4:3031"), true, Some(4180), lan()).unwrap();
        assert_eq!(proxy.url, "http://192.168.1.8:4180/");
    }

    #[test]
    fn default_listener_supports_native_app_without_claiming_a_web_ui() {
        assert_eq!(DEFAULT_BIND, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let bind = SocketAddr::new(DEFAULT_BIND, 3031);
        let app = suggest_app(None, bind, None, lan()).unwrap();
        assert_eq!(app.url, "http://192.168.1.8:3031/");
        assert_eq!(app.direct_host.as_deref(), Some("192.168.1.8:3031"));
        assert_eq!(suggest(None, bind, false, None, lan()).unwrap().url, "");
    }

    #[test]
    fn app_address_respects_explicit_loopback_and_proxy_configuration() {
        assert_eq!(
            suggest_app(None, listener("127.0.0.1:3031"), None, lan())
                .unwrap()
                .url,
            ""
        );
        let app = suggest_app(None, listener("127.0.0.1:3031"), Some(4180), lan()).unwrap();
        assert_eq!(app.url, "http://192.168.1.8:4180/");
        assert!(app.direct_host.is_none());
        let explicit = suggest_app(
            Some("https://node.example"),
            listener("127.0.0.1:3031"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(explicit.url, "https://node.example/");
        assert_eq!(
            suggest_app(None, listener("0.0.0.0:3031"), None, None)
                .unwrap()
                .url,
            ""
        );
        assert_eq!(
            suggest_app(None, listener("0.0.0.0:0"), None, lan())
                .unwrap()
                .url,
            ""
        );
    }

    #[test]
    fn missing_or_unsuitable_network_never_produces_a_fake_qr() {
        for ip in [
            None,
            Some(Ipv4Addr::LOCALHOST),
            Some(Ipv4Addr::UNSPECIFIED),
            Some(Ipv4Addr::new(169, 254, 1, 2)),
            Some(Ipv4Addr::new(8, 8, 8, 8)),
        ] {
            assert_eq!(
                suggest(None, listener("0.0.0.0:3031"), true, None, ip)
                    .unwrap()
                    .url,
                ""
            );
        }
        assert_eq!(
            suggest(None, listener("[::]:3031"), true, None, lan())
                .unwrap()
                .url,
            ""
        );
        assert_eq!(
            suggest(None, listener("0.0.0.0:0"), true, None, lan())
                .unwrap()
                .url,
            ""
        );
    }
}

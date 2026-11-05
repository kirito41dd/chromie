//! 只约束 Web 协议；TLS 与网络信任边界由部署和网关决定。

pub(crate) fn allows(url: &reqwest::Url) -> bool {
    url.host_str().is_some() && matches!(url.scheme(), "http" | "https")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn http_and_https_do_not_depend_on_network_location() {
        for value in [
            "http://192.0.2.10:8732",
            "http://172.16.0.1",
            "http://10.1.2.3",
            "http://192.168.1.2",
            "http://127.0.0.1",
            "http://localhost:8732",
            "http://[::1]",
            "http://[fd12::1]:8732",
            "https://example.com",
            "http://example.com",
            "http://8.8.8.8:8732",
            "http://internal.example:8732",
        ] {
            assert!(allows(&reqwest::Url::parse(value).unwrap()), "{value}");
        }
        for value in [
            "ftp://192.168.1.1",
            "file:///tmp/config",
            "ws://example.com",
        ] {
            assert!(!allows(&reqwest::Url::parse(value).unwrap()), "{value}");
        }
    }
}

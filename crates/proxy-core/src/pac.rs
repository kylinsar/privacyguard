//! PAC（Proxy Auto-Config）脚本生成：只有拦截域名走代理，其余直连。

use std::net::SocketAddr;

pub fn generate(hosts: &[String], listen: SocketAddr) -> String {
    let mut conds = Vec::new();
    for h in hosts {
        let h = h.trim().trim_start_matches('.').to_ascii_lowercase();
        if h.is_empty() {
            continue;
        }
        conds.push(format!("host === \"{h}\" || dnsDomainIs(host, \".{h}\")"));
    }
    let cond = if conds.is_empty() {
        "false".to_string()
    } else {
        conds.join(" ||\n        ")
    };
    format!(
        "// PrivacyGuard PAC - generated, do not edit\n\
function FindProxyForURL(url, host) {{\n\
    host = host.toLowerCase();\n\
    if ({cond}) {{\n\
        return \"PROXY {ip}:{port}; DIRECT\";\n\
    }}\n\
    return \"DIRECT\";\n\
}}\n",
        ip = listen.ip(),
        port = listen.port()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pac_contains_hosts() {
        let s = generate(&["api.openai.com".into(), "chatgpt.com".into()], "127.0.0.1:7890".parse().unwrap());
        assert!(s.contains("dnsDomainIs(host, \".chatgpt.com\")"));
        assert!(s.contains("PROXY 127.0.0.1:7890; DIRECT"));
    }
}

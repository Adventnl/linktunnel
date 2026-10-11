//! Where a forward may go.

use std::net::{IpAddr, SocketAddr};

/// One permitted destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowRule {
    /// A host name or IP address, or `*` for any host.
    pub host: String,
    /// Inclusive port range.
    pub ports: (u16, u16),
}

impl AllowRule {
    /// `host:port`.
    #[must_use]
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_owned(),
            ports: (port, port),
        }
    }

    fn matches(&self, host: &str, port: u16) -> bool {
        (self.host == "*" || self.host.eq_ignore_ascii_case(host))
            && (self.ports.0..=self.ports.1).contains(&port)
    }
}

/// What the target allows.
#[derive(Debug, Clone)]
pub struct ForwardPolicy {
    /// Allow any port on this machine's loopback addresses.
    pub loopback: bool,
    /// Further destinations.
    pub allow: Vec<AllowRule>,
    /// Most simultaneous forwarded connections.
    pub max_connections: usize,
}

impl Default for ForwardPolicy {
    fn default() -> Self {
        Self {
            loopback: true,
            allow: Vec::new(),
            max_connections: 64,
        }
    }
}

impl ForwardPolicy {
    /// Nothing is allowed.
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            loopback: false,
            allow: Vec::new(),
            max_connections: 0,
        }
    }

    /// Is `host:port` permitted, judged on the *name the controller gave*? Names are re-checked
    /// after resolution ([`permits_addr`](Self::permits_addr)) so a name that resolves somewhere
    /// else (DNS rebinding) cannot sneak past the loopback rule.
    #[must_use]
    pub fn permits_name(&self, host: &str, port: u16) -> bool {
        if self.allow.iter().any(|r| r.matches(host, port)) {
            return true;
        }
        self.loopback && (host.eq_ignore_ascii_case("localhost") || is_loopback_literal(host))
    }

    /// Is the resolved address acceptable? A destination admitted only by the loopback rule must
    /// really be loopback; explicitly allowed names may resolve anywhere.
    #[must_use]
    pub fn permits_addr(&self, host: &str, port: u16, addr: SocketAddr) -> bool {
        if self.allow.iter().any(|r| r.matches(host, port)) {
            return true;
        }
        self.loopback && addr.ip().is_loopback()
    }
}

fn is_loopback_literal(host: &str) -> bool {
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_only_by_default() {
        let p = ForwardPolicy::default();
        for ok in ["localhost", "LOCALHOST", "127.0.0.1", "127.1.2.3", "::1"] {
            assert!(p.permits_name(ok, 5432), "{ok}");
        }
        for bad in ["10.0.0.5", "example.com", "192.168.1.1", "0.0.0.0", ""] {
            assert!(!p.permits_name(bad, 80), "{bad}");
        }
        assert!(!ForwardPolicy::deny_all().permits_name("localhost", 80));
    }

    #[test]
    fn explicit_rules_extend_it() {
        let mut p = ForwardPolicy::default();
        p.allow.push(AllowRule::new("nas.local", 445));
        p.allow.push(AllowRule {
            host: "10.0.0.7".into(),
            ports: (8000, 8100),
        });
        assert!(p.permits_name("nas.local", 445));
        assert!(!p.permits_name("nas.local", 22));
        assert!(p.permits_name("10.0.0.7", 8050));
        assert!(!p.permits_name("10.0.0.7", 8101));
        assert!(!p.permits_name("10.0.0.8", 8050));
    }

    #[test]
    fn a_name_that_resolves_off_machine_is_refused_by_the_loopback_rule() {
        let p = ForwardPolicy::default();
        let evil: SocketAddr = "93.184.216.34:80".parse().unwrap();
        // "localhost" passed the name check, but a hostile resolver pointed it elsewhere.
        assert!(!p.permits_addr("localhost", 80, evil));
        assert!(p.permits_addr("localhost", 80, "127.0.0.1:80".parse().unwrap()));
        assert!(p.permits_addr("localhost", 80, "[::1]:80".parse().unwrap()));
        let mut q = p;
        q.allow.push(AllowRule::new("nas.local", 80));
        assert!(
            q.permits_addr("nas.local", 80, evil),
            "explicit allow may resolve anywhere"
        );
    }
}

//! Port allocation within the configured range.

use std::net::{IpAddr, SocketAddr, TcpListener};

use anyhow::{Context, Result};

/// Returns the ports in `range` that nothing is currently listening on.
///
/// Availability is tested by binding, which is the only reliable check, and the
/// listener is dropped immediately. That leaves a window in which someone else
/// could take the port, so callers should treat the answer as a hint and be
/// ready for a later bind to fail.
pub fn free_ports(bind: IpAddr, range: impl IntoIterator<Item = u16>) -> Vec<u16> {
    range.into_iter().filter(|port| is_free(bind, *port)).collect()
}

pub fn is_free(bind: IpAddr, port: u16) -> bool {
    TcpListener::bind(SocketAddr::new(bind, port)).is_ok()
}

/// Picks the first free port in `range`, skipping `reserved`.
pub fn pick(
    bind: IpAddr,
    range: impl IntoIterator<Item = u16>,
    reserved: &[u16],
) -> Result<u16> {
    let range: Vec<u16> = range.into_iter().collect();
    let (low, high) = (
        range.first().copied().unwrap_or(0),
        range.last().copied().unwrap_or(0),
    );
    range
        .into_iter()
        .filter(|port| !reserved.contains(port))
        .find(|port| is_free(bind, *port))
        .with_context(|| {
            format!(
                "no free port in {low}-{high}. Free one up, or widen the range with \
                 `showme config --ports LOW-HIGH`"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    #[test]
    fn detects_a_bound_port_as_taken() {
        let listener = TcpListener::bind((LOCAL, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!is_free(LOCAL, port), "bound port reported free");
        drop(listener);
        assert!(is_free(LOCAL, port));
    }

    #[test]
    fn pick_skips_taken_and_reserved_ports() {
        // Bind two ports, then ask for a range covering them plus a free one.
        let a = TcpListener::bind((LOCAL, 0)).unwrap();
        let taken = a.local_addr().unwrap().port();
        let b = TcpListener::bind((LOCAL, 0)).unwrap();
        let reserved = b.local_addr().unwrap().port();
        drop(b);

        let free = TcpListener::bind((LOCAL, 0)).unwrap();
        let free_port = free.local_addr().unwrap().port();
        drop(free);

        let chosen = pick(LOCAL, [taken, reserved, free_port], &[reserved]).unwrap();
        assert_eq!(chosen, free_port);
    }

    #[test]
    fn pick_errors_when_everything_is_reserved() {
        let error = pick(LOCAL, [3000, 3001], &[3000, 3001]).unwrap_err();
        assert!(error.to_string().contains("no free port"));
    }

    #[test]
    fn free_ports_excludes_bound_ports() {
        let listener = TcpListener::bind((LOCAL, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!free_ports(LOCAL, [port]).contains(&port));
    }
}

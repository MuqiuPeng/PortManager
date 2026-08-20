//! Health probing.
//!
//! "The process exists" and "the service answers" are different facts, and
//! conflating them is why restarts appear to succeed while the app is still
//! 502-ing. Every probe returns which of the two it actually established.

use std::time::Duration;

use runtime_types::{HealthCheck, ServiceStatus};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(750);
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub status: ServiceStatus,
    pub detail: Option<String>,
    pub checked_port: Option<u16>,
}

impl Probe {
    fn healthy(detail: impl Into<String>, port: Option<u16>) -> Self {
        Self {
            status: ServiceStatus::Healthy,
            detail: Some(detail.into()),
            checked_port: port,
        }
    }

    fn unhealthy(detail: impl Into<String>, port: Option<u16>) -> Self {
        Self {
            status: ServiceStatus::Unhealthy,
            detail: Some(detail.into()),
            checked_port: port,
        }
    }
}

/// Run one health check.
///
/// `process_alive` is supplied by the caller because only the lifecycle layer
/// knows the instance's [`ProcessIdentity`](runtime_adapter::ProcessIdentity);
/// a dead process is unhealthy regardless of what any socket says.
pub async fn probe(check: &HealthCheck, port: Option<u16>, process_alive: bool) -> Probe {
    if !process_alive {
        return Probe {
            status: ServiceStatus::Stopped,
            detail: Some("process is not running".to_string()),
            checked_port: port,
        };
    }

    match check {
        HealthCheck::Process => Probe::healthy("process is running", port),

        HealthCheck::Tcp { port: override_port } => {
            let Some(target) = override_port.or(port) else {
                return Probe::unhealthy("tcp health check has no port", None);
            };
            match tcp_connect(target).await {
                Ok(()) => Probe::healthy(format!("tcp connect to {target} succeeded"), Some(target)),
                Err(err) => Probe::unhealthy(format!("tcp connect to {target} failed: {err}"), Some(target)),
            }
        }

        HealthCheck::Http {
            path,
            port: override_port,
            expect_status,
        } => {
            let Some(target) = override_port.or(port) else {
                return Probe::unhealthy("http health check has no port", None);
            };
            match http_get(target, path).await {
                // An empty list means any answer counts. Asked for explicitly
                // by whoever wrote the check, so it cannot happen by accident.
                Ok(status) if expect_status.is_empty() => {
                    Probe::healthy(format!("GET {path} returned {status}"), Some(target))
                }
                Ok(status) if expect_status.contains(&status) => {
                    Probe::healthy(format!("GET {path} returned {status}"), Some(target))
                }
                Ok(status) => Probe::unhealthy(
                    format!("GET {path} returned {status}, expected one of {expect_status:?}"),
                    Some(target),
                ),
                Err(err) => Probe::unhealthy(format!("GET {path} failed: {err}"), Some(target)),
            }
        }
    }
}

async fn tcp_connect(port: u16) -> Result<(), String> {
    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(("127.0.0.1", port))).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err("timed out".to_string()),
    }
}

/// A deliberately minimal HTTP/1.1 client.
///
/// Health checks hit `127.0.0.1` with no redirects, no TLS and no body, so a
/// full HTTP stack would be several hundred KB of dependency for one status line.
async fn http_get(port: u16, path: &str) -> Result<u16, String> {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };

    let work = async {
        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|err| err.to_string())?;

        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: local-runtime\r\nConnection: close\r\nAccept: */*\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|err| err.to_string())?;

        // The status line is all we need, and it arrives in the first packet.
        let mut buffer = [0u8; 256];
        let read = stream.read(&mut buffer).await.map_err(|err| err.to_string())?;
        if read == 0 {
            return Err("connection closed without a response".to_string());
        }

        let head = String::from_utf8_lossy(&buffer[..read]);
        head.split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| format!("malformed status line: {}", head.lines().next().unwrap_or("")))
    };

    match tokio::time::timeout(HTTP_TIMEOUT, work).await {
        Ok(result) => result,
        Err(_) => Err("timed out".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_dead_process_is_stopped_not_unhealthy() {
        let probe = probe(&HealthCheck::Process, Some(3000), false).await;
        assert_eq!(probe.status, ServiceStatus::Stopped);
    }

    #[tokio::test]
    async fn a_bound_port_probes_healthy() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let result = probe(&HealthCheck::Tcp { port: None }, Some(port), true).await;
        assert_eq!(result.status, ServiceStatus::Healthy);
    }

    #[tokio::test]
    async fn a_port_nobody_listens_on_probes_unhealthy() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        // Closing a socket does not always make the port refuse connections in
        // the same instant — a connection already in the backlog can still be
        // completed — so the assertion is that it *becomes* unhealthy, not that
        // it is unhealthy on the first attempt.
        for _ in 0..20 {
            let result = probe(&HealthCheck::Tcp { port: None }, Some(port), true).await;
            if result.status == ServiceStatus::Unhealthy {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("port {port} never stopped accepting connections");
    }

    #[tokio::test]
    async fn any_response_counts_when_no_status_is_demanded() {
        // The real services on a machine answer a bare GET with 302, 307 and
        // 404 as often as with 200, and none of those mean anything is wrong.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut scratch = [0u8; 512];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut scratch).await;
            let _ = tokio::io::AsyncWriteExt::write_all(
                &mut stream,
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
            )
            .await;
        });

        let check = HealthCheck::Http {
            path: "/".to_string(),
            port: None,
            expect_status: Vec::new(),
        };
        let result = probe(&check, Some(port), true).await;
        assert_eq!(result.status, ServiceStatus::Healthy, "{:?}", result.detail);
    }

    #[tokio::test]
    async fn a_port_held_but_never_answered_is_not_healthy() {
        // The case a TCP check cannot see, and the reason this is the default
        // for anything serving HTTP: a wedged dev server goes on accepting
        // connections it will never reply to, and reports healthy forever.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // Accept and then say nothing at all.
            let _keep = listener.accept().await;
            std::future::pending::<()>().await;
        });

        let tcp = probe(&HealthCheck::Tcp { port: None }, Some(port), true).await;
        assert_eq!(
            tcp.status,
            ServiceStatus::Healthy,
            "a tcp check cannot tell the difference"
        );

        let http = probe(
            &HealthCheck::Http {
                path: "/".to_string(),
                port: None,
                expect_status: Vec::new(),
            },
            Some(port),
            true,
        )
        .await;
        assert_eq!(http.status, ServiceStatus::Unhealthy, "{:?}", http.detail);
        assert!(
            http.detail.as_deref().is_some_and(|detail| detail.contains("timed out")),
            "{:?}",
            http.detail
        );
    }

    #[tokio::test]
    async fn an_explicit_status_list_is_still_enforced() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut scratch = [0u8; 512];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut scratch).await;
            let _ = tokio::io::AsyncWriteExt::write_all(
                &mut stream,
                b"HTTP/1.1 500 Server Error\r\nContent-Length: 0\r\n\r\n",
            )
            .await;
        });

        let check = HealthCheck::Http {
            path: "/".to_string(),
            port: None,
            expect_status: vec![200],
        };
        let result = probe(&check, Some(port), true).await;
        assert_eq!(result.status, ServiceStatus::Unhealthy, "{:?}", result.detail);
    }
}

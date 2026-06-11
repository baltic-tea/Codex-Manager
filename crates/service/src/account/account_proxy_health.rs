use reqwest::blocking::Client;
use reqwest::Proxy;
use std::time::{Duration, Instant};
use url::{Host, Url};

const STATUS_FAILED: &str = "failed";
const STATUS_INVALID_URL: &str = "invalid_url";
const STATUS_OK: &str = "ok";
const STATUS_RUNTIME_ERROR: &str = "runtime_error";
const PROXY_TEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PROXY_TEST_TOTAL_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_PROXY_TEST_TARGETS: &[&str] = &[
    "https://www.gstatic.com/generate_204",
    "https://api.ipify.org",
    "https://chatgpt.com/cdn-cgi/trace",
];

#[derive(Debug, Clone)]
pub(crate) struct ProxyHealthCheckResult {
    pub status: &'static str,
    pub latency_ms: Option<i64>,
    pub last_error: Option<String>,
}

pub(crate) fn check_account_proxy(proxy_url: &str) -> ProxyHealthCheckResult {
    check_account_proxy_with_options(proxy_url, DEFAULT_PROXY_TEST_TARGETS, false)
}

fn check_account_proxy_with_options(
    proxy_url: &str,
    targets: &[&str],
    accept_invalid_certs: bool,
) -> ProxyHealthCheckResult {
    let (client, parsed_proxy_url) = match build_proxy_test_client(proxy_url, accept_invalid_certs)
    {
        Ok(result) => result,
        Err(err) => {
            return ProxyHealthCheckResult {
                status: STATUS_INVALID_URL,
                latency_ms: None,
                last_error: Some(err),
            };
        }
    };

    let mut last_error = None;
    for target in targets
        .iter()
        .copied()
        .filter(|value| !value.trim().is_empty())
    {
        let started_at = Instant::now();
        match client.get(target).send() {
            Ok(response) if response.status().is_success() => {
                return ProxyHealthCheckResult {
                    status: STATUS_OK,
                    latency_ms: Some(started_at.elapsed().as_millis().min(i64::MAX as u128) as i64),
                    last_error: None,
                };
            }
            Ok(response) => {
                last_error = Some(format!(
                    "proxy test GET {target} returned HTTP {}",
                    response.status()
                ));
            }
            Err(err) => {
                if looks_like_local_proxy_runtime_error(&parsed_proxy_url, &err) {
                    return ProxyHealthCheckResult {
                        status: STATUS_RUNTIME_ERROR,
                        latency_ms: None,
                        last_error: Some(format!("local proxy runtime unavailable: {err}")),
                    };
                }
                last_error = Some(format!("proxy test GET {target} failed: {err}"));
            }
        }
    }

    ProxyHealthCheckResult {
        status: STATUS_FAILED,
        latency_ms: None,
        last_error: Some(
            last_error.unwrap_or_else(|| "proxy test did not have any target URLs".to_string()),
        ),
    }
}

fn build_proxy_test_client(
    proxy_url: &str,
    accept_invalid_certs: bool,
) -> Result<(Client, Url), String> {
    let parsed = Url::parse(proxy_url)
        .map_err(|err| format!("invalid proxyUrl: {err}. Check the local HTTP/SOCKS proxy URL."))?;
    let proxy = Proxy::all(proxy_url)
        .map_err(|err| format!("invalid proxyUrl: {err}. Check the local HTTP/SOCKS proxy URL."))?;
    let client = Client::builder()
        .connect_timeout(PROXY_TEST_CONNECT_TIMEOUT)
        .timeout(PROXY_TEST_TOTAL_TIMEOUT)
        .danger_accept_invalid_certs(accept_invalid_certs)
        .user_agent(crate::gateway::current_codex_user_agent())
        .proxy(proxy)
        .build()
        .map_err(|err| format!("build proxy test client failed: {err}"))?;
    Ok((client, parsed))
}

fn looks_like_local_proxy_runtime_error(proxy_url: &Url, err: &reqwest::Error) -> bool {
    if !is_loopback_proxy_url(proxy_url) {
        return false;
    }
    if err.is_connect() || err.is_timeout() {
        return true;
    }
    let message = err.to_string().to_ascii_lowercase();
    message.contains("connection refused")
        || message.contains("unsuccessful tunnel")
        || message.contains("tcp connect error")
        || message.contains("error trying to connect")
        || message.contains("proxy connect")
        || message.contains("channel closed")
}

fn is_loopback_proxy_url(proxy_url: &Url) -> bool {
    match proxy_url.host() {
        Some(Host::Ipv4(addr)) => addr.is_loopback(),
        Some(Host::Ipv6(addr)) => addr.is_loopback(),
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        check_account_proxy_with_options, ProxyHealthCheckResult, STATUS_FAILED, STATUS_OK,
        STATUS_RUNTIME_ERROR,
    };
    use rcgen::generate_simple_self_signed;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::{ServerConfig, ServerConnection, StreamOwned};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::{Arc, OnceLock};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn proxy_health_check_succeeds_through_local_http_connect_proxy() {
        let (target_url, target_addr, request_rx, https_handle) =
            spawn_https_response_server(204, "/generate_204");
        let (proxy_url, connect_rx, proxy_handle) = spawn_http_connect_proxy(target_addr);

        let result = check_account_proxy_with_options(&proxy_url, &[target_url.as_str()], true);

        assert_eq!(result.status, STATUS_OK);
        assert!(result.latency_ms.is_some());
        assert_eq!(result.last_error, None);
        assert_eq!(
            connect_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("receive CONNECT line"),
            format!(
                "CONNECT localhost:{} HTTP/1.1",
                target_url
                    .rsplit(':')
                    .next()
                    .expect("target port")
                    .trim_end_matches("/generate_204")
            )
        );
        assert_eq!(
            request_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("receive HTTPS request line"),
            "GET /generate_204 HTTP/1.1"
        );

        proxy_handle.join().expect("join proxy thread");
        https_handle.join().expect("join https thread");
    }

    #[test]
    fn proxy_health_check_marks_non_success_responses_as_failed() {
        let (target_url, target_addr, _request_rx, https_handle) =
            spawn_https_response_server(500, "/generate_204");
        let (proxy_url, _connect_rx, proxy_handle) = spawn_http_connect_proxy(target_addr);

        let result = check_account_proxy_with_options(&proxy_url, &[target_url.as_str()], true);

        assert_failed_with_error(&result, "returned HTTP 500");

        proxy_handle.join().expect("join proxy thread");
        https_handle.join().expect("join https thread");
    }

    #[test]
    fn proxy_health_check_marks_loopback_connect_refused_as_runtime_error() {
        let free_port = reserve_free_port();
        let proxy_url = format!("http://127.0.0.1:{free_port}");

        let result = check_account_proxy_with_options(
            &proxy_url,
            &["https://www.gstatic.com/generate_204"],
            true,
        );

        assert_eq!(result.status, STATUS_RUNTIME_ERROR);
        assert_eq!(result.latency_ms, None);
        let error = result.last_error.as_deref().expect("last_error");
        assert!(error.contains("local proxy runtime unavailable"));
    }

    fn assert_failed_with_error(result: &ProxyHealthCheckResult, expected_fragment: &str) {
        assert_eq!(result.status, STATUS_FAILED);
        assert_eq!(result.latency_ms, None);
        let error = result.last_error.as_deref().expect("last_error");
        assert!(
            error.contains(expected_fragment),
            "unexpected proxy test error: {error}"
        );
    }

    fn spawn_https_response_server(
        status_code: u16,
        path: &str,
    ) -> (String, String, Receiver<String>, thread::JoinHandle<()>) {
        ensure_rustls_crypto_provider();
        let cert = generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("generate self-signed certificate");
        let cert_der: CertificateDer<'static> = cert.cert.der().clone();
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .expect("build rustls server config");
        let server_config = Arc::new(server_config);

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock https server");
        let addr = listener.local_addr().expect("https server local addr");
        let target_addr = format!("127.0.0.1:{}", addr.port());
        let target_url = format!("https://localhost:{}{path}", addr.port());
        let (request_tx, request_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept https test connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set https test read timeout");
            let conn = ServerConnection::new(server_config).expect("create rustls server conn");
            let mut tls = StreamOwned::new(conn, stream);
            let request_line = read_http_request_line(&mut tls);
            let _ = request_tx.send(request_line);

            let reason = if status_code == 204 {
                "No Content"
            } else {
                "Internal Server Error"
            };
            let response = format!(
                "HTTP/1.1 {status_code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            tls.write_all(response.as_bytes())
                .expect("write https response");
            tls.flush().expect("flush https response");
        });
        (target_url, target_addr, request_rx, handle)
    }

    fn spawn_http_connect_proxy(
        target_addr: String,
    ) -> (String, Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock proxy");
        let proxy_addr = listener.local_addr().expect("mock proxy addr");
        let (connect_tx, connect_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut client, _) = listener.accept().expect("accept proxy client");
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set proxy client read timeout");
            let request_line = read_http_request_line(&mut client);
            let _ = connect_tx.send(request_line);

            let mut upstream =
                TcpStream::connect(target_addr.as_str()).expect("connect proxy upstream");
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .expect("write proxy CONNECT response");

            let mut client_reader = client.try_clone().expect("clone proxy client reader");
            let mut upstream_writer = upstream.try_clone().expect("clone proxy upstream writer");
            let upstream_to_client = thread::spawn(move || {
                let _ = std::io::copy(&mut upstream, &mut client);
            });
            let client_to_upstream = thread::spawn(move || {
                let _ = std::io::copy(&mut client_reader, &mut upstream_writer);
            });
            let _ = client_to_upstream.join();
            let _ = upstream_to_client.join();
        });
        (format!("http://{proxy_addr}"), connect_rx, handle)
    }

    fn ensure_rustls_crypto_provider() {
        static RUSTLS_PROVIDER_READY: OnceLock<()> = OnceLock::new();
        let _ = RUSTLS_PROVIDER_READY.get_or_init(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    fn read_http_request_line<T>(stream: &mut T) -> String
    where
        T: Read,
    {
        let mut request = Vec::new();
        let mut buf = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buf).expect("read HTTP request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buf[..read]);
        }
        String::from_utf8_lossy(request.as_slice())
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn reserve_free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .expect("bind free port probe")
            .local_addr()
            .expect("free port addr")
            .port()
    }
}

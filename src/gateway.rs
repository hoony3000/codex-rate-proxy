//! Fixed loopback entry point; rate limiting remains in independent key workers.
use super::*;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct Gateway {
    config: PathBuf,
    settings: Arc<Settings>,
    client: reqwest::Client,
    capacity: Arc<Semaphore>,
}

fn fingerprint(config: &Path, settings: &Settings) -> String {
    let mut hash = Sha256::new();
    hash.update(config.as_os_str().as_encoded_bytes());
    hash.update(b"\0");
    hash.update(&settings.upstream_base_url);
    format!("{:x}", hash.finalize())
}

fn address(settings: &Settings) -> Result<SocketAddr, Box<dyn Error>> {
    let addr: SocketAddr = format!("{}:{}", settings.listen_host, settings.listen_port).parse()?;
    if !addr.ip().is_loopback() || addr.port() == 0 {
        return Err("gateway requires a loopback [server] host and a fixed nonzero port".into());
    }
    Ok(addr)
}

// Called by the synchronous launcher before allocating any per-key process.
pub(crate) fn check(settings: &Settings, config: &Path) -> Result<String, Box<dyn Error>> {
    let addr = address(settings)?;
    let expected = fingerprint(config, settings);
    let result = tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
        let client = reqwest::Client::builder().no_proxy().http1_only()
            .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(3)).build()?;
        let response = client.get(format!("http://{addr}/health")).send().await?;
        if response.status() != StatusCode::OK { return Ok::<bool, reqwest::Error>(false); }
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if data.len() + chunk.len() > 4096 { return Ok(false); }
            data.extend_from_slice(&chunk);
        }
        let value: serde_json::Value = serde_json::from_slice(&data).unwrap_or_default();
        Ok(value["gateway"] == "codex-rate-proxy/1" && value["config"] == expected)
    });
    if !matches!(result, Ok(true)) {
        return Err("shared gateway unavailable or configuration mismatch; start codex-rate-proxy gateway with the same INI".into());
    }
    Ok(format!("http://{addr}/v1"))
}

pub(crate) async fn serve(config: PathBuf) -> Result<(), Box<dyn Error>> {
    let settings = Arc::new(load_settings(&config)?);
    let addr = address(&settings)?;
    // Bind before allocating workers: a second gateway on the same port fails immediately.
    let listener = TcpListener::bind(addr).await?;
    // No total timeout here: worker-side retries and SSE can outlast a single API attempt.
    let client = reqwest::Client::builder().no_proxy().http1_only()
        .redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(3)).build()?;
    let state = Gateway {
        config, client, capacity: Arc::new(Semaphore::new(settings.gateway_max_inflight)), settings,
    };
    // The gateway has a startup snapshot. Workers retain their existing SIGHUP reload support.
    tokio::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut signal) = signal(SignalKind::hangup()) {
            while signal.recv().await.is_some() {
                log("gateway configuration changes require restart; reload workers separately with SIGHUP");
            }
        }
    });
    log(&format!("shared gateway listening on http://{addr}/v1; workers allocated per API key"));
    axum::serve(listener, Router::new().fallback(forward).with_state(state))
        .with_graceful_shutdown(shutdown_signal(None)).await?;
    Ok(())
}

async fn forward(State(state): State<Gateway>, request: Request) -> Response<Body> {
    if request.method() == Method::GET && request.uri().path() == "/health" {
        let body = serde_json::json!({"gateway": "codex-rate-proxy/1",
            "config": fingerprint(&state.config, &state.settings)}).to_string();
        return response_with_body(StatusCode::OK, "application/json", &body);
    }
    let headers = request.headers();
    let key = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, key)| scheme.eq_ignore_ascii_case("Bearer")
            && !key.is_empty() && key.len() <= 16384 && key.bytes().all(|b| (33..=126).contains(&b)))
        .map(|(_, key)| key.to_owned());
    let key = match key {
        Some(key) if headers.get_all(header::AUTHORIZATION).iter().count() == 1 => key,
        _ => return response_with_body(StatusCode::UNAUTHORIZED, "text/plain", "A single Bearer API key is required\n"),
    };
    let permit = match state.capacity.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return response_with_body(StatusCode::SERVICE_UNAVAILABLE, "text/plain", "Gateway request capacity reached\n"),
    };
    let (parts, incoming) = request.into_parts();
    let body = match tokio::time::timeout(state.settings.upstream_timeout,
        to_bytes(incoming, state.settings.max_request_body_bytes)).await {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return response_with_body(StatusCode::PAYLOAD_TOO_LARGE, "text/plain", "Request body too large or unreadable\n"),
        Err(_) => return response_with_body(StatusCode::REQUEST_TIMEOUT, "text/plain", "Request body timeout\n"),
    };
    let config = state.config.clone();
    let max_workers = state.settings.gateway_max_workers;
    let upstream = state.settings.upstream_base_url.clone();
    let worker = tokio::task::spawn_blocking(move || {
        launcher::gateway_worker(&config, &key, max_workers, &upstream).map_err(|_| ())
    }).await;
    let (url, lease) = match worker {
        Ok(Ok(worker)) => worker,
        _ => {
            log("gateway worker unavailable; inspect worker logs or worker capacity");
            return response_with_body(StatusCode::SERVICE_UNAVAILABLE, "text/plain", "Key worker unavailable or capacity reached\n");
        }
    };
    // No gateway retries: replay belongs exclusively to the key worker, before streaming.
    let result = state.client.request(parts.method, build_target_url(&url, &parts.uri))
        .headers(filtered_request_headers(&parts.headers)).body(body).send().await;
    let response = match result {
        Ok(response) => response,
        Err(_) => return response_with_body(StatusCode::BAD_GATEWAY, "text/plain", "Key worker connection failed\n"),
    };
    let status = response.status();
    let headers = response.headers().clone();
    let stream = response.bytes_stream().map(move |chunk| {
        let _guards = (&lease, &permit);
        chunk
    });
    let mut output = Response::new(Body::from_stream(stream));
    *output.status_mut() = status;
    copy_response_headers(&headers, output.headers_mut());
    output
}

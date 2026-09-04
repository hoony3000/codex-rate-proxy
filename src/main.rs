use std::{
    collections::HashMap,
    env,
    error::Error,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::Response,
    Router,
};
use futures_util::StreamExt;
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock},
    time::sleep,
};

const DEFAULT_CONFIG_DIR: &str = ".config/codex-rate-proxy";
const DEFAULT_CONFIG_NAME: &str = "config.ini";

#[derive(Clone, Debug)]
struct Settings {
    listen_host: String,
    listen_port: u16,
    upstream_base_url: String,
    upstream_timeout: Duration,
    max_request_body_bytes: usize,
    min_interval: Duration,
    max_retries: u32,
    backoff_base: Duration,
    backoff_max: Duration,
    backoff_jitter: Duration,
    forward_proxy: Option<String>,
}

#[derive(Clone)]
struct RuntimeConfig {
    settings: Arc<Settings>,
    client: reqwest::Client,
}

#[derive(Clone)]
struct AppState {
    runtime: Arc<RwLock<RuntimeConfig>>,
    last_upstream_start: Arc<Mutex<Option<Instant>>>,
}

type Ini = HashMap<String, HashMap<String, String>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config_path = parse_args()?;
    let settings = Arc::new(load_settings(&config_path)?);
    let client = build_client(&settings)?;
    let state = AppState {
        runtime: Arc::new(RwLock::new(RuntimeConfig {
            settings: Arc::clone(&settings),
            client,
        })),
        last_upstream_start: Arc::new(Mutex::new(None)),
    };

    let bind_address: SocketAddr = format!("{}:{}", settings.listen_host, settings.listen_port)
        .parse()
        .map_err(|error| format!("invalid listen address: {error}"))?;
    let listener = TcpListener::bind(bind_address).await?;

    log(&format!("listening on http://{bind_address}"));
    log(&format!("target: {}", settings.upstream_base_url));
    log(&format!(
        "minimum interval: {:.1}s; 429 retries: {}",
        settings.min_interval.as_secs_f64(),
        settings.max_retries
    ));
    log(if settings.forward_proxy.is_some() {
        "forward proxy: configured"
    } else {
        "forward proxy: direct"
    });

    spawn_reload_handler(state.clone(), config_path);

    let app = Router::new().fallback(proxy_request).with_state(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_ok() {
        log("stopping");
    }
}

#[cfg(unix)]
fn spawn_reload_handler(state: AppState, config_path: PathBuf) {
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sighup = match signal(SignalKind::hangup()) {
            Ok(signal) => signal,
            Err(error) => {
                log(&format!("could not install SIGHUP handler: {error}"));
                return;
            }
        };

        while sighup.recv().await.is_some() {
            reload_runtime_config(&state, &config_path).await;
        }
    });
}

#[cfg(not(unix))]
fn spawn_reload_handler(_state: AppState, _config_path: PathBuf) {}

async fn reload_runtime_config(state: &AppState, config_path: &Path) {
    let mut new_settings = match load_settings(config_path).map_err(|error| error.to_string()) {
        Ok(settings) => settings,
        Err(error) => {
            log(&format!(
                "configuration reload failed; keeping current settings: {error}"
            ));
            return;
        }
    };

    let current = state.runtime.read().await;
    if new_settings.listen_host != current.settings.listen_host
        || new_settings.listen_port != current.settings.listen_port
    {
        log("configuration reload: [server] host/port changes require a restart; keeping current listener");
        new_settings.listen_host = current.settings.listen_host.clone();
        new_settings.listen_port = current.settings.listen_port;
    }
    drop(current);

    let new_client = match build_client(&new_settings).map_err(|error| error.to_string()) {
        Ok(client) => client,
        Err(error) => {
            log(&format!(
                "configuration reload failed; keeping current settings: {error}"
            ));
            return;
        }
    };

    let target = new_settings.upstream_base_url.clone();
    let interval = new_settings.min_interval.as_secs_f64();
    let retries = new_settings.max_retries;
    *state.runtime.write().await = RuntimeConfig {
        settings: Arc::new(new_settings),
        client: new_client,
    };
    log(&format!(
        "configuration reloaded: target={target}; minimum interval={interval:.1}s; 429 retries={retries}"
    ));
}

async fn proxy_request(State(state): State<AppState>, request: Request) -> Response<Body> {
    if request.method() == Method::GET && request.uri().path() == "/health" {
        return response_with_body(StatusCode::OK, "application/json", "{\"status\":\"ok\"}\n");
    }

    let runtime = state.runtime.read().await.clone();
    let settings = runtime.settings;
    let client = runtime.client;

    let (parts, incoming_body) = request.into_parts();
    let body = match to_bytes(incoming_body, settings.max_request_body_bytes).await {
        Ok(body) => body,
        Err(error) => {
            log(&format!("could not read request body: {error}"));
            return response_with_body(
                StatusCode::BAD_REQUEST,
                "text/plain; charset=utf-8",
                "Could not read request body\n",
            );
        }
    };

    let target_url = build_target_url(&settings.upstream_base_url, &parts.uri);
    let headers = filtered_request_headers(&parts.headers);

    for attempt in 0..=settings.max_retries {
        wait_for_upstream_turn(&state, settings.min_interval).await;
        log(&format!(
            "{} {} -> upstream (attempt {}/{})",
            parts.method,
            parts.uri,
            attempt + 1,
            settings.max_retries + 1
        ));

        let result = client
            .request(parts.method.clone(), &target_url)
            .headers(headers.clone())
            .body(body.clone())
            .send()
            .await;

        let upstream = match result {
            Ok(response) => response,
            Err(error) => {
                log(&format!("upstream connection failed: {error}"));
                return response_with_body(
                    StatusCode::BAD_GATEWAY,
                    "text/plain; charset=utf-8",
                    "Upstream connection failed\n",
                );
            }
        };

        if upstream.status() == StatusCode::TOO_MANY_REQUESTS
            && attempt < settings.max_retries
        {
            let retry_after = upstream
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_retry_after);
            let delay = retry_delay(&settings, attempt, retry_after);
            drop(upstream);
            log(&format!(
                "upstream returned 429; retrying in {:.1} seconds",
                delay.as_secs_f64()
            ));
            sleep(delay).await;
            continue;
        }

        return stream_upstream_response(upstream);
    }

    response_with_body(
        StatusCode::INTERNAL_SERVER_ERROR,
        "text/plain; charset=utf-8",
        "Retry loop ended unexpectedly\n",
    )
}

fn stream_upstream_response(upstream: reqwest::Response) -> Response<Body> {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let stream = upstream.bytes_stream().map(|item| {
        item.map_err(|error| -> Box<dyn Error + Send + Sync> { Box::new(error) })
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    copy_response_headers(&headers, response.headers_mut());
    log(&format!("upstream response started with HTTP {status}"));
    response
}

async fn wait_for_upstream_turn(state: &AppState, min_interval: Duration) {
    let mut last_start = state.last_upstream_start.lock().await;
    if let Some(previous) = *last_start {
        let elapsed = previous.elapsed();
        if elapsed < min_interval {
            let delay = min_interval - elapsed;
            log(&format!(
                "rate limit: waiting {:.1} seconds",
                delay.as_secs_f64()
            ));
            sleep(delay).await;
        }
    }
    *last_start = Some(Instant::now());
}

fn retry_delay(settings: &Settings, attempt: u32, retry_after: Option<Duration>) -> Duration {
    let multiplier = 2_f64.powi(attempt.min(30) as i32);
    let calculated = settings.backoff_base.mul_f64(multiplier);
    let capped = calculated.min(settings.backoff_max);
    let server_delay = retry_after.unwrap_or_default();
    capped.max(server_delay) + random_jitter(settings.backoff_jitter)
}

fn random_jitter(maximum: Duration) -> Duration {
    if maximum.is_zero() {
        return Duration::ZERO;
    }
    let fraction = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as f64
        / 1_000_000_000.0;
    maximum.mul_f64(fraction)
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<f64>() {
        return Some(Duration::from_secs_f64(seconds.max(0.0)));
    }
    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(retry_at.duration_since(SystemTime::now()).unwrap_or_default())
}

fn build_target_url(base: &str, uri: &Uri) -> String {
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    if base.ends_with("/v1") && path_and_query.starts_with("/v1/") {
        format!("{}{}", base, &path_and_query[3..])
    } else if path_and_query.starts_with('/') {
        format!("{base}{path_and_query}")
    } else {
        format!("{base}/{path_and_query}")
    }
}

fn filtered_request_headers(source: &HeaderMap) -> HeaderMap {
    let mut destination = HeaderMap::new();
    for (name, value) in source {
        if !is_hop_by_hop(name.as_str())
            && name != header::HOST
            && name != header::CONTENT_LENGTH
        {
            destination.append(name.clone(), value.clone());
        }
    }
    destination
}

fn copy_response_headers(source: &HeaderMap, destination: &mut HeaderMap) {
    for (name, value) in source {
        if !is_hop_by_hop(name.as_str()) {
            destination.append(name.clone(), value.clone());
        }
    }
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn response_with_body(status: StatusCode, content_type: &str, body: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body.to_owned()))
        .expect("static response must be valid")
}

fn build_client(settings: &Settings) -> Result<reqwest::Client, Box<dyn Error>> {
    let mut builder = reqwest::Client::builder()
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(settings.upstream_timeout);
    if let Some(proxy_url) = &settings.forward_proxy {
        builder = builder.proxy(reqwest::Proxy::http(proxy_url)?);
    }
    Ok(builder.build()?)
}

fn parse_args() -> Result<PathBuf, Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let mut config_path = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-c" | "--config" => {
                let value = arguments
                    .next()
                    .ok_or("--config requires a file path")?;
                config_path = Some(PathBuf::from(value));
            }
            "-h" | "--help" => {
                println!(
                    "codex-rate-proxy\n\nUsage: codex-rate-proxy [--config PATH]\n\n\
                     Default config: ~/.config/codex-rate-proxy/{DEFAULT_CONFIG_NAME}"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    Ok(config_path.unwrap_or(default_config_path()?))
}

fn default_config_path() -> Result<PathBuf, Box<dyn Error>> {
    let home = env::var_os("HOME").ok_or("HOME is not set; use --config PATH")?;
    Ok(PathBuf::from(home)
        .join(DEFAULT_CONFIG_DIR)
        .join(DEFAULT_CONFIG_NAME))
}

fn load_settings(path: &Path) -> Result<Settings, Box<dyn Error>> {
    let ini = parse_ini(&fs::read_to_string(path)?)?;
    let upstream_base_url = required(&ini, "upstream", "base_url")?
        .trim_end_matches('/')
        .to_owned();
    if !upstream_base_url.starts_with("http://") {
        return Err("[upstream] base_url must start with http://; HTTPS is not enabled".into());
    }

    let settings = Settings {
        listen_host: get(&ini, "server", "host", "127.0.0.1"),
        listen_port: parse_value(&ini, "server", "port", "8765")?,
        upstream_base_url,
        upstream_timeout: duration_value(&ini, "upstream", "timeout_seconds", "600")?,
        max_request_body_bytes: parse_value(
            &ini,
            "upstream",
            "max_request_body_bytes",
            "134217728",
        )?,
        min_interval: duration_value(&ini, "rate_limit", "min_interval_seconds", "10")?,
        max_retries: parse_value(&ini, "rate_limit", "max_retries", "5")?,
        backoff_base: duration_value(&ini, "rate_limit", "backoff_base_seconds", "5")?,
        backoff_max: duration_value(&ini, "rate_limit", "backoff_max_seconds", "60")?,
        backoff_jitter: duration_value(
            &ini,
            "rate_limit",
            "backoff_jitter_seconds",
            "1",
        )?,
        forward_proxy: optional(&ini, "forward_proxy", "http"),
    };

    if settings.listen_host.is_empty() {
        return Err("[server] host must not be empty".into());
    }
    if settings.max_request_body_bytes == 0 {
        return Err("[upstream] max_request_body_bytes must be greater than zero".into());
    }
    if settings.backoff_max < settings.backoff_base {
        return Err("backoff_max_seconds must be at least backoff_base_seconds".into());
    }
    Ok(settings)
}

fn parse_ini(contents: &str) -> Result<Ini, Box<dyn Error>> {
    let mut result = Ini::new();
    let mut section = String::new();
    for (index, original_line) in contents.lines().enumerate() {
        let line = original_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            if section.is_empty() {
                return Err(format!("empty section name at line {}", index + 1).into());
            }
            continue;
        }
        if section.is_empty() {
            return Err(format!("setting outside a section at line {}", index + 1).into());
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("expected key = value at line {}", index + 1))?;
        result
            .entry(section.clone())
            .or_default()
            .insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    Ok(result)
}

fn get(ini: &Ini, section: &str, key: &str, default: &str) -> String {
    ini.get(section)
        .and_then(|values| values.get(key))
        .cloned()
        .unwrap_or_else(|| default.to_owned())
}

fn optional(ini: &Ini, section: &str, key: &str) -> Option<String> {
    let value = get(ini, section, key, "");
    (!value.trim().is_empty()).then_some(value)
}

fn required<'a>(ini: &'a Ini, section: &str, key: &str) -> Result<&'a str, Box<dyn Error>> {
    ini.get(section)
        .and_then(|values| values.get(key))
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required setting [{section}] {key}").into())
}

fn parse_value<T>(ini: &Ini, section: &str, key: &str, default: &str) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: Error + Send + Sync + 'static,
{
    let value = get(ini, section, key, default);
    value
        .parse::<T>()
        .map_err(|error| format!("invalid [{section}] {key}: {error}").into())
}

fn duration_value(
    ini: &Ini,
    section: &str,
    key: &str,
    default: &str,
) -> Result<Duration, Box<dyn Error>> {
    let seconds: f64 = parse_value(ini, section, key, default)?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(format!("[{section}] {key} must be a non-negative number").into());
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn log(message: &str) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    eprintln!("[{seconds}] {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_v1_without_duplication() {
        let uri: Uri = "/v1/responses?stream=true".parse().unwrap();
        assert_eq!(
            build_target_url("http://llm.example/v1", &uri),
            "http://llm.example/v1/responses?stream=true"
        );
    }

    #[test]
    fn parses_retry_after_seconds() {
        assert_eq!(parse_retry_after("12").unwrap(), Duration::from_secs(12));
    }

    #[test]
    fn parses_ini_values() {
        let ini = parse_ini("[server]\nport = 8765\n[upstream]\nbase_url = http://llm/v1\n")
            .unwrap();
        assert_eq!(get(&ini, "server", "port", "0"), "8765");
        assert_eq!(required(&ini, "upstream", "base_url").unwrap(), "http://llm/v1");
    }
}

//! Linux launcher: per-key daemons, authenticated local control and kernel-backed leases.
//! Keys are persisted only by explicit registration, never in command-line arguments.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::{fd::AsRawFd, unix::{fs::{OpenOptionsExt, PermissionsExt}, net::{UnixListener, UnixStream}, process::CommandExt}},
    process::{Command, Stdio},
    sync::{Mutex as StdMutex, atomic::{AtomicBool, Ordering}},
    thread,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Serialize, Deserialize)]
struct Bootstrap {
    key: String,
    identity: String,
    token: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    protocol: u32,
    identity: String,
    token: String,
    url: String,
    pid: u32,
}

#[derive(Serialize, Deserialize)]
struct Control {
    identity: String,
    token: String,
    operation: String,
}

#[derive(Serialize, Deserialize)]
struct Status {
    identity: String,
    sessions: usize,
    requests: usize,
    idle_seconds: u64,
    stopped: bool,
    stopping: bool,
}

struct Life {
    requests: usize,
    idle_since: Instant,
    stopping: bool,
}

pub struct Managed {
    pub key: String,
    identity: String,
    token: String,
    dir: PathBuf,
    life: StdMutex<Life>,
    pub stop: AtomicBool,
    _run_lock: File,
}

pub struct Activity(Arc<Managed>);

impl Drop for Activity {
    fn drop(&mut self) {
        let mut life = self.0.life.lock().unwrap();
        life.requests -= 1;
        life.idle_since = Instant::now();
    }
}

impl Managed {
    pub fn activity(self: &Arc<Self>) -> Option<Activity> {
        let mut life = self.life.lock().unwrap();
        if life.stopping { return None; }
        life.requests += 1;
        life.idle_since = Instant::now();
        Some(Activity(Arc::clone(self)))
    }

    pub fn publish(self: &Arc<Self>, url: String) -> Result<()> {
        let socket = self.dir.join("control.sock");
        // The daemon's lifetime lock proves no previous managed daemon owns this path.
        if socket.exists() { fs::remove_file(&socket)?; }
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let record = Record {
            protocol: 1, identity: self.identity.clone(), token: self.token.clone(),
            url, pid: std::process::id(),
        };
        let temp = self.dir.join("record.tmp");
        let mut f = private_file(&temp, true)?;
        f.write_all(&serde_json::to_vec(&record)?)?;
        f.sync_all()?;
        fs::rename(temp, self.dir.join("record.json"))?;
        let managed = Arc::clone(self);
        thread::spawn(move || {
            while !managed.stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        let result = (|| -> Result<()> {
                            let request: Control = read_json(&mut stream)?;
                            if request.identity != managed.identity || request.token != managed.token {
                                return Err("control authentication failed".into());
                            }
                            let status = managed.status(&request.operation, None)?;
                            write_json(&mut stream, &status)
                        })();
                        if result.is_err() { /* Never log incoming control data or secrets. */ }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(100));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(())
    }

    fn status(&self, operation: &str, idle_timeout: Option<Duration>) -> Result<Status> {
        let mut life = self.life.lock().unwrap();
        let sessions = live_leases(&self.dir)?;
        if sessions > 0 || life.requests > 0 { life.idle_since = Instant::now(); }
        let idle = life.idle_since.elapsed();
        let available = sessions == 0 && life.requests == 0;
        let should_stop = match operation {
            "stop" | "prune" => available,
            "auto" => available && idle >= idle_timeout.unwrap_or(Duration::MAX),
            "status" => false,
            _ => return Err("unknown control operation".into()),
        };
        if should_stop {
            life.stopping = true;
            self.stop.store(true, Ordering::SeqCst);
        }
        Ok(Status {
            identity: self.identity.clone(), sessions, requests: life.requests,
            idle_seconds: idle.as_secs(), stopped: should_stop, stopping: life.stopping,
        })
    }

    pub fn watchdog(self: &Arc<Self>, state: AppState) {
        let managed = Arc::clone(self);
        tokio::spawn(async move {
            while !managed.stop.load(Ordering::SeqCst) {
                sleep(Duration::from_secs(2)).await;
                let timeout = state.runtime.read().await.settings.idle_timeout;
                // On any inspection error, retain the daemon rather than guessing it is idle.
                let _ = managed.status("auto", Some(timeout));
            }
        });
    }

    pub fn cleanup(&self) {
        // Called after HTTP draining. Lock files remain: unlinking them races with waiting launchers.
        for name in ["record.json", "control.sock", "record.tmp"] {
            let _ = fs::remove_file(self.dir.join(name));
        }
        let _ = live_leases(&self.dir);
    }
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("runtime path must be a real directory".into());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn private_file(path: &Path, truncate: bool) -> Result<File> {
    Ok(OpenOptions::new().read(true).write(true).create(true).truncate(truncate)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)?)
}

fn lock(file: &File, nonblocking: bool) -> Result<bool> {
    let flags = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
    if unsafe { libc::flock(file.as_raw_fd(), flags) } == 0 { return Ok(true); }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock { Ok(false) } else { Err(error.into()) }
}

fn random_id() -> Result<String> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn identity(key: &str, upstream: &str, config: &Path) -> String {
    let mut hash = Sha256::new();
    for field in [key.as_bytes(), upstream.as_bytes(), config.as_os_str().as_encoded_bytes()] {
        hash.update((field.len() as u64).to_le_bytes());
        hash.update(field);
    }
    format!("{:x}", hash.finalize())
}

fn root() -> Result<PathBuf> {
    let home = env::var_os("HOME").ok_or("HOME is not set")?;
    let path = PathBuf::from(home).join(".local/state/codex-rate-proxy");
    private_dir(&path)?;
    Ok(path)
}

// Keep the socket pathname below the Linux sockaddr_un limit, including long HOME paths.
// An abstract socket could avoid this, but using private filesystem sockets works on older tools.
fn instance_dir(id: &str) -> Result<PathBuf> {
    let path = root()?.join(&id[..24]);
    private_dir(&path)?;
    private_dir(&path.join("leases"))?;
    if path.join("control.sock").as_os_str().as_encoded_bytes().len() >= 108 {
        return Err("HOME path is too long for a Unix control socket".into());
    }
    Ok(path)
}

struct Lease { path: PathBuf, file: File }

impl Lease {
    fn new(dir: &Path) -> Result<Self> {
        let name = random_id()?;
        let temporary = dir.join(format!("lease-{name}.tmp"));
        let path = dir.join("leases").join(name);
        let file = private_file(&temporary, false)?;
        lock(&file, false)?;
        // Publish after locking so cleanup never sees an unlocked new lease.
        fs::rename(temporary, &path)?;
        Ok(Self { path, file })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // Leave the pathname until the daemon confirms the kernel lock is released.
        // A surviving Codex child can still hold the inherited descriptor after launcher death.
        let _ = &self.path;
    }
}

fn live_leases(dir: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(dir.join("leases"))? {
        let path = entry?.path();
        let file = match OpenOptions::new().read(true).write(true)
            .custom_flags(libc::O_NOFOLLOW).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if lock(&file, true)? { fs::remove_file(path)?; } else { count += 1; }
    }
    Ok(count)
}

fn write_json<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<()> {
    writer.write_all(&serde_json::to_vec(value)?)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_json<T: for<'a> Deserialize<'a>>(reader: &mut impl Read) -> Result<T> {
    let mut line = String::new();
    BufReader::new(reader.take(65536)).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

fn control(dir: &Path, record: &Record, operation: &str) -> Result<Status> {
    let mut socket = UnixStream::connect(dir.join("control.sock"))?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    write_json(&mut socket, &Control {
        identity: record.identity.clone(), token: record.token.clone(), operation: operation.into(),
    })?;
    let status: Status = read_json(&mut socket)?;
    if status.identity != record.identity { return Err("instance identity mismatch".into()); }
    Ok(status)
}

fn record(dir: &Path) -> Result<Record> {
    let r: Record = serde_json::from_slice(&fs::read(dir.join("record.json"))?)?;
    if r.protocol != 1 { return Err("unsupported control protocol".into()); }
    Ok(r)
}

pub fn bootstrap(config: &Path) -> Result<Arc<Managed>> {
    let boot: Bootstrap = read_json(&mut std::io::stdin())?;
    validate_key(&boot.key)?;
    let settings = load_settings(config)?;
    if identity(&boot.key, &settings.upstream_base_url, config) != boot.identity {
        return Err("configuration changed during startup; retry launch".into());
    }
    let dir = instance_dir(&boot.identity)?;
    let run_lock = private_file(&dir.join("run.lock"), false)?;
    if !lock(&run_lock, true)? { return Err("instance already running".into()); }
    Ok(Arc::new(Managed {
        key: boot.key, identity: boot.identity, token: boot.token, dir,
        life: StdMutex::new(Life { requests: 0, idle_since: Instant::now(), stopping: false }),
        stop: AtomicBool::new(false), _run_lock: run_lock,
    }))
}

fn ensure_proxy(config: &Path, key: &str, dir: &Path, id: &str) -> Result<(Record, Option<std::sync::mpsc::Receiver<()>>)> {
    if let Ok(r) = record(dir) {
        if r.identity == id {
            if let Ok(s) = control(dir, &r, "status") {
                if !s.stopping { return Ok((r, None)); }
            }
        }
    }
    // A failed health probe alone never authorizes a duplicate or PID-based kill.
    let deadline = Instant::now() + Duration::from_secs(15);
    let run_lock = private_file(&dir.join("run.lock"), false)?;
    while !lock(&run_lock, true)? {
        if Instant::now() >= deadline { return Err("existing proxy is unresponsive or draining; try again later".into()); }
        thread::sleep(Duration::from_millis(100));
    }
    drop(run_lock);
    let log_file = private_file(&dir.join("proxy.log"), true)?;
    let boot = Bootstrap { key: key.into(), identity: id.into(), token: random_id()? };
    let mut command = Command::new(env::current_exe()?);
    command.arg("__managed").arg("--config").arg(config)
        .current_dir(dir)
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::from(log_file));
    // This closure performs only the async-signal-safe setsid syscall after fork.
    unsafe { command.pre_exec(|| {
        if libc::setsid() < 0 { return Err(std::io::Error::last_os_error()); }
        Ok(())
    }); }
    let mut child = command.spawn()?;
    let sent = write_json(&mut child.stdin.take().ok_or("missing bootstrap pipe")?, &boot);
    if sent.is_err() { let _ = child.wait(); return Err("could not initialize proxy".into()); }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait()? { return Err(format!("proxy startup failed ({status}); inspect {}", dir.join("proxy.log").display()).into()); }
        if let Ok(r) = record(dir) {
            if r.identity == id && r.token == boot.token {
                if let Ok(s) = control(dir, &r, "status") {
                    if !s.stopping {
                        let (done, reaped) = std::sync::mpsc::channel();
                        thread::spawn(move || { let _ = child.wait(); let _ = done.send(()); });
                        return Ok((r, Some(reaped)));
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill(); let _ = child.wait();
            return Err("proxy startup timed out".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 16384 || !key.bytes().all(|b| (33..=126).contains(&b)) {
        return Err("API key must be a nonempty single line of printable ASCII without spaces".into());
    }
    Ok(())
}

fn profile_path(name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.len() > 64 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err("user name must be 1-64 ASCII letters, digits, underscores or hyphens".into());
    }
    let home = env::var_os("HOME").ok_or("HOME is not set")?;
    let dir = PathBuf::from(home).join(".config/codex-rate-proxy/users");
    private_dir(&dir)?;
    Ok(dir.join(format!("{name}.key")))
}

fn registered_key(name: &str) -> Result<String> {
    let path = profile_path(name)?;
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path).map_err(|_| "cannot read registered user; run register NAME first")?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
        return Err("registered key must be a regular file with permissions 600".into());
    }
    let mut key = String::new();
    file.take(16386).read_to_string(&mut key)?;
    let key = key.trim_end_matches(['\r', '\n']).to_owned();
    validate_key(&key)?;
    Ok(key)
}

fn register(name: &str, source: Option<(&str, &str)>, replace: bool) -> Result<()> {
    let path = profile_path(name)?;
    let dir = path.parent().ok_or("invalid profile directory")?;
    let guard = private_file(&dir.join("register.lock"), false)?;
    lock(&guard, false)?;
    if fs::symlink_metadata(&path).is_ok() && !replace {
        return Err("user already registered; use --replace to update its key".into());
    }
    // Registration is explicit: do not silently save an inherited account-wide key.
    let key = read_key(source.or(Some(("ask", ""))))?;
    let temporary = dir.join(format!(".{}.tmp", random_id()?));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        writeln!(file, "{key}")?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result?;
    println!("registered {name}; launch with --user {name}");
    Ok(())
}

fn cleanup_failed_launch(dir: &Path, r: &Record, created: Option<std::sync::mpsc::Receiver<()>>) {
    let Some(reaped) = created else { return; };
    let result = (|| -> Result<()> {
        let guard = private_file(&dir.join("start.lock"), false)?;
        lock(&guard, false)?;
        // Authenticate the original instance and stop only if no other sessions/requests use it.
        if control(dir, r, "stop")?.stopped {
            if reaped.recv_timeout(Duration::from_secs(5)).is_err() {
                return Err("proxy is still draining".into());
            }
            eprintln!("Stopped unused proxy created by failed launch");
        }
        Ok(())
    })();
    if result.is_err() {
        eprintln!("Could not finish proxy cleanup; inspect codex-rate-proxy list/prune");
    }
}

fn read_key(source: Option<(&str, &str)>) -> Result<String> {
    let value = match source {
        Some(("file", path)) => fs::read_to_string(path)?,
        Some(("env", name)) => env::var(name).map_err(|_| "selected key environment variable is missing")?,
        Some(("stdin", _)) => { let mut s = String::new(); std::io::stdin().take(16386).read_to_string(&mut s)?; s },
        Some(("ask", _)) => rpassword::prompt_password("API key: ")?,
        None => match env::var("OPENAI_API_KEY") {
            Ok(s) if !s.is_empty() => s,
            _ => rpassword::prompt_password("API key: ")?,
        },
        _ => return Err("unsupported key source".into()),
    };
    let key = value.trim_end_matches(['\r', '\n']).to_owned();
    validate_key(&key)?;
    Ok(key)
}

pub fn dispatch() -> Result<Option<i32>> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let operation = arguments.first().map(String::as_str).unwrap_or("");
    if !matches!(operation, "launch" | "url" | "list" | "stop" | "prune" | "register") { return Ok(None); }
    let mut config = default_config_path()?;
    let mut source: Option<(&str, &str)> = None;
    let mut dry_run = false;
    let mut replace = false;
    let mut user: Option<&str> = None;
    let mut codex_args: &[String] = &[];
    let mut i = if operation == "register" { 2 } else { 1 };
    let registration = if operation == "register" {
        Some(arguments.get(1).ok_or("register requires a user name")?.as_str())
    } else { None };
    while i < arguments.len() {
        let arg = arguments[i].as_str();
        if arg == "--" { codex_args = &arguments[i + 1..]; break; }
        match arg {
            "--config" | "-c" => {
                i += 1; config = PathBuf::from(arguments.get(i).ok_or("--config requires a path")?);
            }
            "--key-file" | "--key-env" => {
                if source.is_some() { return Err("choose only one key source".into()); }
                i += 1;
                source = Some((if arg == "--key-file" { "file" } else { "env" }, arguments.get(i).ok_or("key option needs a value")?));
            }
            "--ask-key" | "--key-stdin" => {
                if source.is_some() { return Err("choose only one key source".into()); }
                source = Some((if arg == "--ask-key" { "ask" } else { "stdin" }, ""));
            }
            "--dry-run" => dry_run = true,
            "--replace" => replace = true,
            "--user" => {
                if user.is_some() { return Err("choose only one user".into()); }
                i += 1;
                user = Some(arguments.get(i).ok_or("--user requires a name")?);
            }
            _ => return Err(format!("unknown launcher option: {arg}").into()),
        }
        i += 1;
    }
    if dry_run && operation != "prune" { return Err("--dry-run is only valid for prune".into()); }
    if replace && operation != "register" { return Err("--replace is only valid for register".into()); }
    if user.is_some() && (source.is_some() || matches!(operation, "register" | "list" | "prune")) {
        return Err("--user is a key source for launch/url/stop; do not combine it with other key sources".into());
    }
    if !codex_args.is_empty() && operation != "launch" { return Err("Codex arguments are only valid for launch".into()); }
    if let Some(name) = registration {
        register(name, source, replace)?;
        return Ok(Some(0));
    }
    if matches!(operation, "list" | "prune") {
        if source.is_some() { return Err("list/prune do not accept a key".into()); }
        list_or_prune(operation, dry_run)?;
        return Ok(Some(0));
    }
    let config = fs::canonicalize(config)?;
    let settings = load_settings(&config)?;
    let key = match user { Some(name) => registered_key(name)?, None => read_key(source)? };
    let id = identity(&key, &settings.upstream_base_url, &config);
    let dir = instance_dir(&id)?;
    let start_lock = private_file(&dir.join("start.lock"), false)?;
    lock(&start_lock, false)?;
    if operation == "stop" {
        match record(&dir) {
            Ok(r) if r.identity == id => {
                let s = control(&dir, &r, "stop")?;
                if !s.stopped { return Err("proxy is in use; close its Codex sessions before stopping".into()); }
                println!("stopping {}", r.url);
            }
            _ => println!("no registered proxy for this key and configuration"),
        }
        return Ok(Some(0));
    }
    // Register a kernel-backed lease before checking or creating the daemon.
    // The control server may already have begun stopping; ensure_proxy handles that case.
    let lease = Lease::new(&dir)?;
    let (r, created) = ensure_proxy(&config, &key, &dir, &id)?;
    drop(start_lock);
    if operation == "url" { println!("{}", r.url); return Ok(Some(0)); }
    eprintln!("Proxy: {}", r.url);
    let mut command = Command::new(&settings.codex_binary);
    if source.is_some_and(|(kind, _)| kind == "stdin") {
        if let Ok(tty) = File::open("/dev/tty") { command.stdin(Stdio::from(tty)); }
    }
    command.arg("-c").arg(format!("model_provider={}", serde_json::to_string(&settings.provider)?))
        .arg("-c").arg(format!("model_providers.{}.base_url={}", settings.provider, serde_json::to_string(&r.url)?))
        .arg("-c").arg(format!("model_providers.{}.env_key=\"CODEX_RATE_PROXY_API_KEY\"", settings.provider))
        .arg("-c").arg(format!("model_providers.{}.requires_openai_auth=false", settings.provider))
        .arg("-c").arg(format!("model_providers.{}.request_max_retries=0", settings.provider))
        .arg("-c").arg(format!("model_providers.{}.stream_max_retries=0", settings.provider))
        .arg("-c").arg(format!("model_providers.{}.supports_websockets=false", settings.provider))
        .args(codex_args)
        .env("CODEX_RATE_PROXY_API_KEY", &key);
    // Preserve other proxy bypass entries while guaranteeing loopback bypass for the child.
    let bypass = env::var("NO_PROXY").or_else(|_| env::var("no_proxy")).unwrap_or_default();
    let bypass = format!("{bypass},127.0.0.1,localhost");
    command.env("NO_PROXY", &bypass).env("no_proxy", &bypass);
    // Only Codex inherits this fd. Other spawned daemons never inherit another key's lease.
    let lease_fd = lease.file.as_raw_fd();
    unsafe { command.pre_exec(move || {
        if libc::fcntl(lease_fd, libc::F_SETFD, 0) < 0 { return Err(std::io::Error::last_os_error()); }
        Ok(())
    }); }
    let status = command.status();
    drop(lease);
    if status.as_ref().map_or(true, |s| !s.success()) {
        cleanup_failed_launch(&dir, &r, created);
    }
    let status = status.map_err(|_| "could not launch Codex; check [launcher] codex_binary")?;
    use std::os::unix::process::ExitStatusExt;
    Ok(Some(status.code().unwrap_or(128 + status.signal().unwrap_or(1))))
}

fn list_or_prune(operation: &str, dry_run: bool) -> Result<()> {
    for entry in fs::read_dir(root()?)? {
        let dir = entry?.path();
        if !fs::symlink_metadata(&dir)?.is_dir() { continue; }
        let start_lock = private_file(&dir.join("start.lock"), false)?;
        if !lock(&start_lock, true)? { continue; }
        let r = match record(&dir) { Ok(r) => r, Err(_) => continue };
        match control(&dir, &r, "status") {
            Ok(s) => {
                let unused = s.sessions == 0 && s.requests == 0;
                println!("{} pid={} sessions={} requests={} idle={}s {}", r.url, r.pid, s.sessions, s.requests,
                    s.idle_seconds, if s.stopping { "stopping" } else if unused { "unused" } else { "active" });
                if operation == "prune" && unused && !dry_run {
                    // The server rechecks its state atomically; never kill by a stale PID.
                    let stopped = control(&dir, &r, "prune")?.stopped;
                    println!("  {}", if stopped { "stopping" } else { "became active; kept" });
                }
            }
            Err(_) => {
                println!("{} unavailable (not terminated)", r.url);
                if operation == "prune" && !dry_run {
                    let run_lock = private_file(&dir.join("run.lock"), false)?;
                    if lock(&run_lock, true)? {
                        for name in ["record.json", "control.sock", "record.tmp"] {
                            let path = dir.join(name);
                            if path.exists() { fs::remove_file(path)?; }
                        }
                        let _ = live_leases(&dir);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_separates_keys_targets_and_configs() {
        let a = identity("a", "http://llm/v1", Path::new("/a.ini"));
        assert_eq!(a, identity("a", "http://llm/v1", Path::new("/a.ini")));
        assert_ne!(a, identity("b", "http://llm/v1", Path::new("/a.ini")));
        assert_ne!(a, identity("a", "http://other/v1", Path::new("/a.ini")));
        assert_ne!(a, identity("a", "http://llm/v1", Path::new("/b.ini")));
    }
    #[test]
    fn rejects_multiline_and_empty_keys() {
        for key in ["", "a\nb", "a b", "a\r"] { assert!(validate_key(key).is_err()); }
        assert!(validate_key("fake-test-key").is_ok());
    }
    #[test]
    fn leases_are_removed_only_after_unlock() {
        let dir = env::temp_dir().join(format!("rate-proxy-test-{}", random_id().unwrap()));
        private_dir(&dir.join("leases")).unwrap();
        let lease = Lease::new(&dir).unwrap();
        assert_eq!(live_leases(&dir).unwrap(), 1);
        drop(lease);
        assert_eq!(live_leases(&dir).unwrap(), 0);
        fs::remove_dir(dir.join("leases")).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}

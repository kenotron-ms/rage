use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, OnceLock};

static CLIENT: OnceLock<Mutex<Option<UnixStream>>> = OnceLock::new();

/// Connect to the sandbox supervisor socket specified by `RAGE_SANDBOX_SOCKET`
/// and store the connection for later use by `send_event`.
pub(crate) fn init_from_env() {
    let socket_path = match std::env::var("RAGE_SANDBOX_SOCKET") {
        Ok(p) => p,
        Err(_) => return,
    };
    let stream = match UnixStream::connect(&socket_path) {
        Ok(s) => Some(s),
        Err(_) => return,
    };
    // OnceLock::get_or_init is infallible; if already initialised we just lose
    // the stream, which is acceptable (init_from_env should only be called once
    // via the #[ctor] function).
    let _ = CLIENT.get_or_init(|| Mutex::new(stream));
}

/// Send a file-system event to the sandbox supervisor.
///
/// All errors are silently ignored so that interposed syscalls are never
/// impacted by supervisor communication failures.
pub(crate) fn send_event(op: &str, path: &str) {
    let cell = match CLIENT.get() {
        Some(c) => c,
        None => return,
    };
    let mut guard = match cell.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let stream = match guard.as_mut() {
        Some(s) => s,
        None => return,
    };

    let pid = unsafe { libc::getpid() };

    // JSON-escape path: escape backslashes first, then double-quotes.
    let escaped_path = path.replace('\\', "\\\\").replace('"', "\\\"");

    // JSON-escape op similarly.
    let escaped_op = op.replace('\\', "\\\\").replace('"', "\\\"");

    let msg = format!("{{\"op\":\"{escaped_op}\",\"path\":\"{escaped_path}\",\"pid\":{pid}}}\n");

    let _ = stream.write_all(msg.as_bytes());
}

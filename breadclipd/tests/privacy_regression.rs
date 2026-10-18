use std::{
    io::{BufRead, BufReader},
    os::unix::net::UnixListener,
    process::Command,
    sync::{Arc, Mutex},
    thread,
};

use serde_json::Value;

#[path = "../src/privacy_clients.rs"]
mod privacy_clients;

#[test]
fn later_client_keeps_panic_payload_redacted_and_app_version() {
    if std::env::var_os("CLIP19_PANIC_CHILD").is_some() {
        let _guard = privacy_clients::enable_reporting();
        let _command = privacy_clients::client();
        panic!("CLIP19_SYNTHETIC_PRIVATE_TEXT_7f42");
    }
    let dir = tempfile::tempdir().expect("private runtime directory");
    let runtime = dir.path().join("runtime");
    let socket_dir = runtime.join("bread");
    std::fs::create_dir_all(&socket_dir).unwrap();
    let socket_path = socket_dir.join("breadd.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let messages = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_messages = Arc::clone(&messages);
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let server_running = Arc::clone(&running);
    let server = thread::spawn(move || {
        use std::io::Write;
        while server_running.load(std::sync::atomic::Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut line = String::new();
                    BufReader::new(&stream).read_line(&mut line).unwrap();
                    if !line.is_empty() {
                        server_messages
                            .lock()
                            .unwrap()
                            .push(serde_json::from_str(&line).expect("valid IPC JSON"));
                        let _ = stream.write_all(b"{\"result\":null}\n");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(e) => panic!("socket accept failed: {e}"),
            }
        }
    });

    let home = dir.path().join("home");
    let config = dir.path().join("config");
    let data = dir.path().join("data");
    let state = dir.path().join("state");
    let cache = dir.path().join("cache");
    for p in [&home, &config, &data, &state, &cache] {
        std::fs::create_dir_all(p).unwrap();
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "later_client_keeps_panic_payload_redacted_and_app_version",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("CLIP19_PANIC_CHILD", "1")
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_DATA_HOME", data)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CACHE_HOME", cache)
        .output()
        .expect("run isolated panic probe");
    assert_eq!(output.status.code(), Some(101));
    running.store(false, std::sync::atomic::Ordering::Relaxed);
    server.join().unwrap();

    let messages = messages.lock().unwrap();
    let panic = messages
        .iter()
        .find(|m| m["method"] == "bug.marker")
        .expect("panic marker delivered");
    let marker = &panic["params"]["marker"];
    assert_eq!(marker["message"], "(message omitted by the app)");
    assert_eq!(marker["app_version"], "0.3.0");
    let wire = serde_json::to_string(&*messages).unwrap();
    assert!(!wire.contains("CLIP19_SYNTHETIC_PRIVATE_TEXT_7f42"));
    assert!(!marker["location"].is_null());
}

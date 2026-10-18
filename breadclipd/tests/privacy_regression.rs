use std::{
    io::{BufRead, BufReader},
    os::unix::net::UnixListener,
    process::Command,
    sync::{Arc, Mutex},
    thread,
};

use serde_json::Value;

#[test]
fn later_client_keeps_panic_payload_redacted_and_app_version() {
    if std::env::var_os("CLIP19_PANIC_CHILD").is_some() {
        let old = std::env::var_os("CLIP19_OLD_BEHAVIOR").is_some();
        let mut opts = bread_utils::bug::BugOptions::new("0.3.0");
        opts.panic_message = false;
        opts.forward_logs = false;
        let setup = if old {
            bread_utils::bread_client::BreadClient::connect("clip")
        } else {
            bread_utils::bread_client::BreadClient::connect_without_bug_reporting("clip")
        };
        let _guard = setup.enable_bug_reporting(opts);
        let _command = if old {
            bread_utils::bread_client::BreadClient::connect("clip")
        } else {
            bread_utils::bread_client::BreadClient::connect_without_bug_reporting("clip")
        };
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

    // Run the regression's prior call sequence against another private socket.
    // It must reproduce the leak so the test demonstrates old-fails/new-passes.
    let old_runtime = dir.path().join("old-runtime");
    let old_socket_dir = old_runtime.join("bread");
    std::fs::create_dir_all(&old_socket_dir).unwrap();
    let old_listener = UnixListener::bind(old_socket_dir.join("breadd.sock")).unwrap();
    old_listener.set_nonblocking(true).unwrap();
    let old_messages = Arc::new(Mutex::new(Vec::<Value>::new()));
    let old_sink = Arc::clone(&old_messages);
    let old_running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let old_on = Arc::clone(&old_running);
    let old_server = thread::spawn(move || {
        use std::io::Write;
        while old_on.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok((mut stream, _)) = old_listener.accept() {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                if !line.is_empty() {
                    old_sink
                        .lock()
                        .unwrap()
                        .push(serde_json::from_str(&line).unwrap());
                    let _ = stream.write_all(b"{\"result\":null}\n");
                }
            } else {
                thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    });
    let old_output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "later_client_keeps_panic_payload_redacted_and_app_version",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("CLIP19_PANIC_CHILD", "1")
        .env("CLIP19_OLD_BEHAVIOR", "1")
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &old_runtime)
        .env("XDG_CONFIG_HOME", dir.path().join("old-config"))
        .env("XDG_DATA_HOME", dir.path().join("old-data"))
        .env("XDG_STATE_HOME", dir.path().join("old-state"))
        .env("XDG_CACHE_HOME", dir.path().join("old-cache"))
        .output()
        .unwrap();
    assert_eq!(old_output.status.code(), Some(101));
    old_running.store(false, std::sync::atomic::Ordering::Relaxed);
    old_server.join().unwrap();
    let old_wire = serde_json::to_string(&*old_messages.lock().unwrap()).unwrap();
    assert!(old_wire.contains("CLIP19_SYNTHETIC_PRIVATE_TEXT_7f42"));
    let old_marker = old_messages
        .lock()
        .unwrap()
        .iter()
        .find(|m| m["method"] == "bug.marker")
        .unwrap()["params"]["marker"]
        .clone();
    assert!(old_marker["app_version"].is_null());
}

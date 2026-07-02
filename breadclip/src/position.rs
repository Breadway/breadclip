use std::{env, io::{Read, Write}, os::unix::net::UnixStream};

#[allow(dead_code)]
pub struct WindowInfo {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[allow(dead_code)]
pub struct MonitorInfo {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Query Hyprland for the currently active (focused) window via its IPC socket.
/// Returns `None` if the window is fullscreen or no window is focused.
pub fn get_active_window() -> Option<WindowInfo> {
    let json = hyprctl_json("j/activewindow")?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;

    // Fullscreen windows should cause centred fallback positioning
    if v["fullscreen"].as_i64().unwrap_or(0) != 0 {
        return None;
    }
    // "class" is empty when no window is focused
    if v["class"].as_str().unwrap_or("").is_empty() {
        return None;
    }

    let x = v["at"][0].as_i64()? as i32;
    let y = v["at"][1].as_i64()? as i32;
    let w = v["size"][0].as_i64()? as i32;
    let h = v["size"][1].as_i64()? as i32;
    Some(WindowInfo { x, y, width: w, height: h })
}

/// Query Hyprland for the focused monitor's dimensions.
pub fn get_focused_monitor() -> Option<MonitorInfo> {
    let json = hyprctl_json("j/monitors")?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
    let monitors = v.as_array()?;
    let m = monitors
        .iter()
        .find(|m| m["focused"].as_bool().unwrap_or(false))
        .or_else(|| monitors.first())?;
    Some(MonitorInfo {
        x: m["x"].as_i64()? as i32,
        y: m["y"].as_i64()? as i32,
        width: m["width"].as_i64()? as i32,
        height: m["height"].as_i64()? as i32,
    })
}

/// Send a request to Hyprland's IPC socket and return the response string.
fn hyprctl_json(request: &str) -> Option<String> {
    let sig = env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let rt = env::var("XDG_RUNTIME_DIR").ok()?;
    let socket = format!("{}/hypr/{}/.socket.sock", rt, sig);

    let mut stream = UnixStream::connect(&socket).ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    stream.shutdown(std::net::Shutdown::Write).ok()?;

    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    Some(buf)
}

//! Test-only process. It is not a Godot editor and is never used by the library.
use serde_json::{json, Value};
use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 8
        || args[0] != "--headless"
        || args[1] != "--editor"
        || args[2] != "--path"
        || args[4] != "--lsp-port"
        || args[6] != "--dap-port"
    {
        std::process::exit(11);
    }
    let lsp = args[5].to_string_lossy().parse::<u16>().unwrap_or(0);
    let dap = args[7].to_string_lossy().parse::<u16>().unwrap_or(0);
    let project = PathBuf::from(&args[3]);
    let Ok(cwd) = env::current_dir() else {
        std::process::exit(12)
    };
    if fs::canonicalize(&cwd).ok() != fs::canonicalize(&project).ok() {
        std::process::exit(13);
    }
    let id = env::var("UEBOX_GODOT_INSTANCE_ID").unwrap_or_default();
    let token = env::var("UEBOX_GODOT_TOKEN").unwrap_or_default();
    let port = env::var("UEBOX_GODOT_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    if id.len() != 32
        || token.len() != 64
        || port == 0
        || lsp == 0
        || dap == 0
        || port == lsp
        || port == dap
        || lsp == dap
        || env::var_os("UEBOX_SESSION_PRIVATE_SENTINEL").is_some()
    {
        std::process::exit(14);
    }
    if project.join(".fixture-delayed-listen").exists() {
        thread::sleep(Duration::from_millis(400));
    }
    if let Ok(expected) = fs::read_to_string(project.join(".fixture-expected-home")) {
        let home = PathBuf::from(expected);
        let expected_paths = [
            ("HOME", home.clone()),
            ("USERPROFILE", home.clone()),
            ("APPDATA", home.join("appdata")),
            ("LOCALAPPDATA", home.join("localappdata")),
            ("XDG_CONFIG_HOME", home.join("config")),
            ("XDG_DATA_HOME", home.join("data")),
            ("XDG_CACHE_HOME", home.join("cache")),
            ("TMPDIR", home.join("temp")),
            ("TMP", home.join("temp")),
            ("TEMP", home.join("temp")),
        ];
        let valid = expected_paths
            .iter()
            .all(|(key, value)| env::var_os(key).as_deref() == Some(value.as_os_str()));
        fs::write(
            project.join(".fixture-isolation.tmp"),
            if valid { "ok" } else { "bad" },
        )
        .unwrap();
        fs::rename(
            project.join(".fixture-isolation.tmp"),
            project.join(".fixture-isolation"),
        )
        .unwrap();
    }
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(_) => std::process::exit(15),
    };
    listener.set_nonblocking(true).unwrap();
    fs::write(project.join(".fixture-ready"), b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut connections = 0;
    while project.is_dir() && Instant::now() < deadline {
        if project.join(".fixture-exit").exists() {
            fs::write(project.join(".fixture-finished"), b"done").unwrap();
            return;
        }
        if project.join(".fixture-check-alive").exists() {
            let _ = fs::write(project.join(".fixture-alive-after-drop"), b"alive");
        }
        if project.join(".fixture-error").exists() {
            std::process::exit(23);
        }
        #[cfg(unix)]
        if project.join(".fixture-signal").exists() {
            unsafe extern "C" {
                fn getpid() -> i32;
                fn kill(pid: i32, signal: i32) -> i32;
            }
            unsafe {
                kill(getpid(), 15);
            }
            std::process::exit(24);
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                connections += 1;
                fs::write(
                    project.join(".fixture-connections"),
                    connections.to_string(),
                )
                .unwrap();
                if project.join(".fixture-disconnect").exists() {
                    continue;
                }
                if project.join(".fixture-hang").exists() {
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                if BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .is_err()
                {
                    continue;
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if request["token"] != token || request["target"]["instance_id"] != id {
                    continue;
                }
                let request_id = request["request_id"].clone();
                let path = wire_path(&project);
                if request["target"]["project_path"] != path {
                    continue;
                }
                let response = if project.join(".fixture-malicious-error").exists() {
                    json!({"version": 1, "request_id": request_id, "error": {"code": "unexpected", "message": "UEBOX_TEST_UNTRUSTED_MSG"}})
                } else if project.join(".fixture-remote-error").exists() {
                    json!({"version": 1, "request_id": request_id, "error": {"code": "response_too_large", "message": "fixture only"}})
                } else {
                    let result = match request["command"].as_str() {
                        Some("project.info") => {
                            let name = if project.join(".fixture-private-id").exists() {
                                format!("prefix-{id}-suffix")
                            } else if project.join(".fixture-private-token").exists() {
                                format!("prefix-{token}-suffix")
                            } else if project.join(".fixture-legitimate-hash").exists() {
                                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                                    .to_owned()
                            } else {
                                "Fixture project".to_owned()
                            };
                            let version = if project.join(".fixture-version-private-token").exists()
                            {
                                format!("prefix-{token}-suffix")
                            } else {
                                "fixture".to_owned()
                            };
                            json!({"name": name, "godot_version": version})
                        }
                        Some("scene.tree") => {
                            let path = if project.join(".fixture-scene-private-id").exists() {
                                format!("res://{id}.tscn")
                            } else {
                                "res://main.tscn".to_owned()
                            };
                            let name = if project.join(".fixture-scene-private-token").exists() {
                                format!("prefix-{token}-suffix")
                            } else if project.join(".fixture-legitimate-hash").exists() {
                                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                                    .to_owned()
                            } else {
                                "World".to_owned()
                            };
                            let node_path =
                                if project.join(".fixture-node-path-private-id").exists() {
                                    format!("root/{id}")
                                } else {
                                    ".".to_owned()
                                };
                            let class =
                                if project.join(".fixture-node-class-private-token").exists() {
                                    format!("prefix-{token}-suffix")
                                } else {
                                    "Node2D".to_owned()
                                };
                            json!({"scene_path": path, "nodes": [{"name": name, "path": node_path, "class": class}], "truncated": project.join(".fixture-legitimate-hash").exists()})
                        }
                        _ => continue,
                    };
                    json!({"version": 1, "request_id": request_id, "identity": {"project_path": path, "instance_id": id}, "result": result})
                };
                let _ = writeln!(stream, "{response}");
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(_) => break,
        }
    }
}

fn wire_path(path: &Path) -> String {
    let path = fs::canonicalize(path).unwrap();
    let raw = path.to_str().unwrap();
    if cfg!(windows) {
        let raw = if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else {
            raw.strip_prefix(r"\\?\").unwrap_or(raw).to_owned()
        };
        raw.replace('\\', "/").trim_end_matches('/').to_owned()
    } else {
        raw.to_owned()
    }
}

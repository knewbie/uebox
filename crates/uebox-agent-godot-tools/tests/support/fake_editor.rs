//! Test-only peer, not Godot. It never logs or stores the synthetic secret.
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
    let project = PathBuf::from(&args[3]);
    if fs::canonicalize(env::current_dir().unwrap()).ok() != fs::canonicalize(&project).ok() {
        std::process::exit(12);
    }
    let id = env::var("UEBOX_GODOT_INSTANCE_ID").unwrap_or_default();
    let token = env::var("UEBOX_GODOT_TOKEN").unwrap_or_default();
    let port = env::var("UEBOX_GODOT_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    if id.len() != 32 || token.len() != 64 || port == 0 {
        std::process::exit(13);
    }
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut connections = 0;
    while project.is_dir() && Instant::now() < deadline {
        // Inspect generated test-only request bodies without writing the secret.
        if let Ok(body) = fs::read_to_string(project.join(".fixture-audit-request")) {
            let result = if body.contains(&token) {
                "token"
            } else if body.contains(&id) {
                "instance"
            } else {
                "clear"
            };
            let _ = fs::write(project.join(".fixture-audit-result"), result);
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
                if project.join(".fixture-hold").exists() {
                    fs::write(project.join(".fixture-inflight"), b"waiting").unwrap();
                    let hold_deadline = Instant::now() + Duration::from_secs(10);
                    while project.is_dir()
                        && !project.join(".fixture-release").exists()
                        && Instant::now() < hold_deadline
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                let response = if project.join(".fixture-malicious-error").exists() {
                    json!({"version":1,"request_id":request_id,"error":{"code":"unexpected","message":"UEBOX_TEST_UNTRUSTED_MSG"}})
                } else {
                    let result = match request["command"].as_str() {
                        Some("project.info") => {
                            let name = if project.join(".fixture-leak-instance").exists() {
                                format!("prefix-{id}-suffix")
                            } else if project.join(".fixture-leak-token").exists() {
                                format!("prefix-{token}-suffix")
                            } else if project.join(".fixture-legitimate-hash").exists() {
                                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                                    .to_owned()
                            } else {
                                "Fixture project".to_owned()
                            };
                            json!({"name":name,"godot_version":"fixture"})
                        }
                        Some("scene.tree") => {
                            let scene_path =
                                if project.join(".fixture-scene-leak-instance").exists() {
                                    format!("res://{id}.tscn")
                                } else {
                                    "res://main.tscn".to_owned()
                                };
                            let name = if project.join(".fixture-scene-leak-token").exists() {
                                format!("prefix-{token}-suffix")
                            } else if project.join(".fixture-legitimate-hash").exists() {
                                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                                    .to_owned()
                            } else {
                                "World".to_owned()
                            };
                            json!({"scene_path":scene_path,"nodes":[{"name":name,"path":".","class":"Node2D"}],"truncated":project.join(".fixture-truncated").exists()})
                        }
                        _ => continue,
                    };
                    json!({"version":1,"request_id":request_id,"identity":{"project_path":path,"instance_id":id},"result":result})
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

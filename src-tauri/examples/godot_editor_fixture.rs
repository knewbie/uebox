//! Test-only executable: emulates the loopback Godot bridge, never an editor.
use serde_json::{json, Value};
use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let offset = usize::from(args.first().is_some_and(|v| v == "--headless"));
    if args.len() != offset + 7 || args[offset] != "--editor" || args[offset + 1] != "--path" {
        std::process::exit(2);
    }
    let project = fs::canonicalize(PathBuf::from(&args[offset + 2])).unwrap();
    let id = env::var("UEBOX_GODOT_INSTANCE_ID").unwrap();
    let token = env::var("UEBOX_GODOT_TOKEN").unwrap();
    let port = env::var("UEBOX_GODOT_PORT")
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    fs::write(
        project.join("fixture-ready"),
        std::process::id().to_string(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    while project.is_dir() && Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
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
                if request["target"]["project_path"] != project.to_str().unwrap() {
                    continue;
                }
                if project.join("fixture-disconnect").exists() {
                    continue;
                }
                if project.join("fixture-hold").exists() {
                    let _ = fs::write(project.join("fixture-inflight"), "yes");
                    let until = Instant::now() + Duration::from_secs(4);
                    while project.is_dir()
                        && !project.join("fixture-release").exists()
                        && Instant::now() < until
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                let result = match request["command"].as_str() {
                    Some("project.info") => {
                        json!({"name":"Tauri agent fixture project", "godot_version":"fixture"})
                    }
                    Some("scene.tree") => {
                        json!({"scene_path":"res://main.tscn", "nodes":[{"name":"FixtureWorld", "path":".", "class":"Node2D"}], "truncated":true})
                    }
                    _ => continue,
                };
                let response = json!({"version":1,"request_id":request["request_id"],"identity":{"project_path":project.to_str().unwrap(),"instance_id":id},"result":result});
                let _ = writeln!(stream, "{response}");
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(_) => break,
        }
    }
}

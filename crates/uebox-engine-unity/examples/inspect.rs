//! Connect only to an editor instance whose single-client socket may be replaced.
//! Usage: cargo run --example inspect -- --allow-disconnect-existing-client 127.0.0.1:PORT /absolute/project/root

use std::error::Error;
use std::net::SocketAddr;
use uebox_engine_unity::{BridgeOptions, Target, UnityClient};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--allow-disconnect-existing-client") {
        return Err("explicit --allow-disconnect-existing-client authorization is required".into());
    }
    let address: SocketAddr = args
        .next()
        .ok_or("missing editor socket address")?
        .parse()?;
    let project_root = args.next().ok_or("missing expected project root")?;
    if args.next().is_some() {
        return Err("expected exactly an address and project root".into());
    }
    let mut client = UnityClient::connect_authorized(
        Target::new(address, project_root)?.authorize_takeover(),
        BridgeOptions::default(),
    )?;
    println!("Project: {:?}", client.last_observed_project_info());
    println!("Active scene: {:?}", client.active_scene()?);
    Ok(())
}

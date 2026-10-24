//! spacebox-web: an Axum server that runs the Spacebox engine and exposes full administrative
//! control over it through a REST API, plus a three.js client served from `static/`.

mod api;
mod diff;
mod engine;
mod snapshot;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::{get, patch, post, put};
use tower_http::services::ServeDir;

use crate::engine::Engine;

const USAGE: &str = "\
usage: spacebox-web [--port N] [--data DIR] [--load FILE] [--static DIR]
  --port    port to listen on (default 3000)
  --data    data directory with base.toml, scenarios/ and saves/ (default ../data)
  --load    world to start with, relative to --data (default scenarios/sol.toml)
  --static  client files (default ./static next to this crate)";

fn main() {
    let mut port: u16 = 3000;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut data = manifest.join("../data");
    let mut load = String::from("scenarios/sol.toml");
    let mut static_dir = manifest.join("static");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| exit(&format!("{a} needs a value")))
        };
        match a.as_str() {
            "--port" => port = value().parse().unwrap_or_else(|_| exit("invalid --port")),
            "--data" => data = PathBuf::from(value()),
            "--load" => load = value(),
            "--static" => static_dir = PathBuf::from(value()),
            "-h" | "--help" => exit(""),
            other => exit(&format!("unknown argument {other}")),
        }
    }
    let data = data
        .canonicalize()
        .unwrap_or_else(|e| exit(&format!("data directory {}: {e}", data.display())));
    let (world, warnings) = spacebox_core::load_world(data.join(&load))
        .unwrap_or_else(|e| exit(&format!("cannot load {load}: {e}")));
    for w in warnings {
        eprintln!("warning: {w}");
    }
    let shared = Arc::new(Mutex::new(Engine::new(world, data)));

    let app = Router::new()
        .route("/api/defs", get(api::get_defs))
        .route("/api/state", get(api::get_state))
        .route("/api/tick", post(api::tick))
        .route("/api/sync", post(api::sync))
        .route("/api/command", post(api::command))
        .route("/api/empires", post(api::create_empire))
        .route("/api/empires/{id}", patch(api::patch_empire))
        .route("/api/empires/{id}/techs", post(api::set_tech))
        .route("/api/empires/{id}/storage", post(api::set_storage))
        .route("/api/systems", post(api::create_system))
        .route(
            "/api/systems/{id}",
            patch(api::patch_system).delete(api::delete_system),
        )
        .route("/api/systems/{id}/planets", post(api::create_planet))
        .route(
            "/api/planets/{id}",
            patch(api::patch_planet).delete(api::delete_planet),
        )
        .route("/api/planets/{id}/tiles/{tile}", put(api::edit_tile))
        .route("/api/fleets", post(api::create_fleet))
        .route(
            "/api/fleets/{id}",
            patch(api::patch_fleet).delete(api::delete_fleet),
        )
        .route("/api/world/regenerate", post(api::regenerate))
        .route("/api/world/load", post(api::load))
        .route("/api/world/save", post(api::save_world))
        .route("/api/world/files", get(api::list_files))
        .fallback_service(ServeDir::new(static_dir))
        .with_state(shared);

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async move {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .unwrap_or_else(|e| exit(&format!("cannot listen on {addr}: {e}")));
        println!("spacebox-web listening on http://{addr}");
        axum::serve(listener, app).await.expect("server error");
    });
}

fn exit(msg: &str) -> ! {
    if !msg.is_empty() {
        eprintln!("error: {msg}");
    }
    eprintln!("{USAGE}");
    std::process::exit(if msg.is_empty() { 0 } else { 2 })
}

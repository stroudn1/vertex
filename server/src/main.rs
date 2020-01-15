use actix::prelude::*;
use actix_web::web::{Data, Payload};
use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_web_actors::ws;
use std::{env, fmt::Debug, fs};

mod auth;
mod client;
mod config;
mod database;
mod community;

use crate::config::Config;
use client::ClientWsSession;
use database::DatabaseServer;
use directories::ProjectDirs;
use log::info;
use std::fs::OpenOptions;

#[derive(Debug, Message)]
#[rtype(type = "()")]
pub struct SendMessage<T: Debug> {
    message: T,
}

async fn dispatch_client_ws(
    request: HttpRequest,
    stream: Payload,
    db_server: Data<Addr<DatabaseServer>>,
    config: Data<config::Config>,
) -> Result<HttpResponse, Error> {
    let db_server = db_server.get_ref().clone();

    ws::start(
        ClientWsSession::new(db_server, config),
        &request,
        stream,
    )
}

fn create_files_directories(config: &Config) {
    let dirs = [config.profile_pictures.clone()];

    for dir in &dirs {
        fs::create_dir_all(dir).expect(&format!(
            "Error creating directory {}",
            dir.to_string_lossy()
        ));
    }
}

fn setup_logging() {
    let dirs = ProjectDirs::from("", "vertex_chat", "vertex_server")
        .expect("Error getting project directories");
    let dir = dirs.data_dir().join("logs");

    fs::create_dir_all(&dir).expect(&format!(
        "Error creating log dirs ({})",
        dir.to_string_lossy(),
    ));

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{}] [{}] [{}] {}",
                chrono::Local::now().to_rfc3339(),
                record.level(),
                record.target(),
                message
            ))
        })
        .level(log::LevelFilter::Info)
        .chain(std::io::stdout())
        .chain(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(
                    dir.join(
                        chrono::Local::now()
                            .format("vertex_server_%Y-%m-%d_%H-%M-%S.log")
                            .to_string(),
                    ),
                )
                .expect("Error opening log file"),
        )
        .apply()
        .expect("Error setting logger settings");

    info!("Logging set up");
}

fn main() -> std::io::Result<()> {
    println!("Vertex server starting...");
    setup_logging();

    let args = env::args().collect::<Vec<_>>();
    let addr = args.get(1).cloned().unwrap_or("127.0.0.1:8080".to_string());

    let config = config::load_config();
    create_files_directories(&config);

    let ssl_config = config::ssl_config();

    let mut sys = System::new("vertex_server");
    let db_server = DatabaseServer::new(&mut sys, &config).start();

    HttpServer::new(move || {
        App::new()
            .data(db_server.clone())
            .data(config.clone())
            .service(web::resource("/client/").route(web::get().to(dispatch_client_ws)))
    })
    .bind_openssl(addr.clone(), ssl_config)
    .expect("Error binding to socket")
    .run();

    info!("Vertex server started on addr {}", addr);

    sys.run()
}

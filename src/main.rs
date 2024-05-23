#![feature(iter_intersperse)]

mod snapshot;
mod youtube;
mod html_meta;
mod snapper;
mod robots;
mod bilibili;
mod util;

use std::env;
use std::sync::Arc;
use actix_web::{App, HttpServer, post, Responder, web};
use actix_web::middleware::Logger;
use env_logger::{Env, init_from_env};
use log::info;
use crabo_model::{SnapRequest, SnapResponse};
use fedineko_http_client::{GenericClient, HttpClientParameters, MaxHttpVersion, SuppressedClient};
use proxydon_client::ProxydonClient;
use crate::snapper::Clients;
use crate::snapshot::SnapshotMaker;

const CRABO_USER_AGENT: &str = "fedineko/crabo-0.2";

struct SharedContext<'a> {
    snapper: Arc<SnapshotMaker<'a>>,
    clients: Clients,
}

#[post("/snap")]
async fn snap(
    request: web::Json<SnapRequest>,
    state: web::Data<SharedContext<'_>>,
) -> impl Responder {
    let req = request.into_inner();

    let snapshots = state.snapper
        .snap_many(req.urls, &state.clients, req.bypass_cache)
        .await;

    web::Json(
        SnapResponse {
            snapshots
        }
    )
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    init_from_env(
        Env::default()
            .default_filter_or("info")
            .default_write_style_or("always")
    );

    let host = env::var("CRABO_HOST")
        .unwrap_or("127.0.0.1".into());

    let port: u16 = env::var("CRABO_PORT")
        .unwrap_or("8003".into())
        .parse()
        .unwrap_or(8003);


    let proxydon_endpoint = fedineko_url_utils::required_url_from_config(
        "PROXYDON_ENDPOINT",
        "http://127.0.0.1:8002",
    );

    let youtube_api_key = env::var("YOUTUBE_API_KEY")
        .expect("Crabo needs API key provided in YOUTUBE_API_KEY");

    let snapper = Arc::new(SnapshotMaker::new(youtube_api_key));

    info!("Crabo listens on {}:{}", host, port);
    info!("Proxydon endpoint: {proxydon_endpoint}");

    HttpServer::new(move || {
        let context = SharedContext {
            snapper: snapper.clone(),

            clients: Clients {
                proxydon_client: ProxydonClient::new(&proxydon_endpoint),

                generic_client: GenericClient::new_with_user_agent(
                    CRABO_USER_AGENT
                ),

                no_follow_client: GenericClient::new_with_parameters(
                    HttpClientParameters {
                        extra_headers: vec![
                            GenericClient::user_agent_header(CRABO_USER_AGENT)
                        ],

                        middleware: None,
                        max_http_version: MaxHttpVersion::V2,
                        max_redirects: 0,
                    }
                ),

                suppressed_client: SuppressedClient::new(
                    GenericClient::new_with_user_agent(CRABO_USER_AGENT),
                ),
            },
        };

        App::new()
            .service(snap)
            .app_data(web::Data::new(context))
            .wrap(Logger::default())
    })
        .bind((host, port))?
        .run()
        .await?;

    Ok(())
}
//! An http service which builds programs and hosts related metadata
use axum::{
    body::{Body, Bytes},
    extract::{self, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use cargo_metadata::Package;
use futures::channel::mpsc::{self as futures_mpsc};
use http::Method;
use thiserror::Error;
use tokio::sync::mpsc::{channel, Sender};
use tower_http::cors::{Any, CorsLayer};

use program_metadata_http_service::build::{handle_build_requests, BuildRequest, BuildResponder};

#[derive(Clone)]
struct AppState {
    /// The key value store
    db: sled::Db,
    /// Channel for sending build requests
    build_requests_tx: Sender<BuildRequest>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "3000".to_string());

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_origin(Any);

    let (build_requests_tx, build_requests_rx) = channel(1000);

    let db = sled::open("./db")?;

    let app = Router::new()
        .route("/", get(front_page))
        .route("/programs", get(list_programs))
        .route("/program/:program_hash", get(get_program))
        .route("/add-program-git", post(add_program_git))
        .route("/add-program-tar", post(add_program_tar))
        .with_state(AppState {
            db: db.clone(),
            build_requests_tx,
        })
        .layer(cors);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    let local_addr = listener.local_addr()?;
    println!("Listening on {}", local_addr);

    // Handle requests to build programs in serial in a separate task
    tokio::spawn(async move {
        handle_build_requests(build_requests_rx, db).await;
    });

    axum::serve(listener, app).await?;
    Ok(())
}

/// Add a program from a git repository
async fn add_program_git(
    State(state): State<AppState>,
    git_url: String,
) -> Result<(StatusCode, Body), AppError> {
    let (response_tx, response_rx) = futures_mpsc::channel(1000);
    state
        .build_requests_tx
        .send(BuildRequest::new_git(git_url, BuildResponder(response_tx)))
        .await?;

    Ok((StatusCode::OK, Body::from_stream(response_rx)))
}

/// Add a program given as a tar achive
async fn add_program_tar(
    State(state): State<AppState>,
    input: Bytes,
) -> Result<(StatusCode, Body), AppError> {
    let (response_tx, response_rx) = futures_mpsc::channel(1000);
    state
        .build_requests_tx
        .send(BuildRequest::new_tar(
            input.to_vec(),
            BuildResponder(response_tx),
        ))
        .await?;
    Ok((StatusCode::OK, Body::from_stream(response_rx)))
}

/// Get metadata about a program with a given hash
async fn get_program(
    State(state): State<AppState>,
    extract::Path(program_hash): extract::Path<String>,
) -> Result<String, AppError> {
    let hash = hex::decode(program_hash)?;
    Ok(std::str::from_utf8(&state.db.get(hash)?.ok_or(AppError::ProgramNotFound)?)?.to_string())
}

/// Get hashes of all programs in the db
async fn list_programs(State(state): State<AppState>) -> Result<String, AppError> {
    let mut hashes = Vec::new();
    for res in state.db.iter() {
        let (key, _value) = res?;
        hashes.push(hex::encode(key));
    }
    Ok(serde_json::to_string(&hashes)?)
}

/// The "/" route responds with a web page showing the programs
async fn front_page(State(state): State<AppState>) -> Html<String> {
    let mut programs = Vec::new();
    for res in state.db.iter() {
        if let Ok((key, value)) = res {
            if let Ok(package) = serde_json::from_slice::<Package>(&value) {
                let hash = hex::encode(key);
                programs.push(format!(
                    "<li><a href=\"program/{}\">{} <code>{}</code></a></li>",
                    hash, package.name, hash,
                ));
            }
        }
    }

    Html(format!(
        r#"
        <!doctype html>
        <html>
            <head></head>
            <body>
                <h1>Program metadata http service</h1>
                <ul>{}</ul>
            </body>
        </html>
        "#,
        programs.join("\n"),
    ))
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Utf8Error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("Database error {0}")]
    Db(#[from] sled::Error),
    #[error("Cannot decode hex {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("Program not found")]
    ProgramNotFound,
    #[error("Queue is full: {0}")]
    MpscSend(#[from] tokio::sync::mpsc::error::SendError<BuildRequest>),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = format!("{self}").into_bytes();
        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use cargo_metadata::{CargoOpt, MetadataCommand};
use sp_core::Hasher;
use sp_runtime::traits::BlakeTwo256;
use std::{
    env::set_current_dir,
    path::PathBuf,
    process::{Command, Stdio},
};
use tar::Archive;
use temp_dir::TempDir;
use thiserror::Error;
use tokio::fs::{read_dir, File};
use tokio::io::AsyncReadExt;
// use tower_http::services::ServeFile;

#[derive(Clone)]
struct AppState {
    db: sled::Db,
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/programs/:program_hash", get(get_program))
        .route("/build-tar", post(build_tar))
        .route("/build-git", post(build_git))
        .with_state(AppState {
            db: sled::open("./db").unwrap(),
        });

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Listening on localhost:3000");
    axum::serve(listener, app).await.unwrap();
}

async fn get_program(State(state): State<AppState>, Path(program_hash): Path<String>) -> String {
    let hash = hex::decode(program_hash).unwrap();
    std::str::from_utf8(&state.db.get(hash).unwrap().unwrap())
        .unwrap()
        .to_string()
}

async fn build_tar(
    State(state): State<AppState>,
    input: Bytes,
) -> Result<(StatusCode, String), AppError> {
    let input = input.to_vec();
    let mut archive = Archive::new(&input[..]);
    let temp_dir = TempDir::new().unwrap();
    archive.unpack(temp_dir.path())?; // TODO this should be a unique temporary dir

    set_current_dir(temp_dir.path())?;
    build(state).await
}

async fn build_git(
    State(state): State<AppState>,
    git_url: String,
) -> Result<(StatusCode, String), AppError> {
    let temp_dir = TempDir::new().unwrap();
    let output = Command::new("git")
        .arg("clone")
        .arg("--depth=1")
        .arg(git_url)
        .arg(temp_dir.path())
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .output()?;

    if !output.status.success() {
        return Err(AppError::CompilationFailed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    set_current_dir(temp_dir.path())?;
    build(state).await
}

async fn build(state: AppState) -> Result<(StatusCode, String), AppError> {
    let output = Command::new("cargo")
        .arg("component")
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .output()?;

    if !output.status.success() {
        return Err(AppError::CompilationFailed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let binary_filename = get_binary_filename().await.unwrap();
    println!("{:?}", binary_filename);

    let hash = {
        let mut file = File::open(binary_filename).await?;
        let mut contents = vec![];
        file.read_to_end(&mut contents).await?;
        // TODO this wont let us hash chunks which means we need to read the whole binary into memory
        BlakeTwo256::hash(&contents)
    };

    // Get metadata (name and description) from Cargo.toml file
    let metadata = MetadataCommand::new()
        .manifest_path("./Cargo.toml")
        .features(CargoOpt::AllFeatures)
        .exec()
        .unwrap();
    let root_package_metadata = metadata.root_package().unwrap();
    let root_package_metadata_json = serde_json::to_string(&root_package_metadata).unwrap();

    state
        .db
        .insert(hash, root_package_metadata_json.as_bytes())
        .unwrap();
    // TODO Output the binary
    // TODO clean up (rm temp dir)
    Ok((StatusCode::OK, format!("{:?}", hash)))
}

#[derive(Debug, Error)]
enum AppError {
    #[error("Io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Compilation failed: {0}")]
    CompilationFailed(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = format!("{self}").into_bytes();
        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}

/// Get the name of the first .wasm file we find
async fn get_binary_filename() -> Option<PathBuf> {
    let mut dir_contents = read_dir("target/wasm32-unknown-unknown/release")
        .await
        .unwrap();
    while let Some(entry) = dir_contents.next_entry().await.unwrap() {
        if let Some(extension) = entry.path().extension() {
            if extension.to_str() == Some("wasm") {
                return Some(entry.path());
            }
        }
    }
    None
}

//! An http service which builds programs and hosts related metadata
use axum::{
    body::Bytes,
    extract::{self, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use cargo_metadata::{CargoOpt, MetadataCommand, Package};
use http::Method;
use sp_core::Hasher;
use sp_runtime::traits::BlakeTwo256;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tar::Archive;
use temp_dir::TempDir;
use thiserror::Error;
use tokio::fs::{read_dir, File};
use tokio::io::AsyncReadExt;
use tower_http::cors::{Any, CorsLayer};

#[derive(Clone)]
struct AppState {
    db: sled::Db,
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "3000".to_string());

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_origin(Any);

    let app = Router::new()
        .route("/", get(front_page))
        .route("/programs", get(list_programs))
        .route("/program/:program_hash", get(get_program))
        .route("/add-program-git", post(add_program_git))
        .route("/add-program-tar", post(add_program_tar))
        .with_state(AppState {
            db: sled::open("./db").unwrap(),
        })
        .layer(cors);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .unwrap();
    let local_addr = listener.local_addr().unwrap();
    println!("Listening on {}", local_addr);

    axum::serve(listener, app).await.unwrap();
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

/// Add a program given as a location of a git repo
async fn add_program_git(
    State(state): State<AppState>,
    git_url: String,
) -> Result<(StatusCode, String), AppError> {
    let temp_dir = TempDir::new()?;
    let output = Command::new("git")
        .arg("clone")
        .arg("--depth=1")
        .arg(git_url)
        .arg(temp_dir.path())
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .output()?;

    if !output.status.success() {
        return Err(AppError::GitClone(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    add_program(state, temp_dir.path()).await
}

/// Add a program given as a tar achive
async fn add_program_tar(
    State(state): State<AppState>,
    input: Bytes,
) -> Result<(StatusCode, String), AppError> {
    let input = input.to_vec();
    let mut archive = Archive::new(&input[..]);
    let temp_dir = TempDir::new()?;
    archive.unpack(temp_dir.path())?;

    add_program(state, temp_dir.path()).await
}

/// Build a program, and save metadata under the hash of its binary
async fn add_program(state: AppState, repo_path: &Path) -> Result<(StatusCode, String), AppError> {
    let manifest_path: PathBuf = [repo_path, Path::new("Cargo.toml")].iter().collect();

    // Get metadata from Cargo.toml file
    let metadata = MetadataCommand::new()
        .manifest_path(manifest_path)
        .features(CargoOpt::AllFeatures)
        .exec()?;

    let root_package_metadata = metadata
        .root_package()
        .ok_or(AppError::MetadataMissingRootPackage)?;

    // Get the docker image name from Cargo.toml, if there is one
    let docker_image_name = get_docker_image_name_from_metadata(&root_package_metadata.metadata);

    let binary_dir: PathBuf = [repo_path, Path::new("binary_dir")].iter().collect();

    // Build the program
    let mut command = Command::new("docker");
    command.arg("build");
    if let Some(image_name) = docker_image_name {
        command
            .arg("--build-arg")
            .arg(format!("IMAGE={}", image_name));
    }
    let output = command
        .arg(format!("--output={}", binary_dir.display()))
        .arg(repo_path)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .output()?;

    if !output.status.success() {
        return Err(AppError::CompilationFailed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    // Get the hash of the binary
    let hash = {
        let binary_filename = get_binary_filename(binary_dir).await?;
        let mut file = File::open(binary_filename).await?;
        let mut contents = vec![];
        file.read_to_end(&mut contents).await?;
        // TODO #6 this wont let us hash chunks which means we need to read the whole binary into memory
        BlakeTwo256::hash(&contents)
    };
    log::info!("Hashed binary {:?}", hash);

    // Write metadata to db
    let root_package_metadata_json = serde_json::to_string(&root_package_metadata)?;
    state
        .db
        .insert(hash, root_package_metadata_json.as_bytes())?;

    // TODO #7 Make the binary itself available
    Ok((StatusCode::OK, format!("{:?}", hash)))
}

/// Get the name of the first .wasm file we find in the target directory
async fn get_binary_filename(binary_dir: PathBuf) -> Result<PathBuf, AppError> {
    let mut dir_contents = read_dir(binary_dir).await.unwrap();
    while let Some(entry) = dir_contents.next_entry().await? {
        if let Some(extension) = entry.path().extension() {
            if extension.to_str() == Some("wasm") {
                return Ok(entry.path());
            }
        }
    }
    Err(AppError::CompilationFailed(
        "Cannot find binary after compiling".to_string(),
    ))
}

/// We expect there to be a docker image given in the Cargo.toml file like so:
/// ```toml
/// [package.metadata.entropy-program]
/// docker-image = "peg997/build-entropy-programs:version0.1"
/// ```
/// If this is not present a default image is used
fn get_docker_image_name_from_metadata(metadata: &serde_json::value::Value) -> Option<String> {
    if let serde_json::value::Value::Object(m) = metadata {
        if let Some(serde_json::value::Value::Object(p)) = m.get("entropy-program") {
            if let Some(serde_json::value::Value::String(image_name)) = p.get("docker-image") {
                return Some(image_name.clone());
            }
        }
    }
    None
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
enum AppError {
    #[error("Could not clone git repository: {0}")]
    GitClone(String),
    #[error("Cannot find root package in Cargo.toml")]
    MetadataMissingRootPackage,
    #[error("Error reading Cargo.toml: {0}")]
    Metadata(#[from] cargo_metadata::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Utf8Error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("Database error {0}")]
    Db(#[from] sled::Error),
    #[error("Cannot decode hex {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("Io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Compilation failed: {0}")]
    CompilationFailed(String),
    #[error("Program not found")]
    ProgramNotFound,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = format!("{self}").into_bytes();
        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}

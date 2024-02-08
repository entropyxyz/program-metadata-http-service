use axum::{
    body::Bytes,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use std::{
    env::set_current_dir,
    process::{Command, Stdio},
};
use tar::Archive;
use thiserror::Error;

#[tokio::main]
async fn main() {
    let app = Router::new().route("/build", post(build));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn build(input: Bytes) -> Result<StatusCode, AppError> {
    let input = input.to_vec();
    let mut archive = Archive::new(&input[..]);
    archive.unpack("foo")?; // TODO this should be a unique temporary dir

    set_current_dir("foo")?;
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

    // TODO Output the binary
    // TODO hash it and output hash?
    // TODO clean up (rm temp dir)
    Ok(StatusCode::OK)
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

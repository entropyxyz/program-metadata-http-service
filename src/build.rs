use axum::http::StatusCode;
use cargo_metadata::{CargoOpt, MetadataCommand};
use sp_core::Hasher;
use sp_runtime::traits::BlakeTwo256;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tar::Archive;
use temp_dir::TempDir;
use tokio::fs::{read_dir, File};
use tokio::{io::AsyncReadExt, sync::mpsc::Receiver};

use crate::{AppError, BuildRequest};

pub async fn handle_build_requests(mut build_requests_rx: Receiver<BuildRequest>, db: sled::Db) {
    let program_builder = ProgramBuilder(db);
    while let Some(build_request) = build_requests_rx.recv().await {
        match build_request {
            BuildRequest::Git { url, response } => {
                let result = program_builder.add_program_git(url).await;
                if let Err(_) = response.send(result) {
                    log::error!("Response channel has been dropped while building a program",);
                }
            }
            BuildRequest::Tar {
                raw_archive,
                response,
            } => {
                let result = program_builder.add_program_tar(raw_archive).await;
                if let Err(_) = response.send(result) {
                    log::error!("Response channel has been dropped while building a program",);
                }
            }
        }
    }
}

struct ProgramBuilder(sled::Db);

impl ProgramBuilder {
    /// Add a program given as a location of a git repo
    pub async fn add_program_git(&self, git_url: String) -> Result<(StatusCode, String), AppError> {
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

        self.add_program(temp_dir.path()).await
    }

    /// Add a program given as a tar achive
    async fn add_program_tar(&self, input: Vec<u8>) -> Result<(StatusCode, String), AppError> {
        let mut archive = Archive::new(&input[..]);
        let temp_dir = TempDir::new()?;
        archive.unpack(temp_dir.path())?;

        self.add_program(temp_dir.path()).await
    }

    /// Build a program, and save metadata under the hash of its binary
    async fn add_program(&self, repo_path: &Path) -> Result<(StatusCode, String), AppError> {
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
        let docker_image_name =
            get_docker_image_name_from_metadata(&root_package_metadata.metadata);

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
        self.0.insert(hash, root_package_metadata_json.as_bytes())?;

        // TODO #7 Make the binary itself available
        Ok((StatusCode::OK, format!("{:?}", hash)))
    }
}
/// Get the name of the first .wasm file we find in the target directory
async fn get_binary_filename(binary_dir: PathBuf) -> Result<PathBuf, AppError> {
    let mut dir_contents = read_dir(binary_dir).await?;
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

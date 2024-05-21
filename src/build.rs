use cargo_metadata::{CargoOpt, MetadataCommand};
use futures::channel::mpsc::{self as futures_mpsc, TrySendError};
use serde::{Deserialize, Serialize};
use sp_core::Hasher;
use sp_core::H256;
use sp_runtime::traits::BlakeTwo256;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tar::Archive;
use temp_dir::TempDir;
use tokio::fs::{read_dir, File};
use tokio::{io::AsyncReadExt, sync::mpsc::Receiver};

use crate::AppError;

/// A request to build a program
pub struct BuildRequest {
    request_type: BuildRequestType,
    responder: BuildResponder,
}

impl BuildRequest {
    /// A new build request with a git url
    pub fn new_git(url: String, responder: BuildResponder) -> Self {
        Self {
            request_type: BuildRequestType::Git { url },
            responder,
        }
    }

    /// A new build request with the contents of a tar archive
    pub fn new_tar(raw_archive: Vec<u8>, responder: BuildResponder) -> Self {
        Self {
            request_type: BuildRequestType::Tar { raw_archive },
            responder,
        }
    }
}

/// Input parameters for a build request
pub enum BuildRequestType {
    Git { url: String },
    Tar { raw_archive: Vec<u8> },
}

/// An item in the response stream for a program being built
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BuildResponse {
    /// A message from building on standard output
    StdOut(String),
    /// A message from building on standard error
    StdErr(String),
    /// The final message on a successful build, with the hash and binary blob
    Success {
        hash: H256,
        binary: Vec<u8>,
        binary_filename: String,
    },
}

/// For serializing and sending [BuildResponse]s to the client
#[derive(Debug, Clone)]
pub struct BuildResponder(pub futures_mpsc::Sender<Result<String, AppError>>);

impl BuildResponder {
    fn try_send(
        &mut self,
        build_response: BuildResponse,
    ) -> Result<(), TrySendError<Result<String, AppError>>> {
        self.0
            .try_send(serde_json::to_string(&build_response).map_err(|e| AppError::Json(e)))
    }

    fn try_send_error(&mut self, error: AppError) {
        if self.0.try_send(Err(error)).is_err() {
            log::error!("Client dropped connection while attempting to send error reponse");
        }
    }
}

pub async fn handle_build_requests(mut build_requests_rx: Receiver<BuildRequest>, db: sled::Db) {
    let program_builder = ProgramBuilder(db);
    while let Some(build_request) = build_requests_rx.recv().await {
        let mut responder = build_request.responder;
        match build_request.request_type {
            BuildRequestType::Git { url } => {
                if let Err(error) = program_builder
                    .add_program_git(url, responder.clone())
                    .await
                {
                    responder.try_send_error(error)
                }
            }
            BuildRequestType::Tar { raw_archive } => {
                if let Err(error) = program_builder
                    .add_program_tar(raw_archive, responder.clone())
                    .await
                {
                    responder.try_send_error(error)
                }
            }
        }
    }
}

struct ProgramBuilder(sled::Db);

impl ProgramBuilder {
    /// Add a program given as a location of a git repo
    pub async fn add_program_git(
        &self,
        git_url: String,
        response_tx: BuildResponder,
    ) -> Result<(), AppError> {
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

        self.add_program(temp_dir.path(), response_tx).await
    }

    /// Add a program given as a tar achive
    async fn add_program_tar(
        &self,
        input: Vec<u8>,
        response_tx: BuildResponder,
    ) -> Result<(), AppError> {
        let mut archive = Archive::new(&input[..]);
        let temp_dir = TempDir::new()?;
        archive.unpack(temp_dir.path())?;

        self.add_program(temp_dir.path(), response_tx).await
    }

    /// Build a program, and save metadata under the hash of its binary
    async fn add_program(
        &self,
        repo_path: &Path,
        mut response_tx: BuildResponder,
    ) -> Result<(), AppError> {
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
        let mut process = command
            .arg(format!("--output={}", binary_dir.display()))
            .arg(repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        // .stderr(Stdio::inherit())
        // .stdout(Stdio::inherit())
        // .output()?;

        let mut stdout = process.stdout.take().unwrap();
        let mut stderr = process.stderr.take().unwrap();
        loop {
            let mut buf: [u8; 10_000] = [0; 10_000];
            let read_bytes_stdout = stdout.read(&mut buf)?;
            if read_bytes_stdout > 0 {
                match std::str::from_utf8(&buf[..read_bytes_stdout]) {
                    Ok(output) => {
                        println!("{}", output);
                        if response_tx
                            .try_send(BuildResponse::StdOut(output.to_string()))
                            .is_err()
                        {
                            break;
                        };
                    }
                    Err(error) => log::error!("Bad UTF8 found on stdout {}", error),
                }
            };

            let read_bytes_stderr = stderr.read(&mut buf)?;
            if read_bytes_stderr > 0 {
                match std::str::from_utf8(&buf[..read_bytes_stderr]) {
                    Ok(output) => {
                        println!("{}", output);
                        if response_tx
                            .try_send(BuildResponse::StdErr(output.to_string()))
                            .is_err()
                        {
                            break;
                        };
                    }
                    Err(error) => log::error!("Bad UTF8 found on stderr {}", error),
                }
            };
            if read_bytes_stderr == 0 && read_bytes_stdout == 0 {
                break;
            }
        }
        if !process.wait().unwrap().success() {
            return Err(AppError::CompilationFailed("unknown".to_string()));
        }

        let binary_filename = get_binary_filename(binary_dir).await?;

        let binary_filename_string = binary_filename
            .file_name()
            .and_then(|o| o.to_str())
            .map(|o| o.to_string())
            .unwrap_or_else(|| "program.wasm".to_string());

        // Read the wasm binary
        let binary = {
            let mut file = File::open(binary_filename).await?;
            let mut binary = vec![];
            file.read_to_end(&mut binary).await?;
            binary
        };

        // Hash the binary
        // TODO #6 this wont let us hash chunks which means we need to read the whole binary into memory
        let hash = BlakeTwo256::hash(&binary);
        log::info!("Hashed binary {:?}", hash);

        // Write metadata to db
        let root_package_metadata_json = serde_json::to_string(&root_package_metadata)?;
        self.0.insert(hash, root_package_metadata_json.as_bytes())?;

        response_tx
            .try_send(BuildResponse::Success {
                hash,
                binary,
                binary_filename: binary_filename_string,
            })
            .unwrap();
        // TODO #7 Make the binary itself available
        Ok(())
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

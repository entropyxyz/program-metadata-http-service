//! A simple CLI HTTP client for the program-metadata-http-service
use clap::{Parser, Subcommand};
use futures::StreamExt;
use program_metadata_http_service::build::BuildResponse;
use std::{fs::File, io::Write};

#[derive(Parser, Debug, Clone)]
#[clap(version, about = "CLI tool for testing program-metadata-http-service")]
struct Cli {
    #[clap(subcommand)]
    command: CliCommand,
    /// The server to use - defaults to http://localhost:3000
    #[arg(short, long)]
    server_endpoint: Option<String>,
}

#[derive(Subcommand, Debug, Clone)]
enum CliCommand {
    /// Build a program
    Build {
        /// Url to a git repo containing the program to build
        git_url: String,
    },
    /// List hashes of all programs in the db
    List,
    /// Display metadata about a given program
    Program {
        /// Hex encoded hash of a program
        hash: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let endpoint_addr = cli.server_endpoint.unwrap_or_else(|| {
        std::env::var("PROGRAM_METADATA_SERVICE_ENDPOINT")
            .unwrap_or("http://localhost:3000".to_string())
    });

    match cli.command {
        CliCommand::Build { git_url } => {
            let client = reqwest::Client::new();
            let res = client
                .post(format!("{}/add-program-git", endpoint_addr))
                .body(git_url)
                .send()
                .await?;

            if res.status() == 200 {
                let mut bytes_stream = res.bytes_stream();
                let mut chunks = Vec::new();
                while let Some(Ok(chunk)) = bytes_stream.next().await {
                    // println!("chunk {}", String::from_utf8(chunk.to_vec()).unwrap());

                    chunks.extend_from_slice(&chunk);
                    let result_response: Result<BuildResponse, serde_json::Error> =
                        serde_json::from_slice(&chunks[..]);

                    if let Ok(response) = result_response {
                        chunks.clear();
                        match response {
                            BuildResponse::StdOut(output) => {
                                print!("{}", output);
                            }
                            BuildResponse::StdErr(output) => {
                                eprint!("{}", output);
                            }
                            BuildResponse::Success {
                                hash,
                                binary,
                                binary_filename,
                            } => {
                                println!("Success {:?}", hash);
                                let mut file = File::create(&binary_filename)?;
                                file.write_all(&binary)?;
                                println!("Writen {} bytes to {}", binary.len(), binary_filename);
                            }
                        }
                    }
                }
            } else {
                println!("Failed to build {}", res.text().await?);
            }
        }
        CliCommand::List => {
            let body = reqwest::get(format!("{}/programs", endpoint_addr))
                .await?
                .text()
                .await?;

            println!("{body}");
        }
        CliCommand::Program { hash } => {
            let body = reqwest::get(format!("{}/program/{}", endpoint_addr, hash))
                .await?
                .text()
                .await?;

            println!("{body}");
        }
    }
    Ok(())
}

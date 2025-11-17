use std::io::{self, Read};
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use crate::service::{OracleRequest, OracleService};

#[derive(Parser)]
#[command(name = "oracle", about = "Oracle MCP server and CLI helper")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run a one-off Oracle request from the command line
    Call(CallArgs),
    /// Start the Oracle MCP server over stdio (default)
    Serve,
}

#[derive(Args)]
pub struct CallArgs {
    /// Problem text passed inline
    #[arg(long, value_name = "TEXT", conflicts_with = "problem_file")]
    pub problem: Option<String>,

    /// Read problem text from a file (use '-' for stdin)
    #[arg(long = "problem-file", value_name = "PATH", conflicts_with = "problem")]
    pub problem_file: Option<PathBuf>,

    /// Extra context or notes
    #[arg(long = "extra", value_name = "TEXT")]
    pub extra_context: Option<String>,

    /// File paths to include as context (repeatable)
    #[arg(short = 'f', long = "file", value_name = "PATH")]
    pub files: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct CliError(String);

impl CliError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CliError {}

pub async fn run_cli_call(args: CallArgs) -> Result<(), Box<dyn std::error::Error>> {
    let CallArgs {
        problem,
        problem_file,
        extra_context,
        files,
    } = args;

    let problem_text = load_problem_text(problem, problem_file).await?;
    let files = if files.is_empty() {
        None
    } else {
        Some(
            files
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        )
    };

    let request = OracleRequest {
        problem: problem_text,
        files,
        extra_context,
    };

    let service = OracleService::new();
    let answer = service
        .call_openai(request)
        .await
        .map_err(|err| CliError::new(format!("Oracle encountered an error: {}", err.message)))?;

    println!("{answer}");
    Ok(())
}

async fn load_problem_text(
    inline: Option<String>,
    problem_file: Option<PathBuf>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(text) = inline {
        if text.trim().is_empty() {
            return Err(Box::new(CliError::new("Problem text cannot be empty")));
        }
        return Ok(text);
    }

    if let Some(path) = problem_file {
        if path == Path::new("-") {
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer)?;
            if buffer.trim().is_empty() {
                return Err(Box::new(CliError::new(
                    "Problem text read from stdin is empty",
                )));
            }
            return Ok(buffer);
        }

        let contents = tokio::fs::read_to_string(path).await?;
        if contents.trim().is_empty() {
            return Err(Box::new(CliError::new("Problem text file is empty")));
        }
        return Ok(contents);
    }

    Err(Box::new(CliError::new(
        "Provide --problem TEXT or --problem-file PATH (use '-' for stdin)",
    )))
}

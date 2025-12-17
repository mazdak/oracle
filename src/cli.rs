use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Deserialize;
use tempfile::NamedTempFile;

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
    /// Problem text passed inline (default: read from stdin)
    #[arg(long, value_name = "TEXT")]
    pub problem: Option<String>,

    /// File paths to include as context (repeatable)
    #[arg(short = 'f', long = "file", value_name = "PATH")]
    pub files: Vec<PathBuf>,

    /// Disable automatic file selection when no --file is provided
    #[arg(long = "no-auto-files", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub auto_files: bool,

    /// CLI to use for automatic file selection
    #[arg(long, value_enum)]
    pub selector: Option<SelectorCli>,

    /// Maximum number of files to select automatically
    #[arg(long, value_name = "N", default_value_t = 25)]
    pub max_files: usize,

    /// OpenAI model to use for solving (defaults to gpt-5.2-pro)
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Only print the selected file paths and exit (no solver call)
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum SelectorCli {
    Codex,
    Claude,
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
        files,
        auto_files,
        selector,
        max_files,
        model,
        dry_run,
    } = args;

    let problem_text = load_problem_text(problem)?;
    let mut files = if files.is_empty() {
        None
    } else {
        Some(
            files
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        )
    };

    if files.is_none() && auto_files && max_files > 0 {
        let repo_files = list_repo_files()?;
        let candidate_paths = rank_candidate_paths(&problem_text, &repo_files);
        let selection_prompt =
            build_file_selection_prompt(&problem_text, &candidate_paths, max_files);

        let raw_selection = match selector {
            Some(SelectorCli::Codex) => run_codex_selector(&selection_prompt)?,
            Some(SelectorCli::Claude) => run_claude_selector(&selection_prompt)?,
            None => match run_codex_selector(&selection_prompt) {
                Ok(output) => output,
                Err(err) if is_cmd_not_found_error(&err) => run_claude_selector(&selection_prompt)?,
                Err(err) => return Err(err),
            },
        };

        let selected_files = parse_file_selection(&raw_selection)?;
        let normalized = normalize_selected_files(selected_files, max_files)?;
        if !normalized.is_empty() {
            files = Some(normalized);
        }
    }

    if dry_run {
        if let Some(files) = files {
            for path in files {
                println!("{path}");
            }
        }
        return Ok(());
    }

    let request = OracleRequest {
        problem: problem_text,
        files,
        model,
    };

    let service = OracleService::new();
    let answer = service
        .call_openai(request)
        .await
        .map_err(|err| CliError::new(format!("Oracle encountered an error: {}", err.message)))?;

    println!("{answer}");
    Ok(())
}

fn load_problem_text(inline: Option<String>) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(text) = inline {
        if text.trim().is_empty() {
            return Err(Box::new(CliError::new("Problem text cannot be empty")));
        }
        return Ok(text);
    }

    let mut buffer = String::new();
    io::stdin().read_to_string(&mut buffer)?;
    if buffer.trim().is_empty() {
        return Err(Box::new(CliError::new(
            "Problem text read from stdin is empty",
        )));
    }

    Ok(buffer)
}

fn build_file_selection_prompt(problem: &str, candidates: &[String], max_files: usize) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are a codebase file-selector.\n");
    prompt.push_str("Your job is to choose which files are most relevant for solving the user's task.\n");
    prompt.push_str("You must not run commands, read file contents, propose edits, or provide explanations.\n\n");
    prompt.push_str("Output MUST be ONLY valid JSON matching this shape:\n");
    prompt.push_str("{\"files\":[\"path/to/file1\",\"path/to/file2\"]}\n\n");
    prompt.push_str(&format!(
        "Rules:\n- Choose at most {max_files} files.\n- Only choose from the candidate file paths listed below.\n- Use paths exactly as listed.\n- Do not include directories.\n- Do not wrap the JSON in Markdown (no ```).\n- Do not include any keys other than \"files\".\n- If none are needed, return {{\"files\":[]}}.\n\n",
    ));

    prompt.push_str("User task:\n");
    prompt.push_str(problem);
    prompt.push_str("\n\nCandidate file paths:\n");
    for path in candidates {
        prompt.push_str(path);
        prompt.push('\n');
    }
    prompt
}

fn list_repo_files() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    if let Ok(output) = ProcessCommand::new("git")
        .args(["ls-files", "-z"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if output.status.success() {
            let mut files: Vec<String> = output
                .stdout
                .split(|b| *b == b'\0')
                .filter(|chunk| !chunk.is_empty())
                .map(|chunk| String::from_utf8_lossy(chunk).to_string())
                .collect();
            files.sort();
            return Ok(files);
        }
    }

    let root = std::env::current_dir()?;
    let mut files = Vec::new();
    walk_repo(&root, &root, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk_repo(
    root: &std::path::Path,
    dir: &std::path::Path,
    files: &mut Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    const MAX_FILES: usize = 50_000;
    const SKIP_DIRS: [&str; 6] = [".git", "target", "node_modules", ".idea", ".vscode", ".venv"];

    if files.len() >= MAX_FILES {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();

        if file_type.is_dir() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if SKIP_DIRS.iter().any(|skip| skip == &name) {
                    continue;
                }
            }
            walk_repo(root, &path, files)?;
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(&path);
        files.push(relative.to_string_lossy().into_owned());
        if files.len() >= MAX_FILES {
            return Ok(());
        }
    }

    Ok(())
}

fn rank_candidate_paths(problem: &str, repo_files: &[String]) -> Vec<String> {
    const MAX_CANDIDATES: usize = 4000;
    let tokens = extract_query_tokens(problem);

    let mut scored: Vec<(u32, &String)> = repo_files
        .iter()
        .map(|path| (path_relevance_score(path, &tokens), path))
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));

    scored
        .into_iter()
        .take(MAX_CANDIDATES.min(repo_files.len()))
        .map(|(_, path)| path.clone())
        .collect()
}

fn extract_query_tokens(problem: &str) -> Vec<String> {
    const MAX_TOKENS: usize = 24;
    const STOP_WORDS: [&str; 16] = [
        "the", "and", "for", "with", "from", "that", "this", "into", "when", "then", "than",
        "what", "how", "why", "your", "you",
    ];

    let mut tokens = Vec::new();
    let mut current = String::new();

    let flush = |buf: &mut String, out: &mut Vec<String>| {
        if buf.is_empty() {
            return;
        }
        let token = buf.trim_matches(|c: char| c == '.' || c == '/' || c == '-').to_string();
        buf.clear();

        if token.len() < 3 {
            return;
        }
        let lower = token.to_ascii_lowercase();
        if STOP_WORDS.iter().any(|stop| stop == &lower) {
            return;
        }
        if out.iter().any(|existing| existing == &lower) {
            return;
        }
        out.push(lower);
    };

    for ch in problem.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/') {
            current.push(ch);
            if current.len() > 64 {
                flush(&mut current, &mut tokens);
            }
        } else {
            flush(&mut current, &mut tokens);
            if tokens.len() >= MAX_TOKENS {
                break;
            }
        }
    }
    flush(&mut current, &mut tokens);

    tokens
}

fn path_relevance_score(path: &str, tokens: &[String]) -> u32 {
    let path_lower = path.to_ascii_lowercase();
    let mut score = 0u32;

    if !path.contains(std::path::MAIN_SEPARATOR) && !path.contains('/') && !path.contains('\\') {
        score += 1;
    }

    for token in tokens {
        if !path_lower.contains(token) {
            continue;
        }
        score += (token.len() as u32).min(20);
        if path_lower.ends_with(token) {
            score += 10;
        }
        if token.contains('/') || token.contains('.') {
            score += 5;
        }
    }

    score
}

#[derive(Debug, Deserialize)]
struct FileSelection {
    files: Vec<String>,
}

fn parse_file_selection(raw: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Box::new(CliError::new(
            "Selector returned empty output; expected JSON",
        )));
    }

    let parsed: Result<FileSelection, _> = serde_json::from_str(trimmed);
    if let Ok(selection) = parsed {
        return Ok(selection.files);
    }

    let start = trimmed.find('{');
    let end = trimmed.rfind('}');
    if let (Some(start), Some(end)) = (start, end) {
        if end > start {
            let candidate = &trimmed[start..=end];
            if let Ok(selection) = serde_json::from_str::<FileSelection>(candidate) {
                return Ok(selection.files);
            }
        }
    }

    Err(Box::new(CliError::new(
        "Selector output was not valid JSON matching {\"files\":[...]}",
    )))
}

fn normalize_selected_files(
    selected: Vec<String>,
    max_files: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    if max_files == 0 || selected.is_empty() {
        return Ok(Vec::new());
    }

    let root = std::env::current_dir()?.canonicalize()?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for raw in selected {
        if out.len() >= max_files {
            break;
        }

        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }

        let input_path = PathBuf::from(raw);
        let resolved = if input_path.is_absolute() {
            input_path
        } else {
            root.join(&input_path)
        };

        let canonical = match resolved.canonicalize() {
            Ok(path) => path,
            Err(_) => continue,
        };

        if !canonical.starts_with(&root) || !canonical.is_file() {
            continue;
        }

        let relative = canonical.strip_prefix(&root).unwrap_or(&canonical);
        let relative = relative.to_string_lossy().into_owned();

        if seen.insert(relative.clone()) {
            out.push(relative);
        }
    }

    Ok(out)
}

fn run_codex_selector(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{"files":{"type":"array","items":{"type":"string"}}},"required":["files"]}"#;

    let mut schema_file = NamedTempFile::new()?;
    schema_file.write_all(schema.as_bytes())?;
    schema_file.flush()?;
    let schema_path = schema_file.into_temp_path();
    let schema_path_ref: &std::path::Path = schema_path.as_ref();

    let output_file = NamedTempFile::new()?;
    let output_path = output_file.into_temp_path();
    let output_path_ref: &std::path::Path = output_path.as_ref();

    let reasoning_effort = r#"model_reasoning_effort="medium""#;

    let mut child = ProcessCommand::new("codex")
        .args(["exec", "--sandbox", "read-only", "--output-schema"])
        .arg(schema_path_ref)
        .args(["--output-last-message"])
        .arg(output_path_ref)
        .args(["--model", "gpt-5.1-codex-max", "--config", reasoning_effort])
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| Box::new(err) as Box<dyn std::error::Error>)?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes())?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(Box::new(CliError::new(format!(
            "codex selector failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))));
    }

    Ok(std::fs::read_to_string(output_path_ref)?)
}

fn run_claude_selector(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    let schema = r#"{"type":"object","additionalProperties":false,"properties":{"files":{"type":"array","items":{"type":"string"}}},"required":["files"]}"#;

    let preferred_model = "claude-opus-4-5-20251101";
    match run_claude_selector_with_model(prompt, schema, preferred_model) {
        Ok(output) => Ok(output),
        Err(err) if err.contains("not_found_error") && err.contains("model:") => {
            run_claude_selector_with_model(prompt, schema, "opus").map_err(|err| {
                Box::new(CliError::new(err)) as Box<dyn std::error::Error>
            })
        }
        Err(err) => Err(Box::new(CliError::new(err))),
    }
}

fn run_claude_selector_with_model(
    prompt: &str,
    schema: &str,
    model: &str,
) -> Result<String, String> {
    let mut child = ProcessCommand::new("claude")
        .args([
            "-p",
            "--output-format",
            "json",
            "--json-schema",
            schema,
            "--model",
            model,
            "--tools",
            "",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).map_err(|err| err.to_string())?;
    }

    let output = child.wait_with_output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if !stdout.is_empty() && !stderr.is_empty() {
            return Err(format!("claude selector failed (stdout): {stdout}\n(claude stderr): {stderr}"));
        }
        if !stdout.is_empty() {
            return Err(format!("claude selector failed: {stdout}"));
        }
        if !stderr.is_empty() {
            return Err(format!("claude selector failed: {stderr}"));
        }
        return Err("claude selector failed with no output".to_string());
    }

    let raw = String::from_utf8_lossy(&output.stdout).to_string();
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|err| format!("{err}. Raw output: {raw}"))?;

    if let Some(structured) = value.get("structured_output") {
        return Ok(structured.to_string());
    }

    if let Some(result) = value.get("result").and_then(|v| v.as_str()) {
        let trimmed = result.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    Err("claude selector returned no structured_output or result".to_string())
}

fn is_cmd_not_found_error(err: &Box<dyn std::error::Error>) -> bool {
    let Some(io_err) = err.downcast_ref::<std::io::Error>() else {
        return false;
    };
    io_err.kind() == std::io::ErrorKind::NotFound
}

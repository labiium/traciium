//! traciium — visualize and debug agentic traces from .jsonl files.
//!
//! Usage: `traciium format.yaml [dir-or-file]` for the terminal viewer, or
//! `traciium --web format.yaml [dir-or-file]` for the web viewer.

mod format;
mod profile;
mod server;
mod trace;
mod tui;

use clap::{Args as ClapArgs, Parser, Subcommand};
use format::FormatSpec;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use trace::TraceFile;

#[derive(Parser, Debug)]
#[command(
    name = "traciium",
    version,
    about = "Visualize and debug agentic traces from .jsonl files"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Path to the format.yaml styling/presentation config.
    format: Option<PathBuf>,
    /// A .jsonl file, or a directory to scan recursively for .jsonl files.
    path: Option<PathBuf>,
    /// Host to bind the web server on.
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    /// Port to bind the web server on.
    #[arg(long, default_value_t = 8787)]
    port: u16,
    /// Open the viewer in the default browser after startup.
    #[arg(long)]
    open: bool,
    /// Start the web viewer instead of the terminal viewer.
    #[arg(long)]
    web: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Generate a starter format.yaml from one or more JSONL traces.
    Init(InitArgs),
    /// Profile a trace and show fields, event types, and format-key suggestions.
    Inspect(InspectArgs),
    /// Print format and agent workflow guidance.
    Guide {
        #[command(subcommand)]
        topic: Option<GuideTopic>,
    },
}

#[derive(Subcommand, Debug)]
enum GuideTopic {
    /// Explain the recommended agent workflow in plain prose.
    Info,
    /// Print the format.yaml reference.
    Format,
    /// Print a copy/paste prompt for an agent configuring traciium.
    Prompt,
}

#[derive(ClapArgs, Debug)]
struct InitArgs {
    /// A .jsonl file, or a directory to scan recursively.
    path: PathBuf,
    /// Write the generated YAML here; use '-' for stdout.
    #[arg(short, long, default_value = "-")]
    output: PathBuf,
    /// Replace an existing output file.
    #[arg(long)]
    force: bool,
}

#[derive(ClapArgs, Debug)]
struct InspectArgs {
    /// A .jsonl file, or a directory to scan recursively.
    path: PathBuf,
    /// Emit stable machine-readable JSON instead of the human report.
    #[arg(long)]
    json: bool,
}

pub(crate) fn collect_jsonl(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    if path.is_file() {
        files.push(path.to_path_buf());
    } else if path.is_dir() {
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let mut entries: Vec<_> = std::fs::read_dir(&dir)
                .map_err(|e| e.to_string())?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().map(|e| e == "jsonl").unwrap_or(false) {
                    files.push(p);
                }
            }
        }
    } else {
        return Err(format!("path not found: {}", path.display()));
    }
    files.sort();
    Ok(files)
}

fn run_command(command: Command) -> Result<(), String> {
    match command {
        Command::Guide { topic } => {
            match topic {
                None | Some(GuideTopic::Format) => print!("{FORMAT_GUIDE}"),
                Some(GuideTopic::Info) => print!("{AGENT_INFO}"),
                Some(GuideTopic::Prompt) => print!("{AGENT_PROMPT}"),
            }
            Ok(())
        }
        Command::Inspect(args) => {
            let profile = inspect_path(&args.path)?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?
                );
            } else {
                print_human_profile(&profile);
            }
            Ok(())
        }
        Command::Init(args) => {
            let profile = inspect_path(&args.path)?;
            let yaml = profile::starter_format(&profile);
            if args.output.as_os_str() == "-" {
                print!("{yaml}");
            } else {
                if args.output.exists() && !args.force {
                    return Err(format!(
                        "refusing to overwrite {}; use --force",
                        args.output.display()
                    ));
                }
                std::fs::write(&args.output, yaml)
                    .map_err(|e| format!("cannot write {}: {e}", args.output.display()))?;
                println!(
                    "wrote {} ({} JSON objects, {} event types)",
                    args.output.display(),
                    profile.objects,
                    profile.event_types(&profile.type_field()).len()
                );
            }
            Ok(())
        }
    }
}

fn inspect_path(path: &Path) -> Result<profile::Profile, String> {
    let paths = collect_jsonl(path)?;
    if paths.is_empty() {
        return Err(format!("no .jsonl files found under {}", path.display()));
    }
    let profile = profile::Profile::scan(&paths);
    if profile.objects == 0 {
        return Err(format!(
            "no JSON objects found under {} ({} invalid/non-object records)",
            path.display(),
            profile.invalid_lines
        ));
    }
    Ok(profile)
}

fn print_human_profile(profile: &profile::Profile) {
    let type_field = profile.type_field();
    let timestamp_field = profile.timestamp_field();
    println!("Trace profile");
    println!(
        "  files       {}/{} scanned",
        profile.files_scanned,
        profile.files_scanned + profile.files_skipped
    );
    println!(
        "  records     {} ({} JSON objects, {} invalid/non-object)",
        profile.records, profile.objects, profile.invalid_lines
    );
    println!("  suggested   type_field={type_field}  timestamp_field={timestamp_field}");
    if let Some(field) = profile.optional_field(&["sequence", "seq", "index", "event_index"]) {
        println!("              sequence_field={field}");
    }
    if let Some(field) = profile.optional_field(&["run_id", "run", "trace_id", "session_id"]) {
        println!("              run_field={field}");
    }

    let types = profile.event_types(&type_field);
    println!("\nEvent types ({})", types.len());
    if types.is_empty() {
        println!("  (none found in suggested type field; inspect fields below)");
    } else {
        for (kind, count) in types.iter().take(40) {
            println!("  {kind:<28} {count}");
        }
        if types.len() > 40 {
            println!("  ... and {} more", types.len() - 40);
        }
    }

    println!("\nFields");
    let mut fields: Vec<_> = profile.fields.iter().collect();
    fields.sort_by(|(a, left), (b, right)| right.present.cmp(&left.present).then_with(|| a.cmp(b)));
    for (name, summary) in fields {
        let kinds = summary
            .kinds
            .iter()
            .map(|(kind, count)| format!("{kind}:{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let examples = if summary.examples.is_empty() {
            String::new()
        } else {
            format!("  e.g. {}", summary.examples.join(" | "))
        };
        println!("  {name:<28} {:>6}  {kinds}{examples}", summary.present);
    }
}

const AGENT_INFO: &str = r#"traciium agent guide

Your job is to make a format.yaml that tells traciium how to read a JSONL trace.
Do not guess the schema. Discover it from the data, generate a starting point,
then make only the presentation choices that help a human or another agent
understand the run.

Recommended workflow:

  1. Discover the data:
       traciium inspect /path/to/traces --json
     This reports JSON object counts, invalid records, field names, value types,
     examples, event values, and suggested trace keys. Treat it as the source
     of truth for the schema.

  2. Generate a valid starting config:
       traciium init /path/to/traces --output format.yaml
     The generated YAML selects likely type, timestamp, sequence, and run keys.
     It is intentionally conservative: fields not listed in `events` still
     appear in the viewer.

  3. Customize rendering, not the trace data:
     - Set `trace.type_field` to the key whose value names the event kind.
     - Set `trace.timestamp_field` to a seconds, milliseconds, or ISO timestamp.
     - Set `trace.run_field` only when records should be grouped into runs.
     - Add `events.<event-type>.body` mappings for important fields.
     - Use `markdown` for prose, `code` for source, `output` for command output,
       `tool_args` for tool arguments, `messages` for chat arrays, `kv` for
       objects, and `json` when the complete structure matters.
     - Add `table.columns` for a compact tabular view.

  4. Read the result:
       traciium format.yaml /path/to/traces
     Use the TUI by default. Add `--web` for the browser viewer.

Important details:

  - The input is newline-delimited JSON: one JSON object per line.
  - Format files are YAML configuration; they do not transform or rewrite data.
  - Dot paths such as `metadata.type` and `usage.total` are supported.
  - Unknown event types and unlisted fields use safe generic rendering.
  - Use `traciium guide format` for every supported YAML key and renderer.
  - Use `traciium inspect /path/to/traces --json` when producing config as an
    agent: it is more reliable than reading a human-formatted report.

A good agent should preserve the user's field names, avoid hiding fields unless
there is a clear reason, and explain any inferred key that it changes.
"#;

const AGENT_PROMPT: &str = r#"You are configuring traciium for a JSONL trace.

Follow this workflow exactly:

1. Run:
     traciium inspect <trace-file-or-directory> --json
2. Use that JSON as the schema source. Identify the event discriminator,
   timestamp, optional sequence, and optional run/group field. Do not invent
   field names when an observed field is available.
3. Run:
     traciium init <trace-file-or-directory> --output format.yaml
4. Edit only the generated YAML presentation rules. Keep the `trace` keys
   aligned with the inspected data. Add event-specific `body` renderers for
   the fields a reader needs to understand. Prefer `markdown` for prose,
   `code` for source, `output` for command output, `tool_args` for tool input,
   `messages` for chat messages, `kv` for metadata, and `json` for full data.
5. Check the result with:
     traciium format.yaml <trace-file-or-directory>

Return a valid traciium format.yaml, not a new trace schema. Use only keys from
`traciium guide format`. Keep unlisted fields visible unless they are known
noise. If the trace is ambiguous, state the ambiguity and choose the key
supported by the inspect results.
"#;

const FORMAT_GUIDE: &str = r#"traciium format guide

A format file describes how to read and present JSONL records. Start with:

  traciium init traces/ --output format.yaml
  traciium inspect traces/ --json
  traciium format.yaml traces/

Minimal format:

  trace:
    type_field: t             # event discriminator, required in practice
    timestamp_field: ts       # unix seconds, unix milliseconds, or ISO-8601
    sequence_field: sequence  # optional tie-break ordering field
    run_field: run_id         # optional; groups records into runs

The type and timestamp fields may be dot paths, such as metadata.kind. If a
record has no run field, all records in a file are shown as one run.

Per-event presentation:

  events:
    tool_call:
      label: Tool call
      icon: "▶"
      tone: sky
      open: false
      title_from: name
      badges: [duration_ms]
      duration_field: duration_ms
      body:
        arguments: tool_args
    assistant:
      label: Assistant
      tone: emerald
      body:
        content: markdown

Supported body renderers:
  markdown       prose, headings, lists, links, and fenced code
  code           syntax-highlighted source/configuration
  output         wrapped plain-text command output
  text           plain text with value styles
  json           pretty-printed highlighted JSON
  kv             object as key/value rows
  tokens         usage object as token badges
  messages       chat messages with role labels
  tool_args      primary tool payload plus key/value metadata
  tool_calls     full tool-call cards
  tool_calls_brief one-line tool-call summaries
  list           array values as chips

Shared defaults and table configuration:

  defaults:
    ignore: [schema_version, event_id]
    open: false
    max_text: 20000
    header_fields: [origin]
  table:
    columns: [ts, t, name]
    labels: {ts: time}
    max_cell: 140

Run outcome and conditional styling:

  outcome:
    when: {t: terminal}
    field: success
  conditional_styles:
    - when: {t: terminal, success: false}
      style: {tone: red, weight: bold}

Unknown event types and unlisted fields still render using safe defaults. Use
`traciium inspect <path> --json` when an agent needs to discover field names,
types, examples, and event counts before writing a specialized format file.
"#;

fn main() {
    let args = Args::parse();

    if let Some(command) = args.command {
        if let Err(error) = run_command(command) {
            eprintln!("traciium: {error}");
            std::process::exit(2);
        }
        return;
    }

    let format = args.format.unwrap_or_else(|| {
        eprintln!(
            "traciium: missing format.yaml (try `traciium guide` or `traciium init <traces>`)"
        );
        std::process::exit(2);
    });
    let path = args.path.unwrap_or_else(|| {
        eprintln!("traciium: missing trace path (try `traciium --help`)");
        std::process::exit(2);
    });

    // Load format config.
    let spec: FormatSpec = match std::fs::read_to_string(&format) {
        Ok(text) => serde_yaml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("traciium: invalid format yaml {}: {e}", format.display());
            std::process::exit(2);
        }),
        Err(e) => {
            eprintln!("traciium: cannot read {}: {e}", format.display());
            std::process::exit(2);
        }
    };

    // Discover .jsonl files.
    let paths = match collect_jsonl(&path) {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => {
            eprintln!("traciium: no .jsonl files found under {}", path.display());
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("traciium: {e}");
            std::process::exit(2);
        }
    };

    // Parse every file up front; skip unreadable ones with a warning.
    let mut files: Vec<TraceFile> = Vec::new();
    for p in &paths {
        match trace::load_file(&p.to_string_lossy(), files.len(), &spec) {
            Ok(mut f) => {
                if f.runs.iter().all(|r| r.stats.events == 0) {
                    eprintln!("traciium: skipping empty file {}", p.display());
                    continue;
                }
                f.runs.retain(|r| r.stats.events > 0);
                println!(
                    "  loaded {} ({} run{}, {} events)",
                    p.display(),
                    f.runs.len(),
                    if f.runs.len() == 1 { "" } else { "s" },
                    f.runs.iter().map(|r| r.stats.events).sum::<usize>(),
                );
                files.push(f);
            }
            Err(e) => eprintln!("traciium: skipping {}: {e}", p.display()),
        }
    }
    if files.is_empty() {
        eprintln!("traciium: no parseable .jsonl events found");
        std::process::exit(2);
    }

    if !args.web {
        if let Err(e) = tui::run(&spec, files, &format, &path) {
            eprintln!("traciium: tui: {e}");
            std::process::exit(1);
        }
        return;
    }

    let addr = format!("{}:{}", args.host, args.port);
    let state = Arc::new(server::AppState {
        spec,
        files: RwLock::new(files),
        format_path: format.display().to_string(),
        source_path: path.clone(),
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    println!(
        "traciium: {} file(s) loaded",
        state.files.read().unwrap().len()
    );
    println!("  format  -> {}", format.display());
    println!("  serving -> http://{addr}/");
    if args.open {
        let url = format!("http://{addr}/");
        let _ = std::process::Command::new("xdg-open")
            .arg(&url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    let listener = runtime
        .block_on(tokio::net::TcpListener::bind(&addr))
        .unwrap_or_else(|e| {
            eprintln!("traciium: cannot bind {addr}: {e}");
            std::process::exit(1);
        });
    runtime.block_on(async move {
        axum::serve(listener, server::router(state)).await.unwrap();
    });
}

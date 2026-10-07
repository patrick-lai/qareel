pub mod args;
pub mod browser;
pub mod client;
pub mod demo;
pub mod engine;
pub mod failure;
pub mod guide;
pub mod host;
pub mod paths;
pub mod record;
pub mod reel;
pub mod script;
pub mod serve;

use failure::{Failure, describe, fixable};
use serve::{Request, Response};
use std::io::Read;

const LOCAL: [&str; 9] = ["guide", "init", "serve", "stop", "doctor", "install", "reel", "help", "version"];

fn print_failure(failure: &Failure) {
    eprintln!("error[{}]: {}", failure.code, failure.message);
    if let Some(fix) = &failure.fix {
        eprintln!("fix: {fix}");
    }
}

fn read_input(params: &mut Vec<String>) -> anyhow::Result<Option<String>> {
    let Some(position) = params.iter().position(|param| param == "--file" || param.starts_with("--file=")) else { return Ok(None) };
    let flag = params.remove(position);
    let source = match flag.strip_prefix("--file=") {
        Some(source) => source.to_owned(),
        None if position < params.len() => params.remove(position),
        None => return Err(fixable("args.invalid", "--file needs a path or - for stdin", "qareel demo plan --file plan.json")),
    };
    let mut text = String::new();
    if source == "-" {
        std::io::stdin().take(serve::MAX_REQUEST_BYTES).read_to_string(&mut text)?;
    } else {
        text = std::fs::read_to_string(&source).map_err(|error| fixable("args.file", format!("cannot read {source}: {error}"), "pass an existing file, or - to read stdin"))?;
    }
    Ok(Some(text))
}

async fn remote(command: &str, mut params: Vec<String>) -> anyhow::Result<i32> {
    let input = read_input(&mut params)?;
    let request = Request { version: env!("CARGO_PKG_VERSION").to_owned(), command: command.to_owned(), params, input, cwd: std::env::current_dir()? };
    match client::send(&request, true).await? {
        Some(Response::Ok { text }) => {
            println!("{text}");
            Ok(0)
        }
        Some(Response::Error { code, message, fix }) => {
            print_failure(&Failure { code, message, fix });
            Ok(1)
        }
        None => Err(failure::fail("serve.unavailable", "the qareel session is not running")),
    }
}

async fn stop() -> anyhow::Result<i32> {
    let request = Request { version: env!("CARGO_PKG_VERSION").to_owned(), command: "shutdown".to_owned(), params: Vec::new(), input: None, cwd: std::env::current_dir()? };
    match client::send(&request, false).await? {
        Some(Response::Ok { text }) => println!("{text}"),
        Some(Response::Error { code, message, fix }) => {
            print_failure(&Failure { code, message, fix });
            return Ok(1);
        }
        None => println!("No qareel session is running."),
    }
    Ok(0)
}

async fn local(command: &str, params: &[String]) -> anyhow::Result<i32> {
    match command {
        "guide" => {
            print!("{}", guide::GUIDE);
            Ok(0)
        }
        "help" => {
            match params.first() {
                Some(topic) => print!("{}", guide::command_help(topic)),
                None => print!("{}", guide::OVERVIEW),
            }
            Ok(0)
        }
        "version" if params.iter().any(|param| param == "--json") => {
            println!("{}", serde_json::json!({"version": env!("CARGO_PKG_VERSION"), "protocol": 1, "linux_image": engine::default_image()}));
            Ok(0)
        }
        "version" => {
            println!("qareel {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "init" => guide::init(params),
        "serve" => {
            serve::run().await?;
            Ok(0)
        }
        "stop" => stop().await,
        "doctor" => guide::doctor(false).await,
        "install" => guide::doctor(true).await,
        "reel" => reel::passthrough(&paths::Layout::current()?, params).await,
        _ => Ok(2),
    }
}

pub async fn main(arguments: Vec<String>) -> i32 {
    let Some((command, params)) = arguments.split_first() else {
        print!("{}", guide::OVERVIEW);
        return 0;
    };
    let command = match command.as_str() {
        "-h" | "--help" => "help",
        "-V" | "--version" => "version",
        "navigate" | "goto" => "open",
        "recording" => "record",
        "evaluate" => "eval",
        "key" => "press",
        "wait_for" | "wait-for" => "wait",
        "fill_form" | "fill-form" => "fill",
        other => other,
    };
    if params.iter().any(|param| param == "--help" || param == "-h") {
        print!("{}", guide::command_help(command));
        return 0;
    }
    let known = LOCAL.contains(&command) || matches!(command, "record" | "demo") || browser::spec(command).is_some();
    if !known {
        print_failure(&Failure::new("args.unknown_command", format!("`{command}` is not a qareel command")).with_fix("run `qareel --help`, or `qareel guide` for the QA demo workflow"));
        return 2;
    }
    let outcome = if LOCAL.contains(&command) { local(command, params).await } else { remote(command, params.to_vec()).await };
    match outcome {
        Ok(code) => code,
        Err(error) => {
            print_failure(&describe(&error));
            1
        }
    }
}

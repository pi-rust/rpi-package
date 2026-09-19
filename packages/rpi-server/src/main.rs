mod server;

use server::{generate_token, launch_args, start_rpc_server};
use serde_json::{json, Value};
use std::sync::Arc;

fn print_help() {
    println!("rpi-server - JSON-RPC 2.0 server with WebSocket transport and streaming");
    println!();
    println!("USAGE:");
    println!("    rpi-server [OPTIONS] [-- <RPI_ARGS>]");
    println!();
    println!("OPTIONS:");
    println!("    --bind <ADDR>     Bind address (default: 127.0.0.1)");
    println!("    --port <PORT>     Bind port (default: 9800)");
    println!("    --token <TOKEN>   Auth token (default: auto-generated)");
    println!("    --help, -h        Show this help message");
    println!();
    println!("RPI_ARGS:");
    println!("    All arguments after -- are passed to rpi child processes.");
    println!("    Example: -- --provider anthropic --model claude-3-sonnet");
    println!();
    println!("EXAMPLES:");
    println!("    rpi-server");
    println!("    rpi-server --port 8080");
    println!("    rpi-server -- --provider anthropic --model claude-3-sonnet");
    println!();
    println!("PROTOCOL:");
    println!("    JSON-RPC 2.0 over WebSocket");
    println!("    Connect: ws://<addr>:<port>");
    println!();
    println!("METHODS:");
    println!("    start_session  - Spawn a new rpi session");
    println!("    send           - Send command to session");
    println!("    stop_session   - Stop a session");
    println!("    list_sessions  - List all sessions");
    println!("    server_status  - Get server status");
    println!();
    println!("SUBSCRIPTIONS:");
    println!("    subscribe      - Stream events from a session");
}

fn parse_args() -> Result<(String, u16, String, Value), String> {
    let args: Vec<String> = std::env::args().collect();
    
    let mut bind = "127.0.0.1".to_string();
    let mut port = 9800u16;
    let mut token: Option<String> = None;
    let mut rpi_args_start = None;
    
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--bind" => {
                i += 1;
                if i >= args.len() {
                    return Err("--bind requires a value".into());
                }
                bind = args[i].clone();
            }
            "--port" => {
                i += 1;
                if i >= args.len() {
                    return Err("--port requires a value".into());
                }
                port = args[i].parse().map_err(|_| "invalid port number")?;
            }
            "--token" => {
                i += 1;
                if i >= args.len() {
                    return Err("--token requires a value".into());
                }
                token = Some(args[i].clone());
            }
            "--" => {
                rpi_args_start = Some(i + 1);
                break;
            }
            _ => {
                return Err(format!("unknown option: {}", args[i]));
            }
        }
        i += 1;
    }
    
    let token = token.unwrap_or_else(generate_token);
    
    // Parse rpi arguments
    let mut params = json!({});
    if let Some(start) = rpi_args_start {
        let rpi_args = &args[start..];
        let mut j = 0;
        while j < rpi_args.len() {
            match rpi_args[j].as_str() {
                "--provider" => {
                    j += 1;
                    if j >= rpi_args.len() {
                        return Err("--provider requires a value".into());
                    }
                    params["provider"] = json!(rpi_args[j]);
                }
                "--model" => {
                    j += 1;
                    if j >= rpi_args.len() {
                        return Err("--model requires a value".into());
                    }
                    params["model"] = json!(rpi_args[j]);
                }
                "--thinking" => {
                    j += 1;
                    if j >= rpi_args.len() {
                        return Err("--thinking requires a value".into());
                    }
                    params["thinking"] = json!(rpi_args[j]);
                }
                "--system-prompt" => {
                    j += 1;
                    if j >= rpi_args.len() {
                        return Err("--system-prompt requires a value".into());
                    }
                    params["systemPrompt"] = json!(rpi_args[j]);
                }
                "--no-session" => {
                    params["noSession"] = json!(true);
                }
                "--tools" => {
                    j += 1;
                    if j >= rpi_args.len() {
                        return Err("--tools requires a value".into());
                    }
                    let tools: Vec<String> = rpi_args[j].split(',').map(|s| s.to_string()).collect();
                    params["tools"] = json!(tools);
                }
                _ => {
                    return Err(format!("unknown rpi argument: {}", rpi_args[j]));
                }
            }
            j += 1;
        }
    }
    
    Ok((bind, port, token, params))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let (bind, port, token, params) = parse_args()?;
    
    let (executable, args) = launch_args(&params)?;
    
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .map_err(|e| format!("failed to create runtime: {e}"))?;

    let server = runtime.block_on(async {
        start_rpc_server(&bind, port, token, executable, args).await
    })?;

    println!("press Ctrl+C to stop");

    // Handle Ctrl+C
    let server_clone = Arc::clone(&server);
    ctrlc::set_handler(move || {
        println!("\nstopping server...");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _ = runtime.block_on(server_clone.stop());
        std::process::exit(0);
    }).map_err(|e| format!("failed to set Ctrl+C handler: {e}"))?;

    // Keep main thread alive
    runtime.block_on(async {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
        }
    });

    Ok(())
}

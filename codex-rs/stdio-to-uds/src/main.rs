use std::env;
use std::path::PathBuf;
use std::process;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = env::args_os().skip(1);
    let first = args.next();
    let json_rpc = first.as_deref() == Some(std::ffi::OsStr::new("--json-rpc"));
    let Some(socket_path) = (if json_rpc { args.next() } else { first }) else {
        eprintln!("Usage: codex-stdio-to-uds [--json-rpc] <socket-path>");
        process::exit(1);
    };

    if args.next().is_some() {
        eprintln!("Expected exactly one argument: <socket-path>");
        process::exit(1);
    }

    let socket_path = PathBuf::from(socket_path);
    if json_rpc {
        codex_stdio_to_uds::run_json_rpc(&socket_path).await
    } else {
        codex_stdio_to_uds::run(&socket_path).await
    }
}

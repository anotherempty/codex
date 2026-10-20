//! Explicit local shared-server selection. Never discover or fall back implicitly.

use super::*;
use codex_app_server_client::RemoteAppServerClient;
use codex_app_server_client::RemoteAppServerConnectArgs;
use codex_app_server_client::RemoteAppServerEndpoint;

pub(super) enum ClientStart {
    Embedded(Box<InProcessClientStartArgs>),
    Shared(PathBuf),
}

impl ClientStart {
    pub(super) async fn connect(self) -> anyhow::Result<AppServerClient> {
        match self {
            Self::Embedded(args) => Ok(AppServerClient::InProcess(
                InProcessAppServerClient::start(*args).await?,
            )),
            Self::Shared(socket_path) => Ok(AppServerClient::Remote(
                RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
                    endpoint: RemoteAppServerEndpoint::UnixSocket {
                        socket_path: AbsolutePathBuf::try_from(socket_path)?,
                    },
                    client_name: "codex_exec".to_string(),
                    client_version: env!("CARGO_PKG_VERSION").to_string(),
                    experimental_api: true,
                    mcp_server_openai_form_elicitation: false,
                    opt_out_notification_methods: Vec::new(),
                    channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
                })
                .await?,
            )),
        }
    }
}

pub(super) fn validate(cli: &Cli) -> anyhow::Result<()> {
    validate_options(cli)?;
    let Some(_) = &cli.app_server_socket else {
        return Ok(());
    };
    anyhow::ensure!(
        !is_workload_identity_selected(),
        "shared-server execution cannot use caller workload identity"
    );
    for key in [
        "CODEX_EXEC_SERVER_URL",
        "CODEX_API_KEY",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
    ] {
        anyhow::ensure!(
            std::env::var_os(key).is_none(),
            "--app-server-socket uses server credentials and executor; unset {key} for this invocation"
        );
    }
    Ok(())
}

fn validate_options(cli: &Cli) -> anyhow::Result<()> {
    let Some(socket) = &cli.app_server_socket else {
        return Ok(());
    };
    anyhow::ensure!(
        socket.is_absolute(),
        "--app-server-socket requires an absolute path"
    );
    anyhow::ensure!(
        cli.config_overrides.raw_overrides.is_empty()
            && cli.config_profile_v2.is_none()
            && !cli.ignore_user_config
            && !cli.ignore_rules
            && !cli.strict_config
            && !cli.worktree
            && !cli.oss
            && cli.oss_provider.is_none()
            && !cli.auto_review,
        "--app-server-socket uses server launch configuration; --config, --profile, --ignore-user-config, --ignore-rules, --strict-config, --worktree, --oss and --approve-for-me are unsupported"
    );
    match &cli.command {
        None => {}
        Some(ExecCommand::Resume(args)) => {
            anyhow::ensure!(
                !args.last
                    && args
                        .session_id
                        .as_deref()
                        .is_some_and(|id| Uuid::parse_str(id).is_ok()),
                "shared-server resume requires an explicit thread UUID; names and --last are unsupported"
            );
        }
        _ => anyhow::bail!("--app-server-socket currently supports new turns and resume only"),
    }
    Ok(())
}

pub(super) fn interrupt_receiver(shared: bool) -> anyhow::Result<mpsc::UnboundedReceiver<()>> {
    let (tx, rx) = mpsc::unbounded_channel();
    #[cfg(unix)]
    let mut terminate = if shared {
        Some(tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        )?)
    } else {
        None
    };
    #[cfg(not(unix))]
    let _ = shared;
    tokio::spawn(async move {
        #[cfg(unix)]
        let interrupted = if let Some(ref mut terminate) = terminate {
            tokio::select! {
                result = tokio::signal::ctrl_c() => result.is_ok(),
                signal = terminate.recv() => signal.is_some(),
            }
        } else {
            tokio::signal::ctrl_c().await.is_ok()
        };
        #[cfg(not(unix))]
        let interrupted = tokio::signal::ctrl_c().await.is_ok();
        if interrupted {
            let _ = tx.send(());
        }
    });
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn shared_backend_accepts_new_turn_and_explicit_resume() {
        let socket = std::env::temp_dir().join("codex-shared-test.sock");
        let socket = socket.to_str().unwrap();
        for suffix in [
            vec!["prompt"],
            vec!["resume", "12345678-1234-1234-1234-123456789abc", "prompt"],
        ] {
            let mut args = vec!["codex-exec", "--app-server-socket", socket];
            args.extend(suffix);
            assert!(validate_options(&Cli::parse_from(args)).is_ok());
        }
    }

    #[test]
    fn shared_backend_rejects_local_only_launch_options() {
        let socket = std::env::temp_dir().join("codex-shared-test.sock");
        let socket = socket.to_str().unwrap();
        for option in [
            "--ignore-user-config",
            "--ignore-rules",
            "--strict-config",
            "--worktree",
            "--oss",
            "--approve-for-me",
        ] {
            let cli = Cli::parse_from([
                "codex-exec",
                "--app-server-socket",
                socket,
                option,
                "prompt",
            ]);
            assert!(validate_options(&cli).is_err(), "{option}");
        }
        let mut cli = Cli::parse_from(["codex-exec", "--app-server-socket", socket, "prompt"]);
        cli.config_overrides
            .raw_overrides
            .push("model=other".to_string());
        assert!(validate_options(&cli).is_err());
    }

    #[test]
    fn shared_backend_rejects_ambiguous_resume_and_relative_socket() {
        let socket = std::env::temp_dir().join("codex-shared-test.sock");
        let socket = socket.to_str().unwrap();
        for args in [
            vec![
                "codex-exec",
                "--app-server-socket",
                "relative.sock",
                "prompt",
            ],
            vec![
                "codex-exec",
                "--app-server-socket",
                socket,
                "resume",
                "--last",
                "prompt",
            ],
            vec![
                "codex-exec",
                "--app-server-socket",
                socket,
                "resume",
                "named-thread",
                "prompt",
            ],
        ] {
            assert!(validate_options(&Cli::parse_from(args)).is_err());
        }
    }

    #[test]
    fn default_backend_keeps_local_launch_options() {
        let mut cli = Cli::parse_from(["codex-exec", "--ignore-user-config", "prompt"]);
        cli.config_overrides
            .raw_overrides
            .push("model=other".to_string());
        assert!(validate_options(&cli).is_ok());
    }
}

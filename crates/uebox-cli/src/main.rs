use std::{
    io::{self, Write},
    process::ExitCode,
};

use uebox_cli::run;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let argv = std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string())
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|_| vec!["--invalid-argument-encoding".to_owned()]);
    let cwd = std::env::current_dir().unwrap_or_default();
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let connector = uebox_cli::unix::UnixConnector;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let connector = uebox_cli::UnavailableConnector;
    let result = run(&argv, &cwd, &connector).await;
    let _ = io::stderr().lock().write_all(result.stderr.as_bytes());
    if io::stdout()
        .lock()
        .write_all(result.stdout.as_bytes())
        .is_err()
    {
        return ExitCode::from(8);
    }
    ExitCode::from(result.exit_code)
}

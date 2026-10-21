#[cfg(unix)]
#[tokio::main(worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    use clap::Parser;
    match treeharbor_service::cli::run(treeharbor_service::cli::Cli::parse()).await {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(error) => {
            // Display is deliberately bounded; Debug/source can contain secrets.
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("this development transport requires Linux or macOS");
    std::process::ExitCode::FAILURE
}

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match agnt::cli::run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("agnt: {e:#}");
            ExitCode::FAILURE
        }
    }
}

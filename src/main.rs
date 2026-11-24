use clap::Parser;
use thugsred_smsgateway::cli::{self, Cli, Command};

fn main() {
    let cli = Cli::parse();
    if !matches!(cli.command, Command::Run { .. }) {
        // Command-line tools should end quietly when their output is piped
        // into a program that stops reading (for example `| head`). Rust
        // ignores SIGPIPE by default, which turns that into a panic.
        // SAFETY: called once at start-up, before any thread is spawned, and
        // it only restores the default disposition of SIGPIPE.
        unsafe {
            let _ = nix::sys::signal::signal(
                nix::sys::signal::Signal::SIGPIPE,
                nix::sys::signal::SigHandler::SigDfl,
            );
        }
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = runtime.block_on(cli::main(cli)) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

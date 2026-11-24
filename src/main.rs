use clap::Parser;
use thugsred_smsgateway::cli::{self, Cli};

fn main() {
    let cli = Cli::parse();
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

//! Trusted operator mount helper. Never expose this executable inside an agent.
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.as_slice() == ["--version"] {
        println!("treeharbor-cow-driver {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    #[cfg(target_os = "linux")]
    let result = treeharbor_runtime::run_cow_driver(&args);
    #[cfg(not(target_os = "linux"))]
    let result: treeharbor_runtime::Result<()> = Err(treeharbor_runtime::Error::Unsupported);
    if let Err(error) = result {
        eprintln!(
            "Copy-on-Write mount operation refused or incomplete: {error}. Preserve the original paths and inspect the node before retrying."
        );
        std::process::exit(1);
    }
}

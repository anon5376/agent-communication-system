fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && matches!(args[0].as_str(), "--version" | "-V") {
        println!("qagent {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let mut io = acs::cli::Io::default();
    std::process::exit(acs::cli::run(&args, &mut io));
}

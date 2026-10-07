fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && matches!(args[0].as_str(), "--version" | "-V") {
        println!("acs-desktop {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    std::process::exit(acs::desktop::main());
}

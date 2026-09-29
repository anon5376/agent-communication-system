fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut io = acs::cli::Io::default();
    std::process::exit(acs::cli::run(&argv, &mut io));
}

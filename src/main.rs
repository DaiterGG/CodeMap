fn main() {
    if let Err(error) = hello_cli::web::run() {
        eprintln!("error: could not start CodeMap: {error}");
        std::process::exit(1);
    }
}

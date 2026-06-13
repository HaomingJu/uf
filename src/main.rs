fn main() {
    if let Err(err) = uf::run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

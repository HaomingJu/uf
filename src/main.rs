fn main() {
    if let Err(err) = web_fzf::run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

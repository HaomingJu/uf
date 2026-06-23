pub mod app;
pub mod config;
pub mod log;
pub mod matchers;
pub mod models;
pub mod sources;
pub mod ui;

pub fn run() -> Result<(), String> {
    app::run()
}

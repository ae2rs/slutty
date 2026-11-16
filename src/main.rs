mod config;
mod font;
mod keys;
mod mux;
mod platform;
mod pty;
mod renderer;
mod scan;
mod terminal;

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    platform::run(config::path())
}

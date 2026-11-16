mod config;
mod font;
mod mux;
mod platform;
mod pty;
mod renderer;
mod terminal;

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    platform::run(config::Config::default())
}

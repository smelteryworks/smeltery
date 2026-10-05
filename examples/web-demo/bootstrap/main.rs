//! The web-demo binary: `serve`, `route:list` and the other app commands.

fn main() -> std::process::ExitCode {
    smeltery::run(web_demo::build)
}

//! This crate's one binary. Every role it can take is a subcommand of it.
//!
//! `implementation.md` section 2: "Worker executables can be subcommands of one binary
//! initially; process boundaries do not require separate repos."

fn main() -> std::process::ExitCode {
    fly_session::cli::main()
}

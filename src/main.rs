#![deny(unsafe_op_in_unsafe_fn)]

mod app;
mod hover;
mod mode;
mod platform;

use std::{env, process::ExitCode};

use mode::Command;

fn main() -> ExitCode {
    let command = match Command::parse(env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("CursorPeek: {error}");
            eprintln!("Run `CursorPeek --help` for usage.");
            return ExitCode::from(2);
        }
    };

    match command {
        Command::Help => {
            print!("{}", mode::HELP);
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("CursorPeek {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Run(process_mode) => match app::run(process_mode) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("CursorPeek failed: {error}");
                ExitCode::FAILURE
            }
        },
    }
}

#![windows_subsystem = "windows"]
#![deny(unsafe_op_in_unsafe_fn)]

mod app;
#[cfg(feature = "resolver-corpus")]
mod corpus;
mod hover;
mod mode;
mod platform;
mod preview;
mod resolver;
mod settings;
mod worker;

use std::{
    env,
    io::{self, Write},
    process::ExitCode,
};

use mode::{Command, ProcessMode};

fn main() -> ExitCode {
    let command = match Command::parse(env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            let message = format!("CursorPeek: {error}\nRun `CursorPeek --help` for usage.");
            if !write_stderr(&message) {
                platform::show_error("CursorPeek", &message);
            }
            return ExitCode::from(2);
        }
    };

    match command {
        Command::Help => {
            if !write_stdout(mode::HELP) {
                platform::show_information("CursorPeek help", mode::HELP);
            }
            ExitCode::SUCCESS
        }
        Command::Version => {
            let version = format!("CursorPeek {}", env!("CARGO_PKG_VERSION"));
            if !write_stdout(&version) {
                platform::show_information("CursorPeek version", &version);
            }
            ExitCode::SUCCESS
        }
        Command::Run(process_mode) => match app::run(process_mode) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let message = format!("CursorPeek failed: {error}");
                if !write_stderr(&message) && process_mode == ProcessMode::Main {
                    platform::show_error("CursorPeek", &message);
                }
                ExitCode::FAILURE
            }
        },
    }
}

fn write_stdout(message: &str) -> bool {
    write_message(io::stdout().lock(), message)
}

fn write_stderr(message: &str) -> bool {
    write_message(io::stderr().lock(), message)
}

fn write_message(mut stream: impl Write, message: &str) -> bool {
    let result = stream.write_all(message.as_bytes()).and_then(|()| {
        if message.ends_with('\n') {
            Ok(())
        } else {
            stream.write_all(b"\n")
        }
    });
    result.and_then(|()| stream.flush()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::write_message;

    #[test]
    fn redirected_messages_have_exactly_one_trailing_newline() {
        let mut line = Vec::new();
        assert!(write_message(&mut line, "CursorPeek 0.1.0"));
        assert_eq!(line, b"CursorPeek 0.1.0\n");

        let mut help = Vec::new();
        assert!(write_message(&mut help, "Usage:\n"));
        assert_eq!(help, b"Usage:\n");
    }
}

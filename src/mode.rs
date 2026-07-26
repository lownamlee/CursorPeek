use std::{error::Error, ffi::OsString, fmt};

pub(crate) const HELP: &str = "\
CursorPeek - lightweight Windows Explorer hover preview

Usage:
  CursorPeek
  CursorPeek --input-diagnostics
  CursorPeek --worker-diagnostics
  CursorPeek --help
  CursorPeek --version

Options:
  --input-diagnostics  Measure Raw Input coverage over foreground Explorer for 30 seconds
  --worker-diagnostics Verify worker reuse, idle restart, and contained teardown
  -h, --help           Show this help text
  -V, --version        Show the program version

The DPI, preview-window, preview-worker, recovery-soak, and timeout-diagnostic modes are private.
";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessMode {
    Main,
    InputDiagnostics,
    DpiDiagnostics,
    PreviewWindowDiagnostics,
    PreviewWindowPracticeDiagnostics,
    WorkerDiagnostics,
    WorkerTimeoutDiagnostics,
    RecoverySoakDiagnostics,
    PreviewWorker,
    #[cfg(feature = "resolver-corpus")]
    ResolverCorpusProbe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Run(ProcessMode),
    Help,
    Version,
}

impl Command {
    pub(crate) fn parse<I, S>(args: I) -> Result<Self, ParseError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut args = args.into_iter().map(Into::into);
        let Some(first) = args.next() else {
            return Ok(Self::Run(ProcessMode::Main));
        };

        let command = match first.to_str() {
            Some("-h" | "--help") => Self::Help,
            Some("-V" | "--version") => Self::Version,
            Some("--input-diagnostics") => Self::Run(ProcessMode::InputDiagnostics),
            Some("--dpi-diagnostics") => Self::Run(ProcessMode::DpiDiagnostics),
            Some("--preview-window-diagnostics") => {
                Self::Run(ProcessMode::PreviewWindowDiagnostics)
            }
            Some("--preview-window-practice-diagnostics") => {
                Self::Run(ProcessMode::PreviewWindowPracticeDiagnostics)
            }
            Some("--worker-diagnostics") => Self::Run(ProcessMode::WorkerDiagnostics),
            Some("--worker-timeout-diagnostics") => {
                Self::Run(ProcessMode::WorkerTimeoutDiagnostics)
            }
            Some("--recovery-soak-diagnostics") => Self::Run(ProcessMode::RecoverySoakDiagnostics),
            Some("--preview-worker") => Self::Run(ProcessMode::PreviewWorker),
            #[cfg(feature = "resolver-corpus")]
            Some("--resolver-corpus-probe") => Self::Run(ProcessMode::ResolverCorpusProbe),
            _ => return Err(ParseError::UnexpectedArgument(first)),
        };

        if let Some(extra) = args.next() {
            return Err(ParseError::ExtraArgument(extra));
        }

        Ok(command)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ParseError {
    UnexpectedArgument(OsString),
    ExtraArgument(OsString),
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedArgument(argument) => {
                write!(
                    formatter,
                    "unexpected argument `{}`",
                    argument.to_string_lossy()
                )
            }
            Self::ExtraArgument(argument) => {
                write!(
                    formatter,
                    "unexpected extra argument `{}`",
                    argument.to_string_lossy()
                )
            }
        }
    }
}

impl Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::{Command, ParseError, ProcessMode};
    use std::ffi::OsString;

    #[test]
    fn no_arguments_selects_main_mode() {
        assert_eq!(
            Command::parse(std::iter::empty::<&str>()),
            Ok(Command::Run(ProcessMode::Main))
        );
    }

    #[test]
    fn private_switch_selects_worker_mode() {
        assert_eq!(
            Command::parse(["--preview-worker"]),
            Ok(Command::Run(ProcessMode::PreviewWorker))
        );
    }

    #[cfg(feature = "resolver-corpus")]
    #[test]
    fn private_switch_selects_resolver_corpus_probe() {
        assert_eq!(
            Command::parse(["--resolver-corpus-probe"]),
            Ok(Command::Run(ProcessMode::ResolverCorpusProbe))
        );
    }

    #[test]
    fn diagnostic_switch_selects_input_measurement_mode() {
        assert_eq!(
            Command::parse(["--input-diagnostics"]),
            Ok(Command::Run(ProcessMode::InputDiagnostics))
        );
    }

    #[test]
    fn private_switch_selects_dpi_diagnostic_mode() {
        assert_eq!(
            Command::parse(["--dpi-diagnostics"]),
            Ok(Command::Run(ProcessMode::DpiDiagnostics))
        );
    }

    #[test]
    fn private_switch_selects_preview_window_diagnostic_mode() {
        assert_eq!(
            Command::parse(["--preview-window-diagnostics"]),
            Ok(Command::Run(ProcessMode::PreviewWindowDiagnostics))
        );
        assert_eq!(
            Command::parse(["--preview-window-practice-diagnostics"]),
            Ok(Command::Run(ProcessMode::PreviewWindowPracticeDiagnostics))
        );
    }

    #[test]
    fn worker_diagnostic_switches_select_parent_modes() {
        assert_eq!(
            Command::parse(["--worker-diagnostics"]),
            Ok(Command::Run(ProcessMode::WorkerDiagnostics))
        );
        assert_eq!(
            Command::parse(["--worker-timeout-diagnostics"]),
            Ok(Command::Run(ProcessMode::WorkerTimeoutDiagnostics))
        );
        assert_eq!(
            Command::parse(["--recovery-soak-diagnostics"]),
            Ok(Command::Run(ProcessMode::RecoverySoakDiagnostics))
        );
    }

    #[test]
    fn help_aliases_are_accepted() {
        assert_eq!(Command::parse(["-h"]), Ok(Command::Help));
        assert_eq!(Command::parse(["--help"]), Ok(Command::Help));
    }

    #[test]
    fn version_aliases_are_accepted() {
        assert_eq!(Command::parse(["-V"]), Ok(Command::Version));
        assert_eq!(Command::parse(["--version"]), Ok(Command::Version));
    }

    #[test]
    fn unknown_argument_is_rejected() {
        assert_eq!(
            Command::parse(["--unknown"]),
            Err(ParseError::UnexpectedArgument(OsString::from("--unknown")))
        );
    }

    #[test]
    fn extra_argument_is_rejected() {
        assert_eq!(
            Command::parse(["--help", "extra"]),
            Err(ParseError::ExtraArgument(OsString::from("extra")))
        );
    }
}

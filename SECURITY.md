# Security policy

## Supported versions

Before the first tagged release, security fixes are applied to `main`. After release, the latest
0.x version receives security fixes; older 0.x builds may be asked to upgrade.

## Reporting a vulnerability

Do not open a public issue with vulnerability details.

Use **Security > Advisories > Report a vulnerability** on the GitHub repository to send a private
report to the maintainers. If private vulnerability reporting is unavailable, open a public issue
that asks for a private security contact without including technical details, proof of concept,
logs, file contents, or affected paths.

Please include, when available:

- the affected revision or CursorPeek version;
- Windows version and installation mode;
- impact and expected security boundary;
- minimal reproduction steps or a proof of concept;
- whether the issue is already public;
- any suggested mitigation.

CursorPeek handles untrusted file content, Windows Shell identity, and process-to-process messages.
Reports involving wrong-file resolution, remote or recalled content access, parser memory or
arithmetic safety, IPC authentication, worker escape, focus/input interference, or unsafe
installation behavior are particularly useful.

The maintainers aim to acknowledge a complete report within seven days. They will coordinate
validation, remediation, credit, and disclosure through a private GitHub security advisory when
appropriate. Please allow a reasonable remediation period before public disclosure.

Routine crashes, unsupported formats, and non-sensitive correctness bugs can use the public bug
report form.

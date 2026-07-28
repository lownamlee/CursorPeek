# Project governance

CursorPeek is maintained in public. This document explains who can make project decisions and how
those decisions are recorded.

## Roles

- **Users** run CursorPeek and provide feedback.
- **Contributors** submit issues, documentation, tests, designs, or code.
- **Maintainers** review contributions, protect the product contract, manage security reports, and
  prepare releases.

Maintainer status reflects sustained project work and responsibility, not ownership of community
contributions.

## Decisions

Routine changes may be approved by one maintainer after required checks pass. The maintainers seek
consensus for decisions that change:

- the security or process-containment boundary;
- supported formats or Windows versions;
- runtime or build dependencies;
- licensing, privacy, packaging, or release policy;
- the current documented product contract.

When consensus is not immediate, the proposal remains open while evidence is collected. Decisions
with lasting architectural impact are recorded in the pull request, an issue, or project
documentation. A maintainer with a material conflict of interest must disclose it and avoid being
the sole approver.

## Releases and security

Maintainers may prepare releases, but release artifacts must pass the documented automated and
Windows qualification gates. Security fixes follow [SECURITY.md](SECURITY.md), including private
coordination before public disclosure when appropriate.

## Adding or removing maintainers

An active contributor may become a maintainer after demonstrating sound review judgment,
consistent participation, and care for the project's safety and scope. Existing active
maintainers decide by consensus and record the decision in the project history.

A maintainer may step down at any time. Maintainer access may also be removed for prolonged
inactivity, repeated disregard of project policy, or a Code of Conduct violation. Access changes
should be documented without publishing private or sensitive details.

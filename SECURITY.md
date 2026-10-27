# Security Policy

## Supported versions

SLCR is pre-release. Security fixes go into `trunk` and ship in the next
build. Only the latest release is supported.

| Version | Supported |
| --- | --- |
| Latest release | Yes |
| Anything older | No |

## Reporting a vulnerability

**Please don't report security vulnerabilities in public issues, discussions,
or pull requests.**

Report them privately in one of two ways:

- Email **security@wack.run**.
- Use GitHub's
  [private vulnerability reporting](https://github.com/wack/slcr/security/advisories/new).

Please include:

- The version of `slcr` (`slcr version`) and your operating system.
- A description of the issue and its impact.
- Steps to reproduce it, ideally with a minimal spec, validator file,
  `SLCR.toml`, or tool descriptor.
- Any proof-of-concept code, if you have it.

## What to expect

- We'll acknowledge your report within **3 business days**.
- We'll send an initial assessment within **10 business days**, including
  whether we consider it a vulnerability and how severe it is.
- We'll keep you updated as we work on a fix, and tell you when it ships.
- We'll credit you in the advisory, unless you'd rather stay anonymous.

We ask that you give us a reasonable amount of time to release a fix before
disclosing the issue publicly. We aim to resolve confirmed vulnerabilities
within 90 days of the report, and will coordinate a disclosure date with you.

## Scope

`slcr` runs the tools a project declares, so running `slcr check` in a
repository means running that repository's validators. That is by design.
**Running `slcr check` on a repository you don't trust is like running its
test suite or build script: it can execute arbitrary commands.** Reports
that come down to this alone are out of scope.

Examples of what is in scope:

- `slcr` accepting a tool descriptor whose bytes don't match its BLAKE3 pin
  in `SLCR.toml`.
- Fetching a descriptor over anything other than HTTPS, including after a
  redirect.
- A `path` source reading a file outside the project.
- Commands that should never execute anything, such as `vet`, `render`,
  `query`, `slice`, or `tools list`, running a tool or reaching the network.
- `slcr` writing outside the paths a command is documented to write to.
- Crashes or resource exhaustion triggered by a malformed spec, validator
  file, manifest, or descriptor.

Out of scope:

- Validators running the commands their descriptors declare.
- A tool found on `PATH` being a different program than you expected.
  Descriptors name tools by program name, and `slcr` trusts your `PATH`.
- Vulnerabilities in the tools that validators run, such as test runners or
  linters. Report those to the tool's maintainers.

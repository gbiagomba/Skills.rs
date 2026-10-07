# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Feature in development

## [1.0.2] - 2026-10-07

### Fixed
- `docker-build` job declared `needs: [test-linux, test-macos, test-windows]`, three jobs that do not exist in this workflow. This was a workflow validation error that aborted every CI run at 0 seconds
- Job-level `if: env.PROJECT_TYPE == 'rust'` conditions never evaluated true, because the `env` context is not available in a job-level `if`. The aarch64 build jobs were therefore always skipped, which in turn skipped the `release` job. Project type now resolves through a `setup` job output
- Release notes extraction only matched `## v1.0.0` headings and silently missed the Keep a Changelog `## [1.0.0] - DATE` form used by this repo, always falling back to the tag message
- Replaced the retired `macos-13` runner label with `macos-15-intel`
- Replaced hardcoded `ssltriage` Docker image names, left over from another project, with a `DOCKER_IMAGE_NAME` template variable

### Added
- `setup` job that validates `PROJECT_TYPE` and reports whether buildable source exists, so an unfilled template scaffold reports success instead of failing on an empty project
- Explicit `shell: bash` on multi-line steps so they behave consistently on the Windows runner

### Changed
- `release` job uses `always()` with explicit result checks, so a Python project is still released when the Rust-only aarch64 jobs are skipped

## [1.0.1] - 2026-10-07

### Fixed
- Replaced leftover `Sherlock` project branding in `TRADEMARK_POLICY.md` with the `APP_NAME` placeholder so the template carries no inherited project name
- Removed stray `.gitignore??????` file left behind by an interrupted write

### Changed
- `.gitignore` now excludes `CLAUDE_AGENT_CONVO.txt` session transcripts
- Synced `dev` with `main`, bringing in `actions/checkout@v7` and `actions/setup-python@v7` from [#20](https://github.com/gbiagomba/Template/pull/20) and [#21](https://github.com/gbiagomba/Template/pull/21)

## [1.0.0] - 2025-12-05

### Added
- Initial release
- Core functionality implemented
- Multi-platform support (Linux, macOS, Windows)
- Cross-architecture builds (x64, ARM64)
- GitHub Actions CI/CD pipeline
- Docker support
- Comprehensive README and documentation

### Security
- Secure coding practices applied
- Input validation implemented
- Error handling hardened

---

**AGENT NOTE:** Update this file before EVERY release as per RULE 5

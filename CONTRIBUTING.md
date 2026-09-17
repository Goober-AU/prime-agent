# Contributing to Optimus Agent

Thank you for your interest in contributing to Optimus Agent.

We welcome code contributions, feature suggestions, documentation improvements, general feedback, and bug reports.

## Getting Started

Before starting significant work, please check the existing Issues and Discussions to see whether the topic has already been raised.

For questions, ideas, or proposals, use GitHub Discussions:

https://github.com/telemusai/optimus-agent/discussions

For confirmed bugs or clearly defined tasks, you may open a GitHub Issue:

https://github.com/telemusai/optimus-agent/issues

When reporting a problem, please include enough information to reproduce and understand the issue, including relevant environment details, logs, or screenshots where appropriate.

Do not include API keys, access tokens, credentials, personal information, or other sensitive data.

For security vulnerabilities, please follow [SECURITY.md](SECURITY.md) rather than reporting the issue publicly.

## Pull Requests

Pull requests are welcome.

Before submitting a pull request:

1. Keep changes focused and reasonably scoped.
2. Follow the existing project structure and coding conventions.
3. Add or update tests where appropriate.
4. Run the relevant checks locally before submitting.
5. Clearly describe what the change does and why it is needed.
6. Avoid unrelated refactoring or dependency changes unless they are necessary for the contribution.

For larger changes or new functionality, we recommend opening an Issue or Discussion first so the proposed approach can be considered before significant development work is undertaken.

Contributors using coding agents or other AI-assisted development tools are welcome. Contributors remain responsible for reviewing, understanding, testing, and validating the code they submit.

## Development

Development setup, build instructions, and relevant commands are documented in the [development guide](packages/coding-agent/docs/development.md).

Please follow the repository's existing development and formatting conventions when making changes.

## Changelog Entries

Do not edit `packages/*/CHANGELOG.md` directly.

For changes requiring a changelog entry, add a fragment for each affected package:

`packages/<pkg>/.changes/<slug>.md`

The `<slug>` should be a short kebab-case description of the change or associated issue, for example:

`fix-terminal-resize.md`

Each fragment should contain the relevant changelog bullet, for example:

`- Fixed terminal input handling when resizing the window.`

The release process aggregates these fragments into the appropriate changelog and removes the individual fragment files.

Changes to `packages/<pkg>/src` may require a changelog fragment to pass CI. Where a changelog entry is not appropriate, the `no-changelog` label may be used.

## Review

Pull requests are reviewed based on correctness, scope, maintainability, compatibility with the project, and successful validation.

Maintainers may request changes before merging or close contributions that are no longer applicable or do not align with the direction of the project.

Thank you for helping improve Optimus Agent.

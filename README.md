# cargo-workspace-version
Cargo plugin to check and/or update all local package versions in a workspace

We found it difficult to maintain the same version of all packages within a
cargo workspace when releasing our software. This tool is used as a release
tool to verify that they are all what we are expecting.

The version passed in can start with a leading 'v' so you can simply use a git
tag with the version in it as a release check.

## Installation

    cargo install cargo-workspace-version


## Usage

From the top level of your workspace, run:

    cargo workspace-version check v1.0.0

This will verify that all packages under the workspace have version 1.0.0 as their
version, and also that dependencies on other packages within this cargo workspace
point to the new version. If there are errors reported and you want to switch them,
just run:

    cargo workspace-version update v1.0.0

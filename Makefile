build:
	cargo build
.PHONY: build

# the mxhelper client (clients/rust): a real matrix-sdk peer for encrypted-room e2e tests.
# kept out of `build` since it pulls the (heavy) matrix-sdk; build it explicitly when needed.
helper:
	cargo build --manifest-path clients/rust/Cargo.toml
.PHONY: helper

test:
	cargo test
.PHONY: test

# self-test for the shared python client (clients/python). needs the binary
# built, so precommit runs `build` first.
test-python:
	python3 clients/python/test_fakematrix.py
.PHONY: test-python

lint:
	cargo check
	cargo clippy
.PHONY: lint

format:
	cargo fmt
.PHONY: format

precommit: lint build test test-python
.PHONY: precommit

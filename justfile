# Run the test suite
default: fmt lint build test

# Install development tools
@tools:
    cargo install cargo-nextest
    cargo install cargo-llvm-cov

# Run tests with selectors
@test *selectors:
    cargo nextest run {{selectors}}

# Run tests with coverage
@cover:
    cargo llvm-cov nextest

# Build the binaries
@build:
    cargo build

# Install the binaries
@install:
    cargo install --path .

# Run the application
@run *args:
    cargo run --bin over -- {{args}}

# Format the code
@fmt:
    cargo fmt --all

# Lint the code
@lint:
    cargo clippy

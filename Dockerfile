FROM rust:1.88-bookworm@sha256:af306cfa71d987911a781c37b59d7d67d934f49684058f96cf72079c3626bfe0

RUN apt-get update \
    && apt-get install -y --no-install-recommends libdbus-1-dev libgtk-4-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt

WORKDIR /app
COPY . .
CMD ["cargo", "test", "--all-targets", "--locked"]

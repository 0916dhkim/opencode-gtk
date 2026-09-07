FROM rust:1.88-bookworm@sha256:af306cfa71d987911a781c37b59d7d67d934f49684058f96cf72079c3626bfe0

RUN apt-get update \
    && apt-get install -y --no-install-recommends libdbus-1-dev libgtk-4-dev pkg-config xvfb dbus-x11 \
    && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt

WORKDIR /app
COPY . .
CMD ["sh", "-c", "Xvfb :99 -screen 0 1280x1024x24 >/dev/null 2>&1 & export DISPLAY=:99 GTK_A11Y=none; exec cargo test --all-targets --locked"]

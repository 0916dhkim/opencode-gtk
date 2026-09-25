FROM rust:bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        cmake \
        libdbus-1-dev \
        libexpat1-dev \
        libfontconfig1-dev \
        libfreetype6-dev \
        libgl1-mesa-dev \
        libxkbcommon-dev \
        libwayland-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN rustup toolchain install stable && rustup default stable && rustup component add clippy rustfmt

WORKDIR /app
COPY . .
CMD ["cargo", "test", "--all-targets", "--locked"]

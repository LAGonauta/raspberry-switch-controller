# Base image matching the previous devcontainer.json image.
FROM mcr.microsoft.com/devcontainers/rust:2-1-trixie

# gilrs (libudev-sys) needs libudev headers + pkg-config to build.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libudev-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

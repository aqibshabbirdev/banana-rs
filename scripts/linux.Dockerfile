# Ubuntu 24.04 image with everything needed to build (and headlessly test) the Linux version.
#   docker build -t banana-linux -f scripts/linux.Dockerfile scripts
#   scripts/build-linux.sh
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl build-essential pkg-config \
        libheif-dev libheif-plugin-libde265 \
        libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
        gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav \
        libxkbcommon-dev libxkbcommon-x11-0 libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
        libgl1-mesa-dev libgl1-mesa-dri libegl1 libx11-xcb1 libxcursor1 libxrandr2 libxi6 \
        xvfb xauth \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
ENV PATH=/root/.cargo/bin:$PATH
WORKDIR /src

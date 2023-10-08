FROM fedora:latest
RUN dnf -y install gcc gcc-aarch64-linux-gnu gcc-x86_64-linux-gnu make
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | bash -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"
RUN rustup target install x86_64-unknown-linux-musl
RUN rustup target install aarch64-unknown-linux-musl
WORKDIR /work
CMD ["make"]

FROM fedora:latest as builder
RUN dnf -y install gcc gcc-aarch64-linux-gnu gcc-x86_64-linux-gnu make
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | bash -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"
RUN rustup target install x86_64-unknown-linux-musl
RUN rustup target install aarch64-unknown-linux-musl
WORKDIR /work
CMD ["make"]

FROM builder as stage1 
WORKDIR /tmp
COPY . /tmp
RUN make

FROM builder as stage2
ARG TARGETARCH
WORKDIR /
COPY --from=stage1 /tmp/target/aarch64-unknown-linux-musl/release/dive /dive-arm64
COPY --from=stage1 /tmp/target/x86_64-unknown-linux-musl/release/dive /dive-amd64
RUN echo $BUILDPLATFORM $TARGETPLATFORM $BUILDARCH $TARGETARCH
RUN bash -c 'if [ "$TARGETARCH" == "arm64" ]; then ln /dive-arm64 /dive; else ln /dive-amd64 /dive; fi'

FROM scratch
COPY --from=stage2 /dive /dive
CMD ["/dive"]

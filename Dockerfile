FROM fedora:latest as builder
ARG TARGETARCH
RUN echo $BUILDPLATFORM $TARGETPLATFORM $BUILDARCH $TARGETARCH
RUN dnf -y install gcc make
RUN bash -c 'if [ "$TARGETARCH" == "arm64" ]; then dnf -y install gcc-aarch64-linux-gnu ; else dnf -y install gcc-x86_64-linux-gnu; fi'
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | bash -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"
RUN bash -c 'if [ "$TARGETARCH" == "arm64" ]; then rustup target install aarch64-unknown-linux-musl ; else rustup target install x86_64-unknown-linux-musl; fi'
WORKDIR /work
COPY . /work
RUN make TARGETARCH=${TARGETARCH}

FROM scratch
WORKDIR /
COPY --from=builder /work/target/*/release/dive /dive
CMD ["/dive", "scan"]

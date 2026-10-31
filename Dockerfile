# Image Linux du nœud Kynovara (utile quand Windows bloque les programmes non
# signés, par exemple avec Smart App Control).
#
#   docker build -t kynovara:0.3.2 .
#   docker run -d --name kynovara-testnet --restart unless-stopped \
#     -v kynovara-testnet:/data kynovara:0.3.2 --mine --mining-address <tkyn1…> --threads 2
#   docker exec -it kynovara-testnet kynovara-cli --datadir /data info
FROM rust:1.97-slim-trixie AS build
# Version exacte de rust-toolchain.toml, sans télécharger rustfmt ni clippy.
ENV RUSTUP_TOOLCHAIN=1.97.0
WORKDIR /src
COPY . .
RUN cargo test --workspace --release && cargo build --release

FROM debian:trixie-slim
RUN useradd --create-home --uid 10001 kynovara && mkdir /data && chown kynovara /data
COPY --from=build /src/target/release/kynovarad /src/target/release/kynovara-cli /usr/local/bin/
COPY --from=build /src/AVIS.md /src/LICENSE-MIT /src/LICENSE-APACHE /usr/share/doc/kynovara/
USER kynovara
VOLUME /data
EXPOSE 19733
ENTRYPOINT ["kynovarad", "--datadir", "/data"]

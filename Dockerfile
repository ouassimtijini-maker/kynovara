# Image Linux du nœud Kynovara (utile quand Windows bloque les programmes non
# signés, par exemple avec Smart App Control).
#
#   docker build -t kynovara:0.4.0 .
#   docker run -d --name kynovara-testnet --restart unless-stopped \
#     -v kynovara-testnet:/data kynovara:0.4.0 --mine --mining-address <tkyn1…> --threads 2
#   docker exec -it kynovara-testnet kynovara-cli --datadir /data info
#
# Mémoire : RandomX demande environ 256 Mio par fil de minage, plus 512 Mio au
# plus pour la vérification des blocs.
FROM rust:1.97-slim-trixie AS build
# RandomX est une bibliothèque C++ : il faut CMake et un compilateur C++.
RUN apt-get update && apt-get install -y --no-install-recommends cmake g++ make && rm -rf /var/lib/apt/lists/*
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

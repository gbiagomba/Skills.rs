# Dockerfile
# Satisfies RULE 2 - maintain Dockerfile
#
# Multi-stage: a Rust builder, then a slim Debian runtime with CA certificates
# and a non-root user.
#
# No git or samba packages are installed. The git backend uses `gix` and the SMB
# backend uses `smb2`, both of which are pure Rust, so the runtime image needs no
# protocol helper libraries. CA certificates are present because HTTPS
# verification is always on and is never downgraded.
#
# IMPORTANT, and easy to get wrong: agent discovery inside a container sees the
# CONTAINER's filesystem, not the host's. `skill agents` will report whatever is
# present in the image, which is normally nothing. To manage a host installation
# you must deliberately map the paths in, for example:
#
#   docker run --rm \
#     -v "$HOME/.claude:/home/skill/.claude" \
#     -v "$HOME/skills:/home/skill/skills" \
#     -v "$PWD/my-skill:/src/my-skill:ro" \
#     skill copy /src/my-skill claude
#
# Note also that `skill link` creates symlinks using CONTAINER paths. A link
# written inside a container is very likely to be broken when the host reads it,
# so prefer `skill copy` when working through a container.

FROM rust:slim-bookworm AS builder

WORKDIR /app

# Cache dependency compilation separately from the source.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo "fn main() {}" > src/main.rs \
    && echo "" > src/lib.rs \
    && cargo build --release --locked 2>/dev/null || true \
    && rm -rf src

COPY src ./src
# Touch so cargo does not reuse the placeholder build.
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

# Runtime stage
FROM debian:bookworm-slim AS final

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

ARG APP_NAME=skill
ARG APP_VERSION=1.1.0
ARG APP_MAINTAINER="Gilles Biagomba <gilles.infosec@gmail.com>"

LABEL maintainer="${APP_MAINTAINER}"
LABEL version="${APP_VERSION}"
LABEL org.opencontainers.image.title="${APP_NAME}"
LABEL org.opencontainers.image.description="A cross-agent skill manager with safe copying, linking, migration, updates, and synchronization."
LABEL org.opencontainers.image.licenses="GPL-3.0-only"

RUN useradd --create-home --shell /usr/sbin/nologin skill
COPY --from=builder /app/target/release/skill /usr/local/bin/skill

USER skill
WORKDIR /home/skill

ENTRYPOINT ["skill"]
CMD ["--help"]

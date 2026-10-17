FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc
WORKDIR /app
# Build dependencies first, so they stay cached while only the code changes.
COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs \
    && cargo build --release --locked && rm -rf src
COPY migrations ./migrations
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

# A static binary on distroless: CA certificates and a non-root user, no shell.
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /app/target/release/pmo /pmo
ENV PORT=8080
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["/pmo"]

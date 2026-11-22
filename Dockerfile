# Hosted demo for https://yoke.santa.build
#
# Three stages: the static page, the real yoke binary, and the Go bridge that
# serves both. The runtime is distroless and non-root; /tmp is a tmpfs in
# compose, because every visitor gets a throwaway directory there.

FROM node:22-bookworm-slim AS web
WORKDIR /web
COPY demo/web/package.json demo/web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY demo/web/ ./
RUN npm run build

FROM rust:1-bookworm AS yoke
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY src ./src
RUN cargo build --release --locked --bin yoke

FROM golang:1.25-bookworm AS bridge
WORKDIR /bridge
COPY demo/bridge/go.mod demo/bridge/go.sum ./
RUN go mod download
COPY demo/bridge/ ./
COPY --from=web /bridge/web/dist ./web/dist
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -ldflags="-s -w" -o /out/bridge .

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=yoke /src/target/release/yoke /yoke
COPY --from=bridge /out/bridge /bridge
COPY demo/workspace /workspace-template
ENV ADDR=0.0.0.0:8080 \
    YOKE_DEMO_TMP=/tmp/yoke-demo \
    YOKE_DEMO_BIN=/yoke \
    YOKE_DEMO_SEED=/workspace-template
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/bridge", "-healthcheck"]
ENTRYPOINT ["/bridge"]

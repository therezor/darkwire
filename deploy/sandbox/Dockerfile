FROM node:22-bookworm-slim AS web
WORKDIR /src
RUN corepack enable
COPY . .
RUN pnpm install --frozen-lockfile && pnpm --filter @ghostwire/web... build

FROM rust:1.97-bookworm AS build
WORKDIR /src
COPY . .
COPY --from=web /src/packages/web/dist packages/web/dist
RUN cargo build --locked --release -p ghostai -p ghostai-sandbox

FROM debian:bookworm-slim AS app
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/ghostai /usr/local/bin/ghostai
ENTRYPOINT ["ghostai"]
CMD ["serve", "--host", "0.0.0.0", "--port", "3000", "--workspace", "/workspaces/default"]

FROM debian:bookworm-slim AS service
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates docker.io && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/ghostai-sandbox /usr/local/bin/ghostai-sandbox
ENTRYPOINT ["ghostai-sandbox"]
CMD ["serve-env"]

# The image `containers/dev.yaml` points at. Built separately and referenced by
# its id, never by this tag: a container definition that named a tag would be
# repointable after approval.
FROM alpine:3.23 AS tools
RUN apk add --no-cache git util-linux ca-certificates
USER 1000:1000
WORKDIR /workspace

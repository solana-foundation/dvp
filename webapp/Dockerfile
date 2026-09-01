# Canonical artifact-registry Dockerfile (Bun + SvelteKit + adapter-node).
# Cloud Build runs: docker build --build-arg BASE_PATH=/{app} .
# Do not rename stages or remove the BASE_PATH plumbing.

# ---------- build stage ----------
FROM oven/bun:1 AS build
WORKDIR /app

COPY package.json bun.lock ./
RUN bun install --frozen-lockfile

COPY . .

# BASE_PATH is baked into the client bundle at build time via
# svelte.config.js (kit.paths.base). The platform passes /{app}.
ARG BASE_PATH=""
ENV BASE_PATH=${BASE_PATH}
RUN bun run build

# Re-install with production deps only for the runtime image.
RUN rm -rf node_modules && bun install --frozen-lockfile --production

# ---------- runtime stage ----------
FROM oven/bun:1 AS runtime
WORKDIR /app

COPY --from=build /app/build ./build
COPY --from=build /app/node_modules ./node_modules
COPY --from=build /app/package.json ./package.json

ENV NODE_ENV=production
# Cloud Run injects PORT; default to 8080 (the platform's contract port).
ENV PORT=8080
EXPOSE 8080

# Drop root: the oven/bun base image ships an unprivileged `bun` user, and the
# app only needs read access to the copied files.
USER bun

CMD ["bun", "./build/index.js"]

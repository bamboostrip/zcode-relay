# zcode-relay Rust 版 Dockerfile（静态 musl 二进制 + distroless，最小镜像）
# config.json 不打进镜像（含敏感 key），由 docker-compose 以 volume 挂载进 /data。

# ---- 阶段 1：编译静态 musl 二进制 ---- #
FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y musl-tools && rm -rf /var/lib/apt/lists/*
# 配置 musl 目标
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /build
# 先拷依赖清单（利用 docker 层缓存：改代码不重编译依赖）
COPY Cargo.toml ./
# 创建假的 src 让 cargo 预编译依赖
RUN mkdir src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release --target x86_64-unknown-linux-musl || true
# 拷真实源码
COPY src/ ./src/
# 触碰 main.rs 确保 rebuild（因为依赖已缓存，这次只编译业务代码）
RUN touch src/main.rs && cargo build --release --target x86_64-unknown-linux-musl

# ---- 阶段 2：distroless 运行时（无 shell，极小） ---- #
FROM gcr.io/distroless/cc-debian12:nonroot

WORKDIR /app
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/zcode-relay /app/zcode-relay

ENV RELAY_CONFIG=/data/config.json \
    RUST_LOG=info

USER nonroot:nonroot
EXPOSE 8787

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD ["/app/zcode-relay", "--healthcheck"]

CMD ["/app/zcode-relay"]

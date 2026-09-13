# 构建阶段：利用 Cargo.lock 锁定依赖；libs 为仓库内嵌子模块
FROM rust:1.98-slim AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --locked

# 运行阶段：最小镜像 + 非 root 用户
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 10001 app
COPY --from=builder /src/target/release/im-service /usr/local/bin/im-service
USER app
EXPOSE 8082
ENTRYPOINT ["im-service"]

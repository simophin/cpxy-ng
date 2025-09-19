FROM rust

WORKDIR /usr/src/app
COPY . .
RUN cargo build --release -p server

FROM debian
COPY --from=0 /usr/src/app/target/release/server /usr/local/bin/server

ENV KEY=
ENV BIND_ADDRESS=0.0.0.0:3000
EXPOSE 3000

CMD ["server"]
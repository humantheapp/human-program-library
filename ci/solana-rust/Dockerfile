FROM rust:1.63 as builder

# Change this if want to migrate to different version
ENV SOL_VERSION "1.10.39"
ENV SOL_TOOLCHAIN "1.62.1-x86_64-unknown-linux-gnu"
# sha256 checksum of source archive. Always verify this!
ENV SOL_CHECKSUM "022c44db22d53fcedc3bdf87236fe9eeac5f6d11046cb0b3ae24d6e6fc71b3f4"

RUN apt-get update -y && apt-get install -y curl bash build-essential pkg-config openssl perl libudev-dev clang cmake

RUN useradd -r user --home "/build" && mkdir /build && chown -R user /build
USER user

WORKDIR "/build"

RUN curl -fsSL https://github.com/solana-labs/solana/archive/refs/tags/v$SOL_VERSION.tar.gz > source.tar.gz && \
   #echo "downloaded checksum: " && sha256sum source.tar.gz && \
   echo "$SOL_CHECKSUM  source.tar.gz" | sha256sum -c - && \
   tar -xzf source.tar.gz && cd solana-$SOL_VERSION && \
   rustup install $SOL_TOOLCHAIN && \
   rustup component add rustfmt --toolchain $SOL_TOOLCHAIN && \
   ./scripts/cargo-install-all.sh ..

FROM rust:1.63-slim

RUN apt-get update -y && \
   apt-get install -y curl make tar bzip2 && \
   rm -rf /var/lib/apt/lists/*

RUN useradd -r user --home "/home/user" && mkdir /home/user && chown -R user /home/user
USER user

WORKDIR /bin/
COPY --from=builder /build/bin ./

ENV PATH /bin:$PATH
CMD ["solana", "--version"]
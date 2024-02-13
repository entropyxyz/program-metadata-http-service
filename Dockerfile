# To build this program, and put the .wasm binary in the directory 'output':
# docker build --output=binary-dir .
ARG IMAGE=peg997/build-entropy-programs:version0.1
FROM #IMAGE AS base

WORKDIR /usr/src/programs
COPY . .

RUN cargo component build --release --target wasm32-unknown-unknown

FROM scratch AS binary
COPY --from=base /usr/src/programs/target/wasm32-unknown-unknown/release/*.wasm /

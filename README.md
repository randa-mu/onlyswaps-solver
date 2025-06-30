# onlyswaps-solver

## Build
First retrieve and build submodules with `./build-dependencies.sh` then run `cargo build`

## Test
`cargo test`

## Docker build
`docker build .`

## Configuration
| Environment Variable | Mandatory? | Description                                                                         | Example                                  | Default                 |
| -------------------- | ---------- | ----------------------------------------------------------------------------------- | ---------------------------------------- |-------------------------|
| `SOLVER_PRIVATE_KEY` | Yes        | A hex-encoded private key, with or without the `0x`                                 | `0xdeadbeefdeadbeefdeadbeefdeadbeefdead` | —                       |
| `SOLVER_CONFIG_PATH` | No         | Path to your solver configuration JSON (must match format in `config_default.json`) | `/data/config.json`                      | `~/.solver/config.json` |
| `SOLVER_PORT`        | No         | Port on which to host the healthcheck endpoint                                      | `8080`                                   | `8080`                  |

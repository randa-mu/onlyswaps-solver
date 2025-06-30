#!/usr/bin/env bash

git submodule update --init --recursive
mkdir -p solidity-build
(cd onlysubs-solidity && npm ci && npm run build)

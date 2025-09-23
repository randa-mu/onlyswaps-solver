#!/usr/bin/env bash

git submodule update --init --recursive
mkdir -p solidity-build
(cd onlyswaps-solidity && npm ci && npm run build)

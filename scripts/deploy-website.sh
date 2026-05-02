#!/usr/bin/env bash
set -euo pipefail

# OVHcloud static hosting deployment
# Set environment variables in .env.deploy or export them:
#   OVH_HOST - SSH hostname (e.g., ssh.clusterXXX.hosting.ovh.net)
#   OVH_USER - SSH username
#   OVH_PATH - Remote path (default: www)

OVH_HOST="${OVH_HOST:?Set OVH_HOST env var (e.g., ssh.clusterXXX.hosting.ovh.net)}"
OVH_USER="${OVH_USER:?Set OVH_USER env var}"
OVH_PATH="${OVH_PATH:-www}"
BUILD_DIR="website/dist"

echo ":: Building site..."
cd "$(git rev-parse --show-toplevel)/website"
pnpm run build
cd ..

echo ":: Deploying to ${OVH_HOST}:${OVH_PATH}..."
rsync -avz --delete \
  --exclude='.DS_Store' \
  --exclude='*.map' \
  "${BUILD_DIR}/" \
  "${OVH_USER}@${OVH_HOST}:${OVH_PATH}/"

echo ":: Done. Site deployed to https://mailypoppins.dev"

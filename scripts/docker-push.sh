#!/usr/bin/env bash
# Build and push ferricula to Docker Hub: kord/ferricula
# Usage: ./scripts/docker-push.sh [tag]
#   tag defaults to the current git SHA (short)
#   Pass "latest" to also tag/push latest

set -euo pipefail

IMAGE="kord/ferricula"
SHA=$(git rev-parse --short HEAD)
TAG="${1:-$SHA}"

echo "==> Building $IMAGE:$SHA"
docker build -t "$IMAGE:$SHA" .

if [ "$TAG" != "$SHA" ]; then
  echo "==> Tagging $IMAGE:$TAG"
  docker tag "$IMAGE:$SHA" "$IMAGE:$TAG"
fi

echo "==> Pushing $IMAGE:$SHA"
docker push "$IMAGE:$SHA"

if [ "$TAG" != "$SHA" ]; then
  echo "==> Pushing $IMAGE:$TAG"
  docker push "$IMAGE:$TAG"
fi

echo "==> Done: $IMAGE:$SHA"

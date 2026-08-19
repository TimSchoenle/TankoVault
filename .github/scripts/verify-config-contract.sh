#!/usr/bin/env bash
# Check that a built image actually publishes the configuration contract it was built to publish.
#
# usage: verify-config-contract.sh <image-ref> <expected.labels> <expected.json>
#
# The checkpoint the whole scheme rests on. `deploy/docker/Dockerfile` carries the three
# `dev.terrace.config.*` values by hand — a `LABEL` key takes no interpolation, and `--build-arg`
# cannot reach a file produced inside a builder stage — so the labels are only as good as
# something checking them.
#
# It checks the **image**, never the Dockerfile. A source diff cannot see a base image that
# overrode a label, a `LABEL` line deleted on a branch nobody diffed, or a build argument that
# silently failed to interpolate. `xtask config-contract --check` is the source-side half and
# catches a prefix change one step earlier; this is the half that is evidence.
#
# Two comparisons, and the second is the one the design gave up a hash label to get:
#
#   1. the image's labels against the `.labels` file the *same generator run* wrote, for presence
#      and equality of the three, nothing more. Extra labels are ignored on purpose — every image
#      carries `org.opencontainers.image.*` and whatever its base contributed, and none of that is
#      this document's business. This mirrors `Contract::verify_labels` exactly.
#   2. the copy embedded at `/config/contract.json` against the copy exported to the host. Those
#      are the two carriers a consumer can read, and a stale embedded copy is precisely the
#      failure nothing downstream can see. The build is the one place that holds both for free.
#
# Every violation is reported before it exits: a build that names one missing label and hides two
# is a second round trip.
#
# `docker cp` from a stopped container, not `docker run cat`: these images are `FROM scratch` with
# no shell.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <image-ref> <expected.labels> <expected.json>" >&2
  exit 2
fi

image="$1"
expected_labels="$2"
expected_contract="$3"

for file in "$expected_labels" "$expected_contract"; do
  if [ ! -s "$file" ]; then
    echo "error: ${file} is missing or empty, so there is nothing to check the image against." >&2
    echo "       It comes out of the build's \`contract\` target; see deploy/docker/Dockerfile." >&2
    exit 1
  fi
done

status=0
labels_json="$(mktemp)"
trap 'rm -f "$labels_json"' EXIT

# `.Config.Labels` — capital C, capital L. `crane config` reports the same object under
# `.config.Labels` instead, and reading the wrong one yields `null`, which a careless comparison
# treats as "nothing to compare" and passes.
docker inspect --format '{{json .Config.Labels}}' "$image" > "$labels_json"
if [ "$(jq -r 'if . == null then "null" else "ok" end' "$labels_json")" != "ok" ]; then
  echo "error: ${image} reports no labels object at all; \`docker inspect\` gave null." >&2
  exit 1
fi

while IFS='=' read -r name expected; do
  [ -n "$name" ] || continue
  actual="$(jq -r --arg n "$name" '.[$n] // ""' "$labels_json")"
  if [ "$actual" != "$expected" ]; then
    if [ -z "$actual" ]; then
      echo "error: the image carries no '$name', so nothing can discover this contract from its config blob" >&2
    else
      echo "error: the image's '$name' is '$actual', and this contract's is '$expected'" >&2
    fi
    status=1
  fi
done < "$expected_labels"

# The embedded copy, read at the path the image's own label names rather than at a path this
# script assumes: a build that moved the file and updated the label is correct, and one that moved
# it without updating the label has already failed above.
path="$(jq -r --arg n 'dev.terrace.config.contract.path' '.[$n] // ""' "$labels_json")"
if [ -n "$path" ]; then
  probe="verify-config-contract-$$"
  docker create --name "$probe" "$image" > /dev/null
  embedded="$(mktemp)"
  if docker cp "${probe}:${path}" "$embedded" 2>/dev/null; then
    if ! cmp -s "$embedded" "$expected_contract"; then
      echo "error: the document at ${path} inside the image is not the one this build generated." >&2
      echo "       The embedded copy is what an air-gapped consumer reads, and a stale one is" >&2
      echo "       invisible to everything downstream." >&2
      status=1
    fi
  else
    echo "error: ${image} carries no file at ${path}, which its own label says holds the contract" >&2
    status=1
  fi
  docker rm "$probe" > /dev/null
  rm -f "$embedded"
fi

if [ "$status" -eq 0 ]; then
  echo "${image}: contract labels and the embedded document match the build's own output"
fi
exit "$status"

#!/usr/bin/env bash
# Run the prebuilt `wasm-bindgen` that `dx build` downloads, inside the image it runs in.
#
# usage: check-wasm-tools.sh [dockerfile] [lockfile]
#
# `dx` does not build wasm-bindgen: it downloads the GitHub release matching the version in
# `web/frontend/Cargo.lock` and executes it in the Dockerfile's `wasm-chef` lineage. Nothing pins
# what that binary links against, so a lockfile bump can raise its glibc floor past the base
# image's (0.2.130's aarch64 build needs GLIBC_2.38; bookworm ships 2.36). The image build only
# reaches that step after the full wasm compile, and pull requests build amd64 only, where the
# release is static musl. This runs the same binary in the same image on the runner's own
# architecture, in seconds, so CI runs it once per architecture on native runners.
#
# The asset per architecture mirrors dioxus-cli 0.7.10's `git_install_url`
# (src/wasm_bindgen.rs); re-check that function when `dx` is bumped. The other tools `dx`
# downloads in this build, wasm-opt (binaryen) and esbuild, are static binaries and cannot hit
# this class of failure.
set -euo pipefail

if [ "$#" -gt 2 ]; then
  echo "usage: $0 [dockerfile] [lockfile]" >&2
  exit 2
fi

dockerfile="${1:-deploy/docker/Dockerfile}"
lockfile="${2:-web/frontend/Cargo.lock}"
stage="wasm-chef"

# The image reference of `FROM [--flag ...] <image> AS wasm-chef`.
image="$(awk -v stage="$stage" '
  toupper($1) == "FROM" && NF >= 4 && tolower($(NF - 1)) == "as" && $NF == stage {
    for (i = 2; i < NF - 1; i++) if ($i !~ /^--/) ref = $i
    print ref
  }' "$dockerfile")"
if [ -z "$image" ] || [ "$(printf '%s\n' "$image" | wc -l)" -ne 1 ]; then
  echo "expected exactly one 'FROM <image> AS ${stage}' in ${dockerfile}, found: ${image:-none}" >&2
  exit 1
fi

# One resolved version: two would mean `dx` picks one of them, and this check must not guess.
versions="$(awk '
  $0 == "name = \"wasm-bindgen\"" {
    if ((getline line) > 0 && line ~ /^version = "/) {
      gsub(/^version = "|"$/, "", line)
      print line
    }
  }' "$lockfile")"
if [ -z "$versions" ] || [ "$(printf '%s\n' "$versions" | wc -l)" -ne 1 ]; then
  echo "expected exactly one wasm-bindgen version in ${lockfile}, found: ${versions:-none}" >&2
  exit 1
fi
version="$versions"

case "$(uname -m)" in
  x86_64) triple="x86_64-unknown-linux-musl" ;;
  aarch64 | arm64) triple="aarch64-unknown-linux-gnu" ;;
  *)
    echo "unsupported runner architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

asset="wasm-bindgen-${version}-${triple}.tar.gz"
url="https://github.com/rustwasm/wasm-bindgen/releases/download/${version}/${asset}"

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

echo "image:        ${image}"
echo "wasm-bindgen: ${version} (${triple})"
curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "${workdir}/${asset}" "$url"
curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "${workdir}/${asset}.sha256sum" "${url}.sha256sum"
# Compared by digest, not `sha256sum --check`: the published file names the asset by the path it
# had on upstream's build machine, not by its download name.
expected_sha="$(awk 'NR == 1 { print $1 }' "${workdir}/${asset}.sha256sum")"
actual_sha="$(sha256sum "${workdir}/${asset}" | awk '{ print $1 }')"
if ! printf '%s' "$expected_sha" | grep -Eq '^[0-9a-f]{64}$' || [ "$expected_sha" != "$actual_sha" ]; then
  echo "checksum mismatch for ${asset}: published '${expected_sha}', downloaded '${actual_sha}'" >&2
  exit 1
fi
tar -xzf "${workdir}/${asset}" -C "$workdir"

bin_dir="${workdir}/wasm-bindgen-${version}-${triple}"
if [ ! -x "${bin_dir}/wasm-bindgen" ]; then
  echo "${asset} has no executable wasm-bindgen at its expected path" >&2
  exit 1
fi

# No network: the binary only has to start and print its version.
if ! output="$(docker run --rm --network none --pull missing \
  --volume "${bin_dir}:/tools:ro" --entrypoint /tools/wasm-bindgen \
  "$image" --version 2>&1)"; then
  printf '%s\n' "$output" >&2
  echo "::error file=${dockerfile}::wasm-bindgen ${version} (${triple}) does not run in ${image}." \
    "If the error above names a missing GLIBC version, move the ${stage} base image to a newer" \
    "Debian release." >&2
  exit 1
fi
printf '%s\n' "$output"
if [ "$output" != "wasm-bindgen ${version}" ]; then
  echo "unexpected version output; expected 'wasm-bindgen ${version}'" >&2
  exit 1
fi

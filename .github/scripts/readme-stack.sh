#!/usr/bin/env bash
#
# Emits, as strict JSON on stdout, the README facts that live outside `Cargo.toml`: the pinned
# toolchain channel and the PostgreSQL and Redis majors the compose file runs. The README workflow
# passes this to `readme-variables` as `extra`, under `stack`, which reads the manifest itself.
#
#     bash .github/scripts/readme-stack.sh
#
# POSIX tools only, no `jq`, so it runs in a default Git for Windows shell. Every read fails
# loudly: a silent fallback would render a README that looks finished and states a version
# nothing in the repository agrees with.
set -euo pipefail

cd "$(dirname "$0")/../.."

# Reads an anchored `key = "value"` from a TOML file, against an alphabet the caller names.
#
# There is no JSON encoder here, so the pattern is the safety argument: it admits no quote and no
# backslash, which is what lets the final `printf` interpolate the value raw.
field() {
    local file="$1" key="$2" pattern="$3" value
    value="$(sed -n "s/^${key} = \"\([^\"]*\)\".*/\1/p" "${file}" | head -n1)"

    if [ -z "${value}" ]; then
        echo "readme-stack: no top-level '${key}' in ${file}" >&2
        return 1
    fi

    if ! printf '%s' "${value}" | grep -Eq "${pattern}"; then
        echo "readme-stack: '${key} = \"${value}\"' in ${file} is outside the alphabet ${pattern}" >&2
        return 1
    fi

    printf '%s' "${value}"
}

# The major version of the image the compose file runs for one service, looked up by service name
# so that a change of image keeps answering.
#
# The tag is read the way `xtask repo_lint`'s `major_of` reads it: drop an `@sha256:` digest, take
# what follows the last colon, then its leading digits. `pg18` and `8-alpine` both answer.
image_major() {
    local service="$1" major
    major="$(
        awk -v want="${service}:" '
            /^  [a-z][a-z0-9_-]*:$/ { service = $1 }
            service == want && $1 == "image:" {
                tag = $2
                sub(/@.*$/, "", tag)
                sub(/^.*:/, "", tag)
                if (match(tag, /^[a-z]*[0-9]+/)) {
                    version = substr(tag, RSTART, RLENGTH)
                    gsub(/[^0-9]/, "", version)
                    print version
                }
                exit
            }
        ' deploy/docker-compose.yml
    )"

    if [ -z "${major}" ]; then
        echo "readme-stack: no image major for the '${service}' service in deploy/docker-compose.yml" >&2
        return 1
    fi

    printf '%s' "${major}"
}

toolchain="$(field rust-toolchain.toml channel '^[0-9]+(\.[0-9]+){0,2}$')"
postgres="$(image_major postgres)"
redis="$(image_major redis)"

printf '{"stack":{"channel":"%s","postgres":"%s","redis":"%s"}}\n' "${toolchain}" "${postgres}" "${redis}"

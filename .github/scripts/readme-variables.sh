#!/usr/bin/env bash
#
# Emits the variable payload for `.github/templates/README.md.hbs` as strict JSON on stdout.
#
# Every version, licence and one-liner the README quotes has a home somewhere else in the
# repository, and every one of them was a hand-maintained copy of that home until this existed.
# Two of the first five were already wrong: the tech-stack line advertised PostgreSQL 17 and
# Redis 7 while the deployable compose file had been on 18 and 8 for months, and nothing failed,
# because prose is not a gate.
#
# `version`, `license` and `description` were added when the README moved onto the estate
# standard. The release version in particular is the one a badge and an image tag both quote, and
# release-please rewrites `[workspace.package] version` on every release — a typed copy would
# have been stale within a week.
#
# Run it yourself to see what CI will render with:
#
#     bash .github/scripts/readme-variables.sh
#
# Deliberately POSIX tools only, no `jq`: it is absent from a default Git for Windows shell, and
# a script that only runs on the CI runner is a script nobody checks their edit against.
#
# Every read fails loudly rather than defaulting. A silent fallback here renders a README that
# looks finished and states a version nothing in the repository agrees with — which is the exact
# failure this file exists to end.
set -euo pipefail

cd "$(dirname "$0")/../.."

# Reads an anchored `key = "value"` from a TOML file, against an alphabet the caller names.
#
# The pattern is the whole safety argument. There is no JSON encoder here, so a value that
# reached the `printf` at the bottom carrying a quote or a backslash would emit a payload
# `render-template` refuses to parse — or, worse, one it parses as something else. Every caller
# passes a pattern that admits neither, which is why the printf can interpolate raw.
#
# Anchoring is what keeps it to the table it means: a dependency's version sits inside an inline
# table (`figment = { version = "0.10", … }`) and never starts a line, and every workspace member
# inherits with `edition.workspace = true` from a manifest this never opens. `[workspace.package]`
# is the only table in `Cargo.toml` whose keys start a line and are spelled the way this reads
# them, so `version`, `license` and `description` all mean the workspace's own.
field() {
    local file="$1" key="$2" pattern="$3" value
    value="$(sed -n "s/^${key} = \"\([^\"]*\)\".*/\1/p" "${file}" | head -n1)"

    if [ -z "${value}" ]; then
        echo "readme-variables: no top-level '${key}' in ${file}" >&2
        return 1
    fi

    if ! printf '%s' "${value}" | grep -Eq "${pattern}"; then
        echo "readme-variables: '${key} = \"${value}\"' in ${file} is outside the alphabet ${pattern}" >&2
        return 1
    fi

    printf '%s' "${value}"
}

# The major version of the image the compose file runs for one service.
#
# By service name rather than by image name, so the answer survives the repository moving —
# `postgres:18-alpine` became `pgvector/pgvector:pg18` when migration 0027 made the extension a
# hard dependency, and a grep for the old name would have started reporting nothing.
#
# The tag is read the way `xtask repo_lint`'s `major_of` reads it, because it is the same
# question about the same line: drop an `@sha256:…` digest first (its separator is the one the
# tag uses), take what follows the last colon, then the leading digits of that — `pg18` and
# `8-alpine` both answer.
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
        echo "readme-variables: no image major for the '${service}' service in deploy/docker-compose.yml" >&2
        return 1
    fi

    printf '%s' "${major}"
}

version="$(field Cargo.toml version '^[0-9]+\.[0-9]+\.[0-9]+$')"
license="$(field Cargo.toml license '^[A-Za-z0-9][A-Za-z0-9 ().+-]*$')"
description="$(field Cargo.toml description "^[A-Za-z0-9][A-Za-z0-9 ,.:;()/'-]*\$")"
edition="$(field Cargo.toml edition '^[0-9]{4}$')"
msrv="$(field Cargo.toml rust-version '^[0-9]+(\.[0-9]+){0,2}$')"
toolchain="$(field rust-toolchain.toml channel '^[0-9]+(\.[0-9]+){0,2}$')"
postgres="$(image_major postgres)"
redis="$(image_major redis)"

printf '{"version":"%s","license":"%s","description":"%s","edition":"%s","msrv":"%s","toolchain":"%s","postgres":"%s","redis":"%s"}\n' \
    "${version}" "${license}" "${description}" "${edition}" "${msrv}" "${toolchain}" "${postgres}" "${redis}"

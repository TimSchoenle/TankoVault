# Local tooling. `just` with no arguments lists what there is.
#
# Everything a contributor has to run by hand lives here rather than in a script under
# `.github/scripts/`, so that the command a README quotes, the command CI runs and the command a
# developer types are one string. Recipes that only wrap `cargo` are here for the same reason:
# the flags are the part people get wrong.
#
#     https://github.com/casey/just
#
# There is deliberately no recipe that *checks* the generated artefacts. Checking is
# `cargo run -p xtask -- config-contract`, which reads the committed documents and the Dockerfile
# regions and reports where the drift is; a second implementation here would be a second opinion.
# The built-image half of the same question is `.github/scripts/verify-config-contract.sh`, which
# needs an image and a build's own export and so cannot live here at all.

# The generator, and where its output belongs. These four lines are the only per-repository part
# of this file. `package` rather than `example`: this generator is a binary, because it takes a
# `--service` naming which of the nine published images to describe, and a `--service` is not
# something `cargo run --example` has anywhere to put.
package := "tankovault-config-contract"
features := ""
contracts := "docs/contracts"
dockerfile := "deploy/docker/Dockerfile"

# Exported to every recipe, matching the workflow-level `env:` in `ci.yml`: the `sqlx` macros
# resolve against the committed `.sqlx/` cache rather than against a live database. Without it a
# developer whose `DATABASE_URL` points at an unmigrated database gets a compile error from a
# macro, about a query, in a crate they were not editing.
export SQLX_OFFLINE := "true"

# The markers `--format dockerfile` emits around the LABEL block. Defined by terrace-config, not
# by this repository: cutting the region by line count reads correctly right up until a fourth
# label is added, and then compares two of three lines and passes.
begin := "# terrace-config:labels:begin"
end := "# terrace-config:labels:end"

[private]
default:
    @just --list --unsorted

[doc('Rewrite everything generated from the nine service config roots')]
regenerate: contract-json dockerfile-labels

[doc('Name every service that publishes an image, one per line')]
[group('generate')]
services:
    #!/usr/bin/env bash
    set -euo pipefail
    args=(run --quiet -p "{{ package }}")
    [ -n "{{ features }}" ] && args+=(--features "{{ features }}")
    cargo "${args[@]}" -- --services

[doc('Print one rendering of one service: json|markdown|markdown-loader|markdown-keys|toml|json-schema|contract|labels|dockerfile')]
[group('generate')]
render service format:
    #!/usr/bin/env bash
    set -euo pipefail
    args=(run --quiet -p "{{ package }}")
    [ -n "{{ features }}" ] && args+=(--features "{{ features }}")
    cargo "${args[@]}" -- --service "{{ service }}" --format "{{ format }}"

# Rendered without `--version`, `--revision` or `--created`, so it is byte-reproducible across
# rebuilds and releases: the committed copies describe the configuration surface, and the copies
# inside the images additionally name the build they came from. That is what lets the committed
# copy, the copy exported by the build, the copy inside the image and the artifact attached to
# the digest all be compared as bytes.

[doc('Rewrite the committed contract document of every service')]
[group('generate')]
contract-json:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p "{{ contracts }}"
    for service in $(just services); do
        just render "$service" contract > "{{ contracts }}/${service}.json"
        echo "wrote {{ contracts }}/${service}.json"
    done

# One rendering for every region, and any service will do: all three values are constants of the
# *deployment* — the envelope version, the in-image path and the loader prefix — which is what
# lets one block serve nine images and why this does not render nine identical ones.
#
# Every marked region is rewritten, not the first: this Dockerfile carries three runtime stages
# and each needs the labels, so a rewrite that stopped at the first would leave two stale and
# report success. The file is rebuilt around the markers rather than substituted in place —
# `sed` cannot replace a multi-line block portably, and `--format dockerfile` emits both markers
# along with the block between them.

[doc('Rewrite every LABEL region in the Dockerfile')]
[group('generate')]
dockerfile-labels:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! grep -qF '{{ begin }}' "{{ dockerfile }}" || ! grep -qF '{{ end }}' "{{ dockerfile }}"; then
        echo "error: {{ dockerfile }} carries no '{{ begin }}' … '{{ end }}' region, so the" >&2
        echo "       generated LABEL block has nowhere to go. Paste the output of" >&2
        echo "       'just render <service> dockerfile' into it once, markers included." >&2
        exit 1
    fi
    block="$(mktemp)"
    rewritten="$(mktemp)"
    trap 'rm -f "$block" "$rewritten"' EXIT
    just render "$(just services | head -n 1)" dockerfile > "$block"
    awk -v b='{{ begin }}' -v e='{{ end }}' -v f="$block" '
        skipping { if ($0 == e) skipping = 0; next }
        $0 == b {
            while ((getline line < f) > 0) print line
            close(f)
            skipping = 1
            next
        }
        { print }
    ' "{{ dockerfile }}" > "$rewritten"
    mv "$rewritten" "{{ dockerfile }}"
    echo "wrote the LABEL regions in {{ dockerfile }}"

[doc('Format, lint and test — what a pull request is going to run anyway')]
[group('check')]
verify: fmt lint test

[group('check')]
fmt:
    cargo fmt --all

# `--workspace --all-targets` rather than `--all-features`, because that is what `ci.yml`'s `lint`
# job runs: the feature combinations are a job of their own, and compiling the tree a second way
# for a check nobody is waiting on is how a local gate stops being run.
[doc('Clippy, with the flags the gate uses')]
[group('check')]
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# `SQLX_OFFLINE` is set at the workflow level in `ci.yml` and has to be set here too: the `sqlx`
# macros resolve against `.sqlx/` rather than against a live database, and without it a developer
# with no `DATABASE_URL` gets a compile error that says nothing about the cause.
[doc('The test suite, resolving the sqlx macros offline')]
[group('check')]
test:
    SQLX_OFFLINE=true cargo test --workspace --all-targets

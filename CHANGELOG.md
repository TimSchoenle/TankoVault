# Changelog

Notable changes, newest first. Format loosely follows [Keep a Changelog]; this project has not
cut a release yet, so everything is under *Unreleased* and every crate is at `0.1.0`.

Releases are automated with [release-please](https://github.com/googleapis/release-please):
conventional commits drive a release pull request, and merging it tags the repository and
publishes the nine service images. See [`docs/RELEASING.md`](docs/RELEASING.md).

Publishing is no longer gated. It was blocked by `OP-6` until 2026-08-01 — `wreq-util` was
GPL-3.0 and distributing the images is *conveying* — and after that by the absence of a
`LICENSE` (`OPS-10.4`). Both are resolved: the project is licensed under
[PolyForm Noncommercial 1.0.0](LICENSE), and merging a release pull request now pushes.

## [3.2.0](https://github.com/TimSchoenle/TankoVault/compare/v3.1.0...v3.2.0) (2026-08-08)


### Features

* add 2fa ([#139](https://github.com/TimSchoenle/TankoVault/issues/139)) ([288ace0](https://github.com/TimSchoenle/TankoVault/commit/288ace009fef068d7188d263406d0c95a52cfceb))


### Bug Fixes

* **ci:** split release-please's tagging pass from its pull-request pass ([#140](https://github.com/TimSchoenle/TankoVault/issues/140)) ([785ce01](https://github.com/TimSchoenle/TankoVault/commit/785ce01d2c4df3f9d48ab42ffd8ca2757bf1d0d7))


### Miscellaneous

* **deps:** update rust crate data-encoding to v2.11.1 ([#141](https://github.com/TimSchoenle/TankoVault/issues/141)) ([2049224](https://github.com/TimSchoenle/TankoVault/commit/2049224420294085a5473afd2528afb03f4a2a94))

## [3.1.0](https://github.com/TimSchoenle/TankoVault/compare/v3.0.0...v3.1.0) (2026-08-07)


### Features

* **domain,api:** give the installer's first account a super user grant ([#135](https://github.com/TimSchoenle/TankoVault/issues/135)) ([8416fd4](https://github.com/TimSchoenle/TankoVault/commit/8416fd418bd3d5dcffb303e5c2ea6b9b7663efdd))
* **frontend:** installer options, an in-app autostart switch, and a theme-aware scrollbar ([#136](https://github.com/TimSchoenle/TankoVault/issues/136)) ([76a09e1](https://github.com/TimSchoenle/TankoVault/commit/76a09e1df7b939cd0252d7799e2ba6e6ae8a7c52))
* **frontend:** page discover on a scroll sentinel and address the position ([#137](https://github.com/TimSchoenle/TankoVault/issues/137)) ([ccd7182](https://github.com/TimSchoenle/TankoVault/commit/ccd7182a53d325d9296a442cd5dacfdfe463d64b))


### Bug Fixes

* **deps:** update rust crate sha2 to 0.11 ([#100](https://github.com/TimSchoenle/TankoVault/issues/100)) ([47417a4](https://github.com/TimSchoenle/TankoVault/commit/47417a400b9db7e84b0dd7b05afa0795dde56a8c))

## [3.0.0](https://github.com/TimSchoenle/TankoVault/compare/v2.1.0...v3.0.0) (2026-08-07)


### ⚠ BREAKING CHANGES

* **catalogue,recsys:** gate adult content behind a deployment flag and a per-reader opt-in ([#129](https://github.com/TimSchoenle/TankoVault/issues/129))

### Features

* **api,frontend:** let readers choose which source a series opens on ([#127](https://github.com/TimSchoenle/TankoVault/issues/127)) ([d0e6dd5](https://github.com/TimSchoenle/TankoVault/commit/d0e6dd59de3ac855e63d7363056ea844051483cd))
* **catalogue,recsys:** gate adult content behind a deployment flag and a per-reader opt-in ([#129](https://github.com/TimSchoenle/TankoVault/issues/129)) ([2feeb24](https://github.com/TimSchoenle/TankoVault/commit/2feeb24db5d64d4da24caa0b9324889cd42aff45))
* **frontend:** update the desktop client from the github releases ([#132](https://github.com/TimSchoenle/TankoVault/issues/132)) ([9d22618](https://github.com/TimSchoenle/TankoVault/commit/9d22618aedca620a804a0bf9083a797faf6935bf))


### Bug Fixes

* **ci:** retry the release's by-digest push past GHCR's throttled token ([#133](https://github.com/TimSchoenle/TankoVault/issues/133)) ([4fed264](https://github.com/TimSchoenle/TankoVault/commit/4fed264923e44c34e903ac5aea2bbc736db6f6f5))
* **matching:** stop an alias hit attaching a source to the wrong series ([#131](https://github.com/TimSchoenle/TankoVault/issues/131)) ([6da14d5](https://github.com/TimSchoenle/TankoVault/commit/6da14d53447c6173a4f690e29c20d17f3d25bfa5))

## [2.1.0](https://github.com/TimSchoenle/TankoVault/compare/v2.0.0...v2.1.0) (2026-08-07)


### Features

* **frontend:** keep the desktop refresh credential in the OS keyring ([#121](https://github.com/TimSchoenle/TankoVault/issues/121)) ([3c895ed](https://github.com/TimSchoenle/TankoVault/commit/3c895ed83d1f187b3d7acafa588c600e53414e09))


### Bug Fixes

* **ci:** make the desktop release actually ship ([#124](https://github.com/TimSchoenle/TankoVault/issues/124)) ([906a49f](https://github.com/TimSchoenle/TankoVault/commit/906a49f71d6e16d257ad2b27a4675945c6617cde))
* **ci:** retry the release's registry calls past GHCR throttling ([#128](https://github.com/TimSchoenle/TankoVault/issues/128)) ([7f27c14](https://github.com/TimSchoenle/TankoVault/commit/7f27c145c45f1d47f1251529e14e2369be0f1e28))
* **frontend:** size result grids to the window so a page fills whole rows ([#122](https://github.com/TimSchoenle/TankoVault/issues/122)) ([79e13f9](https://github.com/TimSchoenle/TankoVault/commit/79e13f9793849a7faa56b3afcc10618f81e1d1f2))


### Performance Improvements

* **db:** make the watchlist load in one page's worth of work, and count one source per provider ([#125](https://github.com/TimSchoenle/TankoVault/issues/125)) ([d3011bd](https://github.com/TimSchoenle/TankoVault/commit/d3011bdad8a4e814e750b0d859074007282bbe80))

## [2.0.0](https://github.com/TimSchoenle/TankoVault/compare/v1.5.2...v2.0.0) (2026-08-07)


### ⚠ BREAKING CHANGES

* **notifications:** readable rows, grouping, and preferences that gate delivery ([#117](https://github.com/TimSchoenle/TankoVault/issues/117))

### Features

* **frontend:** build and publish a native desktop client ([#113](https://github.com/TimSchoenle/TankoVault/issues/113)) ([a213bc0](https://github.com/TimSchoenle/TankoVault/commit/a213bc092c5c9c551246bc0b0e33c087d0c1dd8a))
* **matching,sync:** journal every automatic merge and sync decision, and make both revertible ([#120](https://github.com/TimSchoenle/TankoVault/issues/120)) ([789c828](https://github.com/TimSchoenle/TankoVault/commit/789c828a9400e7ceee8560c0b376323817d53ab4))
* **notifications:** readable rows, grouping, and preferences that gate delivery ([#117](https://github.com/TimSchoenle/TankoVault/issues/117)) ([c07ace7](https://github.com/TimSchoenle/TankoVault/commit/c07ace7842074eea5409041a126211c2d9753438))


### Bug Fixes

* **db:** clear the dense indices in their own statement ([#118](https://github.com/TimSchoenle/TankoVault/issues/118)) ([6d47743](https://github.com/TimSchoenle/TankoVault/commit/6d477435a9a85193ef368e85dc6bbe668a7d3f7e))


### CI

* pin SOURCE_DATE_EPOCH to a constant so the build cache hits ([#115](https://github.com/TimSchoenle/TankoVault/issues/115)) ([682585b](https://github.com/TimSchoenle/TankoVault/commit/682585b87102a8d221d3658c9ca1e4aa7ac13a54))


### Miscellaneous

* **deps:** update timschoenle/actions/.github/workflows/maintenance-auto-approve-renovate.yaml to vworkflows-maintenance-auto-approve-renovate-v1.4.18 ([#119](https://github.com/TimSchoenle/TankoVault/issues/119)) ([bcbbf8c](https://github.com/TimSchoenle/TankoVault/commit/bcbbf8c7febb38dd80b04e3ae59d220f673d8dd1))

## [1.5.2](https://github.com/TimSchoenle/TankoVault/compare/v1.5.1...v1.5.2) (2026-08-07)


### Bug Fixes

* **adapters:** read kunmanga's latest feed from the updates slider ([#114](https://github.com/TimSchoenle/TankoVault/issues/114)) ([3efc407](https://github.com/TimSchoenle/TankoVault/commit/3efc407b51836fdd0b04b3e480c4ed7dc602d47d))


### CI

* stop SOURCE_DATE_EPOCH invalidating every build cache key ([#110](https://github.com/TimSchoenle/TankoVault/issues/110)) ([fa369db](https://github.com/TimSchoenle/TankoVault/commit/fa369db9f656110d3d3bc3eb8e9ed25209399e49))


### Build System

* check out generated artefacts as LF, and make rustfmt emit it ([#111](https://github.com/TimSchoenle/TankoVault/issues/111)) ([e695adb](https://github.com/TimSchoenle/TankoVault/commit/e695adbbebb2a1342c88286ab429baf5053fd3d0))

## [1.5.1](https://github.com/TimSchoenle/TankoVault/compare/v1.5.0...v1.5.1) (2026-08-06)


### Bug Fixes

* **recsys:** stop a dead build holding the claim forever ([#108](https://github.com/TimSchoenle/TankoVault/issues/108)) ([31b6b62](https://github.com/TimSchoenle/TankoVault/commit/31b6b6268356f38796edf0b8abcf9990df9bffa5))

## [1.5.0](https://github.com/TimSchoenle/TankoVault/compare/v1.4.3...v1.5.0) (2026-08-06)


### Features

* **console:** operator console v2 — phases 1–3 ([#106](https://github.com/TimSchoenle/TankoVault/issues/106)) ([b433660](https://github.com/TimSchoenle/TankoVault/commit/b43366091155401d5f3f1a6daee869b19fcea412))


### Miscellaneous

* **deps:** update timschoenle/actions/.github/workflows/maintenance-timed-auto-pr-approve.yaml to vworkflows-maintenance-timed-auto-pr-approve-v1.2.29 ([#104](https://github.com/TimSchoenle/TankoVault/issues/104)) ([8ee2b70](https://github.com/TimSchoenle/TankoVault/commit/8ee2b7032605c4e0a4a5a7126be98387e508d6e9))
* **deps:** update timschoenle/actions/actions/helm/update-chart-version to vactions-helm-update-chart-version-v1.6.1 ([#105](https://github.com/TimSchoenle/TankoVault/issues/105)) ([107d4e0](https://github.com/TimSchoenle/TankoVault/commit/107d4e01ac36001b78755ac69761042320a7e6a0))

## [1.4.3](https://github.com/TimSchoenle/TankoVault/compare/v1.4.2...v1.4.3) (2026-08-06)


### Documentation

* **console:** plan the operator console v2 overhaul ([#102](https://github.com/TimSchoenle/TankoVault/issues/102)) ([f42aa3e](https://github.com/TimSchoenle/TankoVault/commit/f42aa3eec0974004b54d799981e1b2e3533bcb25))

## [1.4.2](https://github.com/TimSchoenle/TankoVault/compare/v1.4.1...v1.4.2) (2026-08-06)


### Miscellaneous

* **deps:** update all non-major action updates (patch) ([#97](https://github.com/TimSchoenle/TankoVault/issues/97)) ([fb5cd40](https://github.com/TimSchoenle/TankoVault/commit/fb5cd402039ee7dc17a00a6b7ac899c0692d2788))
* **deps:** update cargo non-major (patch) ([#99](https://github.com/TimSchoenle/TankoVault/issues/99)) ([e5a643c](https://github.com/TimSchoenle/TankoVault/commit/e5a643c6b808fa1217eec99c1695f2e8dd0acd7b))
* **deps:** update debian:13-slim docker digest to 3a39a05 ([#96](https://github.com/TimSchoenle/TankoVault/issues/96)) ([9575583](https://github.com/TimSchoenle/TankoVault/commit/9575583ea5a1a3c8b15e7f5212a6fd087fe5e60a))

## [1.4.1](https://github.com/TimSchoenle/TankoVault/compare/v1.4.0...v1.4.1) (2026-08-05)


### Bug Fixes

* **recsys:** stop the taste profile 500ing on a reader with a long series ([#92](https://github.com/TimSchoenle/TankoVault/issues/92)) ([bedb6a2](https://github.com/TimSchoenle/TankoVault/commit/bedb6a2d78aac5cf7f95cd30fb825a142e26c0d1))

## [1.4.0](https://github.com/TimSchoenle/TankoVault/compare/v1.3.3...v1.4.0) (2026-08-05)


### Features

* **recsys:** wire every published recommendation surface into the SPA ([#91](https://github.com/TimSchoenle/TankoVault/issues/91)) ([f2e6ab7](https://github.com/TimSchoenle/TankoVault/commit/f2e6ab7559409d80a3e7c31d22de247ba52ee0ee))


### Code Refactoring

* split the six largest source files into cohesive modules ([#89](https://github.com/TimSchoenle/TankoVault/issues/89)) ([53b11b2](https://github.com/TimSchoenle/TankoVault/commit/53b11b2a1d1b42073678a7cc077702f57a339196))

## [1.3.3](https://github.com/TimSchoenle/TankoVault/compare/v1.3.2...v1.3.3) (2026-08-05)


### Bug Fixes

* **metadata:** make the priority config govern both writers, not one ([#87](https://github.com/TimSchoenle/TankoVault/issues/87)) ([8c0d9a3](https://github.com/TimSchoenle/TankoVault/commit/8c0d9a3db0e369bf50d3ce2924c13c82d95d53c1))

## [1.3.2](https://github.com/TimSchoenle/TankoVault/compare/v1.3.1...v1.3.2) (2026-08-05)


### CI

* stop auto-fix rebuilding the world, and bound every apt-get ([#84](https://github.com/TimSchoenle/TankoVault/issues/84)) ([eb68989](https://github.com/TimSchoenle/TankoVault/commit/eb68989832dc2cf4ff81e08b8fa9dd26203e7b51))
* strip the newline that made every cosign signature fail ([#86](https://github.com/TimSchoenle/TankoVault/issues/86)) ([bcc4a7d](https://github.com/TimSchoenle/TankoVault/commit/bcc4a7d0d9d0e2e1268a0eabf7940305a6c9b2af))

## [1.3.1](https://github.com/TimSchoenle/TankoVault/compare/v1.3.0...v1.3.1) (2026-08-05)


### CI

* cut the release critical path and fix keyless signing ([#82](https://github.com/TimSchoenle/TankoVault/issues/82)) ([dd79d2a](https://github.com/TimSchoenle/TankoVault/commit/dd79d2a2b7973277ceb368060cd6f67d64086c91))

## [1.3.0](https://github.com/TimSchoenle/TankoVault/compare/v1.2.2...v1.3.0) (2026-08-05)


### Features

* **recsys:** the suggestion-system design, pgvector, and a guard on merges ([#72](https://github.com/TimSchoenle/TankoVault/issues/72)) ([5d8eaf4](https://github.com/TimSchoenle/TankoVault/commit/5d8eaf400d4b8663e483b5d020937cd5d051b846))


### Performance Improvements

* **dashboard:** read only the unread tail, and stop Postgres running on defaults ([#78](https://github.com/TimSchoenle/TankoVault/issues/78)) ([cebc25d](https://github.com/TimSchoenle/TankoVault/commit/cebc25d5ad42889806e81e994e73aa2bcc1ba147))


### CI

* fold every auto-fix workflow into one job and one commit ([#80](https://github.com/TimSchoenle/TankoVault/issues/80)) ([52516ec](https://github.com/TimSchoenle/TankoVault/commit/52516ec927bda41542f1f3c66470eaacbc80e374))


### Miscellaneous

* **deps:** update redis:8-alpine docker digest to 978f0e0 ([#77](https://github.com/TimSchoenle/TankoVault/issues/77)) ([432e966](https://github.com/TimSchoenle/TankoVault/commit/432e96633c913a62bc76dc48e316c0296436bcca))
* **deps:** update taiki-e/install-action action to v2.85.7 ([#79](https://github.com/TimSchoenle/TankoVault/issues/79)) ([5edef72](https://github.com/TimSchoenle/TankoVault/commit/5edef72b0a18238519cc134e04a4c8b48d70a3ca))
* **deps:** update zizmorcore/zizmor-action action to v0.6.2 ([#75](https://github.com/TimSchoenle/TankoVault/issues/75)) ([e3b2b77](https://github.com/TimSchoenle/TankoVault/commit/e3b2b77e5d3fa100ba6fdb0bbdb9b06bb8e264da))

## [1.2.2](https://github.com/TimSchoenle/TankoVault/compare/v1.2.1...v1.2.2) (2026-08-04)


### Bug Fixes

* **ci:** sync the 1.2.1 generated artefacts and stop the release PR losing its own fixups ([#73](https://github.com/TimSchoenle/TankoVault/issues/73)) ([82322ab](https://github.com/TimSchoenle/TankoVault/commit/82322ab65538a7d9833162cc379d3737504a3dd7))

## [1.2.1](https://github.com/TimSchoenle/TankoVault/compare/v1.2.0...v1.2.1) (2026-08-04)


### Performance Improvements

* take the console rollups off the request path, stop scoring trigram candidates row by row, and fix a batch-aborting chapter upsert ([#69](https://github.com/TimSchoenle/TankoVault/issues/69)) ([231d644](https://github.com/TimSchoenle/TankoVault/commit/231d6442b46d2c4ac96833700f9c2e243afa0505))


### Miscellaneous

* **deps:** update taiki-e/install-action action to v2.85.6 ([#71](https://github.com/TimSchoenle/TankoVault/issues/71)) ([e86c2f0](https://github.com/TimSchoenle/TankoVault/commit/e86c2f05e4183484b0b9c23665616180d388214d))

## [1.2.0](https://github.com/TimSchoenle/TankoVault/compare/v1.1.1...v1.2.0) (2026-08-04)


### Features

* layout at scale, a bottom tab bar, and operator-published legal documents ([#68](https://github.com/TimSchoenle/TankoVault/issues/68)) ([43837ad](https://github.com/TimSchoenle/TankoVault/commit/43837ad749a3530d9b77b495b417cd885649ce3d))
* **observability:** a described metric catalogue, 23 new metrics, and a gate on both ([#66](https://github.com/TimSchoenle/TankoVault/issues/66)) ([7838410](https://github.com/TimSchoenle/TankoVault/commit/7838410bb62986d6102db312847b1fe6a0229b87))

## [1.1.1](https://github.com/TimSchoenle/TankoVault/compare/v1.1.0...v1.1.1) (2026-08-04)


### Bug Fixes

* **frontend:** per-page titles, background fill past the fold, no AniList pill — and page the notifications inbox ([#62](https://github.com/TimSchoenle/TankoVault/issues/62)) ([ec32688](https://github.com/TimSchoenle/TankoVault/commit/ec326889d9542d99231f515b348c7d51f0123a90))


### Miscellaneous

* **deps:** update timschoenle/actions/actions/common/commit-changes to vactions-common-commit-changes-v1.3.0 ([#64](https://github.com/TimSchoenle/TankoVault/issues/64)) ([dbacb87](https://github.com/TimSchoenle/TankoVault/commit/dbacb87069021fb5f97b324007a8cfbe6a00bca6))
* **deps:** update timschoenle/actions/actions/rust/auto-format to vactions-rust-auto-format-v1.1.9 ([#63](https://github.com/TimSchoenle/TankoVault/issues/63)) ([c20efd0](https://github.com/TimSchoenle/TankoVault/commit/c20efd0e1101a684d283058201f95a892fbeccf6))

## [1.1.0](https://github.com/TimSchoenle/TankoVault/compare/v1.0.0...v1.1.0) (2026-08-04)


### Features

* **ci:** publish only the images a release changed ([#61](https://github.com/TimSchoenle/TankoVault/issues/61)) ([4f3d4df](https://github.com/TimSchoenle/TankoVault/commit/4f3d4dffb61d2e807bbfd9634e9f691731ae2b38))


### Bug Fixes

* restore a green CI on main after the 1.0.0 release ([#57](https://github.com/TimSchoenle/TankoVault/issues/57)) ([f74446e](https://github.com/TimSchoenle/TankoVault/commit/f74446e843b7b2d9ae6fb89a2024a4d5683eebaa))
* VACUUM the plan fixture so GIN costs stop racing autovacuum ([#60](https://github.com/TimSchoenle/TankoVault/issues/60)) ([cc3c6ae](https://github.com/TimSchoenle/TankoVault/commit/cc3c6ae1fa5f96c360df6b4d3c6549c1c935d1fd))


### CI

* move the layer cache to GHCR and group the Rust caches ([#59](https://github.com/TimSchoenle/TankoVault/issues/59)) ([36d6972](https://github.com/TimSchoenle/TankoVault/commit/36d6972ff1194498fc20ef0ff779af371d3dc661))

## [1.0.0](https://github.com/TimSchoenle/TankoVault/compare/v0.4.1...v1.0.0) (2026-08-03)


### ⚠ BREAKING CHANGES

* replace FlareSolverr with TRAWL as the solver back-end ([#55](https://github.com/TimSchoenle/TankoVault/issues/55))

### Features

* replace FlareSolverr with TRAWL as the solver back-end ([#55](https://github.com/TimSchoenle/TankoVault/issues/55)) ([646cd03](https://github.com/TimSchoenle/TankoVault/commit/646cd03309639ec19f43ebd1a601cf9e1a6fa98f))


### Performance Improvements

* improve slow SQL peformance ([c0c05d5](https://github.com/TimSchoenle/TankoVault/commit/c0c05d5d4d802c34d662b6300bf002f799239f9f))


### Tests

* add sql runtime cost tests ([9b66ce1](https://github.com/TimSchoenle/TankoVault/commit/9b66ce1d554e2511c1c84e7c2f3fac645ef0d953))


### CI

* propose the chart bump after a release publishes ([#54](https://github.com/TimSchoenle/TankoVault/issues/54)) ([e33bbc1](https://github.com/TimSchoenle/TankoVault/commit/e33bbc10a9f0129d9bad1e21a14b910da590166c))


### Miscellaneous

* **deps:** update actions/download-artifact action to v8 ([#40](https://github.com/TimSchoenle/TankoVault/issues/40)) ([70c0d6a](https://github.com/TimSchoenle/TankoVault/commit/70c0d6a6ff3d0e376ea1fb1a3406f6c7bee61812))
* **deps:** update actions/download-artifact action to v8 ([#56](https://github.com/TimSchoenle/TankoVault/issues/56)) ([17e3482](https://github.com/TimSchoenle/TankoVault/commit/17e3482bc07c57b3d74c373e95fda7d51a464ed9))
* **deps:** update timschoenle/actions/.github/workflows/maintenance-auto-approve-renovate.yaml to vworkflows-maintenance-auto-approve-renovate-v1.4.17 ([#48](https://github.com/TimSchoenle/TankoVault/issues/48)) ([610b2c6](https://github.com/TimSchoenle/TankoVault/commit/610b2c694d8fb7635ced01cad69898bffee72559))
* **deps:** update timschoenle/actions/.github/workflows/maintenance-timed-auto-pr-approve.yaml to vworkflows-maintenance-timed-auto-pr-approve-v1.2.28 ([#53](https://github.com/TimSchoenle/TankoVault/issues/53)) ([a4d2b8b](https://github.com/TimSchoenle/TankoVault/commit/a4d2b8b0c2739d7c238e8a881f9f68f2285b1b7f))


### Dependencies

* **deps:** lock file maintenance ([#49](https://github.com/TimSchoenle/TankoVault/issues/49)) ([6ce45cf](https://github.com/TimSchoenle/TankoVault/commit/6ce45cf111880cbcca54b3b79e66f40ad4fed719))

## [0.4.1](https://github.com/TimSchoenle/TankoVault/compare/v0.4.0...v0.4.1) (2026-08-02)


### Bug Fixes

* config symlink handling and test ([085f8d7](https://github.com/TimSchoenle/TankoVault/commit/085f8d7222f14e51bf1418ae971997d148bcdcfc))


### Miscellaneous

* cleanup ([8008e67](https://github.com/TimSchoenle/TankoVault/commit/8008e679126fc2c3794f257ffd82a5823b2b264d))

## [0.4.0](https://github.com/TimSchoenle/TankoVault/compare/v0.3.2...v0.4.0) (2026-08-02)


### Features

* add chapter adapter outlier detection ([9cc7560](https://github.com/TimSchoenle/TankoVault/commit/9cc7560395ab2f1f9cdd78c2832aec8603e868dc))


### CI

* fix GHCR publish ([a442379](https://github.com/TimSchoenle/TankoVault/commit/a442379ec15a5647741d122c91ad4f00932ca784))
* improve container caching & build times ([d988948](https://github.com/TimSchoenle/TankoVault/commit/d98894801d04571f83b543d7a18133ec6c88e5cd))

## [0.3.2](https://github.com/TimSchoenle/TankoVault/compare/v0.3.1...v0.3.2) (2026-08-02)


### Bug Fixes

* dockerhub release ([9a774dc](https://github.com/TimSchoenle/TankoVault/commit/9a774dc24c556e3a0502d2640527b4db3fa83047))

## [0.3.1](https://github.com/TimSchoenle/TankoVault/compare/v0.3.0...v0.3.1) (2026-08-02)


### Bug Fixes

* GHCR release ([c126e22](https://github.com/TimSchoenle/TankoVault/commit/c126e22e4ff445187e50a1a5ff3f6f9017b5253c))

## [0.3.0](https://github.com/TimSchoenle/TankoVault/compare/v0.2.1...v0.3.0) (2026-08-02)


### Features

* add file-backed configuration layers ([31cd512](https://github.com/TimSchoenle/TankoVault/commit/31cd5127731720c7ffeb85f5ab9669a2ba16b5fd))


### CI

* fix failing container structure tests ([30b857e](https://github.com/TimSchoenle/TankoVault/commit/30b857efdbfc2a39e37e980b983562620db2fe71))


### Build System

* add license ([c1db408](https://github.com/TimSchoenle/TankoVault/commit/c1db4086f2affbf56e20540ce13ec125257135ae))
* ship with third party licenses ([d5ce20b](https://github.com/TimSchoenle/TankoVault/commit/d5ce20b6db95434b76a672a4957ae7dc4cfb38de))

## [0.2.1](https://github.com/TimSchoenle/TankoVault/compare/v0.2.0...v0.2.1) (2026-08-02)


### Bug Fixes

* release please not triggering release ([6abbebf](https://github.com/TimSchoenle/TankoVault/commit/6abbebfc089294a569521ffd11ac82dc30ac11b3))


### CI

* remove latest docker target ([7e7696d](https://github.com/TimSchoenle/TankoVault/commit/7e7696d3e69af122533923ceec018ee9de555e74))


### Miscellaneous

* lockfile update ([8f6efc6](https://github.com/TimSchoenle/TankoVault/commit/8f6efc62fe8dae73f5ca57f7c2d4ea5ec658c4d0))

## [0.2.0](https://github.com/TimSchoenle/TankoVault/compare/v0.1.0...v0.2.0) (2026-08-02)


### Features

* add anilist meta enrincher ([#3](https://github.com/TimSchoenle/TankoVault/issues/3)) ([39346df](https://github.com/TimSchoenle/TankoVault/commit/39346df2fdc24882c4636efd9c687d04e4092dff))
* add independent read history tracking feature ([#2](https://github.com/TimSchoenle/TankoVault/issues/2)) ([002f9a6](https://github.com/TimSchoenle/TankoVault/commit/002f9a642c4c670ee20cc8c0a403520a97ac2c4e))
* add passkey support ([7dbd7c7](https://github.com/TimSchoenle/TankoVault/commit/7dbd7c7f86f0e8a7ce0b7ac05f7e91e6221efe32))
* add prototype ([09a7c06](https://github.com/TimSchoenle/TankoVault/commit/09a7c0665cb196d8ee166d2b3ad1fcb9262c04c0))
* **Email:** add proper email features ([#5](https://github.com/TimSchoenle/TankoVault/issues/5)) ([c0c0dce](https://github.com/TimSchoenle/TankoVault/commit/c0c0dced3e3c1b6d40eb9ae7133919bdfa0211e1))
* **Frontend:** improve anilink syncing ([e4e5719](https://github.com/TimSchoenle/TankoVault/commit/e4e5719fa9a48c1c5e58a923fe23b83eb1a157c6))
* **Frontend:** start implementing anilist sync feature ([b6e35a1](https://github.com/TimSchoenle/TankoVault/commit/b6e35a130e2bd1d50f452440ccdc07edfcb01be2))
* improve crawler through wreq ([c879cd0](https://github.com/TimSchoenle/TankoVault/commit/c879cd04ba1c68140bb7253580c279f4261f1b91))
* smart merge duplicated entries from the same adapter ([e68fef5](https://github.com/TimSchoenle/TankoVault/commit/e68fef5307948367b96d6519ff3953cc9e1b888f))


### Bug Fixes

* **Adapter:** kunmanga missing entries ([f3750e8](https://github.com/TimSchoenle/TankoVault/commit/f3750e84371f9fc5edd944d14fbc7550e4391623))
* **Adapters:** mandara full scan detection ([f75adfa](https://github.com/TimSchoenle/TankoVault/commit/f75adfa445264ad7b46242c7dc89f3f06aaafda1))
* **Auth:** invalid season token invalidation ([b409dbb](https://github.com/TimSchoenle/TankoVault/commit/b409dbbf001ba7fdda1464d05e49fa4e207ece96))
* ci failures ([4aaecbb](https://github.com/TimSchoenle/TankoVault/commit/4aaecbb9ae6a346fda4da61053145c99ff5c779e))
* correctly calculate new chapters ([72908dd](https://github.com/TimSchoenle/TankoVault/commit/72908ddb8d0fbe6bf1e71ccdd417f38f7785ee0c))
* **deps:** update cargo non-major (minor) ([#26](https://github.com/TimSchoenle/TankoVault/issues/26)) ([bcaae5c](https://github.com/TimSchoenle/TankoVault/commit/bcaae5c6dae3ba75224eaf015056a296f082ce62))
* **deps:** update rust crate jsonwebtoken to v11 ([#31](https://github.com/TimSchoenle/TankoVault/issues/31)) ([0e2f22b](https://github.com/TimSchoenle/TankoVault/commit/0e2f22b8c857942d5041a29b9a10aec951248e70))
* display re-name not working ([20f02bb](https://github.com/TimSchoenle/TankoVault/commit/20f02bb36abe4f8871cb077e78d17939446c7270))
* **Frontend:** chapter tracking bar not updating ([6290c9e](https://github.com/TimSchoenle/TankoVault/commit/6290c9ebe568beb1fbb0c92173ebbd0d1a221ff0))
* **Frontend:** fix load behaviour on first load ([e841063](https://github.com/TimSchoenle/TankoVault/commit/e84106380a21f0cf923f163bbc662a574677c1ed))
* **Frontend:** passkey login ([722fbc1](https://github.com/TimSchoenle/TankoVault/commit/722fbc143dc854206a6d68954110e1250b2e0455))
* **Frontend:** refresh access tokens ([68a9762](https://github.com/TimSchoenle/TankoVault/commit/68a9762bc2523679f91d419419ea017e73c0a6af))
* **Frontend:** wasm filename ([d25ba71](https://github.com/TimSchoenle/TankoVault/commit/d25ba7145402803c86415b8c3187fbf83e2a8911))
* merge system ([15b7053](https://github.com/TimSchoenle/TankoVault/commit/15b70535ba5fd29325c353ac1eb976dc25f7f798))
* new dependencies ([0571768](https://github.com/TimSchoenle/TankoVault/commit/05717689980379e4c6a35c8d00fb06a7ae4c1188))
* security flags ([#41](https://github.com/TimSchoenle/TankoVault/issues/41)) ([e0c7bfc](https://github.com/TimSchoenle/TankoVault/commit/e0c7bfc7d3b842051ddafde89a5527dfc31c6575))
* source scanners not finding all entries ([ace94eb](https://github.com/TimSchoenle/TankoVault/commit/ace94eb882cc28f671f026fb2d79aeac31093b83))
* sync operations on sup parts ([d0be8a7](https://github.com/TimSchoenle/TankoVault/commit/d0be8a72075f315005fccd0869869446433c7a36))
* **Sync:** anilist same source duplication issue ([cd800c8](https://github.com/TimSchoenle/TankoVault/commit/cd800c852c70382df6cd759c8566dbf55bf3ee1d))
* **Sync:** correctly mark whole chapters when marking parts ([b031eb1](https://github.com/TimSchoenle/TankoVault/commit/b031eb17e84c701e8c4558ced1528d7c91f7726f))
* weaken frontend nginx CSP to fix wasm frontend ([3d798e6](https://github.com/TimSchoenle/TankoVault/commit/3d798e69553951f113d6186bb25f90e68504f6ef))
* **Worker:** increase max page default value ([abd56bb](https://github.com/TimSchoenle/TankoVault/commit/abd56bbd422b4169a6275fe3f87f69e93fcb6493))


### Documentation

* add raw frontend design document ([488146a](https://github.com/TimSchoenle/TankoVault/commit/488146a67d2266fdd251c729e223391708c8ce4b))
* add simple readme ([ca02fc1](https://github.com/TimSchoenle/TankoVault/commit/ca02fc1b46a13b1e2ee3048630ee8640c1a98e0e))
* **Frontend:** add detailed implementation plan ([9d48e28](https://github.com/TimSchoenle/TankoVault/commit/9d48e28e251d4a85bf0bf41be578565c30c06a18))
* reduce AI gates ([d1835ae](https://github.com/TimSchoenle/TankoVault/commit/d1835aeb2c3dcc8dac6ab9e8eb76717d6de25c7d))
* simplify comments ([1a0ac95](https://github.com/TimSchoenle/TankoVault/commit/1a0ac956f7f88e385b0d81fa6792e7f7ae80eb68))


### Code Refactoring

* add metric endpoints to all services ([dc3e245](https://github.com/TimSchoenle/TankoVault/commit/dc3e245bbb37abb0eb995a3ab14a95ae982789f9))
* add paswword pepper ([4dd4077](https://github.com/TimSchoenle/TankoVault/commit/4dd40778b0a8de5312ff65aef63a060a95baaec6))
* add secrecy to harden security ([35d661c](https://github.com/TimSchoenle/TankoVault/commit/35d661c6e4c4e4d5c204cd6f7fdf1cb96a8a5c39))
* audit based cleanup ([#7](https://github.com/TimSchoenle/TankoVault/issues/7)) ([701a7cb](https://github.com/TimSchoenle/TankoVault/commit/701a7cb71d756684fb6c3385d6125a39d807c429))
* correctly include authors, descriptions and tags ([16c322b](https://github.com/TimSchoenle/TankoVault/commit/16c322b1e2f9f6a09d935507e9afb578588776d0))
* enable sql compiletime verification ([f972873](https://github.com/TimSchoenle/TankoVault/commit/f972873c105005bb6be35bc900ee227c75af8ba8))
* **Frontend:** add propper i18n message support ([f7a138d](https://github.com/TimSchoenle/TankoVault/commit/f7a138d189ad03a494c0568ccddda64ae1c0f260))
* **Frontend:** correctly indicate chapter parts ([bb3a776](https://github.com/TimSchoenle/TankoVault/commit/bb3a776cc823385ac69b5bcd8965647ae3e8ae22))
* **Frontend:** implement most frontend changes ([affed5e](https://github.com/TimSchoenle/TankoVault/commit/affed5eccab8e5e9b1187205ad7313ec4facb8b5))
* **Frontend:** modularize components ([88da866](https://github.com/TimSchoenle/TankoVault/commit/88da8664f80438c3fcc883e2ee753ee54c30d6d8))
* **Frontend:** rework watchlist ([6081695](https://github.com/TimSchoenle/TankoVault/commit/6081695bdc108213f48c423340125e19a96d03b1))
* implement a fair priority queue ([6389680](https://github.com/TimSchoenle/TankoVault/commit/6389680f4a9935192ed41741c67cb92e8e19eeca))
* improve adapter error handeling ([1fa3048](https://github.com/TimSchoenle/TankoVault/commit/1fa3048ea175d3fb908c26063fc45876fcfeb2ab))
* improve anilist metadata sync ([4cb132f](https://github.com/TimSchoenle/TankoVault/commit/4cb132fc1713c44a1f95e3802123cb44ea33a6ce))
* improve anilist sync behaviour ([60e340b](https://github.com/TimSchoenle/TankoVault/commit/60e340bfde475e804b643a0f2f68eb4bd2401be4))
* improve anilist sync matcher ([06590c7](https://github.com/TimSchoenle/TankoVault/commit/06590c7e356ff1bb3f73fb6b150593907e56606b))
* improve continue reading section ([fcb1632](https://github.com/TimSchoenle/TankoVault/commit/fcb16326ba162e1c004734909732dcd07c01624e))
* improve merge dedupe feature ([a562d42](https://github.com/TimSchoenle/TankoVault/commit/a562d42eeed6824af90dd15979a0866153c00002))
* migrate all rust based docker builds to use scratch runtimes ([33d4649](https://github.com/TimSchoenle/TankoVault/commit/33d46492a6061ea62953b4a591dd0f6c558b0cc8))
* migrate away from nginx to minimal axum server ([01ed204](https://github.com/TimSchoenle/TankoVault/commit/01ed204ae1ed1c74b489bc9717d1938760ce6044))
* migrate to postgres 19 ([851d275](https://github.com/TimSchoenle/TankoVault/commit/851d27530480635439067fc6336916e25814e572))
* migrate to propper OpenAPI definitions and generation ([#4](https://github.com/TimSchoenle/TankoVault/issues/4)) ([081f407](https://github.com/TimSchoenle/TankoVault/commit/081f407b068f75a574d435609b3ba14138195acd))
* re-design chapter overview and console view ([9bae1b6](https://github.com/TimSchoenle/TankoVault/commit/9bae1b6bca9ec70e280d01f077c22896e63d0935))
* rework frontend ([62820c1](https://github.com/TimSchoenle/TankoVault/commit/62820c19c3ddcc2c07ea49b160907679309f3493))
* standardize backend ([#6](https://github.com/TimSchoenle/TankoVault/issues/6)) ([fcbb9eb](https://github.com/TimSchoenle/TankoVault/commit/fcbb9eb145faecba718c453af1448e72d3825024))


### Tests

* add more access integration checks ([8df8a2c](https://github.com/TimSchoenle/TankoVault/commit/8df8a2cc40a23d49f44df94e06c476eef4dfea8f))


### CI

* add docker cst rules ([a6c7638](https://github.com/TimSchoenle/TankoVault/commit/a6c76384c7960b8aee5815f9a16a6574027c4bef))
* add release logics ([8ecfe0a](https://github.com/TimSchoenle/TankoVault/commit/8ecfe0ab5b5c2a464a888350fee328bfe55dd6f4))
* improve ([35b4c75](https://github.com/TimSchoenle/TankoVault/commit/35b4c756d6cc917b803e7f40cc93f75977feec35))
* remove dependabot ([129b04a](https://github.com/TimSchoenle/TankoVault/commit/129b04ae9d6b6dd9368ec2ac3ec13223253bf0b0))


### Build System

* fix missing libstdc dependency in docker builds ([e3a260f](https://github.com/TimSchoenle/TankoVault/commit/e3a260f7fd1f73ff2e044c3eb031b9a9e16bd144))
* improve docker caching ([6895b2f](https://github.com/TimSchoenle/TankoVault/commit/6895b2fe0d5e3ed13e63aef041ef6946c85c3eda))
* improve docker container release setup ([c9f0c68](https://github.com/TimSchoenle/TankoVault/commit/c9f0c68ccc08f6729cb9e73a1a68db3d7440d288))
* merge all standalone docker build ([cd385a9](https://github.com/TimSchoenle/TankoVault/commit/cd385a997a9770cddeb03642a4eb9ae273d53c6c))
* update flaresolverr image to v3.5.0 ([998ec3b](https://github.com/TimSchoenle/TankoVault/commit/998ec3bb698250558f536d639a408e38eff95e27))


### Miscellaneous

* **deps:** pin dependencies ([#18](https://github.com/TimSchoenle/TankoVault/issues/18)) ([bb6b42b](https://github.com/TimSchoenle/TankoVault/commit/bb6b42b87a449953d68955f079cf27a9a2e3da1e))
* **deps:** update actions/download-artifact digest to 018cc2c ([#39](https://github.com/TimSchoenle/TankoVault/issues/39)) ([6c0b2e7](https://github.com/TimSchoenle/TankoVault/commit/6c0b2e7247c4cd3f2fca4a6926fc1a5fc6627282))
* **deps:** update cargo non-major (patch) ([#19](https://github.com/TimSchoenle/TankoVault/issues/19)) ([e3128c8](https://github.com/TimSchoenle/TankoVault/commit/e3128c86d2f72f75197fae9877bff76550b5ea81))
* **deps:** update debian docker tag to v13 ([#27](https://github.com/TimSchoenle/TankoVault/issues/27)) ([22d9e48](https://github.com/TimSchoenle/TankoVault/commit/22d9e4890a890d32e3cdfb8986bad47f96f5d0f1))
* **deps:** update dependency node to v24 ([#28](https://github.com/TimSchoenle/TankoVault/issues/28)) ([00aecba](https://github.com/TimSchoenle/TankoVault/commit/00aecbabdfae7cb19a9afa7e94a4155c3f92e162))
* **deps:** update grafana/grafana docker tag to v11.6.16 ([#21](https://github.com/TimSchoenle/TankoVault/issues/21)) ([a48b4c0](https://github.com/TimSchoenle/TankoVault/commit/a48b4c019a04045be9fd4f31f4968046a69bb499))
* **deps:** update grafana/grafana docker tag to v13 ([#29](https://github.com/TimSchoenle/TankoVault/issues/29)) ([f1d963e](https://github.com/TimSchoenle/TankoVault/commit/f1d963ef8d3ae201c60f50bc2e0950ce7e796b7e))
* **deps:** update natsio/prometheus-nats-exporter docker tag to v0.20.1 ([#23](https://github.com/TimSchoenle/TankoVault/issues/23)) ([e6fe21b](https://github.com/TimSchoenle/TankoVault/commit/e6fe21b6f9acb734a52a5a967b66afdbac69bc60))
* **deps:** update postgres docker tag to v18 ([#37](https://github.com/TimSchoenle/TankoVault/issues/37)) ([f22cba0](https://github.com/TimSchoenle/TankoVault/commit/f22cba01285a20ad28d97ff801ef15e48fb53752))
* **deps:** update prom/blackbox-exporter docker tag to v0.28.0 ([#24](https://github.com/TimSchoenle/TankoVault/issues/24)) ([63544c3](https://github.com/TimSchoenle/TankoVault/commit/63544c3ea540e2850a09bb735a39e6f11bcff0e1))
* **deps:** update prom/prometheus docker tag to v3.13.1 ([#25](https://github.com/TimSchoenle/TankoVault/issues/25)) ([69aaa5a](https://github.com/TimSchoenle/TankoVault/commit/69aaa5af4fa06f69581b768c16b69432cf551a9e))
* **deps:** update prom/prometheus docker tag to v3.13.2 ([#35](https://github.com/TimSchoenle/TankoVault/issues/35)) ([f547d13](https://github.com/TimSchoenle/TankoVault/commit/f547d130cf623286ab31c3a6ca29ed8c55d4b220))
* **deps:** update prom/prometheus docker tag to v3.5.5 ([#22](https://github.com/TimSchoenle/TankoVault/issues/22)) ([9f074a9](https://github.com/TimSchoenle/TankoVault/commit/9f074a9ad24f23aea90ee2d55782e39817265f61))
* **deps:** update redis docker tag to v8 ([#34](https://github.com/TimSchoenle/TankoVault/issues/34)) ([ed20e85](https://github.com/TimSchoenle/TankoVault/commit/ed20e855839778ad559b8815f69e993e958c517d))
* **deps:** update rust crate dioxus to v0.7.10 ([#36](https://github.com/TimSchoenle/TankoVault/issues/36)) ([7b3cca0](https://github.com/TimSchoenle/TankoVault/commit/7b3cca07c7e33f1bcece1cba3e825cb1e838d328))
* **deps:** update rust crate syn to v3 ([#30](https://github.com/TimSchoenle/TankoVault/issues/30)) ([4d84e74](https://github.com/TimSchoenle/TankoVault/commit/4d84e7462f0bb0a6da6bdbdd1449185a59f59e14))
* **deps:** update rust crate syn to v3.0.3 ([#33](https://github.com/TimSchoenle/TankoVault/issues/33)) ([6ba513c](https://github.com/TimSchoenle/TankoVault/commit/6ba513c090799963457eda7a38c623d2d1602d94))
* update wreq to remove GPL-3.0 licence ([3fdee96](https://github.com/TimSchoenle/TankoVault/commit/3fdee96dd05154daefd2254cc0e43a047de84914))


### Dependencies

* **deps:** lock file maintenance ([#32](https://github.com/TimSchoenle/TankoVault/issues/32)) ([0584614](https://github.com/TimSchoenle/TankoVault/commit/0584614a0e6c384b495615ca3fe1f75b78f384b6))

## [Unreleased]

### Security

- Refresh cookies use the `__Host-` prefix and are `Secure` by default; the local-HTTP opt-out
  keeps the old name and path, so flipping it signs everyone out once.
- `GET /v1/me/stream` takes a single-use 30-second ticket instead of an access token in the
  query string, and re-checks account suspension on every reconnect. Live notifications had in
  fact **never worked** — the client sent `?token=` while the handler read `?access_token=`.
- The internal tier (`sync`, `control-plane`, `render`, `challenge-solver`) requires
  `X-Internal-Token`, and only `frontend` publishes a host port.
- Account enumeration closed on login, password reset and confirmation resend; email changes
  require the current password and revoke every session.
- **Every lookup by email or username was case-sensitive** despite the columns being `citext` —
  a total, silent lockout for anyone whose casing differed. Fixed at all four comparison sites.

### Added

- **A full release pipeline, driven by release-please.** Conventional commits maintain a release
  pull request; merging it tags the repository and publishes all nine service images as
  `linux/amd64` + `linux/arm64` manifest lists to **Docker Hub and GHCR**, each signed with
  cosign keyless and carrying an SPDX SBOM attestation. Replaces the tag-triggered, GHCR-only,
  amd64-only `release.yml`, which is deleted — keeping both would have double-built every
  release, since release-please pushes the tag the old workflow triggered on.
  [`docs/RELEASING.md`](docs/RELEASING.md) covers the flow, the required secrets and the two
  decisions that are still a human's.
  - `release-type: simple`, not `rust`, and the reason is written down in three places because
    it is the thing most likely to be "corrected": the Rust strategy rewrites `[package]
    version` in the root manifest and every member, but this workspace is a *virtual* manifest
    whose 26 members all inherit `version.workspace = true`. `rust` would rewrite 26 members
    into literal versions and leave the actual source of truth at `0.1.0`.
  - `update-lockfile.yaml` syncs `Cargo.lock` on every pull request and is load-bearing rather
    than a convenience: the version bump changes every member's recorded version, and a stale
    lockfile fails every `--locked` build in the repository — which would leave the release PR
    permanently red on a problem it created itself.
  - Publishing stays behind `ALLOW_IMAGE_PUBLISH`. The blocker is no longer a dependency licence
    (`OP-6` is resolved) but the absent `LICENSE` file. *(Both resolved since; the gate is gone.)*
- **Passkeys (`WebAuthn`), end to end.** A passkey is a first-class credential alongside the
  password: register one from Account → Security, then sign in with no identifier and no
  password at all (discoverable credentials, `UserVerificationPolicy::Required`, so it is two
  factors in one gesture). Keys can be named, renamed and revoked, and show when they were last
  used. Behind `accounts.passkeys`, which gates the sign-in ceremony and the management surface
  together — leaving one half reachable would mean a live credential its owner cannot revoke.
  Needs `TANKOVAULT_AUTH__WEBAUTHN_ORIGIN` (falls back to `TANKOVAULT_EMAIL__BASE_URL`); an
  unconfigured deployment answers `503` rather than `404`, so a missing setting cannot be
  mistaken for a feature that is not in this build.
  - Built on `webauthn-rs` 0.6 rather than the stable 0.5 line, because 0.5 links `openssl-sys`
    and the `scratch` runtime images ship no OpenSSL — a link failure that would surface at
    exec time in production, not at build time in CI.
  - Ceremony state lives in Postgres and is consumed by a `DELETE ... RETURNING`, so a challenge
    cannot be replayed and a `finish` cannot land on a replica that never saw the `start`.
  - Adding a key requires the current password. An access token lasts fifteen minutes; a passkey
    is permanent, and without the check anyone holding a token briefly could install a credential
    that survives every later password change and session revocation.
- `xtask ci` runs every offline gate CI runs, in CI's order.
- `xtask coverage-ratchet` fails the build when line coverage drops below
  `.github/coverage-floor.txt`.
- `deploy/observability/`: 31 recording rules, 25 alerts and a provisioned dashboard, behind a
  compose overlay so a plain `up` is unchanged.
- `docs/CONFIGURATION.md` — the env-var reference, ~70 keys.
- Reversible migrations, container healthchecks, resource limits, and a read-only root
  filesystem on every tier including `render`.

### Fixed

- Reading progress has two frontiers (whole and part). Five implementations of "has this user
  read this chapter?" disagreed, so part releases counted as unread in three places, a dashboard
  card could not be cleared, and the notifier announced chapters the reader had finished.
- Marking a part release read left the whole-chapter frontier behind, so reading `46.1` reported
  everything up to `45` as unread and kept pushing the stale number to AniList — which has no
  concept of parts and can only be told a whole chapter. Marking a part now also advances the
  whole frontier to the last chapter before the one the part belongs to.
- `parse_number` returned `f64::INFINITY` for an overlong digit run, which stores, freezes
  `latest_chapter` forever, and serialises to `null` on the bus.
- The notifier acked after a *failed* fan-out — at-most-once delivery, losing notifications with
  one `warn!`.
- `http_requests_in_flight` leaked on every client disconnect, so the one gauge an operator
  reaches for to answer "is this saturated?" only ever rose.
- The admin console's pending-conflict count was per user rather than per linked account.

### Changed

- **`wreq` 5.3 → 6.0.0-rc.29 and `wreq-util` 2.2 → 3.0.0-rc.14, to get off GPL-3.0** (`OP-6`).
  The crawl stack's emulation profiles come from `wreq-util`, which was GPL-3.0 on the 2.x line;
  since GPL-3.0 obligations attach on *conveying*, that made pushing an image a source-offer
  obligation over the whole combined work, and it is why the release workflow has never pushed.
  Upstream relicensed — GPL-3.0 → LGPL-3.0 at `3.0.0-rc.9` → Apache-2.0 at `3.0.0-rc.12` — so
  the fix was a version bump, not a relicensing of anything here. Pre-release on both is
  deliberate and load-bearing: Apache-2.0 `wreq-util` requires `wreq ^6.0.0-rc`, and 2.2.6 stays
  GPL-3.0 forever because a relicence is not retroactive. `deny.toml` now allows neither GPL-3.0
  nor LGPL-3.0, so a downgrade fails the licence gate instead of quietly re-opening `OP-6`.
  - API churn in `crates/fetch/src/base.rs`: `wreq` 6 carries `http::Uri` where 5 carried
    `url::Url` (`Response::url` → `uri`, redirect `Attempt` exposes fields rather than
    accessors), and `wreq-util` 3 renamed all three emulation types *and swapped one name* —
    the enum of concrete builds is now `Profile`, and `Emulation` is what `EmulationOption`
    used to be. Profiles moved to the newest build per family with it (Chrome 137 → 149,
    Firefox 139 → 151, Safari 18.5 → 26.4, Edge 134 → 148).
  - The BoringSSL binding is renamed `boring-sys2` → `btls-sys`, which the Dockerfile and
    `CONTRIBUTING.md` referred to by name; the build toolchain it needs is unchanged.
  - **`btls-sys` links `libstdc++`, which `boring-sys2` did not**, so the `scratch` runtime now
    ships `libstdc++.so.6` alongside the musl loader and `libgcc_s`. Without it `worker` and
    `challenge-solver` build cleanly and then die at exec with `Error loading shared library
    libstdc++.so.6`. `render` and `frontend` do not link it and their stages still omit it.
    The builder stage gained a **linkage contract** that fails the build when any binary needs
    a library its runtime stage does not ship — this class of breakage is invisible to every
    other gate, because only a loader running the real binary in the real image can see it.
  - `charset`/`encoding_rs` left `wreq`'s default features in 6 and has **not** been re-enabled:
    nothing here decodes by declared charset — `base.rs` decodes `bytes_stream()` as UTF-8
    itself and the solver clients only call `.json()`.
- **The Dockerfile actually builds on arm64 now.** `ci.yml`'s `docker-arm64` job asserted that
  "nothing in the build is x86-specific"; five places named `ld-musl-x86_64.so.1` literally, and
  the loader is `ld-musl-aarch64.so.1` on arm64, so every runtime stage would have failed at
  `COPY`. Nothing caught it because that job is gated on an unset `ENABLE_ARM64_CI` and had
  never run — an assertion nothing executes is a comment, not evidence. The builder now stages
  `/sysroot`, `/sysroot-nocxx` and `/sysroot-browser` trees from `uname -m` and the runtime
  stages copy those wholesale. The Debian tree deliberately carries no `/lib`, because BuildKit
  cannot copy a directory over that merged-`/usr` symlink; the stage asserts the symlink holds
  rather than trusting it. Structure tests gained `cst/loader-amd64.yaml` / `loader-arm64.yaml`
  for the one assertion that cannot be architecture-neutral.
- The `api` binary no longer links `wreq`/BoringSSL: the adapter dry-run moved to the worker,
  which already carries the crawl stack. 557 → 487 crates, and one TLS stack instead of two.
- Postgres 17 everywhere; the reference stack was on a beta major.
- Every suppression is `#[expect(..., reason = "...")]`; seven turned out to suppress nothing.

Every entry above traces to a row in `docs/audit/PROGRESS.md`, which carries the full reasoning
and the test that pins it.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
.1.0/

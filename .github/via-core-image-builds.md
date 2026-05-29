# Core Image Builds

This is the maintainer/operator guide for how Via's **core Docker images**
(`via-server`, `via-external-node`, `snapshots-creator`) are built, cached, and
published in CI.

It is written to be read top to bottom by someone who has never touched this path.
It first explains the Via build system in general (which is under-documented), then the
one idea this whole pipeline is organized around, then the specific contracts you must
not break. It describes the system as it is and *why* it is that way — not how it
changed over time.

---

## 1. The Via build system, briefly

Via is a Bitcoin-anchored ZK rollup built on a fork of **ZKsync Era** (you will see this
heritage everywhere: `matterlabs/zksync-*` base images, the root npm package
`zksync-root`, `ZkStack.yaml`, the `zk` CLI, `WORKDIR /usr/src/zksync` in one of the
Dockerfiles). Understanding two facts about the repo explains almost everything about
the build:

**It is two build systems in one repo.**

- **A large Rust workspace** — the root `Cargo.toml` defines ~113 members, almost all
  under `core/` (plus sibling crate trees like `via_verifier/`, `via_indexer/`,
  `prover/`). The shipped image binaries (`via_server`, `via_external_node`,
  `snapshots_creator`, …) live in `core/bin/*`. The toolchain is pinned in
  `rust-toolchain` (`nightly-2024-08-01`), and `.cargo/config.toml` is deliberately
  minimal (`git-fetch-with-cli = true` only — **no hidden `RUSTFLAGS` or custom
  linker**, which matters later).
- **A JS/TS + Solidity system** — yarn workspaces (`infrastructure/zk`,
  `infrastructure/via`, `contracts/l1-contracts`, `sdk/zksync-rs`) that compile the L1,
  L2, and system contracts.

**You drive it through thin CLI wrappers in `bin/`:**

- `bin/via` → `yarn --cwd infrastructure/via via "$@"` — the Via dev/orchestration CLI.
- `bin/zk` → the inherited ZKsync `zk` CLI.
- `zkstack_cli/` — the newer Rust-based ZK Stack CLI.

The key consequence for image builds: **a runnable Via node is a Rust binary *plus*
compiled contract artifacts.** The Rust compile and the contract compile are different
toolchains, and the image pipeline keeps them separate (see §3).

---

## 2. The one idea behind the image pipeline

Compiling the Rust workspace from scratch costs roughly **8 minutes (snapshots-creator)
to ~25 minutes (server / external-node) per image**, and CI must prove on **every PR**
that the production images still build. A full cold compile per image per PR is not
affordable.

So the entire pipeline is organized around a single idea:

> **Separate the work that rarely changes from the work that changes every PR, cache the
> rarely-changing work once on trusted refs, and let every PR reuse it — so a PR only
> pays to compile what it actually changed.**

Almost every design decision in this path is one lever pulling on that idea. Hold this
in mind and the rest of the doc is just details:

| The cost | The lever | Where |
|---|---|---|
| Recompiling all third-party dependencies on every source edit | **cargo-chef** splits "cook dependencies" from "build our source" into separate, separately-cached Docker stages | the three Dockerfiles |
| Recomputing that dependency layer in every PR's runner | **A shared BuildKit cache** computed once on trusted refs and *read* by all PRs | `cache-from`/`cache-to` + `export_build_cache` |
| The final link, which reruns on *every* source change and can't be cached | **mold**, a faster linker, on the final build only | the three Dockerfiles |
| Shipping a noisy build context that destabilizes cache keys | **A deny-all `.dockerignore` allowlist** of just the compile inputs | `.dockerignore` |
| Mixing the Solidity/JS toolchain into the hot Rust path | **Contracts built once, outside Docker, injected as an artifact** | `prepare-contracts` job |
| Hidden infra dependencies that make checks un-runnable | **GitHub-hosted runners only** — no self-hosted/custom labels | `build-core-template.yml` |

If you are about to "simplify" something here, first identify which lever it is and
which cost it removes. Most of the non-obvious choices are load-bearing.

---

## 3. What a core image is, and how it's assembled

All three images are built by **one reusable workflow**,
`.github/workflows/build-core-template.yml`, from the repo root as build context
(`context: .`). Each Dockerfile is a multi-stage build that ends in a slim runtime
image containing the binary plus component-specific runtime assets.

The pipeline has three phases:

1. **`prepare-contracts`** — builds the L2 and system contracts with the yarn toolchain
   and uploads `./contracts` as a workflow artifact. The image builds depend on these
   compiled artifacts being present (hence the Dockerfile note *"Will work locally only
   after prior contracts build"*). `via-server` and `via-external-node` then **copy the
   contract artifacts into their runtime image** (external-node also ships the `sqlx`
   binary, an `entrypoint.sh`, and DAL migrations); `snapshots-creator` ships only its
   binary.

2. **The build matrix** — one job per `(component, platform)`:
   - `via-server` → `linux/amd64`
   - `snapshots-creator` → `linux/amd64`
   - `via-external-node` → `linux/amd64` **and** `linux/arm64`

   Two mutually exclusive jobs implement this:
   - **`build-images`** (`action != "push"`): PRs, merge queue, and build-only branch
     pushes such as `main`, `staging`, and `trying`. Builds
     with `load: true` so the image exists locally for validation. **Never the publish path.**
   - **`build-and-push-images`** (`action == "push"`): tag/release publishing. Uses
     **Buildx direct push** and refuses to start if `image_tag_suffix` is empty.

3. **`create_manifest`** (publish only) — assembles a multi-arch manifest from the
   per-platform tags with `docker manifest create`. This is why Buildx `provenance` is
   **disabled** on the push step: attestation descriptors would break `manifest create`.

Images always publish Docker Hub tags as `vianetwork/<component>`. When
`GAR_JSON_KEY` is set, the same run also publishes matching Google Artifact Registry
tags as
`europe-west3-docker.pkg.dev/viaorg-prod-net-landing-0/via/<component>`. When
`GAR_JSON_KEY` is absent, the push and manifest jobs skip Artifact Registry login,
tags, and manifests and continue with Docker Hub only.

Publishing still requires the Docker Hub secrets `DOCKERHUB_USER` and
`DOCKERHUB_TOKEN`. `build-and-push-images` computes the Buildx `IMAGE_TAGS` list from
the available registry credentials, so missing `GAR_JSON_KEY` is not a workflow
failure by itself.

### Base images

| Stage | Image | Pinned? |
|---|---|---|
| Build toolchain (`chef`) | `zksync-build-base:latest` | No — `:latest` is intentional; pinning by digest is a separate reproducibility decision, not a correctness requirement |
| cargo-chef binary source | `lukemathwalker/cargo-chef:0.1.77-rust-bookworm` | Yes — copied in only to avoid compiling cargo-chef on cold builds |
| Runtime (all three) | `debian:bookworm-slim` + apt runtime deps | — |

**Heads-up — the three Dockerfiles are not perfectly uniform.** They drifted and are
worth normalizing if you touch them:

- The build base is referenced two ways: `via-server` / `via-external-node` use
  `matterlabs/zksync-build-base:latest` (Docker Hub) with `WORKDIR /usr/src/via`, while
  `snapshots-creator` uses `ghcr.io/matter-labs/zksync-build-base:latest` (GHCR) with
  `WORKDIR /usr/src/zksync`.
- Runtime apt deps differ slightly (`via-external-node` omits `liburing-dev`).
- All three carry unused `SCCACHE_*` / `RUSTC_WRAPPER` build args inherited from the
  upstream sccache setup; they are empty in this pipeline.

### The build context allowlist

`.dockerignore` ignores everything (`*`) and then allow-lists **only** what the builds
need: the Cargo workspace inputs (`Cargo.toml`, `Cargo.lock`, `rust-toolchain`, `core/`,
`prover/`, `via_verifier/`, `via_indexer/`), the contract sources + prebuilt artifact
dirs (`contracts/`, several `contracts/system-contracts/...artifacts` paths), the JS
tooling needed to build contracts (`package.json`, `yarn.lock`, `infrastructure/zk`,
`infrastructure/local-setup-preparation`, `sdk/zksync-rs`), selected `etc/` and `bin/`
paths, and the three core Dockerfiles. This keeps the context small, keeps cache keys
stable, and prevents local state or secrets from leaking into a build. **When you add a
workspace path a build needs, add it here too** — otherwise the build can't see it, and
note that the list is an explicit allowlist, not the whole repo (e.g. `zkstack_cli/` is
*not* currently included).

---

## 4. The dependency-cache contract (the part that quietly breaks)

This is the most important section to internalize, because when it breaks, nothing
*fails* — builds just silently go slow.

Each Dockerfile is staged like this:

1. **`planner`** — `cargo chef prepare` writes a dependency recipe (`recipe.json`) from
   the workspace manifests.
2. **`cacher`** — `cargo chef cook --release ... --bin <targets>` compiles **only the
   dependency crates** from that recipe. *This is the layer the shared cache is for.*
3. **`builder`** — copies the full source tree, overlays the cooked Cargo home +
   `target/` from `cacher`, and runs the final `cargo build` for the same `--bin` set.

The payoff only happens if the cooked dependency layer from `cacher` is actually reused
by the final `cargo build`. It is reused **only when Cargo's build fingerprint is
identical between the cook step and the final build.** If `cacher` restores from cache
but the final build recompiles dependencies anyway, some Cargo input differs between the
two steps. The inputs that must match:

- Rust toolchain version
- the selected Cargo features and the `--bin` set
- `RUSTFLAGS`
- `RUSTC_WRAPPER`
- linker configuration
- the workspace manifests and `Cargo.lock`

**Rule of thumb:** any change that affects how dependencies compile must be applied
identically to the `cargo chef cook` step and the final `cargo build`, or not at all.

---

## 5. mold (the final-link lever), and why placement matters

The Dockerfiles run `mold -run cargo build ...` for **only the final build**. `mold -run`
intercepts Linux ELF linking at the process level — it does **not** set `RUSTFLAGS` or
edit `.cargo/config.toml`. That detail is the whole point:

- It changes **only the final link step**, so it does *not* alter Cargo's fingerprint and
  therefore does *not* invalidate the cooked-dependency cache (§4). Expressing mold via
  `RUSTFLAGS` instead would change a fingerprinted input and throw the cache away for no
  gain.
- `mold` is `apt install`'d in the `builder` stage **before `COPY . .`**, so BuildKit
  caches that layer across source-only changes. **Keep that ordering.**
- Installing it in the shared `chef` stage *would* work but `chef` is the parent of
  `cacher`, so editing `chef` invalidates the cooked-dependency cache and can force a
  validating PR into a cold dependency rebuild. Leave linker tooling in `builder` unless
  you have a reason and accept that cost.

mold attacks the one cost cargo-chef can't: the final link reruns on every source change,
so it can never be cached.

---

## 6. The cache trust model (asymmetric on purpose)

The shared dependency cache lives in the GitHub Actions cache under scopes
`core-<component>-<platform>`. The read/write split is **deliberately asymmetric**:

- **Readers (`cache-from`, import-only):** pull requests, merge queue, and pushes to
  `staging` / `trying`. They benefit from the cache but never write it.
- **Writers (`cache-to: type=gha,mode=max,ignore-error=true`):**
  - pushes to **`main`** — `ci.yml` sets `export_build_cache` only when the ref is
    `refs/heads/main`;
  - **tag/release** publishing via `build-docker-from-tag.yml`;
  - the **weekly warmer** `warm-core-build-cache.yml` (Mondays 03:17 UTC, or manual),
    which also exports only when run from `main`.

**Why asymmetric.** Every PR exporting a `mode=max` cache spends 2–3 min/job and churns
the repo-wide cache budget when several PRs are active. Restricting writes to trusted,
repo-owned refs means PRs ride on a clean shared cache instead of fighting over it.

**Operator consequences — read these before debugging a "slow build":**

1. A PR that changes dependency metadata (`Cargo.toml` / `Cargo.lock`) will get **cold
   `cargo chef cook` builds** until the change lands on `main`, a release publishes, or
   the weekly warmer runs. That is the intended trade-off, not a regression.
2. Warm builds depend on the **repo's Actions cache cap being above the default 10 GiB**
   (currently **20 GiB**; the footprint is ≈ 15 GiB). This cap is a *repository
   setting, not in this code*. If warm builds suddenly start recompiling dependencies,
   check the cache cap, retention, and eviction **before** suspecting the Dockerfiles.

---

## 7. Keep CI path filters in sync

`.github/workflows/ci.yml` decides whether to invoke this workflow, via its `core`
changed-files filter. When you add, rename, or remove a core image, update **both**:

- the matrices in `build-core-template.yml` (`build-images`, `build-and-push-images`,
  **and** `create_manifest`), and
- the `core` file list in `ci.yml`.

Otherwise a Dockerfile-only PR can pass without building the image it changed.

Two facts about that filter today:

- It also triggers on `Cargo.toml`, `Cargo.lock`, `core/**`, and `zkstack_cli/**` —
  correct, because those feed the cargo-chef recipe.
- It still lists `docker/contract-verifier/**`, `docker/external-node/**`, and
  `docker/server/**`, which have **no image in the active matrix**. Editing those paths
  triggers a core build that builds none of them. Treat them as legacy entries; the
  filter list and the build matrix are *not* 1:1.

---

## 8. PR review checklist

For any change to this path, verify:

- Dockerfile-only changes under active image dirs actually run the core build in CI.
- `cargo chef cook` is cached on warm builds when dependency metadata didn't change.
- The final `cargo build` does not rebuild dependency crates unexpectedly.
- PR builds stay **import-only**; only `main` / tag / scheduled callers set
  `export_build_cache`.
- Runners stay GitHub-hosted (`ubuntu-24.04` / `ubuntu-24.04-arm`).
- Any new workspace path the build needs is allow-listed in `.dockerignore`.
- Runtime images copy only the intended binaries and runtime assets.
- Publishing still targets Docker Hub, and still targets Artifact Registry when
  `GAR_JSON_KEY` is available.

If a change *intentionally* invalidates the dependency cache, say so in the PR
description and weigh the cold-build cost against the benefit.

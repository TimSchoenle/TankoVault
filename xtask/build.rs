//! Installs `hooks/pre-commit` into `.git/hooks/pre-commit` so the `OpenAPI` artifact
//! regeneration (see `src/main.rs`'s `openapi` command) happens automatically on
//! commit instead of relying on a developer to remember it. Runs on every build of
//! this crate; cheap no-op once the hook is up to date, silent no-op outside a git
//! checkout (e.g. the Docker build, whose context excludes `.git` — see
//! `.dockerignore`).

use std::fs;
use std::path::Path;

const MANAGED_MARKER: &str = "tankovault: managed by xtask/build.rs";

fn main() {
    println!("cargo:rerun-if-changed=hooks/pre-commit");
    println!("cargo:rerun-if-changed=build.rs");

    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ has a parent directory")
        .to_path_buf();
    let git_dir = repo_root.join(".git");
    if !git_dir.exists() {
        return; // not a git checkout (e.g. Docker build context) — nothing to install
    }

    let hooks_dir = git_dir.join("hooks");
    let hook_path = hooks_dir.join("pre-commit");
    let template = include_str!("hooks/pre-commit");

    // Never clobber a hook we didn't install ourselves.
    let safe_to_write = match fs::read_to_string(&hook_path) {
        Ok(existing) => existing == template || existing.contains(MANAGED_MARKER),
        Err(_) => true,
    };
    if !safe_to_write {
        println!(
            "cargo:warning=xtask: leaving existing .git/hooks/pre-commit in place (not ours to overwrite); \
             OpenAPI regeneration will only happen via `cargo run -p xtask -- openapi` or CI"
        );
        return;
    }

    if fs::read_to_string(&hook_path)
        .map(|s| s == template)
        .unwrap_or(false)
    {
        return; // already up to date
    }

    if fs::create_dir_all(&hooks_dir)
        .and_then(|()| fs::write(&hook_path, template))
        .is_err()
    {
        return; // best-effort; never fail the build over a convenience hook
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755));
    }

    println!(
        "cargo:warning=xtask: installed .git/hooks/pre-commit (auto-regenerates the OpenAPI artifacts)"
    );
}

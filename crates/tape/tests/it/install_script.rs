#![cfg(unix)]
//! Install script checksum gate (#236).
//!
//! `install.sh` fails closed. A missing `.sha256` file or a missing hash tool
//! is an error with a message, unless `TAPE_INSTALL_SKIP_VERIFY=1` is set.
//! The first field of the sidecar is the expected hash. The script says when
//! it takes the `gh` token, and never prints the token.
//!
//! The tests run offline. A fake `curl` serves the archive and the sidecar.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Tools that `install.sh` runs. Each one is linked from the host into the fake PATH.
const TOOLS: &[&str] = &[
    "sh", "tar", "gzip", "uname", "mktemp", "awk", "tr", "cat", "cp", "grep", "head", "sed", "rm",
    "mkdir", "mv", "chmod", "dirname", "basename",
];

const TOKEN: &str = "secret-token-value";

/// The fake curl. The URL is the argument that starts with `https://`, and `-o`
/// names the output file. A `.sha256` URL with no sidecar in the fixture exits 22.
const FAKE_CURL: &str = r#"#!/bin/sh
fixture="$FIXTURE_DIR"
url=""
out=""
prev=""
for arg in "$@"; do
  case "$arg" in https://*) url="$arg" ;; esac
  if [ "$prev" = "-o" ]; then out="$arg"; fi
  prev="$arg"
done
case "$url" in
  *.sha256)
    [ -f "$fixture/archive.tar.gz.sha256" ] || exit 22
    cp "$fixture/archive.tar.gz.sha256" "$out" ;;
  *.tar.gz)
    cp "$fixture/archive.tar.gz" "$out" ;;
  *) exit 22 ;;
esac
"#;

/// The fake gh. `auth status` succeeds and `auth token` prints the token.
const FAKE_GH: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth status") exit 0 ;;
  "auth token") echo secret-token-value ;;
  *) exit 1 ;;
esac
"#;

enum Sidecar {
    /// The sidecar download fails.
    Missing,
    /// `<hash>  <asset>` in `sha256sum` format, with the real hash of the archive.
    Matching,
    /// This text, written as it is.
    Raw(String),
}

struct Fixture {
    dir: tempfile::TempDir,
    target: &'static str,
}

impl Fixture {
    fn bin(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }

    fn installed(&self) -> PathBuf {
        self.out().join("tape")
    }

    fn asset(&self) -> String {
        format!("tape-{}.tar.gz", self.target)
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The target that `install.sh` picks on this host, or `None` when the script
/// has no binary for it. The tests skip on such a host.
fn host_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

/// The path of `name` on the host: the system copy in `/usr/bin` or `/bin`
/// first, so a wrapper early on the host PATH is not linked in, else the first
/// match on the host PATH.
fn find_on_host_path(name: &str) -> Option<PathBuf> {
    for dir in ["/usr/bin", "/bin"] {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// The first field of the `sha256sum` or `shasum -a 256` output for `path`.
fn host_sha256(path: &Path) -> String {
    let out = match find_on_host_path("sha256sum") {
        Some(tool) => Command::new(tool).arg(path).output(),
        None => Command::new(find_on_host_path("shasum").expect("sha256sum or shasum on the host"))
            .args(["-a", "256"])
            .arg(path)
            .output(),
    }
    .expect("run the host hash tool");
    String::from_utf8(out.stdout)
        .expect("hash output is UTF-8")
        .split_whitespace()
        .next()
        .expect("hash output has a first field")
        .to_string()
}

fn write_exec(path: &Path, text: &str) {
    fs::write(path, text).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod script");
}

/// Build a fixture: an archive `tape-<target>.tar.gz` that holds `tape-<target>/tape`,
/// a sidecar of the given kind, and a fake PATH in `bin`.
fn fixture(target: &'static str, sidecar: Sidecar, hash_tools: bool, gh: bool) -> Fixture {
    let fx = Fixture {
        dir: tempfile::tempdir().expect("tempdir"),
        target,
    };
    let root = fx.dir.path();
    let bin = fx.bin();
    fs::create_dir_all(&bin).expect("create bin");

    let pkg = root.join("pkg").join(format!("tape-{target}"));
    fs::create_dir_all(&pkg).expect("create pkg");
    write_exec(&pkg.join("tape"), "#!/bin/sh\necho tape-test\n");
    let archive = root.join("archive.tar.gz");
    let status = Command::new("tar")
        .arg("-C")
        .arg(root.join("pkg"))
        .arg("-czf")
        .arg(&archive)
        .arg(format!("tape-{target}"))
        .status()
        .expect("run tar");
    assert!(status.success(), "tar failed");

    let sidecar_text = match sidecar {
        Sidecar::Missing => None,
        Sidecar::Matching => Some(format!("{}  {}\n", host_sha256(&archive), fx.asset())),
        Sidecar::Raw(text) => Some(text),
    };
    if let Some(text) = sidecar_text {
        fs::write(root.join("archive.tar.gz.sha256"), text).expect("write sidecar");
    }

    write_exec(&bin.join("curl"), FAKE_CURL);
    for tool in TOOLS {
        if let Some(src) = find_on_host_path(tool) {
            symlink(src, bin.join(tool)).expect("link tool");
        }
    }
    if hash_tools {
        for tool in ["shasum", "sha256sum"] {
            if let Some(src) = find_on_host_path(tool) {
                symlink(src, bin.join(tool)).expect("link hash tool");
            }
        }
    }
    if gh {
        write_exec(&bin.join("gh"), FAKE_GH);
    }
    fx
}

/// Run `install.sh` with a clean environment and the fake PATH.
fn run_install(fx: &Fixture, extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new("sh");
    cmd.arg(repo_root().join("install.sh"))
        .env_clear()
        .env("PATH", fx.bin())
        .env("HOME", fx.dir.path())
        .env("TAPE_INSTALL", fx.out())
        .env("TAPE_VERSION", "tape@0.0.0-test")
        .env("FIXTURE_DIR", fx.dir.path());
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.output().expect("run install.sh")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_succeeds_with(out: &Output, needle: &str) {
    assert!(
        out.status.success(),
        "expected success, got {:?}; stderr:\n{}",
        out.status,
        stderr(out)
    );
    assert!(
        stderr(out).contains(needle),
        "stderr lacks {needle:?}:\n{}",
        stderr(out)
    );
}

fn assert_fails_with(out: &Output, needle: &str) {
    assert!(
        !out.status.success(),
        "expected failure, got success; stderr:\n{}",
        stderr(out)
    );
    assert!(
        stderr(out).contains(needle),
        "stderr lacks {needle:?}:\n{}",
        stderr(out)
    );
}

#[test]
fn installs_when_sidecar_matches_first_field() {
    let Some(target) = host_target() else {
        eprintln!("skipped: no install.sh binary for this host");
        return;
    };
    let fx = fixture(target, Sidecar::Matching, true, false);
    let out = run_install(&fx, &[]);
    assert_succeeds_with(&out, "sha256 verified");
    assert!(fx.installed().exists(), "tape was not installed");
}

#[test]
fn fails_closed_when_sidecar_is_missing() {
    let Some(target) = host_target() else {
        eprintln!("skipped: no install.sh binary for this host");
        return;
    };
    let fx = fixture(target, Sidecar::Missing, true, false);
    let out = run_install(&fx, &[]);
    assert_fails_with(&out, "could not download the checksum");
    assert!(
        !fx.installed().exists(),
        "tape was installed without a check"
    );
}

#[test]
fn fails_closed_without_a_hash_tool() {
    let Some(target) = host_target() else {
        eprintln!("skipped: no install.sh binary for this host");
        return;
    };
    let fx = fixture(target, Sidecar::Matching, false, false);
    let out = run_install(&fx, &[]);
    assert_fails_with(&out, "no sha256 tool");
    assert!(
        !fx.installed().exists(),
        "tape was installed without a check"
    );
}

#[test]
fn rejects_a_wrong_hash() {
    let Some(target) = host_target() else {
        eprintln!("skipped: no install.sh binary for this host");
        return;
    };
    let zeros = "0".repeat(64);
    let fx = fixture(
        target,
        Sidecar::Raw(format!("{zeros}  tape-{target}.tar.gz\n")),
        true,
        false,
    );
    let out = run_install(&fx, &[]);
    assert_fails_with(&out, "sha256 mismatch");
    assert!(
        !fx.installed().exists(),
        "tape was installed with a wrong hash"
    );
}

#[test]
fn skip_flag_installs_and_says_so() {
    let Some(target) = host_target() else {
        eprintln!("skipped: no install.sh binary for this host");
        return;
    };
    let fx = fixture(target, Sidecar::Missing, true, false);
    let out = run_install(&fx, &[("TAPE_INSTALL_SKIP_VERIFY", "1")]);
    assert_succeeds_with(&out, "sha256 check skipped");
    assert!(fx.installed().exists(), "tape was not installed");
}

#[test]
fn says_when_it_uses_the_gh_token() {
    let Some(target) = host_target() else {
        eprintln!("skipped: no install.sh binary for this host");
        return;
    };
    let fx = fixture(target, Sidecar::Matching, true, true);
    let out = run_install(&fx, &[]);
    assert_succeeds_with(&out, "using the GitHub token from gh auth");
    assert!(
        !stderr(&out).contains(TOKEN),
        "the token was printed:\n{}",
        stderr(&out)
    );
}

#[test]
fn no_gh_message_when_gh_token_is_set() {
    let Some(target) = host_target() else {
        eprintln!("skipped: no install.sh binary for this host");
        return;
    };
    let fx = fixture(target, Sidecar::Matching, true, true);
    let out = run_install(&fx, &[("GH_TOKEN", "x")]);
    assert!(out.status.success(), "install failed:\n{}", stderr(&out));
    assert!(
        !stderr(&out).contains("from gh auth"),
        "the gh message printed although GH_TOKEN is set:\n{}",
        stderr(&out)
    );
}

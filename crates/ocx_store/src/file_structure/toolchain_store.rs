// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The rendered toolchain tree: one grammar ([`ToolchainHome`]), two tiers.
//!
//! ```text
//! {root}/
//! ├── .gitignore                     "*", ensure-present
//! ├── active -> shells/default/      the PATH-facing indirection
//! ├── links/<group>/<entry>/         directory link (junction on Windows) to a package root
//! └── shells/default/bin/            launcher trampolines, DEFAULT group only
//! ```

use std::path::{Path, PathBuf};

use ocx_util::fs::BoundedReadError;

const TOOLCHAIN_BIN_DIR: &str = "bin";

const GITIGNORE_FILE: &str = ".gitignore";

const ACTIVE_LINK: &str = "active";

/// Every user-supplied name lives below this, so no group can collide with a depth-1 name.
const LINKS_DIR: &str = "links";

const SHELLS_DIR: &str = "shells";

/// The one shell a render writes; multi-shell selection is ocx-sh/ocx#363.
pub const DEFAULT_SHELL: &str = "default";

/// The closed set of depth-1 names a home owns; never derive it from an accessor, whose `file_name` is `"bin"`.
pub const TREE_OWN_DEPTH1_NAMES: [&str; 4] = [GITIGNORE_FILE, ACTIVE_LINK, LINKS_DIR, SHELLS_DIR];

/// Derived, never remembered: a copied `$OCX_HOME` carries a stale junction and a stamp that agrees with it.
///
/// Relative on POSIX so a moved home points into itself; absolute on Windows because a junction requires it.
fn expected_active_target(root: &Path, shell: &str) -> PathBuf {
    if cfg!(windows) {
        root.join(SHELLS_DIR).join(shell)
    } else {
        Path::new(SHELLS_DIR).join(shell)
    }
}

/// Raw bytes, never `Path`'s `PartialEq`, which ignores the trailing separator `readlink(2)` distinguishes.
///
/// `case_insensitive` is a parameter, not a `cfg!`, so Linux CI reaches both arms.
fn targets_match(actual: &Path, expected: &Path, case_insensitive: bool) -> bool {
    let actual = dunce::simplified(actual).as_os_str();
    let expected = dunce::simplified(expected).as_os_str();
    if case_insensitive {
        actual.eq_ignore_ascii_case(expected)
    } else {
        actual == expected
    }
}

/// Compared byte-for-byte before writing, so two renders of one input leave identical trees.
const GITIGNORE_CONTENT: &[u8] = b"*\n";

/// Which of [`ToolchainHome::entry`]'s two components a [`ToolchainPathError`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolchainPathComponent {
    Group,
    Entry,
}

impl std::fmt::Display for ToolchainPathComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Group => "group",
            Self::Entry => "entry",
        })
    }
}

/// A group or entry name that cannot become a path component of a rendered toolchain tree.
///
/// Validated here, not at a producer: names also arrive from `-g` and a hostile clone's `ocx.lock`.
/// Interpolated with `{:?}`, never raw, or a newline in an untrusted name forges log lines (CWE-117).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, ocx_exit::Classify)]
pub enum ToolchainPathError {
    /// An empty component would make `<group>/<entry>` name the group directory itself.
    #[error("toolchain {component} name is empty")]
    #[exit(
        ConfigError,
        slug = "toolchain_name_empty",
        summary = "A toolchain group or entry name is empty"
    )]
    Empty { component: ToolchainPathComponent },

    /// Any Unicode control character, `U+0080`–`U+009F` included: the name reaches emitted `PATH` values (CWE-77) and logs (CWE-117).
    #[error("toolchain {component} name {value:?} contains a control character")]
    #[exit(
        ConfigError,
        slug = "toolchain_name_control_character",
        summary = "A toolchain group or entry name contains a control character"
    )]
    ControlCharacter {
        component: ToolchainPathComponent,
        value: String,
    },

    /// `/` or `\`, on every platform, or one component widens into several.
    #[error("toolchain {component} name {value:?} contains a path separator")]
    #[exit(
        ConfigError,
        slug = "toolchain_name_separator",
        summary = "A toolchain group or entry name contains a path separator"
    )]
    Separator {
        component: ToolchainPathComponent,
        value: String,
    },

    /// Any `:`, on every platform: on Windows `PathBuf::push("C:")` discards the home root.
    #[error("toolchain {component} name {value:?} carries a path prefix")]
    #[exit(
        ConfigError,
        slug = "toolchain_name_path_prefix",
        summary = "A toolchain group or entry name carries a path prefix"
    )]
    PathPrefix {
        component: ToolchainPathComponent,
        value: String,
    },

    /// A trailing `.` or space, on every platform: Windows strips it, so `foo.` and `foo` name one directory.
    #[error("toolchain {component} name {value:?} ends with a dot or a space")]
    #[exit(
        ConfigError,
        slug = "toolchain_name_trailing_dot_or_space",
        summary = "A toolchain group or entry name ends with a dot or a space"
    )]
    TrailingDotOrSpace {
        component: ToolchainPathComponent,
        value: String,
    },

    /// `.` or `..`.
    #[error("toolchain {component} name {value:?} is a relative path component")]
    #[exit(
        ConfigError,
        slug = "toolchain_name_relative",
        summary = "A toolchain group or entry name is a relative path component"
    )]
    Relative {
        component: ToolchainPathComponent,
        value: String,
    },
}

/// The rendered toolchain tree at one root; every path question about it is answered here.
///
/// ```text
/// <root>/
/// ├── .gitignore                     "*", ensure-present, both tiers
/// ├── active -> shells/default/      the PATH-facing indirection
/// ├── links/<group>/<entry>/         directory link to a package root
/// └── shells/default/bin/            launcher trampolines, DEFAULT group only
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainHome {
    root: PathBuf,
}

impl ToolchainHome {
    /// Pure; `root` containment is enforced upstream, at the `config.toml` seam.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Never join a group or entry name onto this; only [`Self::entry`] and [`Self::links_group`] validate them.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Must be a link, so it is exempt from the renderer's symlinked-leaf refusal; see [`Self::active_is_valid`].
    pub fn active(&self) -> PathBuf {
        self.root.join(ACTIVE_LINK)
    }

    /// The PATH-facing `<root>/active/bin`; resolves through `active`, so never write, prune or fingerprint through it.
    pub fn bin(&self) -> PathBuf {
        self.active().join(TOOLCHAIN_BIN_DIR)
    }

    /// The physical render target, immune to a repointed `active`.
    ///
    /// `shell` is not validated; a shell name from configuration must go through `validate_component` first.
    pub fn shell_bin(&self, shell: &str) -> PathBuf {
        self.root.join(SHELLS_DIR).join(shell).join(TOOLCHAIN_BIN_DIR)
    }

    /// `active`'s one legal target; the heal and [`Self::active_is_valid`] both use this, never a second derivation.
    pub fn expected_active_target(&self, shell: &str) -> PathBuf {
        expected_active_target(&self.root, shell)
    }

    /// Whether `<root>/active` is a link at exactly its derived target; read-only, shared by render heal and prompt gate.
    ///
    /// Equality, not containment: an escaping `active` puts an arbitrary directory on `PATH` (CWE-426).
    pub fn active_is_valid(&self, shell: &str) -> bool {
        let active = self.active();
        // `is_link`, never `is_symlink`, which is false for a Windows junction.
        if !ocx_util::fs::symlink::is_link(&active) {
            return false;
        }
        // Raw `read_link`, never canonicalised: `shells/../shells/default` resolves right but is not what this tree writes.
        // An error is invalid, not unchanged: on Windows `is_link` is true for reparse points `read_link` cannot read.
        match std::fs::read_link(&active) {
            Ok(target) => targets_match(&target, &expected_active_target(&self.root, shell), cfg!(windows)),
            Err(_) => false,
        }
    }

    pub fn gitignore(&self) -> PathBuf {
        self.root.join(GITIGNORE_FILE)
    }

    /// Writes the ignore file unless it already matches; returns whether it wrote. Blocking.
    ///
    /// Ensure-present, not write-once, or one `git clean` unhides the tree for good.
    ///
    /// # Errors
    ///
    /// The stat, read or write fails; an absent file is not an error.
    pub fn ensure_gitignore(&self) -> Result<bool, ocx_util::error::FileError> {
        let path = self.gitignore();
        // Type-checked before opening: a hostile clone's link to `/dev/zero` exhausts memory and a FIFO hangs forever.
        // A link whose target already holds `*\n` must still be replaced, or repointing it revokes the ignore.
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                // `read_bounded`, never `fs::read`: a multi-gigabyte file of zeros is nearly free in a packfile (CWE-400).
                match ocx_util::fs::read_bounded(&path, GITIGNORE_CONTENT.len() as u64) {
                    Ok(current) if current == GITIGNORE_CONTENT => return Ok(false),
                    Ok(_) => {}
                    Err(BoundedReadError::TooLarge { .. }) => {}
                    Err(BoundedReadError::NotRegularFile { .. }) => {}
                    Err(BoundedReadError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
                    Err(BoundedReadError::Io { source, .. }) => {
                        return Err(ocx_util::error::FileError::new(&path, source));
                    }
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ocx_util::error::FileError::new(&path, e)),
        }

        std::fs::create_dir_all(&self.root).map_err(|e| ocx_util::error::FileError::new(&self.root, e))?;
        // Never `fs::write`, which follows a hostile `.gitignore` symlink to `~/.bashrc` and truncates it.
        ocx_util::fs::write_bytes_atomic(&path, GITIGNORE_CONTENT)
            .map_err(|e| ocx_util::error::FileError::new(&path, e))?;
        Ok(true)
    }

    /// `<root>/links/<group>/<entry>`; write it with `symlink::update`, never `ReferenceManager`, whose back-ref pins a GC root forever.
    ///
    /// # Errors
    ///
    /// [`ToolchainPathError`] for a refused component.
    pub fn entry(&self, group: &str, entry: &str) -> Result<PathBuf, ToolchainPathError> {
        let group_dir = self.links_group(group)?;
        validate_component(ToolchainPathComponent::Entry, entry)?;
        Ok(group_dir.join(entry))
    }

    /// `<root>/links/<group>`.
    ///
    /// # Errors
    ///
    /// [`ToolchainPathError`] for a refused group.
    pub fn links_group(&self, group: &str) -> Result<PathBuf, ToolchainPathError> {
        validate_component(ToolchainPathComponent::Group, group)?;
        Ok(self.root.join(LINKS_DIR).join(group))
    }
}

fn validate_component(component: ToolchainPathComponent, value: &str) -> Result<(), ToolchainPathError> {
    if value.is_empty() {
        return Err(ToolchainPathError::Empty { component });
    }
    // First, so no later refusal interpolates a raw control byte into its message.
    if value.chars().any(char::is_control) {
        return Err(ToolchainPathError::ControlCharacter {
            component,
            value: value.to_string(),
        });
    }
    // On characters, never `Path::components()`, which knows only the host's separators.
    if value.contains('/') || value.contains('\\') {
        return Err(ToolchainPathError::Separator {
            component,
            value: value.to_string(),
        });
    }
    if value.contains(':') {
        return Err(ToolchainPathError::PathPrefix {
            component,
            value: value.to_string(),
        });
    }
    // Before the trailing-dot check, so `.` and `..` report as relative.
    if value == "." || value == ".." {
        return Err(ToolchainPathError::Relative {
            component,
            value: value.to_string(),
        });
    }
    if value.ends_with('.') || value.ends_with(' ') {
        return Err(ToolchainPathError::TrailingDotOrSpace {
            component,
            value: value.to_string(),
        });
    }
    Ok(())
}

/// The global toolchain home: a thin wrapper over [`ToolchainHome`], never a second grammar.
#[derive(Debug, Clone)]
pub struct ToolchainStore {
    home: ToolchainHome,
}

impl ToolchainStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            home: ToolchainHome::new(root),
        }
    }

    pub fn home(&self) -> &ToolchainHome {
        &self.home
    }

    /// Never join a group or entry name onto this; see [`ToolchainHome::root`].
    pub fn root(&self) -> &Path {
        self.home.root()
    }

    pub fn bin(&self) -> PathBuf {
        self.home.bin()
    }

    pub fn shell_bin(&self, shell: &str) -> PathBuf {
        self.home.shell_bin(shell)
    }

    pub fn gitignore(&self) -> PathBuf {
        self.home.gitignore()
    }

    /// # Errors
    ///
    /// See [`ToolchainHome::ensure_gitignore`].
    pub fn ensure_gitignore(&self) -> Result<bool, ocx_util::error::FileError> {
        self.home.ensure_gitignore()
    }

    /// # Errors
    ///
    /// See [`ToolchainHome::entry`].
    pub fn entry(&self, group: &str, entry: &str) -> Result<PathBuf, ToolchainPathError> {
        self.home.entry(group, entry)
    }

    /// # Errors
    ///
    /// See [`ToolchainHome::links_group`].
    pub fn links_group(&self, group: &str) -> Result<PathBuf, ToolchainPathError> {
        self.home.links_group(group)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        DEFAULT_SHELL, ToolchainHome, ToolchainPathComponent, ToolchainPathError, ToolchainStore,
        expected_active_target, targets_match,
    };

    /// C-016 — **the global home ignores `toolchain_dir` entirely.**
    /// `$OCX_HOME/toolchain/` never moves, whatever any tier declares.
    ///
    /// It asserts over `FileStructure`, so it lives beside the store it asserts
    /// on rather than beside the config key it ignores: the config tier names
    /// nothing under `file_structure` (inversion 1.20). The sandbox helper is
    /// still `config`'s, because the anchor it produces has to satisfy C-018's
    /// own system-location refusal — a downward reach, and the allowed one.
    #[test]
    fn the_global_toolchain_home_ignores_every_declared_root() {
        let Some(sandbox) = ocx_config::sandbox_or_skip() else {
            return;
        };
        let env = ocx_env::overrides::lock();
        env.set(
            &ocx_env::OCX_HOME,
            sandbox.path().to_str().expect("anchor path is utf-8"),
        );
        env.set(
            &ocx_env::OCX_TOOLCHAIN_DIR,
            sandbox.path().join("elsewhere").to_str().expect("path is utf-8"),
        );

        let structure = crate::file_structure::FileStructure::new();
        assert_eq!(
            structure.toolchain.root(),
            sandbox.path().join("toolchain"),
            "the global home is $OCX_HOME/toolchain and no toolchain_dir tier moves it"
        );
    }

    /// Component names D-V14 refuses, one per refusal reason, reused by the
    /// containment guard so an admitted hostile name is caught rather than
    /// presumed impossible.
    const HOSTILE_COMPONENTS: &[&str] = &[
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "/etc",
        // Control characters, refused before every other rule so no later
        // message interpolates a raw byte. `\u{85}` is the C1 row a C0-only
        // rewrite would drop.
        "a\0b",
        "a\nb",
        "a\rb",
        "a\tb",
        "a\u{7f}b",
        "a\u{85}b",
        // Windows path prefixes: `PathBuf::push` replaces `self` entirely for
        // a path with a prefix but no root, so these discard the home root —
        // and, after the move under `links/`, discard that component too.
        "C:",
        "C:x",
        "a:b",
        // Windows strips trailing dots and spaces when resolving a path, so
        // two distinct lock keys would name one rendered directory. `bin` and
        // `.gitignore` themselves are no longer refused (C-073); these
        // trailing forms still are, and this is now their only fixture.
        "bin.",
        "bin ",
        ".gitignore.",
    ];

    /// Assert `bytes` are C-004's ignore pattern, exactly.
    ///
    /// The plan settled the spelling after this helper was first written: one
    /// line, `*`, newline-terminated. An exact byte equality rather than a set
    /// of accepted spellings, because C-047's byte-identical-across-two-renders
    /// contract is about bytes — a helper that tolerated two spellings could
    /// not tell a render that alternates between them from one that does not.
    fn assert_is_the_ignore_pattern(bytes: &[u8]) {
        assert_eq!(
            bytes,
            b"*\n",
            "C-004 — the ignore file must carry exactly the newline-terminated `*` pattern, got {:?}",
            String::from_utf8_lossy(bytes)
        );
    }

    // ── C-001: the store grammar ─────────────────────────────────────────────

    /// C-001 — a home answers back the root it was built with, unchanged.
    #[test]
    fn a_toolchain_home_reports_the_root_it_was_built_with() {
        let home = ToolchainHome::new("/w/proj/.ocx/toolchain");
        assert_eq!(home.root(), Path::new("/w/proj/.ocx/toolchain"));
    }

    /// C-078 — `bin()` keeps its name and its meaning (the directory the three
    /// PATH routes point at, and the one C-010's lookup-PATH exclusion is
    /// derived from) and now reaches it **through** the `active` link.
    #[test]
    fn the_path_facing_trampoline_directory_resolves_through_active() {
        let home = ToolchainHome::new("/w/proj/.ocx/toolchain");
        assert_eq!(home.bin(), PathBuf::from("/w/proj/.ocx/toolchain/active/bin"));
        assert_eq!(
            home.bin().parent(),
            Some(Path::new("/w/proj/.ocx/toolchain/active")),
            "C-078 — the PATH-facing directory is `bin` under the `active` link, not under the root"
        );
    }

    /// C-004 — the ignore file is `.gitignore`, directly under the home root.
    #[test]
    fn the_ignore_file_is_dot_gitignore_directly_under_the_home_root() {
        let home = ToolchainHome::new("/w/proj/.ocx/toolchain");
        assert_eq!(home.gitignore(), PathBuf::from("/w/proj/.ocx/toolchain/.gitignore"));
        assert_eq!(
            home.gitignore().parent(),
            Some(home.root()),
            "the ignore file must be a direct child of the home root"
        );
    }

    /// C-001 — the store is a wrapper over one home, never a second grammar, so
    /// every path accessor must answer exactly what that one home answers. Both
    /// spellings C-010 derives its exclusion from resolve to the same rule.
    #[test]
    fn the_store_forwards_every_path_accessor_to_its_one_home() {
        let store = ToolchainStore::new("/ocx/toolchain");
        assert_eq!(store.root(), store.home().root(), "root() must forward");
        assert_eq!(store.bin(), store.home().bin(), "bin() must forward");
        assert_eq!(
            store.shell_bin("default"),
            store.home().shell_bin("default"),
            "shell_bin() must forward"
        );
        assert_eq!(store.gitignore(), store.home().gitignore(), "gitignore() must forward");
        assert_eq!(
            store.links_group("ci"),
            store.home().links_group("ci"),
            "links_group() must forward"
        );
    }

    /// C-001, D-V14 — `entry` forwards too, so the store cannot admit a
    /// component the home refuses, nor spell an accepted one differently.
    #[test]
    fn the_store_forwards_entry_to_its_one_home() {
        let store = ToolchainStore::new("/ocx/toolchain");
        assert_eq!(
            store.entry("default", "cmake"),
            store.home().entry("default", "cmake"),
            "an accepted pair must resolve identically through both spellings"
        );
        // `"a/b"`, not `"bin"`: `bin` is an accepted name after C-073, so the
        // old probe would have gone on comparing two `Ok`s and silently stopped
        // testing refusal-forwarding at all.
        assert_eq!(
            store.entry("a/b", "cmake"),
            store.home().entry("a/b", "cmake"),
            "a refused component must be refused identically through both spellings"
        );
        assert!(
            store.entry("a/b", "cmake").is_err(),
            "the control: the probe pair must actually be refused, or this test compares two Ok values"
        );
    }

    // ── C-004: the ignore file ───────────────────────────────────────────────

    /// C-004 — an absent ignore file is written with the `*` pattern, and the
    /// call reports that it wrote.
    #[test]
    fn ensure_gitignore_writes_the_star_pattern_when_the_file_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = ToolchainHome::new(tmp.path().join("toolchain"));
        std::fs::create_dir_all(home.root()).unwrap();

        assert!(
            home.ensure_gitignore()
                .expect("writing an absent ignore file must succeed"),
            "C-004 — the first call writes, so it reports true"
        );
        assert_is_the_ignore_pattern(&std::fs::read(home.gitignore()).unwrap());
    }

    /// C-004 — ensure-present, not write-once: content that differs is
    /// rewritten, so one `git clean` cannot permanently unhide the tree.
    #[test]
    fn ensure_gitignore_rewrites_content_that_differs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = ToolchainHome::new(tmp.path().join("toolchain"));
        std::fs::create_dir_all(home.root()).unwrap();
        std::fs::write(home.gitignore(), b"# not the ignore pattern\n").unwrap();

        assert!(
            home.ensure_gitignore()
                .expect("rewriting a differing ignore file must succeed"),
            "C-004 — differing content is rewritten, so the call reports true"
        );
        assert_is_the_ignore_pattern(&std::fs::read(home.gitignore()).unwrap());
    }

    /// C-004, C-047 — a file already carrying the pattern is left exactly as it
    /// is and the call reports that it wrote nothing; two consecutive calls
    /// leave byte-identical content, which is render idempotence's half of this
    /// contract.
    ///
    /// The seed is `ensure_gitignore`'s own first write rather than a literal,
    /// so the test cannot pass merely because it guessed the same spelling.
    #[test]
    fn ensure_gitignore_leaves_a_matching_file_untouched_and_reports_no_write() {
        let tmp = tempfile::tempdir().unwrap();
        let home = ToolchainHome::new(tmp.path().join("toolchain"));
        std::fs::create_dir_all(home.root()).unwrap();

        assert!(home.ensure_gitignore().expect("the first call must write"));
        let after_first = std::fs::read(home.gitignore()).unwrap();

        assert!(
            !home.ensure_gitignore().expect("a second call must succeed"),
            "C-004 — an already-matching file is not rewritten, so the call reports false"
        );
        assert_eq!(
            std::fs::read(home.gitignore()).unwrap(),
            after_first,
            "C-047 — two consecutive calls must leave byte-identical content"
        );
    }

    /// C-004 — the write must not follow a symlink planted at the ignore path.
    ///
    /// A hostile clone can ship `.ocx/toolchain/.gitignore` as a symlink to
    /// `~/.bashrc`: `fs::write` follows and truncates the victim, while
    /// temp-in-parent-then-rename replaces the link itself. This is the case
    /// that discriminates `write_bytes_atomic` from `fs::write`, and it is why
    /// the contract names the helper rather than leaving the write unspecified.
    #[cfg(unix)]
    #[test]
    fn ensure_gitignore_replaces_a_planted_symlink_instead_of_writing_through_it() {
        let tmp = tempfile::tempdir().unwrap();
        let decoy = tmp.path().join("decoy_outside_the_tree");
        let decoy_content: &[u8] = b"# the victim's file, which must survive untouched\n";
        std::fs::write(&decoy, decoy_content).unwrap();

        let home = ToolchainHome::new(tmp.path().join("toolchain"));
        std::fs::create_dir_all(home.root()).unwrap();
        std::os::unix::fs::symlink(&decoy, home.gitignore()).unwrap();

        home.ensure_gitignore()
            .expect("a planted symlink must not make the write fail");

        assert_eq!(
            std::fs::read(&decoy).unwrap(),
            decoy_content,
            "C-004 — the decoy outside the tree must be byte-identical afterwards"
        );
        let metadata = std::fs::symlink_metadata(home.gitignore()).unwrap();
        assert!(
            metadata.file_type().is_file(),
            "C-004 — the ignore path must end up a regular file, not the planted symlink"
        );
        assert_is_the_ignore_pattern(&std::fs::read(home.gitignore()).unwrap());
    }

    /// C-004 — a planted symlink is replaced **even when its target already
    /// carries the pattern**.
    ///
    /// The sibling above uses differing decoy content, so it only ever
    /// exercises the branch where the write already runs: it stays green with
    /// no type check at all. This case is the one that reds without the
    /// `symlink_metadata` gate — a link whose target holds `*\n`
    /// short-circuits the content compare, `ensure_gitignore` reports "already
    /// correct", and the link survives every render. The ignore capability is
    /// then attacker-revocable: repoint the target later and the rendered tree
    /// becomes committable, with no render ever noticing.
    ///
    /// The invariant the write establishes is therefore a *type*, not a byte
    /// string: the ignore path must be a regular file afterwards.
    #[cfg(unix)]
    #[test]
    fn ensure_gitignore_replaces_a_planted_symlink_whose_target_already_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let decoy = tmp.path().join("decoy_already_carrying_the_pattern");
        std::fs::write(&decoy, b"*\n").unwrap();

        let home = ToolchainHome::new(tmp.path().join("toolchain"));
        std::fs::create_dir_all(home.root()).unwrap();
        std::os::unix::fs::symlink(&decoy, home.gitignore()).unwrap();

        home.ensure_gitignore()
            .expect("a planted symlink must not make the write fail");

        let metadata = std::fs::symlink_metadata(home.gitignore()).unwrap();
        assert!(
            metadata.file_type().is_file(),
            "C-004 — the ignore path must end up a regular file; a matching link that survives leaves the \
             ignore capability revocable by repointing it"
        );
        assert_is_the_ignore_pattern(&std::fs::read(home.gitignore()).unwrap());
    }

    /// C-004 — a regular file larger than the cap is rewritten to the pattern.
    ///
    /// This pins the `TooLarge` → write mapping, which is the branch a hostile
    /// clone's multi-gigabyte `.gitignore` lands on: over-cap means "longer
    /// than the canonical content", which means "differs from it", which is a
    /// write and never an error.
    ///
    /// It does **not** prove the read was bounded. The cap is exactly the
    /// canonical content's length, so over-cap and "content differs" are one
    /// answer by construction, and an unbounded read reaches this same
    /// assertion — after loading the whole file, which is the CWE-400. The
    /// bound is proved by the sibling guard below.
    #[test]
    fn ensure_gitignore_rewrites_a_regular_file_larger_than_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let home = ToolchainHome::new(tmp.path().join("toolchain"));
        std::fs::create_dir_all(home.root()).unwrap();
        // A plain regular file, no symlink and no device: this is the arm the
        // `symlink_metadata` gate lets through by design. 4 KiB stands in for
        // the real hazard, which is gigabytes no test can afford to plant.
        std::fs::write(home.gitignore(), vec![0u8; 4096]).unwrap();

        assert!(
            home.ensure_gitignore()
                .expect("an over-cap ignore file must not make the call fail"),
            "C-004 — content past the cap differs from the pattern, so the call writes and reports true"
        );
        assert_is_the_ignore_pattern(&std::fs::read(home.gitignore()).unwrap());
    }

    /// C-004 — the ignore file is read through the bounded helper, capped at
    /// the canonical content's own length.
    ///
    /// A source-text guard because no behavioural one exists. The cap equals
    /// `GITIGNORE_CONTENT.len()`, so bounded and unbounded agree on every
    /// input and differ only in what the process held while deciding — the
    /// tightness that makes the fix safe is the same tightness that hides it.
    /// `read_bounded`'s own module answers this by splitting the ceiling out
    /// over a reader whose `Take::limit` a test can interrogate; one layer up,
    /// at a path, there is no handle left to ask.
    ///
    /// Scoped to `ToolchainHome::ensure_gitignore`'s body with comment lines
    /// stripped, so neither this paragraph nor the body's own rationale can
    /// satisfy the guard, and every needle below is spelled only in this test
    /// half, which the split excludes. The `symlink_metadata` assertion proves
    /// the extraction landed on the intended function rather than silently on
    /// `ToolchainStore`'s one-line forward. The negative needle is a tripwire
    /// for the likely accident — a revert to `std::fs::read` — not the
    /// contract; the positive ones are the contract.
    #[test]
    fn ensure_gitignore_reads_through_the_bounded_helper() {
        let production = include_str!("toolchain_store.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half");
        let start = production
            .find("pub fn ensure_gitignore")
            .expect("the needle stopped matching — this guard would now pass on any code at all");
        let tail = &production[start..];
        let end = tail
            .find("\n    }\n")
            .expect("the function must close at the impl's own indent");
        let body: String = tail[..end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            body.contains("symlink_metadata"),
            "the extraction landed on the wrong function, so the rest of this guard means nothing:\n{body}"
        );
        assert!(
            body.contains("read_bounded(&path, GITIGNORE_CONTENT.len() as u64)"),
            "C-004 — the ignore file must be read through the bounded helper, capped at the canonical \
             content's own length; a hostile clone ships a plain regular `.gitignore` of arbitrary size \
             and the stat gate passes it (CWE-400):\n{body}"
        );
        assert!(
            !body.contains("fs::read("),
            "C-004 — an unbounded read of a path inside an attacker-controlled tree:\n{body}"
        );
    }

    /// C-004 — one code path serves both tiers: the `FileStructure`-owned global
    /// store and a project home write the same bytes to their own trees.
    #[test]
    fn ensure_gitignore_serves_the_global_store_and_a_project_home_alike() {
        let tmp = tempfile::tempdir().unwrap();

        let global = ToolchainStore::new(tmp.path().join("ocx-home").join("toolchain"));
        std::fs::create_dir_all(global.root()).unwrap();
        let project = ToolchainHome::new(tmp.path().join("proj").join(".ocx").join("toolchain"));
        std::fs::create_dir_all(project.root()).unwrap();

        assert!(global.ensure_gitignore().expect("the global tier must write"));
        assert!(project.ensure_gitignore().expect("the project tier must write"));
        assert_eq!(
            std::fs::read(global.gitignore()).unwrap(),
            std::fs::read(project.gitignore()).unwrap(),
            "C-004 — both tiers are one code path, so both files carry the same bytes"
        );
    }

    // ── D-V14: `entry` validates its own inputs ──────────────────────────────

    /// D-V14 — an empty component collapses away and would make `<group>/<entry>`
    /// name the group directory itself.
    #[test]
    fn entry_refuses_an_empty_component() {
        let home = ToolchainHome::new("/ocx/toolchain");
        assert_eq!(
            home.entry("", "cmake"),
            Err(ToolchainPathError::Empty {
                component: ToolchainPathComponent::Group
            })
        );
        assert_eq!(
            home.entry("default", ""),
            Err(ToolchainPathError::Empty {
                component: ToolchainPathComponent::Entry
            })
        );
    }

    /// D-V14 — `.` and `..` navigate rather than name.
    #[test]
    fn entry_refuses_a_relative_path_component() {
        let home = ToolchainHome::new("/ocx/toolchain");
        for name in [".", ".."] {
            assert_eq!(
                home.entry(name, "cmake"),
                Err(ToolchainPathError::Relative {
                    component: ToolchainPathComponent::Group,
                    value: name.to_string()
                })
            );
            assert_eq!(
                home.entry("default", name),
                Err(ToolchainPathError::Relative {
                    component: ToolchainPathComponent::Entry,
                    value: name.to_string()
                })
            );
        }
    }

    /// D-V14 — **both** separators, on **every** platform, and deliberately not
    /// behind a `#[cfg(windows)]`.
    ///
    /// A name containing `\` is one component on Unix and two on Windows, so a
    /// platform-conditional refusal would render `ocx.lock`'s `a\b` as a
    /// directory literally named `a\b` on Linux while refusing it on Windows —
    /// and would put its regression test behind a cfg the CI leg that actually
    /// runs never compiles, which is a green indistinguishable from never
    /// having run.
    #[test]
    fn entry_refuses_either_path_separator_on_every_platform() {
        let home = ToolchainHome::new("/ocx/toolchain");
        for name in ["a/b", "a\\b", "/etc", "etc/"] {
            assert!(
                matches!(
                    home.entry(name, "cmake"),
                    Err(ToolchainPathError::Separator {
                        component: ToolchainPathComponent::Group,
                        ..
                    })
                ),
                "D-V14 — group {name:?} carries a path separator and must be refused"
            );
            assert!(
                matches!(
                    home.entry("default", name),
                    Err(ToolchainPathError::Separator {
                        component: ToolchainPathComponent::Entry,
                        ..
                    })
                ),
                "D-V14 — entry {name:?} carries a path separator and must be refused"
            );
        }
    }

    /// D-V14 — a control character is refused in either component, on every
    /// platform.
    ///
    /// `ocx.lock` is a file a hostile clone ships, and C-065 makes a lock
    /// entry's name a component of an emitted `PATH` value: a newline in one
    /// forges a log line (CWE-117) and forges a `$GITHUB_PATH` entry (CWE-77).
    /// Four sinks absorb it today, which is four independent defences for one
    /// input — this refuses it once, where the untrusted value first becomes a
    /// path.
    ///
    /// C1 (`U+0085`) is in the set as well as C0: it is invisible in a terminal
    /// for the same reason, and `validate_namespace` states the same rule.
    #[test]
    fn entry_refuses_a_control_character_in_either_component() {
        let home = ToolchainHome::new("/ocx/toolchain");
        for name in ["a\nb", "a\rb", "a\tb", "a\u{0}b", "a\u{7f}b", "a\u{85}b"] {
            assert!(
                matches!(
                    home.entry(name, "cmake"),
                    Err(ToolchainPathError::ControlCharacter {
                        component: ToolchainPathComponent::Group,
                        ..
                    })
                ),
                "D-V14 — group {name:?} carries a control character and must be refused"
            );
            assert!(
                matches!(
                    home.entry("default", name),
                    Err(ToolchainPathError::ControlCharacter {
                        component: ToolchainPathComponent::Entry,
                        ..
                    })
                ),
                "D-V14 — entry {name:?} carries a control character and must be refused"
            );
        }
        // The refusal is on control characters, not on "unusual" bytes: a name a
        // publisher may legitimately ship still passes.
        assert!(home.entry("default", "cmake-3.28_x86").is_ok());
    }

    /// D-V14 — a Windows path **prefix** is refused on **every** platform.
    ///
    /// `PathBuf::push` documents that a path carrying a prefix but no root
    /// *replaces `self`*: on Windows `root.join("C:").join("cmake")` is
    /// `C:cmake` with the home root discarded, and `entry("default", "C:")`
    /// collapses the whole path to `C:` — containment gone, from a group key
    /// a hostile `ocx.lock` ships.
    ///
    /// Deliberately not behind a `#[cfg(windows)]`, and this assertion reds on
    /// a Linux host, which is the point: `Path::is_absolute` only parses those
    /// prefixes on Windows, so a cfg-gated refusal would put its only
    /// regression test behind a leg CI never compiles. The shipped
    /// `join_under_root` states the same rule for the same reason.
    #[test]
    fn entry_refuses_a_windows_path_prefix_on_every_platform() {
        let home = ToolchainHome::new("/ocx/toolchain");
        for name in ["C:", "C:x", "a:b", "C:\\Windows"] {
            assert!(
                matches!(
                    home.entry(name, "cmake"),
                    Err(ToolchainPathError::PathPrefix {
                        component: ToolchainPathComponent::Group,
                        ..
                    }) | Err(ToolchainPathError::Separator {
                        component: ToolchainPathComponent::Group,
                        ..
                    })
                ),
                "D-V14 — group {name:?} carries a path prefix and must be refused"
            );
            assert!(
                matches!(
                    home.entry("default", name),
                    Err(ToolchainPathError::PathPrefix {
                        component: ToolchainPathComponent::Entry,
                        ..
                    }) | Err(ToolchainPathError::Separator {
                        component: ToolchainPathComponent::Entry,
                        ..
                    })
                ),
                "D-V14 — entry {name:?} carries a path prefix and must be refused"
            );
        }

        // The prefix-only spellings must reach the prefix arm specifically —
        // without this the separator refusal alone would satisfy the matcher
        // above for `C:\Windows` and the new check could be deleted unnoticed.
        assert!(
            matches!(home.entry("C:", "cmake"), Err(ToolchainPathError::PathPrefix { .. })),
            "D-V14 — a bare drive letter is a prefix refusal, not a separator one"
        );
    }

    /// C-015, C-013 — a trailing `.` or space is refused, because Windows
    /// strips both when it resolves a path.
    ///
    /// `"bin."` and `"bin "` do not `eq_ignore_ascii_case("bin")`, so without
    /// this refusal such a group lands on the trampoline directory the
    /// reservation exists to protect. Unconditional on every platform, and
    /// these assertions red on a Linux host.
    #[test]
    fn entry_refuses_a_trailing_dot_or_space_on_every_platform() {
        let home = ToolchainHome::new("/ocx/toolchain");
        for name in ["bin.", "bin ", ".gitignore.", "cmake.", "cmake "] {
            assert!(
                matches!(
                    home.entry(name, "cmake"),
                    Err(ToolchainPathError::TrailingDotOrSpace {
                        component: ToolchainPathComponent::Group,
                        ..
                    })
                ),
                "C-015 — group {name:?} resolves to a stripped name on Windows and must be refused"
            );
            assert!(
                matches!(
                    home.entry("default", name),
                    Err(ToolchainPathError::TrailingDotOrSpace {
                        component: ToolchainPathComponent::Entry,
                        ..
                    })
                ),
                "C-013 — entry {name:?} resolves to a stripped name on Windows and must be refused"
            );
        }

        // `.` and `..` end with a dot too, and must keep naming themselves as
        // relative components — the ordering of the two checks is load-bearing.
        assert!(matches!(
            home.entry(".", "cmake"),
            Err(ToolchainPathError::Relative { .. })
        ));
    }

    /// C-071, C-073 — the tree's own names are **accepted** as group and entry
    /// names, because no user-supplied name is a depth-1 component any more.
    ///
    /// `bin` and `.gitignore` were reserved for a collision that the move
    /// under `links/` removes by construction; `links`, `shells` and `active`
    /// are deliberately *not* reserved in their place — a group name sits one
    /// level below every tree-own name, so `[group.links]` renders at
    /// `links/links/<entry>` and collides with nothing. Three separate readings
    /// of this record have proposed re-reserving them, so the acceptance is
    /// asserted positively rather than left as an absence.
    ///
    /// RED: restore the `eq_ignore_ascii_case` arm in `validate_component`, or
    /// add one for the depth-1 set, and every row here exits `Err`.
    #[test]
    fn the_trees_own_names_are_accepted_as_group_and_entry_names() {
        let home = ToolchainHome::new("/ocx/toolchain");
        for name in [
            "bin",
            "Bin",
            "BIN",
            ".gitignore",
            ".GITIGNORE",
            "links",
            "shells",
            "active",
            "Links",
            "Shells",
            "Active",
        ] {
            assert_eq!(
                home.entry(name, "cmake")
                    .unwrap_or_else(|e| panic!("C-073 — group {name:?} must be accepted, got {e}")),
                PathBuf::from("/ocx/toolchain/links").join(name).join("cmake"),
                "C-071 — a tree-own name is an ordinary group one level below `links`"
            );
            assert_eq!(
                home.entry("default", name)
                    .unwrap_or_else(|e| panic!("C-073 — entry {name:?} must be accepted, got {e}")),
                PathBuf::from("/ocx/toolchain/links/default").join(name),
                "C-071 — a tree-own name is an ordinary entry two levels below `links`"
            );
            assert_eq!(
                home.links_group(name)
                    .unwrap_or_else(|e| panic!("C-072 — links_group({name:?}) must be accepted, got {e}")),
                PathBuf::from("/ocx/toolchain/links").join(name),
                "C-072 — the group accessor admits exactly what `entry` admits"
            );
        }
    }

    /// D-V14, C-072 — an ordinary pair resolves under `links/`, and `default`
    /// specifically stays a legal group directory: it is the group the
    /// trampolines expose, so refusing it would make the common tree
    /// unrenderable.
    #[test]
    fn entry_accepts_an_ordinary_group_and_entry_pair() {
        let home = ToolchainHome::new("/ocx/toolchain");
        assert_eq!(
            home.entry("default", "cmake").expect("`default`/`cmake` must resolve"),
            PathBuf::from("/ocx/toolchain/links/default/cmake")
        );
        assert_eq!(
            home.entry("ci", "python3.13")
                .expect("an ordinary group/entry pair must resolve"),
            PathBuf::from("/ocx/toolchain/links/ci/python3.13")
        );
    }

    /// C-071, D-V14 — containment. Every accepted pair lands exactly three
    /// components below the home root with `links` first, and every hostile
    /// component is refused outright through **both** accessors, so no admitted
    /// name can widen into a fourth component or escape the tree.
    ///
    /// Component-wise on `strip_prefix`, never `starts_with`: `..` passes a
    /// `starts_with` check, so a prefix test would admit exactly the escape
    /// this guard exists to catch.
    ///
    /// Modelled on the shipped `cache_files_reject_hostile_slug` guard in
    /// `state_store.rs`, and run over the hostile list rather than only the
    /// accepted one — a refusal that leaked through would otherwise be presumed
    /// impossible instead of checked.
    #[test]
    fn every_resolved_entry_stays_exactly_three_components_below_the_home_root() {
        let home = ToolchainHome::new("/ocx/toolchain");

        for (group, entry) in [("default", "cmake"), ("ci", "python3.13"), ("Release", "MSBuild")] {
            let path = home.entry(group, entry).expect("an accepted pair must resolve");
            let tail: Vec<_> = path
                .strip_prefix(home.root())
                .expect("a resolved entry must stay under the home root")
                .components()
                .collect();
            assert_eq!(tail.len(), 3, "expected links/<group>/<entry>, got {path:?}");
            assert_eq!(
                tail[0].as_os_str(),
                "links",
                "C-071 — depth 1 is tree-owned, so a rendered entry starts at `links`, got {path:?}"
            );
        }

        for name in HOSTILE_COMPONENTS {
            assert!(
                home.entry(name, "cmake").is_err(),
                "D-V14 — group {name:?} must be refused, never rendered"
            );
            assert!(
                home.entry("default", name).is_err(),
                "D-V14 — entry {name:?} must be refused, never rendered"
            );
            assert!(
                home.links_group(name).is_err(),
                "C-072 — links_group({name:?}) must be refused, never rendered"
            );
        }
    }

    /// C-072 — the two accessors cannot drift: `links_group` refuses **exactly**
    /// what `entry`'s group component refuses, with the same variant and the
    /// same component, and an accepted `entry` is its `links_group` plus one
    /// component.
    ///
    /// The equality is on the whole error, not on `is_err()`: the refusal order
    /// in `validate_component` is load-bearing (a control byte outranks a
    /// separator so no later message interpolates a raw control byte), and only
    /// a variant-level comparison can see that order diverge.
    ///
    /// RED: give `links_group` its own inlined checks instead of routing them
    /// both through `validate_component`, and reorder any two — `"a\nb/c"` then
    /// reports `Separator` through one accessor and `ControlCharacter` through
    /// the other.
    #[test]
    fn links_group_and_entry_refuse_the_same_component_identically() {
        let home = ToolchainHome::new("/ocx/toolchain");

        for name in HOSTILE_COMPONENTS.iter().chain(["a\nb/c", "a b."].iter()) {
            assert_eq!(
                home.links_group(name).unwrap_err(),
                home.entry(name, "cmake").unwrap_err(),
                "C-072 — {name:?} must be refused identically through both accessors"
            );
        }

        for (group, entry) in [("default", "cmake"), ("bin", "ocx"), ("ci", "python3.13")] {
            let resolved = home.entry(group, entry).expect("an accepted pair must resolve");
            assert_eq!(
                resolved.parent(),
                Some(
                    home.links_group(group)
                        .expect("an accepted group must resolve")
                        .as_path()
                ),
                "C-072 — entry() is links_group() plus exactly one component"
            );
        }
    }

    /// C-073 — `TrailingDotOrSpace` survives the reservation's deletion, and it
    /// is now the *only* rule refusing `bin.` and `bin `.
    ///
    /// Kept as its own test because the reservation's removal takes `"bin"`,
    /// `"Bin"` and `".gitignore"` out of the refused set: deleting the whole
    /// `bin` family together would silently drop R6's only fixture with them.
    /// The rule it now defends is a render-identity collision, not a reserved
    /// name — Windows strips the trailing form, so `foo.` and `foo` would name
    /// one `links/<group>` directory from two distinct lock keys.
    #[test]
    fn a_trailing_dot_or_space_is_still_refused_after_the_reservation_is_gone() {
        let home = ToolchainHome::new("/ocx/toolchain");
        for name in ["bin.", "bin ", ".gitignore.", "links.", "cmake "] {
            assert!(
                matches!(
                    home.entry(name, "cmake"),
                    Err(ToolchainPathError::TrailingDotOrSpace {
                        component: ToolchainPathComponent::Group,
                        ..
                    })
                ),
                "group {name:?} must still be refused as a trailing dot or space"
            );
            assert!(
                matches!(
                    home.links_group(name),
                    Err(ToolchainPathError::TrailingDotOrSpace { .. })
                ),
                "links_group({name:?}) must still be refused as a trailing dot or space"
            );
        }
    }

    /// D-V14 — the grammar has **no length cap and no confusable folding**, and
    /// that is a decision rather than an omission.
    ///
    /// The length cap is `SLUG_MAX_LEN` at the `ocx.toml` parse seam, which
    /// `ocx.lock` group keys bypass by D-V14's own premise; adding one here
    /// would be a second, silently different rule. A Unicode look-alike is a
    /// *distinct* directory, which is the correct outcome once no depth-1 name
    /// can be shadowed. Asserted positively so either becoming a refusal is a
    /// deliberate change and not a drift.
    #[test]
    fn the_grammar_caps_no_length_and_folds_no_confusable() {
        let home = ToolchainHome::new("/ocx/toolchain");
        let long = "a".repeat(300);
        assert!(home.entry(&long, "cmake").is_ok(), "the grammar imposes no length cap");

        // Cyrillic `с` (U+0441), not ASCII `c`.
        let confusable = "сmake";
        assert_ne!(confusable, "cmake", "the control: the two spellings differ in bytes");
        assert_eq!(
            home.entry("default", confusable)
                .expect("a Unicode look-alike is an ordinary name"),
            PathBuf::from("/ocx/toolchain/links/default").join(confusable)
        );
    }

    // ── C-079/C-080: the `active` validity predicate ─────────────────────────

    /// Plant `state` at `<root>/active` and answer the predicate.
    ///
    /// Takes the raw target rather than a typed state so every row of the
    /// truth table below reads as the on-disk value it describes; `None` means
    /// "not a link", handled by the caller.
    fn active_link_to(root: &Path, target: &str) -> bool {
        std::fs::create_dir_all(root).unwrap();
        // The derived target must exist on disk, or every row of the table
        // below is decided by an I/O failure rather than by the predicate: a
        // canonicalising mutation errors on all of them, fails closed, and reds
        // the *positive* row instead of the ones it targets — a mutation
        // killing the fixture, not the property. Measured, not reasoned.
        std::fs::create_dir_all(root.join(super::SHELLS_DIR).join(DEFAULT_SHELL)).unwrap();
        let link = root.join("active");
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(target, &link).unwrap();
        ToolchainHome::new(root).active_is_valid(DEFAULT_SHELL)
    }

    /// C-080 — the predicate is true for **exactly one** value, the derived
    /// target, and false for every other on-disk state.
    ///
    /// One row per state class the corrupt-states hunt enumerated, driven over
    /// a real `TempDir` because the question is about the filesystem, not about
    /// string handling.
    ///
    /// RED, one mutation per class: replace the equality with
    /// `target.starts_with("shells")` and the wrong-shell, dot-prefixed and
    /// `..`-traversing rows all flip to `true` — the wrong-shell row is the
    /// security-relevant one, since it names another shell's real `bin`.
    /// One contained row's target, spelled the way the platform stores one.
    ///
    /// POSIX keeps the relative `shells/ghost`; Windows keeps an absolute
    /// `<root>\\shells\\ghost`, because [`expected_active_target`] derives an
    /// absolute target there and a junction accepts nothing else. A row planted
    /// with the POSIX spelling on Windows is refused for being *relative*, so it
    /// answers `false` without ever reaching the comparison it exists to
    /// exercise — the whole row would be a green that cannot go red.
    ///
    /// Component-wise rather than by separator substitution: the point is to
    /// name a real, contained, wrong directory, and only `join` spells that
    /// correctly on both platforms.
    fn contained(root: &Path, components: &[&str]) -> String {
        let mut path = if cfg!(windows) {
            root.to_path_buf()
        } else {
            PathBuf::new()
        };
        for component in components {
            path.push(component);
        }
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn active_is_valid_admits_exactly_the_derived_target() {
        let tmp = tempfile::tempdir().unwrap();

        // The one legal value, derived exactly as the renderer derives it —
        // relative on POSIX, absolute on Windows. Not circular: the literal
        // output of `expected_active_target` is pinned on both platforms by
        // `targets_match_folds_case_only_when_told_to` below, so this row
        // asserts what it says it does — the predicate *admits* that value.
        let legal_root = tmp.path().join("v0");
        let legal = expected_active_target(&legal_root, DEFAULT_SHELL);
        assert!(
            active_link_to(&legal_root, &legal.to_string_lossy()),
            "C-079 — the derived target is the one valid value"
        );

        // Every other link target. The first four are real, contained, wrong
        // directories and are spelled per platform; the rest are *about* their
        // raw spelling (traversal, trailing separator, a differently-spelled
        // equivalent) or are absolute-outside, and so are planted verbatim on
        // both platforms, where each is a distinct stored target either way.
        let contained_rows = [
            vec!["shells", "ghost"], // a shell that does not exist
            vec!["shells", "other"], // a real, wrong shell — the security row
            vec!["active"],          // self
            vec!["shells"],          // one component short
        ];
        let verbatim_rows = [
            "/tmp/evil",                // absolute, outside the home
            "../../escape",             // relative, outside the home
            "./shells/default",         // resolves correctly, spelled differently
            "shells/../shells/default", // resolves correctly, contained, traversing
            "shells/default/",          // resolves correctly, trailing separator
        ];

        let mut row = 0;
        let refuse = |root: PathBuf, target: String| {
            assert!(
                !active_link_to(&root, &target),
                "C-080 — `active -> {target:?}` is not the derived target and must be invalid"
            );
        };
        for components in &contained_rows {
            let root = tmp.path().join(format!("v-{row}"));
            row += 1;
            let target = contained(&root, components);
            refuse(root, target);
        }
        for target in verbatim_rows {
            let root = tmp.path().join(format!("v-{row}"));
            row += 1;
            refuse(root, target.to_string());
        }
    }

    /// C-080 — a path that is not a link is invalid, whatever else it is.
    ///
    /// `read_link` on a real directory fails `EINVAL`, so a predicate written
    /// as `is_link(p) && read_link(p).map_or(true, ..)` would report these
    /// `true`; the `is_link` gate is what actually refuses them here, and the
    /// `Err`-is-invalid rule is what refuses a Windows non-junction reparse
    /// point, which no Linux leg can plant.
    #[test]
    fn active_is_valid_refuses_every_non_link_kind() {
        let tmp = tempfile::tempdir().unwrap();

        let absent = tmp.path().join("absent");
        std::fs::create_dir_all(&absent).unwrap();
        assert!(
            !ToolchainHome::new(&absent).active_is_valid(DEFAULT_SHELL),
            "an absent `active` is invalid, and the render creates it"
        );

        let populated = tmp.path().join("populated");
        std::fs::create_dir_all(populated.join("active").join("bin")).unwrap();
        assert!(
            !ToolchainHome::new(&populated).active_is_valid(DEFAULT_SHELL),
            "the `cp -rL` outcome — a real directory where the link belongs — is invalid"
        );

        let file = tmp.path().join("file");
        std::fs::create_dir_all(&file).unwrap();
        std::fs::write(file.join("active"), b"not a link").unwrap();
        assert!(
            !ToolchainHome::new(&file).active_is_valid(DEFAULT_SHELL),
            "a regular file where the link belongs is invalid"
        );
    }

    /// C-079 — the expected target is a pure function of `(root, shell)` and
    /// the comparison's case fold is a **parameter**, not a `cfg!`.
    ///
    /// The fold only fires on Windows, so a `cfg!` inside the comparison would
    /// leave one of its two arms unreachable on the only CI leg that runs —
    /// the RUL-10/C-025 unreachable-red class this record rejected option D
    /// for. Driving `targets_match` with the flag as data exercises both arms
    /// on Linux.
    #[test]
    fn targets_match_folds_case_only_when_told_to() {
        assert!(targets_match(
            Path::new("shells/Default"),
            Path::new("shells/default"),
            true
        ));
        assert!(!targets_match(
            Path::new("shells/Default"),
            Path::new("shells/default"),
            false
        ));
        // Raw, never `PathBuf`-normalised: a trailing separator is a different
        // stored target, and `PathBuf`'s own `PartialEq` would call these equal.
        assert!(!targets_match(
            Path::new("shells/default/"),
            Path::new("shells/default"),
            false
        ));
        assert_eq!(
            expected_active_target(Path::new("/ocx/toolchain"), DEFAULT_SHELL),
            if cfg!(windows) {
                PathBuf::from("/ocx/toolchain").join("shells").join("default")
            } else {
                PathBuf::from("shells/default")
            },
            "C-079 — POSIX derives the relative target, Windows the absolute one"
        );
    }

    /// C-080 — `ocx_util::fs::symlink::is_link`, never `Path::is_symlink`, and raw
    /// `read_link`, never a canonicalising probe.
    ///
    /// A structural guard because no behavioural test on Linux can see the
    /// first half: there `is_link` *is* `Path::is_symlink`, so swapping them
    /// leaves every row of the truth table answering the same way. It is the
    /// Windows junction rows the swap breaks, and this repository has no
    /// Windows leg. Modelled on `the_forge_client_disables_redirects` in
    /// `forge/http.rs`: comments stripped so the rationale above cannot satisfy
    /// the needle, and the test module cut off so the assertion's own string
    /// literals cannot either.
    #[test]
    fn the_active_predicate_probes_links_the_junction_aware_way() {
        let source = include_str!("toolchain_store.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields a first part");
        let code: String = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            code.contains("ocx_util::fs::symlink::is_link("),
            "C-080 — the predicate must probe through `ocx_util::fs::symlink::is_link`, which sees a Windows junction"
        );
        assert_eq!(
            code.matches("is_symlink(").count(),
            0,
            "C-080 — `Path::is_symlink` reports false for a junction, so the predicate must never call it"
        );
        assert_eq!(
            code.matches("canonicalize").count(),
            0,
            "C-080 — the comparison is on the raw stored target; a canonicalising probe admits `shells/../shells/default`"
        );
    }

    // ── exit-code registration ───────────────────────────────────────────────
}

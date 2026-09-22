use std::fs;
use std::path::Path;

use codex_exec_server::FileSystemEnvironmentAccessor;
use codex_exec_server::LOCAL_FS;
use codex_utils_path_uri::PathUri;
use codex_utils_plugins::SkillDiscoveryMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::DirectorySymlinkPolicy;
use super::HiddenDirectoryPolicy;
use super::SkillDiscovery;
use super::SkillDiscoveryOptions;
use super::discover_skills;

fn write_skill(root: &Path, directory: &str) {
    let path = root.join(directory).join("SKILL.md");
    fs::create_dir_all(path.parent().expect("skill parent")).expect("create skill directory");
    fs::write(path, "---\nname: demo\ndescription: Demo skill\n---\n")
        .expect("write skill frontmatter");
}

fn canonical_uri(path: &Path) -> PathUri {
    PathUri::from_host_native_path(fs::canonicalize(path).expect("canonical path"))
        .expect("absolute path URI")
}

async fn discover(root: &Path) -> SkillDiscovery {
    discover_skills(
        &FileSystemEnvironmentAccessor::unrestricted(&LOCAL_FS),
        &canonical_uri(root),
        SkillDiscoveryOptions {
            directory_symlinks: DirectorySymlinkPolicy::Follow,
            hidden_directories: HiddenDirectoryPolicy::Skip,
            mode: SkillDiscoveryMode::Recursive,
        },
    )
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn discovers_hidden_directory_through_visible_symlink() {
    let root = TempDir::new().expect("root temp dir");
    write_skill(root.path(), ".hidden/search");
    std::os::unix::fs::symlink(root.path().join(".hidden"), root.path().join("visible"))
        .expect("create visible skill directory symlink");

    let discovery = discover(root.path()).await;

    assert_eq!(discovery.warnings, Vec::<String>::new());
    assert_eq!(
        discovery
            .skills
            .into_iter()
            .map(|skill| skill.path)
            .collect::<Vec<_>>(),
        vec![
            canonical_uri(root.path())
                .join("visible/search/SKILL.md")
                .expect("visible skill URI"),
        ]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn discovers_symlinked_skill_files_without_following_directory_cycles_forever() {
    let root = TempDir::new().expect("root temp dir");
    let external = TempDir::new().expect("external temp dir");
    write_skill(external.path(), "external");
    write_skill(root.path(), "cycle/demo");
    fs::create_dir_all(root.path().join("linked-file")).expect("create linked skill directory");
    std::os::unix::fs::symlink(
        external.path().join("external/SKILL.md"),
        root.path().join("linked-file/SKILL.md"),
    )
    .expect("create skill file symlink");
    std::os::unix::fs::symlink(root.path().join("cycle"), root.path().join("cycle/loop"))
        .expect("create directory symlink cycle");

    let discovery = tokio::time::timeout(
        std::time::Duration::from_secs(/*secs*/ 5),
        discover(root.path()),
    )
    .await
    .expect("directory symlink cycles must not prevent discovery from completing");

    assert_eq!(
        discovery
            .skills
            .into_iter()
            .map(|skill| skill.path)
            .collect::<Vec<_>>(),
        vec![
            canonical_uri(root.path())
                .join("cycle/demo/SKILL.md")
                .expect("valid skill URI"),
            canonical_uri(root.path())
                .join("linked-file/SKILL.md")
                .expect("linked skill URI"),
        ]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn file_symlinks_respect_scope_depth_and_hidden_directory_limits() {
    let root = TempDir::new().expect("root temp dir");
    let external = TempDir::new().expect("external temp dir");
    write_skill(external.path(), "source");
    for directory in ["visible", ".hidden", "d0/d1/d2/d3/d4/d5/d6"] {
        fs::create_dir_all(root.path().join(directory)).expect("create skill directory");
        std::os::unix::fs::symlink(
            external.path().join("source/SKILL.md"),
            root.path().join(directory).join("SKILL.md"),
        )
        .expect("link skill document");
    }
    fs::create_dir_all(root.path().join("broken")).expect("create broken skill directory");
    std::os::unix::fs::symlink(
        external.path().join("missing.md"),
        root.path().join("broken/SKILL.md"),
    )
    .expect("link missing document");

    for (policy, mode, expected) in [
        (
            DirectorySymlinkPolicy::Follow,
            SkillDiscoveryMode::Recursive,
            vec!["visible/SKILL.md"],
        ),
        (
            DirectorySymlinkPolicy::Follow,
            SkillDiscoveryMode::DirectChildren,
            vec!["visible/SKILL.md"],
        ),
        (
            DirectorySymlinkPolicy::Ignore,
            SkillDiscoveryMode::Recursive,
            vec![],
        ),
    ] {
        let discovery = discover_skills(
            &FileSystemEnvironmentAccessor::unrestricted(&LOCAL_FS),
            &canonical_uri(root.path()),
            SkillDiscoveryOptions {
                directory_symlinks: policy,
                hidden_directories: HiddenDirectoryPolicy::Skip,
                mode,
            },
        )
        .await;
        assert_eq!(
            discovery
                .skills
                .into_iter()
                .map(|skill| skill.path)
                .collect::<Vec<_>>(),
            expected
                .into_iter()
                .map(|path| canonical_uri(root.path()).join(path).unwrap())
                .collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn respects_maximum_skill_scan_depth() {
    let root = TempDir::new().expect("root temp dir");
    let within_depth = "d0/d1/d2/d3/d4/d5";
    let beyond_depth = "d0/d1/d2/d3/d4/d5/d6";
    write_skill(root.path(), within_depth);
    write_skill(root.path(), beyond_depth);

    let discovery = discover(root.path()).await;

    assert_eq!(
        discovery
            .skills
            .into_iter()
            .map(|skill| skill.path)
            .collect::<Vec<_>>(),
        vec![
            canonical_uri(root.path())
                .join(&format!("{within_depth}/SKILL.md"))
                .expect("reachable skill URI"),
        ]
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Contract of the reference-page generator (`ocx_schema docs`): reader floors, determinism, page shape,
//! the page-size cap and the `--check` drift exit codes.
//!
//! The floors guard the reader, not the renderer: a model that read nothing renders valid empty pages
//! and `--check` would pass over them forever.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ocx_exit::ExitCode;
use ocx_schema::docs::{
    self, CommandEntry, DocsError, DocsModel, EXIT_CLEAN, EXIT_DRIFT, EXIT_USAGE, MAX_ENTRIES_PER_PAGE, Page,
};

const MIN_COMMANDS: usize = 70;
const MIN_COMMAND_FLAGS: usize = 280;
const MIN_GLOBAL_FLAGS: usize = 12;
const MIN_PUBLIC_ENV_VARS: usize = 50;
const MIN_CONFIG_KEYS: usize = 50;
const EXIT_CODE_COUNT: usize = 13;

fn model() -> &'static DocsModel {
    static MODEL: OnceLock<DocsModel> = OnceLock::new();
    MODEL.get_or_init(docs::read_model)
}

fn render(model: &DocsModel) -> Vec<Page> {
    docs::render(model).unwrap_or_else(|e| panic!("render failed: {e:?}"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ocx_schema_docs_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir is creatable");
    dir
}

fn args(out: &Path, check: bool) -> Vec<String> {
    let mut args = vec!["--out".to_owned(), out.display().to_string()];
    if check {
        args.push("--check".to_owned());
    }
    args
}

/// Every file under `dir` as relative path -> bytes.
fn read_tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("tree is readable") {
            let path = entry.expect("entry is readable").path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).expect("path is under root").to_path_buf();
                out.insert(rel, std::fs::read(&path).expect("file is readable"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn page<'a>(pages: &'a [Page], path: &str) -> &'a Page {
    pages.iter().find(|p| p.path == Path::new(path)).unwrap_or_else(|| {
        panic!(
            "no page {path}; got {:?}",
            pages.iter().map(|p| &p.path).collect::<Vec<_>>()
        )
    })
}

#[test]
fn reader_floors_hold() {
    let m = model();
    let flags: usize = m.commands.iter().map(|c| c.flags.len()).sum();
    let public_env = m.env_vars.iter().filter(|e| e.visibility == "public").count();
    assert!(m.commands.len() >= MIN_COMMANDS, "commands: {}", m.commands.len());
    assert!(flags >= MIN_COMMAND_FLAGS, "command flags: {flags}");
    assert!(
        m.global_flags.len() >= MIN_GLOBAL_FLAGS,
        "global flags: {}",
        m.global_flags.len()
    );
    assert!(public_env >= MIN_PUBLIC_ENV_VARS, "public env vars: {public_env}");
    assert!(
        m.config_keys.len() >= MIN_CONFIG_KEYS,
        "config keys: {}",
        m.config_keys.len()
    );
    assert!(!m.project_keys.is_empty(), "project keys: none read");
}

#[test]
fn model_holds_every_exit_code_in_declaration_order() {
    let codes: Vec<u8> = model().exit_codes.iter().map(|e| e.code).collect();
    let declared: Vec<u8> = ExitCode::ALL.iter().map(|c| *c as u8).collect();
    assert_eq!(
        declared.len(),
        EXIT_CODE_COUNT,
        "ExitCode::ALL changed size; update the contract"
    );
    assert_eq!(codes, declared);
}

#[test]
fn rendering_twice_is_byte_identical() {
    assert_eq!(render(model()), render(model()));
}

#[test]
fn writing_twice_is_byte_identical() {
    let (a, b) = (scratch("det_a"), scratch("det_b"));
    assert_eq!(docs::run(&args(&a, false), model()), EXIT_CLEAN);
    assert_eq!(docs::run(&args(&b, false), model()), EXIT_CLEAN);
    let (tree_a, tree_b) = (read_tree(&a), read_tree(&b));
    assert!(!tree_a.is_empty(), "nothing written");
    assert_eq!(tree_a, tree_b);
}

#[test]
fn the_fixed_pages_exist() {
    let pages = render(model());
    for fixed in [
        "reference/command-line/index.md",
        "reference/environment.md",
        "reference/configuration.md",
        "reference/project-file.md",
        "reference/exit-codes.md",
    ] {
        page(&pages, fixed);
    }
}

#[test]
fn package_spans_a_publishing_and_a_consuming_page_with_no_numbered_slug() {
    let pages = render(model());
    let publishing = &page(&pages, "reference/command-line/package-publishing.md").body;
    let consuming = &page(&pages, "reference/command-line/package-consuming.md").body;
    assert!(publishing.contains("title: \"ocx package: publishing\""));
    assert!(
        publishing.contains("{#package}"),
        "the bare `ocx package` entry opens the first page"
    );
    for (anchor, home) in [
        ("{#package-push}", publishing),
        ("{#package-cascade-check}", publishing),
        ("{#package-description-pull}", publishing),
        ("{#package-exec}", consuming),
        ("{#package-verify}", consuming),
        ("{#which}", consuming),
    ] {
        assert!(home.contains(anchor), "{anchor} is on the wrong page");
    }
    assert!(
        pages.iter().all(|p| {
            let name = p.path.to_string_lossy();
            !name.ends_with("package.md") && !name.contains("package-2")
        }),
        "a numbered or ungrouped package page came back"
    );
}

#[test]
fn a_package_subcommand_on_no_page_is_an_error() {
    let mut model = DocsModel::default();
    model.commands.push(CommandEntry {
        path: vec!["package".to_owned(), "frobnicate".to_owned()],
        summary: "Frobnicate.".to_owned(),
        ..CommandEntry::default()
    });
    assert_eq!(
        docs::render(&model),
        Err(DocsError::UngroupedCommand {
            command: "package frobnicate".to_owned()
        })
    );
}

#[test]
fn every_page_opens_with_front_matter_title_then_the_reference_declaration() {
    for p in render(model()) {
        let lines: Vec<&str> = p.body.lines().collect();
        assert_eq!(lines.first(), Some(&"---"), "{}: no front matter", p.path.display());
        assert!(
            lines.get(1).is_some_and(|l| l.starts_with("title:")),
            "{}: title is not the first key",
            p.path.display()
        );
        let close = lines[1..]
            .iter()
            .position(|l| *l == "---")
            .unwrap_or_else(|| panic!("{}: front matter never closes", p.path.display()))
            + 1;
        assert_eq!(
            lines.get(close + 1),
            Some(&"<!-- doc_type: reference -->"),
            "{}: declaration must follow the front matter",
            p.path.display()
        );
    }
}

#[test]
fn every_heading_carries_an_explicit_anchor() {
    let anchored = |l: &str| {
        l.rsplit_once(" {#").is_some_and(|(_, id)| {
            id.strip_suffix('}').is_some_and(|id| {
                !id.is_empty()
                    && id
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            })
        })
    };
    for p in render(model()) {
        let mut fenced = false;
        for (n, line) in p.body.lines().enumerate() {
            if line.starts_with("```") {
                fenced = !fenced;
            }
            if !fenced && line.starts_with('#') {
                assert!(
                    anchored(line),
                    "{}:{}: heading without {{#id}}: {line}",
                    p.path.display(),
                    n + 1
                );
            }
        }
    }
}

#[test]
fn every_flag_link_on_the_environment_page_lands_on_a_command_line_heading() {
    let pages = render(model());
    let body = |path: &str| {
        pages
            .iter()
            .find(|p| p.path == Path::new(path))
            .unwrap_or_else(|| panic!("{path} is rendered"))
            .body
            .clone()
    };
    let index = body("reference/command-line/index.md");
    let environment = body("reference/environment.md");
    let mut links = 0;
    for target in environment.split("](./command-line/index.md#").skip(1) {
        let id = target.split(')').next().expect("link closes");
        links += 1;
        assert!(
            index.contains(&format!("{{#{id}}}")),
            "no heading {{#{id}}} on the command-line index"
        );
    }
    assert!(links > 0, "the environment page links no flag, so this proves nothing");
}

/// Every help text the clap tree carries, hidden commands and flags included.
fn help_texts(cmd: &clap::Command, out: &mut Vec<String>) {
    let styled = [
        cmd.get_about(),
        cmd.get_long_about(),
        cmd.get_before_help(),
        cmd.get_before_long_help(),
        cmd.get_after_help(),
        cmd.get_after_long_help(),
    ];
    out.extend(styled.into_iter().flatten().map(ToString::to_string));
    for arg in cmd.get_arguments() {
        out.extend(
            [arg.get_help(), arg.get_long_help()]
                .into_iter()
                .flatten()
                .map(ToString::to_string),
        );
    }
    for sub in cmd.get_subcommands() {
        help_texts(sub, out);
    }
}

/// `(path, anchor)` of every `https://ocx.sh/docs/<path>[#<anchor>]` in `text`; `path` has no slashes at either end.
fn docs_urls(text: &str) -> Vec<(String, Option<String>)> {
    const PREFIX: &str = "https://ocx.sh/docs";
    text.match_indices(PREFIX)
        .map(|(start, _)| {
            let tail = &text[start + PREFIX.len()..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || "_./#-".contains(c)))
                .unwrap_or(tail.len());
            let url = tail[..end].trim_end_matches('.');
            let (path, anchor) = url
                .split_once('#')
                .map_or((url, None), |(path, anchor)| (path, Some(anchor)));
            (path.trim_matches('/').to_owned(), anchor.map(str::to_owned))
        })
        .collect()
}

/// Every URL into the docs that the compiled-in help text carries must land on a page of the site, on
/// an anchor that page declares.
#[test]
fn every_docs_url_in_the_help_text_lands_on_a_page_and_anchor() {
    let docs_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("website/src/content/docs");
    assert!(
        docs_root.is_dir(),
        "{} is not a directory; the URL check needs the committed docs tree",
        docs_root.display()
    );
    let rendered = render(model());

    let mut texts = Vec::new();
    help_texts(&ocx_schema::cli::ocx_command(), &mut texts);
    let urls: Vec<_> = texts.iter().flat_map(|text| docs_urls(text)).collect();
    assert!(
        urls.len() >= 20,
        "found only {} docs URLs; the extractor read nothing",
        urls.len()
    );

    let mut bad = Vec::new();
    for (path, anchor) in urls {
        let candidates = if path.is_empty() {
            vec!["index.md".to_owned(), "index.mdx".to_owned()]
        } else {
            vec![
                format!("{path}.md"),
                format!("{path}.mdx"),
                format!("{path}/index.md"),
                format!("{path}/index.mdx"),
            ]
        };
        let Some((rel, on_disk)) = candidates.iter().find_map(|rel| {
            std::fs::read_to_string(docs_root.join(rel))
                .ok()
                .map(|text| (rel, text))
        }) else {
            bad.push(format!("/docs/{path}: no page among {candidates:?}"));
            continue;
        };
        let Some(anchor) = anchor else { continue };
        let generated = rendered
            .iter()
            .find(|p| p.path == Path::new(rel))
            .map(|p| p.body.as_str());
        let declared = |text: &str| {
            text.contains(&format!("{{#{anchor}}}"))
                || text.contains(&format!("\\{{#{anchor}\\}}"))
                || (rel == "reference/command-line/index.md" && text.contains(&format!("| `{anchor}` |")))
        };
        if !declared(&on_disk) && !generated.is_some_and(declared) {
            bad.push(format!("/docs/{path}#{anchor}: {rel} declares no such anchor"));
        }
    }
    assert!(bad.is_empty(), "help text links that land nowhere:\n{}", bad.join("\n"));
}

#[test]
fn ocx_home_keeps_its_lowercase_anchor() {
    let pages = render(model());
    assert!(
        page(&pages, "reference/environment.md").body.contains("{#ocx-home}"),
        "environment.md lost the ocx-home anchor"
    );
}

#[test]
fn exit_codes_page_names_every_code_and_meaning() {
    let pages = render(model());
    let body = &page(&pages, "reference/exit-codes.md").body;
    for entry in &model().exit_codes {
        assert!(
            body.contains(&entry.summary),
            "exit-codes.md misses `{}`",
            entry.summary
        );
        assert!(
            body.contains(&entry.code.to_string()),
            "exit-codes.md misses code {}",
            entry.code
        );
    }
}

#[test]
fn exit_codes_page_explains_every_slug() {
    let pages = render(model());
    let body = &page(&pages, "reference/exit-codes.md").body;
    for slug in model().exit_codes.iter().flat_map(|entry| &entry.slugs) {
        assert!(!slug.summary.is_empty(), "slug `{}` has no meaning", slug.slug);
        assert!(
            body.contains(&format!("`{}` | {}", slug.slug, slug.summary)),
            "exit-codes.md misses the meaning of `{}`",
            slug.slug
        );
    }
}

#[test]
fn a_command_alias_renders_as_an_alias_fact() {
    let mut model = DocsModel::default();
    model.commands.push(CommandEntry {
        path: vec!["exec".to_owned()],
        summary: "Run a command.".to_owned(),
        aliases: vec!["x".to_owned()],
        ..CommandEntry::default()
    });
    let pages = render(&model);
    assert!(
        pages.iter().any(|p| p.body.contains("- Alias: `x`")),
        "no page states the alias of exec"
    );
}

#[test]
fn trust_policy_signer_fields_are_documented() {
    let pages = render(model());
    let config = &page(&pages, "reference/configuration.md").body;
    assert!(
        config.contains("trust.policy.signers"),
        "configuration.md lost the signer fields"
    );
}

fn group_of(size: usize) -> DocsModel {
    DocsModel {
        commands: (0..size)
            .map(|i| CommandEntry {
                path: vec!["big".to_owned(), format!("cmd{i}")],
                summary: format!("Command {i}."),
                ..CommandEntry::default()
            })
            .collect(),
        ..DocsModel::default()
    }
}

#[test]
fn a_group_page_over_the_entry_cap_is_an_error() {
    assert!(
        docs::render(&group_of(MAX_ENTRIES_PER_PAGE)).is_ok(),
        "a full page is allowed"
    );
    match docs::render(&group_of(MAX_ENTRIES_PER_PAGE + 1)) {
        Err(DocsError::PageTooLarge { entries, .. }) => assert!(entries > MAX_ENTRIES_PER_PAGE),
        other => panic!("expected PageTooLarge, got {other:?}"),
    }
}

#[test]
fn check_exits_one_after_a_source_doc_change_and_zero_after_regenerating() {
    let out = scratch("drift");
    assert_eq!(docs::run(&args(&out, false), model()), EXIT_CLEAN, "initial write");
    assert_eq!(docs::run(&args(&out, true), model()), EXIT_CLEAN, "clean tree");

    let mut mutated = model().clone();
    let first = mutated.commands.first_mut().expect("model holds commands");
    first.summary.push_str(" Mutated.");

    assert_eq!(docs::run(&args(&out, true), &mutated), EXIT_DRIFT, "source doc changed");
    assert_eq!(docs::run(&args(&out, false), &mutated), EXIT_CLEAN, "regenerate");
    assert_eq!(
        docs::run(&args(&out, true), &mutated),
        EXIT_CLEAN,
        "clean after regenerating"
    );
}

#[test]
fn check_reports_each_drifted_path_and_a_stray_page_in_the_generated_directory() {
    let out = scratch("paths");
    let pages = render(model());
    assert_eq!(docs::run(&args(&out, false), model()), EXIT_CLEAN);
    std::fs::write(out.join("reference/exit-codes.md"), "stale").expect("tamper");
    std::fs::write(out.join("reference/command-line/stray.md"), "stray").expect("plant");
    let mut drifted = docs::drifted(&out, &pages).expect("tree is readable");
    drifted.sort();
    assert_eq!(
        drifted,
        vec![
            PathBuf::from("reference/command-line/stray.md"),
            PathBuf::from("reference/exit-codes.md")
        ]
    );
}

#[test]
fn authored_pages_beside_the_generated_ones_are_not_drift() {
    let out = scratch("authored");
    assert_eq!(docs::run(&args(&out, false), model()), EXIT_CLEAN);
    std::fs::write(out.join("reference/metadata.md"), "authored").expect("plant");
    assert_eq!(docs::run(&args(&out, true), model()), EXIT_CLEAN);
    assert_eq!(docs::run(&args(&out, false), model()), EXIT_CLEAN);
    assert!(
        out.join("reference/metadata.md").exists(),
        "regenerating removed an authored page"
    );
}

#[test]
fn a_bad_invocation_exits_two() {
    let empty = DocsModel::default();
    for bad in [vec![], vec!["--check".to_owned()], vec!["--bogus".to_owned()]] {
        assert_eq!(docs::run(&bad, &empty), EXIT_USAGE, "{bad:?}");
    }
}

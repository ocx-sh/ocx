use std::process::ExitCode;

use clap::Parser;
use ocx_lib::log;

use crate::api;

/// Remove unreferenced objects from the local object store.
///
/// An object is unreferenced when its `refs/` directory is empty or absent —
/// no candidate or current symlink points to it anymore.  This happens after
/// `ocx uninstall` (without `--purge`) or when symlinks are removed manually.
///
/// Use `--dry-run` to preview what would be removed without making any changes.
#[derive(Parser)]
pub struct Clean {
    /// Show what would be removed without actually removing anything.
    #[clap(long = "dry-run")]
    dry_run: bool,
}

impl Clean {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let fs = context.file_structure();
        let object_dirs = fs.objects.list_all()?;

        log::debug!(
            "Scanning {} object(s) for unreferenced entries{}.",
            object_dirs.len(),
            if self.dry_run { " (dry run)" } else { "" },
        );

        let mut removed = Vec::new();

        for obj in object_dirs {
            let refs_dir = obj.refs_dir();
            let is_empty = !refs_dir.is_dir()
                || std::fs::read_dir(&refs_dir)
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(true);

            log::trace!(
                "Object '{}': refs/ {}.",
                obj.dir.display(),
                if is_empty { "is empty" } else { "has references" },
            );

            if is_empty {
                log::info!(
                    "{} unreferenced object: {}",
                    if self.dry_run { "Would remove" } else { "Removing" },
                    obj.dir.display(),
                );
                if !self.dry_run {
                    std::fs::remove_dir_all(&obj.dir)
                        .map_err(|e| ocx_lib::Error::InternalFile(obj.dir.clone(), e))?;
                }
                removed.push(obj.dir.clone());
            }
        }

        log::debug!(
            "{} {} object(s).",
            if self.dry_run { "Would remove" } else { "Removed" },
            removed.len(),
        );

        context
            .api()
            .report_clean(api::data::clean::Clean::new(removed, self.dry_run))?;

        Ok(ExitCode::SUCCESS)
    }
}

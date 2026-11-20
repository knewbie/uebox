use std::path::PathBuf;

use serde::Deserialize;
use uebox_project_library::ImportRecord;

const MAX_IMPORT_BYTES: usize = 1_048_576;
const MAX_IMPORT_PROJECTS: usize = 4096;
const INVALID: &str = "project_import_invalid";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    path_family: String,
    projects: Vec<TransferRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferRecord {
    engine: String,
    name: String,
    path: String,
}

pub(super) fn parse(input: &str) -> Result<Vec<ImportRecord>, &'static str> {
    if input.len() > MAX_IMPORT_BYTES {
        return Err("project_import_size");
    }
    let manifest: Manifest = serde_json::from_str(input).map_err(|_| INVALID)?;
    if manifest.format != "uebox-project-registration" || manifest.version != 1 {
        return Err("project_import_version");
    }
    if manifest.path_family != if cfg!(windows) { "windows" } else { "posix" } {
        return Err("project_import_platform");
    }
    if manifest.projects.len() > MAX_IMPORT_PROJECTS {
        return Err("project_import_size");
    }
    manifest
        .projects
        .into_iter()
        .map(|record| {
            let engine = super::parse_engine(&record.engine).ok_or(INVALID)?;
            if record.name.trim().is_empty()
                || record.name.contains('\0')
                || record.path.contains('\0')
            {
                return Err(INVALID);
            }
            let path = PathBuf::from(record.path);
            if !path.is_absolute() {
                return Err(INVALID);
            }
            Ok(ImportRecord {
                engine,
                name: record.name,
                path,
            })
        })
        .collect()
}

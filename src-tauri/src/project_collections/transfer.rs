use std::{collections::HashMap, ffi::OsString, path::PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};
use uebox_project_collections::LegacyCollection;
use uebox_project_library::EngineKind;

use crate::project_library::{with_library, ProjectRegistrations};

use super::{collection_error, view, with_collection_file, CollectionView, INVALID, UNAVAILABLE};

const MAX_MEMBERS: usize = 16_384;
const MAX_SAFE_ORDER: u64 = 9_007_199_254_740_991;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Manifest {
    format: String,
    version: u32,
    path_family: String,
    collections: Vec<Row>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Row {
    id: String,
    name: Option<String>,
    sort_order: i64,
    members: Vec<Member>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
enum Engine {
    Unreal,
    Unity,
    Godot,
}

impl Engine {
    fn engine_name(&self) -> &'static str {
        match self {
            Self::Unreal => "unreal",
            Self::Unity => "unity",
            Self::Godot => "godot",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    engine: Engine,
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportReport {
    collections: Vec<CollectionView>,
    inserted: usize,
    already_imported: usize,
}

fn parse(text: &str) -> Result<Vec<Row>, &'static str> {
    if text.len() > 1024 * 1024 {
        return Err("project_collections_import_limit");
    }
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| INVALID)?;
    if !value.is_object()
        || !value["collections"].as_array().is_some_and(|rows| {
            rows.iter().all(|row| {
                row.is_object()
                    && row["members"]
                        .as_array()
                        .is_some_and(|members| members.iter().all(serde_json::Value::is_object))
            })
        })
    {
        return Err(INVALID);
    }
    let input: Manifest = serde_json::from_value(value).map_err(|_| INVALID)?;
    if input.format != "uebox-project-collections" || input.version != 1 {
        return Err("project_collections_import_version");
    }
    if input.path_family != if cfg!(windows) { "windows" } else { "posix" } {
        return Err("project_collections_import_platform");
    }
    if input.collections.len() > 4096
        || input
            .collections
            .iter()
            .map(|row| row.members.len())
            .sum::<usize>()
            > MAX_MEMBERS
    {
        return Err("project_collections_import_limit");
    }
    if input.collections.iter().any(|row| {
        row.sort_order.unsigned_abs() > MAX_SAFE_ORDER
            || row.members.iter().any(|member| {
                member.path.contains('\0') || !PathBuf::from(&member.path).is_absolute()
            })
    }) {
        return Err(INVALID);
    }
    Ok(input.collections)
}

#[tauri::command]
pub(crate) async fn import_project_collections<R: Runtime>(
    app: AppHandle<R>,
    manifest: String,
) -> Result<ImportReport, &'static str> {
    let rows = parse(&manifest)?;
    let root = app.state::<ProjectRegistrations>().app_data_dir(&app)?;
    // A new empty registry can receive only empty collections.
    let create_library = !rows.is_empty() && rows.iter().all(|row| row.members.is_empty());
    with_library(app, create_library, move |library| {
        let Some(library) = library else {
            return if rows.is_empty() {
                Ok(ImportReport {
                    collections: Vec::new(),
                    inserted: 0,
                    already_imported: 0,
                })
            } else {
                Err("project_collections_import_missing_project")
            };
        };
        let directory_ids: HashMap<(&str, OsString), i64> = library
            .list()
            .map_err(|_| UNAVAILABLE)?
            .into_iter()
            .map(|record| {
                let engine = match record.engine {
                    EngineKind::Unreal => "unreal",
                    EngineKind::Unity => "unity",
                    EngineKind::Godot => "godot",
                };
                ((engine, record.path.into_os_string()), record.id)
            })
            .collect();
        let ids_per_collection = rows
            .iter()
            .map(|row| {
                row.members
                    .iter()
                    .map(|member| {
                        directory_ids
                            .get(&(member.engine.engine_name(), OsString::from(&member.path)))
                            .copied()
                            .ok_or("project_collections_import_missing_project")
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let legacy: Vec<_> = rows
            .iter()
            .zip(&ids_per_collection)
            .map(|(row, ids)| LegacyCollection {
                id: &row.id,
                name: row.name.as_deref(),
                sort_order: row.sort_order,
                project_ids: ids,
            })
            .collect();
        with_collection_file(&root, !legacy.is_empty(), move |collections| {
            let Some(collections) = collections else {
                return Ok(ImportReport {
                    collections: Vec::new(),
                    inserted: 0,
                    already_imported: 0,
                });
            };
            let result = collections
                .import_legacy(library, &legacy)
                .map_err(collection_error)?;
            Ok(ImportReport {
                collections: view(library, collections)?,
                inserted: result.inserted,
                already_imported: result.already_present,
            })
        })
    })
    .await
}

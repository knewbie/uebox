use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use crate::{output::Fault, Engine, OnlineProject};

pub(crate) fn validate(projects: &[OnlineProject]) -> Result<(), Fault> {
    let mut ids = HashSet::new();
    if projects.len() > 4096
        || projects.iter().any(|project| {
            !project.path.is_absolute()
                || project
                    .path
                    .to_str()
                    .is_none_or(|path| !public_field(path, 4096))
                || !public_field(&project.name, 256)
                || !public_field(&project.connection_id, 256)
                || !ids.insert(&project.connection_id)
        })
    {
        return Err(Fault::IncompatibleServer);
    }
    Ok(())
}

fn public_field(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn markers(root: &Path) -> Result<Vec<Engine>, Fault> {
    let mut found = Vec::new();
    if root.join("project.godot").is_file() {
        found.push(Engine::Godot);
    }
    if root.join("ProjectSettings/ProjectVersion.txt").is_file() {
        found.push(Engine::Unity);
    }
    let mut unreal_markers = 0;
    for entry in fs::read_dir(root).map_err(|_| Fault::InvalidArgument)? {
        let entry = entry.map_err(|_| Fault::InvalidArgument)?;
        if project_file(&entry.path()) == Some(Engine::Unreal)
            && entry
                .file_type()
                .map_err(|_| Fault::InvalidArgument)?
                .is_file()
        {
            found.push(Engine::Unreal);
            unreal_markers += 1;
            if unreal_markers == 2 {
                break;
            }
        }
    }
    Ok(found)
}

fn project_file(path: &Path) -> Option<Engine> {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("uproject"))
    {
        Some(Engine::Unreal)
    } else if path.file_name().and_then(|name| name.to_str()) == Some("project.godot") {
        Some(Engine::Godot)
    } else {
        None
    }
}

fn one_marker(root: &Path) -> Result<Option<Engine>, Fault> {
    match markers(root)?.as_slice() {
        [] => Ok(None),
        [engine] => Ok(Some(*engine)),
        _ => Err(Fault::ProjectAmbiguous),
    }
}

pub(crate) fn local_hint(
    explicit: Option<PathBuf>,
    cwd: PathBuf,
) -> Result<Option<(Engine, PathBuf)>, Fault> {
    if let Some(path) = explicit {
        if !path.is_absolute() && !cwd.is_absolute() {
            return Err(Fault::InvalidArgument);
        }
        let path = fs::canonicalize(cwd.join(path)).map_err(|_| Fault::InvalidArgument)?;
        if path.is_file() {
            let engine = project_file(&path).ok_or(Fault::InvalidArgument)?;
            let root = path.parent().ok_or(Fault::InvalidArgument)?;
            // A root-only announcement cannot distinguish two UE project files in one directory.
            if engine == Engine::Unreal
                && markers(root)?
                    .iter()
                    .filter(|kind| **kind == engine)
                    .count()
                    > 1
            {
                return Err(Fault::ProjectAmbiguous);
            }
            return Ok(Some((engine, root.to_owned())));
        }
        let engine = one_marker(&path)?.ok_or(Fault::InvalidArgument)?;
        return Ok(Some((engine, path)));
    }
    if cwd.is_absolute() {
        let cwd = fs::canonicalize(cwd).map_err(|_| Fault::InvalidArgument)?;
        for root in cwd.ancestors() {
            if let Some(engine) = one_marker(root)? {
                return Ok(Some((engine, root.to_owned())));
            }
        }
    }
    Ok(None)
}

fn comparable(path: &Path) -> String {
    if cfg!(windows) {
        let normalized = path.to_string_lossy().replace('\\', "/");
        let normalized = if let Some(unc) = normalized.strip_prefix("//?/UNC/") {
            format!("//{unc}")
        } else {
            normalized
                .strip_prefix("//?/")
                .unwrap_or(&normalized)
                .to_owned()
        };
        return normalized.trim_end_matches('/').to_lowercase();
    }
    path.to_string_lossy().trim_end_matches('/').to_owned()
}

pub(crate) fn select(
    projects: &[OnlineProject],
    engine: Option<Engine>,
    hint: Option<(Engine, PathBuf)>,
) -> Result<&OnlineProject, Fault> {
    let hint = hint.map(|(engine, path)| (engine, comparable(&path)));
    let mut matches = projects.iter().filter(|project| {
        engine.is_none_or(|engine| engine == project.engine)
            && hint.as_ref().is_none_or(|(engine, path)| {
                *engine == project.engine && *path == comparable(&project.path)
            })
    });
    let target = matches.next().ok_or(Fault::ProjectNotConnected)?;
    if matches.next().is_some() {
        return Err(Fault::ProjectAmbiguous);
    }
    Ok(target)
}

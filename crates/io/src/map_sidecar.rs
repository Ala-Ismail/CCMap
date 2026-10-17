//! Bounded persistence for optional map registration beside a drawing.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use cadcraft_map::MapMetadata;

use crate::{IoError, Result};

/// Largest accepted map sidecar, preventing a malformed adjacent file from consuming memory.
pub const MAX_MAP_SIDECAR_BYTES: u64 = 64 * 1024;
const MAX_TEMP_ATTEMPTS: u8 = 16;

/// Return the registration sidecar path for a drawing without changing the drawing path itself.
pub fn map_sidecar_path(drawing_path: &Path) -> Result<PathBuf> {
    let name = drawing_path.file_name().ok_or_else(|| IoError::Format("drawing path must include a file name".into()))?;
    let mut sidecar_name = OsString::from(name);
    sidecar_name.push(".cadcraft-map.json");
    Ok(drawing_path.with_file_name(sidecar_name))
}

/// Read a map registration sidecar, if the drawing has one.
pub fn read_map_sidecar(drawing_path: &Path) -> Result<Option<MapMetadata>> {
    let path = map_sidecar_path(drawing_path)?;
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(file_error(&path, error)),
    };
    let declared_len = file.metadata().map_err(|error| file_error(&path, error))?.len();
    if declared_len > MAX_MAP_SIDECAR_BYTES {
        return Err(IoError::Format("map sidecar exceeds the size limit".into()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(declared_len).map_err(|_| IoError::Format("map sidecar length is unsupported".into()))?);
    Read::by_ref(&mut file).take(MAX_MAP_SIDECAR_BYTES + 1).read_to_end(&mut bytes).map_err(|error| file_error(&path, error))?;
    if bytes.len() as u64 > MAX_MAP_SIDECAR_BYTES {
        return Err(IoError::Format("map sidecar exceeds the size limit".into()));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|error| IoError::Format(format!("invalid map sidecar: {error}")))
}

/// Replace the map registration sidecar for a drawing.
///
/// The new JSON is fully flushed before replacement. Unix replaces the destination atomically;
/// Windows stages the old file as a backup because the standard library cannot atomically replace
/// an existing destination there without platform-specific unsafe code.
pub fn write_map_sidecar(drawing_path: &Path, metadata: &MapMetadata) -> Result<()> {
    let path = map_sidecar_path(drawing_path)?;
    let bytes = serde_json::to_vec_pretty(metadata).map_err(|error| IoError::Format(format!("could not serialize map sidecar: {error}")))?;
    if bytes.len() as u64 > MAX_MAP_SIDECAR_BYTES {
        return Err(IoError::Format("map sidecar exceeds the size limit".into()));
    }
    let temporary = create_temporary_sidecar(&path, &bytes)?;
    replace_file(&temporary, &path)
}

fn create_temporary_sidecar(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    for attempt in 0..MAX_TEMP_ATTEMPTS {
        let temporary = temporary_path(path, attempt)?;
        match OpenOptions::new().write(true).create_new(true).open(&temporary) {
            Ok(mut file) => {
                let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
                if let Err(error) = write_result {
                    let _ = fs::remove_file(&temporary);
                    return Err(file_error(&temporary, error));
                }
                return Ok(temporary);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(file_error(&temporary, error)),
        }
    }
    Err(IoError::Format("could not allocate a temporary map sidecar".into()))
}

fn temporary_path(path: &Path, attempt: u8) -> Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| IoError::Format("map sidecar path must include a file name".into()))?;
    let mut temporary_name = OsString::from(name);
    temporary_name.push(format!(".tmp-{}-{attempt}", std::process::id()));
    Ok(path.with_file_name(temporary_name))
}

fn replace_file(temporary: &Path, destination: &Path) -> Result<()> {
    #[cfg(not(windows))]
    match fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(file_error(destination, error))
        }
    }

    #[cfg(windows)]
    {
        let backup = move_existing_file_aside(destination)?;
        match fs::rename(temporary, destination) {
            Ok(()) => {
                if let Some(backup) = backup {
                    let _ = fs::remove_file(backup);
                }
                Ok(())
            }
            Err(error) => {
                if let Some(backup) = backup {
                    let _ = fs::rename(backup, destination);
                }
                let _ = fs::remove_file(temporary);
                Err(file_error(destination, error))
            }
        }
    }
}

#[cfg(windows)]
fn move_existing_file_aside(destination: &Path) -> Result<Option<PathBuf>> {
    for attempt in 0..MAX_TEMP_ATTEMPTS {
        let backup = backup_path(destination, attempt)?;
        match fs::rename(destination, &backup) {
            Ok(()) => return Ok(Some(backup)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(file_error(destination, error)),
        }
    }
    Err(IoError::Format("could not allocate a backup map sidecar".into()))
}

#[cfg(windows)]
fn backup_path(path: &Path, attempt: u8) -> Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| IoError::Format("map sidecar path must include a file name".into()))?;
    let mut backup_name = OsString::from(name);
    backup_name.push(format!(".bak-{}-{attempt}", std::process::id()));
    Ok(path.with_file_name(backup_name))
}

fn file_error(path: &Path, error: std::io::Error) -> IoError {
    IoError::Format(format!("map sidecar `{}`: {error}", path.display()))
}

#[cfg(test)]
#[allow(clippy::unreachable)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use cadcraft_map::{Coordinate, CrsId, MapMetadata, Registration};

    use super::*;

    fn test_directory() -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_nanos());
        let path = std::env::temp_dir().join(format!("cadcraft-map-sidecar-{}-{nanos}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => path,
            Err(error) => unreachable!("test directory must be creatable: {error}"),
        }
    }

    fn metadata() -> MapMetadata {
        let origin = match Coordinate::new(0.0, 0.0) {
            Ok(value) => value,
            Err(error) => unreachable!("test coordinate must be valid: {error}"),
        };
        let registration = match Registration::new(CrsId::Gda2020MgaZone55, origin, origin, 1.0, 0.0) {
            Ok(value) => value,
            Err(error) => unreachable!("test registration must be valid: {error}"),
        };
        MapMetadata::new(registration)
    }

    #[test]
    fn sidecar_round_trips_without_touching_the_drawing() {
        let directory = test_directory();
        let drawing = directory.join("survey.dxf");
        let original = b"drawing bytes";
        match fs::write(&drawing, original) {
            Ok(()) => {}
            Err(error) => unreachable!("test drawing must be writable: {error}"),
        }
        let value = metadata();
        assert!(write_map_sidecar(&drawing, &value).is_ok());
        match read_map_sidecar(&drawing) {
            Ok(Some(actual)) => assert_eq!(actual, value),
            Ok(None) => unreachable!("written sidecar must be present"),
            Err(error) => unreachable!("written sidecar must be readable: {error}"),
        }
        let updated = MapMetadata::new(
            match Registration::new(
                CrsId::Gda2020MgaZone55,
                match Coordinate::new(1.0, 2.0) {
                    Ok(value) => value,
                    Err(error) => unreachable!("test coordinate must be valid: {error}"),
                },
                match Coordinate::new(3.0, 4.0) {
                    Ok(value) => value,
                    Err(error) => unreachable!("test coordinate must be valid: {error}"),
                },
                2.0,
                0.0,
            ) {
                Ok(value) => value,
                Err(error) => unreachable!("test registration must be valid: {error}"),
            },
        );
        assert!(write_map_sidecar(&drawing, &updated).is_ok());
        match read_map_sidecar(&drawing) {
            Ok(Some(actual)) => assert_eq!(actual, updated),
            Ok(None) => unreachable!("replaced sidecar must be present"),
            Err(error) => unreachable!("replaced sidecar must be readable: {error}"),
        }
        match fs::read(&drawing) {
            Ok(actual) => assert_eq!(actual, original),
            Err(error) => unreachable!("drawing must remain readable: {error}"),
        }
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn absent_or_oversized_sidecar_is_safe() {
        let directory = test_directory();
        let drawing = directory.join("survey.dxf");
        match read_map_sidecar(&drawing) {
            Ok(None) => {}
            Ok(Some(_)) => unreachable!("absent sidecar must not produce metadata"),
            Err(error) => unreachable!("absent sidecar must be harmless: {error}"),
        }
        let sidecar = match map_sidecar_path(&drawing) {
            Ok(value) => value,
            Err(error) => unreachable!("test path must be valid: {error}"),
        };
        let bytes = vec![b'x'; MAX_MAP_SIDECAR_BYTES as usize + 1];
        match fs::write(sidecar, bytes) {
            Ok(()) => {}
            Err(error) => unreachable!("test sidecar must be writable: {error}"),
        }
        assert!(read_map_sidecar(&drawing).is_err());
        let _ = fs::remove_dir_all(directory);
    }
}

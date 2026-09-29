pub const USERLAND_ENTRY_PATH_CAPACITY: usize = 255;

pub fn entry_name(path: &str) -> &str {
    &path[path.rfind('/').map_or(0, |index| index + 1)..]
}

pub(crate) fn validate_entry_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("path_empty");
    }
    if !path.is_ascii() {
        return Err("path_ascii");
    }
    if path.len() > USERLAND_ENTRY_PATH_CAPACITY {
        return Err("path_length");
    }
    if entry_name(path).is_empty() {
        return Err("path_name");
    }
    Ok(())
}

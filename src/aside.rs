//! Writing a file whole: aside first, then moved in place, so a run beside this one, or one that
//! stops midway, never leaves it half written.

use std::path::Path;

/// `contents`, in place of what `path` holds; the file aside is removed when the move fails.
pub(crate) fn write(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let name = path.file_name().expect("a file to write has a name");
    let aside = path.with_file_name(format!(
        "{}-{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&aside, contents)?;
    std::fs::rename(&aside, path).inspect_err(|_| {
        std::fs::remove_file(&aside).ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_nothing_aside() {
        let dir = std::env::temp_dir().join(format!("drive-aside-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.txt");
        write(&path, "old").unwrap();
        write(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["out.txt"]);
        // A folder in the file's place refuses the move, and the aside goes with it.
        let taken = dir.join("taken");
        std::fs::create_dir(&taken).unwrap();
        assert!(write(&taken, "x").is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

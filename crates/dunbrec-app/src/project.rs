//! Projects: a folder that holds a project's schematics and firmware source
//! files together.
//!
//! A project is just a folder. Every `.dbr` in it (and its subfolders) is a
//! schematic of the project, and firmware sources are ordinary files beside
//! them (`.asm`, `.s`, `.ino`, `.cpp`), so other editors, version control and
//! `arduino-cli` see them as they are.
//!
//! A source file is known to a schematic by its path relative to the
//! schematic's own folder, with `/` separators (`main.asm`, `fw/blink.ino`).
//! A microcontroller's firmware field holds that path, and while the schematic
//! is open the file's text sits in [`Schematic::sources`] under the same
//! name, where the Code panel, the debugger and the simulator find it. The
//! `.dbr` file does not store those texts (see [`for_saving`]); saving writes
//! them to their files ([`write_sources`]). Sources kept inside a schematic
//! by older versions (`source:<name>`) still work, and [`extract_embedded`]
//! moves them out into files.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use dunbrec_core::{
    ComponentKind, SOURCE_PREFIX, Schematic, SourceFile, SourceLanguage, source_language_of,
};

/// What a project entry is, for the icon and for what a double-click does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Folder,
    /// A `.dbr` schematic.
    Schematic,
    /// A firmware source file the simulator builds.
    Source(SourceLanguage),
    /// A header or other source text (not firmware by itself).
    Header,
    /// A compiled image.
    Hex,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    /// The folder's contents (empty for files).
    pub children: Vec<Entry>,
}

/// What an entry named `name` is, going by its extension.
pub fn kind_of(name: &str) -> EntryKind {
    if let Some(language) = source_language_of(name) {
        return EntryKind::Source(language);
    }
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "dbr" => EntryKind::Schematic,
        "h" | "hpp" | "c" | "inc" => EntryKind::Header,
        "hex" => EntryKind::Hex,
        _ => EntryKind::Other,
    }
}

/// Folders that are never listed: hidden ones and build output of other tools.
fn skipped(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "target" | "node_modules")
}

const MAX_DEPTH: usize = 8;
const MAX_ENTRIES: usize = 5000;

/// The contents of `root` as a tree: folders first, then files, each
/// alphabetical without regard to case.
pub fn scan(root: &Path) -> Vec<Entry> {
    let mut budget = MAX_ENTRIES;
    scan_dir(root, 0, &mut budget)
}

fn scan_dir(dir: &Path, depth: usize, budget: &mut usize) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for item in read.flatten() {
        if *budget == 0 {
            break;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        let path = item.path();
        let is_dir = item.file_type().is_ok_and(|t| t.is_dir());
        if is_dir {
            if skipped(&name) || depth >= MAX_DEPTH {
                continue;
            }
            *budget -= 1;
            let children = scan_dir(&path, depth + 1, budget);
            entries.push(Entry {
                name,
                path,
                kind: EntryKind::Folder,
                children,
            });
        } else {
            if name.starts_with('.') {
                continue;
            }
            *budget -= 1;
            entries.push(Entry {
                kind: kind_of(&name),
                name,
                path,
                children: Vec::new(),
            });
        }
    }
    entries.sort_by(|a, b| {
        let folder = |e: &Entry| e.kind != EntryKind::Folder;
        folder(a)
            .cmp(&folder(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

/// Every `.dbr` under `entries`, depth first.
pub fn schematics(entries: &[Entry]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in entries {
        match e.kind {
            EntryKind::Schematic => out.push(e.path.clone()),
            EntryKind::Folder => out.extend(schematics(&e.children)),
            _ => {}
        }
    }
    out
}

/// An open project: its folder and what was in it at the last scan.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub entries: Vec<Entry>,
}

impl Project {
    pub fn open(root: &Path) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!("{} is not a folder", root.display()));
        }
        Ok(Self {
            root: root.to_path_buf(),
            entries: scan(root),
        })
    }

    pub fn refresh(&mut self) {
        self.entries = scan(&self.root);
    }

    pub fn name(&self) -> String {
        self.root.file_name().map_or_else(
            || self.root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    pub fn schematics(&self) -> Vec<PathBuf> {
        schematics(&self.entries)
    }

    /// True if `path` is inside the project folder.
    pub fn contains(&self, path: &Path) -> bool {
        normalize(path).starts_with(normalize(&self.root))
    }
}

// ----------------------------------------------------------- paths ---

/// `path` with `.` and `..` resolved by reading it, without touching the
/// disk.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The path of `target` relative to the folder `from`, with `/` separators
/// (`../main.asm`, `fw/blink.ino`). If the two share no root (another
/// drive), the full path of `target`.
pub fn relative_to(from: &Path, target: &Path) -> String {
    let (from, target) = (normalize(from), normalize(target));
    let (a, b): (Vec<_>, Vec<_>) = (from.components().collect(), target.components().collect());
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return slashes(&target);
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..a.len() {
        parts.push("..".into());
    }
    for c in &b[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    parts.join("/")
}

fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// A name for a file or folder: no separators or characters Windows rejects,
/// no trailing dot or space, not a reserved name.
pub fn valid_name(name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.len() > 120 {
        return false;
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    if name
        .chars()
        .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "LPT1"
            | "LPT2"
            | "LPT3"
    )
}

// -------------------------------------------------------- creating ---

/// What a new source file starts as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewSource {
    Assembly,
    Sketch,
}

impl NewSource {
    pub fn extension(self) -> &'static str {
        match self {
            NewSource::Assembly => "asm",
            NewSource::Sketch => "ino",
        }
    }

    /// The starting text: the blink program for the first assembly file,
    /// an empty one after that, and the blink sketch for a sketch.
    pub fn starter(self, name: &str, first: bool) -> String {
        match self {
            NewSource::Assembly if first => crate::code_ui::STARTER.to_string(),
            NewSource::Assembly => format!("; {name}\n"),
            NewSource::Sketch => crate::code_ui::STARTER_SKETCH.to_string(),
        }
    }
}

/// `name` with `ext` appended unless it already ends with it.
pub fn with_extension(name: &str, ext: &str) -> String {
    let name = name.trim();
    if name.to_ascii_lowercase().ends_with(&format!(".{ext}")) {
        name.to_string()
    } else {
        format!("{name}.{ext}")
    }
}

/// Name of the first circuit of a new project.
pub const FIRST_CIRCUIT: &str = "Circuit 1";

/// Create the project folder `dir` (it may exist if it is empty) and a first
/// circuit in it, `Circuit 1.dbr`. Returns the circuit's path.
pub fn create_project(dir: &Path) -> Result<PathBuf, String> {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("choose a name for the project folder")?;
    if !valid_name(&name) {
        return Err(format!("“{name}” cannot be used as a folder name"));
    }
    if dir.exists() {
        let empty = dir.is_dir()
            && std::fs::read_dir(dir)
                .map_err(|e| e.to_string())?
                .next()
                .is_none();
        if !empty {
            return Err(format!(
                "{} is not empty. Choose an empty folder (the folder dialog has a button to make \
                 a new one), or use Open project for a folder that already holds a project.",
                dir.display()
            ));
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{FIRST_CIRCUIT}.dbr"));
    dunbrec_io::dbr::save(&Schematic::new(), &path).map_err(|e| e.to_string())?;
    Ok(path)
}

fn ensure_free(path: &Path) -> Result<(), String> {
    if path.exists() {
        Err(format!(
            "{} already exists",
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned()
            )
        ))
    } else {
        Ok(())
    }
}

/// Create an empty schematic `name` (`.dbr` added) in `dir`.
pub fn new_schematic(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(format!("“{}” cannot be used as a file name", name.trim()));
    }
    let path = dir.join(with_extension(name, "dbr"));
    ensure_free(&path)?;
    dunbrec_io::dbr::save(&Schematic::new(), &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Create a source file `name` (its extension added unless it has one that
/// says it is a source) in `dir`.
pub fn new_source(dir: &Path, name: &str, kind: NewSource) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(format!("“{}” cannot be used as a file name", name.trim()));
    }
    let name = name.trim();
    let name = match source_language_of(name) {
        Some(_) => name.to_string(),
        None => with_extension(name, kind.extension()),
    };
    let path = dir.join(&name);
    ensure_free(&path)?;
    let first = !has_source(dir);
    std::fs::write(&path, kind.starter(&name, first))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

fn has_source(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|r| {
        r.flatten()
            .any(|e| source_language_of(&e.file_name().to_string_lossy()).is_some())
    })
}

/// Create the folder `name` in `dir`.
pub fn new_folder(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(format!("“{}” cannot be used as a folder name", name.trim()));
    }
    let path = dir.join(name.trim());
    ensure_free(&path)?;
    std::fs::create_dir(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    Ok(path)
}

// ---------------------------------------------- sources and schematics ---

/// True if the source called `name` is a file (named like one) rather than
/// text kept inside the schematic.
pub fn is_file_source(name: &str) -> bool {
    source_language_of(name).is_some()
}

/// The firmware reference of every microcontroller, mutably.
fn firmware_refs(s: &mut Schematic) -> impl Iterator<Item = &mut String> {
    s.components.iter_mut().filter_map(|c| match &mut c.kind {
        ComponentKind::Mcu { firmware, .. } => Some(firmware),
        _ => None,
    })
}

/// The names of the source files the microcontrollers of `s` refer to.
pub fn referenced_files(s: &Schematic) -> BTreeSet<String> {
    s.components
        .iter()
        .filter_map(|c| match &c.kind {
            ComponentKind::Mcu { firmware, .. } => Some(firmware.trim().to_string()),
            _ => None,
        })
        .filter(|f| is_file_source(f))
        .collect()
}

fn read_source(dir: &Path, name: &str) -> Result<SourceFile, String> {
    let language =
        source_language_of(name).ok_or_else(|| format!("{name} is not a source file"))?;
    let path = dir.join(name);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(SourceFile { language, text })
}

/// Open the source files `s` refers to: read each one from beside the
/// schematic (`dir`) into `s.sources`. Returns what could not be read.
pub fn hydrate(s: &mut Schematic, dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for name in referenced_files(s) {
        if s.sources.contains_key(&name) {
            continue;
        }
        match read_source(dir, &name) {
            Ok(file) => {
                s.sources.insert(name, file);
            }
            Err(e) => problems.push(e),
        }
    }
    problems
}

/// Open the source file `name` (relative to `dir`) in `s`, if it is not
/// open already.
pub fn attach(s: &mut Schematic, dir: &Path, name: &str) -> Result<(), String> {
    if s.sources.contains_key(name) {
        return Ok(());
    }
    let file = read_source(dir, name)?;
    s.sources.insert(name.to_string(), file);
    Ok(())
}

/// The schematic as the `.dbr` file keeps it: without the texts of the
/// source files, which have files of their own.
pub fn for_saving(s: &Schematic) -> Schematic {
    let mut out = s.clone();
    out.sources.retain(|name, _| !is_file_source(name));
    out
}

/// Write the open source files to their files beside the schematic (`dir`),
/// leaving alone those that already hold the same text. Returns the names
/// written.
pub fn write_sources(s: &Schematic, dir: &Path) -> Result<Vec<String>, String> {
    let mut written = Vec::new();
    for (name, file) in &s.sources {
        if !is_file_source(name) {
            continue;
        }
        let path = dir.join(name);
        if std::fs::read_to_string(&path).is_ok_and(|disk| disk == file.text) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, &file.text)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        written.push(name.clone());
    }
    Ok(written)
}

/// Take up changes made to the source files elsewhere: reread each open
/// file and keep its text from disk if that differs. Returns the names
/// that changed.
pub fn sync_from_disk(s: &mut Schematic, dir: &Path) -> Vec<String> {
    let mut changed = Vec::new();
    for (name, file) in &mut s.sources {
        if !is_file_source(name) {
            continue;
        }
        if let Ok(disk) = std::fs::read_to_string(dir.join(name))
            && disk != file.text
        {
            file.text = disk;
            changed.push(name.clone());
        }
    }
    changed
}

/// Point microcontrollers at a renamed or moved file. `dbr_dir_old` and
/// `dbr_dir_new` are the folder of the schematic before and after (the
/// same unless the schematic's own folder was renamed); `from` and `to` are
/// the file or folder before and after. References and open sources that
/// named something under `from` are rewritten. Returns how many changed.
pub fn rewrite_refs(
    s: &mut Schematic,
    dbr_dir_old: &Path,
    dbr_dir_new: &Path,
    from: &Path,
    to: &Path,
) -> usize {
    let (from, to) = (normalize(from), normalize(to));
    let mut count = 0;
    let mapping = |name: &str| -> Option<String> {
        let old_abs = normalize(&dbr_dir_old.join(name));
        let rest = old_abs.strip_prefix(&from).ok()?;
        let new_abs = if rest.as_os_str().is_empty() {
            to.clone()
        } else {
            to.join(rest)
        };
        let new_ref = relative_to(dbr_dir_new, &new_abs);
        (new_ref != name).then_some(new_ref)
    };
    for firmware in firmware_refs(s) {
        let current = firmware.trim().to_string();
        if !is_file_source(&current) {
            continue;
        }
        if let Some(new_ref) = mapping(&current) {
            *firmware = new_ref;
            count += 1;
        }
    }
    let renames: Vec<(String, String)> = s
        .sources
        .keys()
        .filter(|n| is_file_source(n))
        .filter_map(|n| mapping(n).map(|new| (n.clone(), new)))
        .collect();
    for (old, new) in renames {
        if let Some(file) = s.sources.remove(&old) {
            s.sources.insert(new, file);
            count += 1;
        }
    }
    count
}

/// What renaming a file or folder did to the project's schematics.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Renamed {
    /// Schematic files rewritten on disk to follow the rename.
    pub rewritten: Vec<PathBuf>,
}

/// Rename `from` to `to` (in the same folder, or any other free path), then
/// make every schematic of the project that referred to it, or to something
/// in it, refer to the new place. `open` is the schematic open in the app,
/// which is left alone: the caller updates it in memory with
/// [`rewrite_refs`].
pub fn rename(root: &Path, from: &Path, to: &Path, open: Option<&Path>) -> Result<Renamed, String> {
    ensure_free(to)?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(from, to).map_err(|e| format!("cannot rename {}: {e}", from.display()))?;
    let (from_n, to_n) = (normalize(from), normalize(to));
    let mut out = Renamed::default();
    for dbr_new in scan_schematics(root) {
        if open.is_some_and(|o| normalize(o) == normalize(&dbr_new)) {
            continue;
        }
        // Where this schematic was before the rename (it moves with a
        // renamed folder).
        let dbr_old = match normalize(&dbr_new).strip_prefix(&to_n) {
            Ok(rest) if rest.as_os_str().is_empty() => from_n.clone(),
            Ok(rest) => from_n.join(rest),
            Err(_) => dbr_new.clone(),
        };
        let (old_dir, new_dir) = (
            dbr_old.parent().unwrap_or(Path::new("")).to_path_buf(),
            dbr_new.parent().unwrap_or(Path::new("")).to_path_buf(),
        );
        let Ok(mut s) = dunbrec_io::dbr::load(&dbr_new) else {
            continue;
        };
        if rewrite_refs(&mut s, &old_dir, &new_dir, &from_n, &to_n) > 0 {
            dunbrec_io::dbr::save(&s, &dbr_new).map_err(|e| e.to_string())?;
            out.rewritten.push(dbr_new);
        }
    }
    Ok(out)
}

fn scan_schematics(root: &Path) -> Vec<PathBuf> {
    schematics(&scan(root))
}

/// Delete a file, or a folder with everything in it.
pub fn delete(path: &Path) -> Result<(), String> {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|e| format!("cannot delete {}: {e}", path.display()))
}

/// Move the sources kept inside `s` by older versions into files in `dir`:
/// each `source:<name>` becomes `<name>.asm` or `<name>.ino` (a number is
/// added if a different file has that name already), the microcontrollers
/// are pointed at it, and the embedded text is dropped. Returns
/// `(old name, file name)` pairs.
pub fn extract_embedded(s: &mut Schematic, dir: &Path) -> Result<Vec<(String, String)>, String> {
    let embedded: Vec<String> = s
        .sources
        .keys()
        .filter(|n| !is_file_source(n))
        .cloned()
        .collect();
    let mut moved = Vec::new();
    for name in embedded {
        let file = s.sources[&name].clone();
        let ext = match file.language {
            SourceLanguage::Asm => "asm",
            SourceLanguage::Cpp => "ino",
        };
        let mut candidate = format!("{name}.{ext}");
        let mut n = 2;
        loop {
            let taken = s.sources.contains_key(&candidate)
                || std::fs::read_to_string(dir.join(&candidate)).is_ok_and(|t| t != file.text);
            if !taken {
                break;
            }
            candidate = format!("{name}{n}.{ext}");
            n += 1;
        }
        let path = dir.join(&candidate);
        std::fs::write(&path, &file.text)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        let reference = format!("{SOURCE_PREFIX}{name}");
        for firmware in firmware_refs(s) {
            if firmware.trim() == reference {
                *firmware = candidate.clone();
            }
        }
        s.sources.remove(&name);
        s.sources.insert(candidate.clone(), file);
        moved.push((name, candidate));
    }
    Ok(moved)
}

/// How many sources `s` keeps inside itself (older style).
pub fn embedded_count(s: &Schematic) -> usize {
    s.sources.keys().filter(|n| !is_file_source(n)).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::Point;

    /// A fresh scratch folder.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dunbrec-proj-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mcu(firmware: &str) -> ComponentKind {
        ComponentKind::Mcu {
            firmware: firmware.into(),
            clock: 16e6,
        }
    }

    fn schematic_with(firmware: &str) -> Schematic {
        let mut s = Schematic::new();
        s.add_component(mcu(firmware), Point::new(0, 0));
        s
    }

    fn firmware_of(s: &Schematic) -> String {
        s.components
            .iter()
            .find_map(|c| match &c.kind {
                ComponentKind::Mcu { firmware, .. } => Some(firmware.clone()),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn paths_are_made_relative_with_slashes() {
        let base = Path::new("/p/sub");
        assert_eq!(relative_to(base, Path::new("/p/sub/main.asm")), "main.asm");
        assert_eq!(relative_to(base, Path::new("/p/main.asm")), "../main.asm");
        assert_eq!(
            relative_to(base, Path::new("/p/fw/x/blink.ino")),
            "../fw/x/blink.ino"
        );
        assert_eq!(
            relative_to(Path::new("/p"), Path::new("/p/./a/../b.asm")),
            "b.asm"
        );
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_name("main.asm"));
        assert!(valid_name("my project"));
        for bad in [
            "", " ", ".", "..", "a/b", "a\\b", "a:b", "x?", "con", "NUL.txt", "a.",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert_eq!(with_extension("blink", "asm"), "blink.asm");
        assert_eq!(with_extension("Blink.ASM", "asm"), "Blink.ASM");
    }

    #[test]
    fn a_new_project_has_a_folder_and_a_first_schematic() {
        let root = scratch("new");
        let dir = root.join("Blinker");
        let first = create_project(&dir).unwrap();
        assert_eq!(first, dir.join("Circuit 1.dbr"));
        assert!(dunbrec_io::dbr::load(&first).unwrap().components.is_empty());
        // Not on top of something that is already there.
        assert!(create_project(&dir).unwrap_err().contains("not empty"));
        // An empty existing folder is fine.
        std::fs::create_dir(root.join("Empty")).unwrap();
        assert!(create_project(&root.join("Empty")).is_ok());
        assert!(create_project(&root.join("bad:name")).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_tree_lists_folders_first_and_knows_each_kind() {
        let root = scratch("scan");
        std::fs::create_dir_all(root.join("fw")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        for (path, text) in [
            ("b.dbr", ""),
            ("A.dbr", ""),
            ("fw/blink.ino", ""),
            ("fw/blink.hex", ""),
            ("fw/util.h", ""),
            ("main.asm", ""),
            ("notes.txt", ""),
            (".hidden", ""),
            (".git/config", ""),
            ("target/x.dbr", ""),
        ] {
            std::fs::write(root.join(path), text).unwrap();
        }
        let tree = scan(&root);
        let names: Vec<&str> = tree.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["fw", "A.dbr", "b.dbr", "main.asm", "notes.txt"]);
        let fw = &tree[0];
        assert_eq!(fw.kind, EntryKind::Folder);
        let inner: Vec<(&str, EntryKind)> = fw
            .children
            .iter()
            .map(|e| (e.name.as_str(), e.kind))
            .collect();
        assert_eq!(
            inner,
            [
                ("blink.hex", EntryKind::Hex),
                ("blink.ino", EntryKind::Source(SourceLanguage::Cpp)),
                ("util.h", EntryKind::Header),
            ]
        );
        assert_eq!(tree[3].kind, EntryKind::Source(SourceLanguage::Asm));
        assert_eq!(schematics(&tree).len(), 2);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn new_files_get_the_right_extension_and_starter() {
        let root = scratch("create");
        let a = new_source(&root, "blink", NewSource::Assembly).unwrap();
        assert_eq!(a, root.join("blink.asm"));
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            crate::code_ui::STARTER
        );
        // The second assembly file starts empty.
        let b = new_source(&root, "second.asm", NewSource::Assembly).unwrap();
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "; second.asm\n");
        let c = new_source(&root, "fade", NewSource::Sketch).unwrap();
        assert_eq!(c, root.join("fade.ino"));
        assert!(
            new_source(&root, "blink", NewSource::Assembly)
                .unwrap_err()
                .contains("already exists")
        );
        assert!(new_source(&root, "a/b", NewSource::Assembly).is_err());
        let d = new_schematic(&root, "amp").unwrap();
        assert_eq!(d, root.join("amp.dbr"));
        assert!(new_schematic(&root, "amp.dbr").is_err());
        assert!(new_folder(&root, "fw").unwrap().is_dir());
        assert!(new_folder(&root, "fw").is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn source_files_are_opened_saved_and_kept_out_of_the_dbr() {
        let root = scratch("sources");
        std::fs::create_dir_all(root.join("fw")).unwrap();
        std::fs::write(root.join("fw").join("main.asm"), "nop\n").unwrap();
        let mut s = schematic_with("fw/main.asm");
        // Opening reads the file the microcontroller refers to.
        assert!(hydrate(&mut s, &root).is_empty());
        assert_eq!(s.sources["fw/main.asm"].text, "nop\n");
        assert_eq!(s.sources["fw/main.asm"].language, SourceLanguage::Asm);
        // A file that is not there is reported, not fatal.
        let mut missing = schematic_with("gone.asm");
        let problems = hydrate(&mut missing, &root);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("cannot read"));
        // An old-style source stays inside the schematic.
        s.sources.insert(
            "old".into(),
            SourceFile {
                language: SourceLanguage::Asm,
                text: "ret\n".into(),
            },
        );
        let saved = for_saving(&s);
        assert!(!saved.sources.contains_key("fw/main.asm"));
        assert!(saved.sources.contains_key("old"));
        // Editing and saving writes the file, and only when it changed.
        s.sources.get_mut("fw/main.asm").unwrap().text = "nop\nnop\n".into();
        assert_eq!(write_sources(&s, &root).unwrap(), ["fw/main.asm"]);
        assert_eq!(
            std::fs::read_to_string(root.join("fw").join("main.asm")).unwrap(),
            "nop\nnop\n"
        );
        assert!(write_sources(&s, &root).unwrap().is_empty());
        // A new file in a new folder gets the folder.
        s.sources.insert(
            "deep/er/x.ino".into(),
            SourceFile {
                language: SourceLanguage::Cpp,
                text: "void setup(){}".into(),
            },
        );
        assert_eq!(write_sources(&s, &root).unwrap(), ["deep/er/x.ino"]);
        // Changes made elsewhere are picked up.
        std::fs::write(root.join("fw").join("main.asm"), "ldi r16, 1\n").unwrap();
        assert_eq!(sync_from_disk(&mut s, &root), ["fw/main.asm"]);
        assert_eq!(s.sources["fw/main.asm"].text, "ldi r16, 1\n");
        assert!(sync_from_disk(&mut s, &root).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_schematic_opened_from_disk_builds_its_firmware_from_the_files() {
        let root = scratch("firmware");
        let text = "sbi DDRB, PB5
loop: rjmp loop
";
        std::fs::write(root.join("main.asm"), text).unwrap();
        let path = root.join("x.dbr");
        dunbrec_io::dbr::save(&for_saving(&schematic_with("main.asm")), &path).unwrap();
        // The .dbr holds only the name; opening brings the file in.
        let mut loaded = dunbrec_io::dbr::load(&path).unwrap();
        assert!(loaded.sources.is_empty());
        assert!(hydrate(&mut loaded, &root).is_empty());
        let netlist = dunbrec_core::build_netlist(&loaded);
        let image = dunbrec_sim::load_firmware("main.asm", Some(&root), &netlist.sources).unwrap();
        assert_eq!(image, dunbrec_mcu::asm_text::assemble(text).unwrap().image);
        // Edited in memory, the new text is what runs.
        loaded.sources.get_mut("main.asm").unwrap().text = "nop
loop: rjmp loop
"
        .into();
        let netlist = dunbrec_core::build_netlist(&loaded);
        let edited = dunbrec_sim::load_firmware("main.asm", Some(&root), &netlist.sources).unwrap();
        assert_ne!(edited, image);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn renaming_a_source_keeps_every_schematic_pointing_at_it() {
        let root = scratch("rename");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("main.asm"), "nop\n").unwrap();
        // Three schematics: two in the root and one in a subfolder, all
        // using main.asm, and one using a different file.
        let top = root.join("top.dbr");
        let other = root.join("other.dbr");
        let deep = root.join("sub").join("deep.dbr");
        let unrelated = root.join("unrelated.dbr");
        dunbrec_io::dbr::save(&schematic_with("main.asm"), &top).unwrap();
        dunbrec_io::dbr::save(&schematic_with("main.asm"), &other).unwrap();
        dunbrec_io::dbr::save(&schematic_with("../main.asm"), &deep).unwrap();
        dunbrec_io::dbr::save(&schematic_with("builtin:blink"), &unrelated).unwrap();

        let renamed = rename(
            &root,
            &root.join("main.asm"),
            &root.join("blink.asm"),
            Some(&top),
        )
        .unwrap();
        assert!(root.join("blink.asm").is_file() && !root.join("main.asm").exists());
        // The open schematic is left for the caller; the others follow.
        let mut files: Vec<_> = renamed
            .rewritten
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(files, ["deep.dbr", "other.dbr"]);
        assert_eq!(
            firmware_of(&dunbrec_io::dbr::load(&other).unwrap()),
            "blink.asm"
        );
        assert_eq!(
            firmware_of(&dunbrec_io::dbr::load(&deep).unwrap()),
            "../blink.asm"
        );
        assert_eq!(
            firmware_of(&dunbrec_io::dbr::load(&unrelated).unwrap()),
            "builtin:blink"
        );
        assert_eq!(
            firmware_of(&dunbrec_io::dbr::load(&top).unwrap()),
            "main.asm"
        );

        // The caller does the open one in memory, with its open source.
        let mut s = dunbrec_io::dbr::load(&top).unwrap();
        s.sources.insert(
            "main.asm".into(),
            SourceFile {
                language: SourceLanguage::Asm,
                text: "nop\n".into(),
            },
        );
        let n = rewrite_refs(
            &mut s,
            &root,
            &root,
            &root.join("main.asm"),
            &root.join("blink.asm"),
        );
        assert_eq!(n, 2);
        assert_eq!(firmware_of(&s), "blink.asm");
        assert!(s.sources.contains_key("blink.asm") && !s.sources.contains_key("main.asm"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn renaming_a_folder_updates_references_into_it() {
        let root = scratch("rename-dir");
        std::fs::create_dir_all(root.join("fw")).unwrap();
        std::fs::write(root.join("fw").join("blink.asm"), "nop\n").unwrap();
        // One schematic outside the folder, one inside it using a file
        // outside the folder and one inside.
        std::fs::write(root.join("lib.asm"), "nop\n").unwrap();
        let outside = root.join("a.dbr");
        let inside = root.join("fw").join("b.dbr");
        let mut both = schematic_with("blink.asm");
        both.add_component(mcu("../lib.asm"), Point::new(10, 0));
        dunbrec_io::dbr::save(&schematic_with("fw/blink.asm"), &outside).unwrap();
        dunbrec_io::dbr::save(&both, &inside).unwrap();

        let r = rename(&root, &root.join("fw"), &root.join("firmware"), None).unwrap();
        assert_eq!(r.rewritten, std::slice::from_ref(&outside));
        assert_eq!(
            firmware_of(&dunbrec_io::dbr::load(&outside).unwrap()),
            "firmware/blink.asm"
        );
        // Inside the folder both references still work unchanged.
        let moved = dunbrec_io::dbr::load(root.join("firmware").join("b.dbr")).unwrap();
        assert_eq!(
            referenced_files(&moved),
            BTreeSet::from(["blink.asm".to_string(), "../lib.asm".to_string()])
        );
        assert!(rename(&root, &root.join("a.dbr"), &root.join("firmware"), None).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn old_embedded_sources_move_out_into_files() {
        let root = scratch("extract");
        let mut s = schematic_with("source:main");
        s.add_component(mcu("source:main"), Point::new(5, 0));
        s.add_component(mcu("source:fade"), Point::new(9, 0));
        s.add_component(mcu("builtin:blink"), Point::new(13, 0));
        for (name, language, text) in [
            ("main", SourceLanguage::Asm, "nop\n"),
            ("fade", SourceLanguage::Cpp, "void setup(){}"),
        ] {
            s.sources.insert(
                name.into(),
                SourceFile {
                    language,
                    text: text.into(),
                },
            );
        }
        // A different file is in the way of the first name.
        std::fs::write(root.join("main.asm"), "something else\n").unwrap();
        assert_eq!(embedded_count(&s), 2);
        let moved = extract_embedded(&mut s, &root).unwrap();
        assert_eq!(
            moved,
            [
                ("fade".to_string(), "fade.ino".to_string()),
                ("main".to_string(), "main2.asm".to_string())
            ]
        );
        assert_eq!(
            std::fs::read_to_string(root.join("main2.asm")).unwrap(),
            "nop\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("fade.ino")).unwrap(),
            "void setup(){}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("main.asm")).unwrap(),
            "something else\n"
        );
        let refs: Vec<String> = s
            .components
            .iter()
            .filter_map(|c| match &c.kind {
                ComponentKind::Mcu { firmware, .. } => Some(firmware.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            refs,
            ["main2.asm", "main2.asm", "fade.ino", "builtin:blink"]
        );
        assert_eq!(embedded_count(&s), 0);
        assert!(s.sources.contains_key("main2.asm") && s.sources.contains_key("fade.ino"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}

//! Projects: a file that lists a design's schematics and firmware source
//! files, so they are kept together.
//!
//! A project is a small file (`My amp.dbrproj`) that the user makes wherever
//! they like. It holds the list of the files that belong to the project,
//! as paths relative to the project file's own folder. Anything else in that
//! folder is simply not part of the project and is never shown. The files
//! themselves are ordinary files (`.dbr` schematics, `.asm` and `.ino`
//! sources), so other editors, version control and `arduino-cli` see them as
//! they are.
//!
//! A firmware source is known to a schematic by its path relative to the
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
use serde::{Deserialize, Serialize};

/// Extension of a project file.
pub const EXTENSION: &str = "dbrproj";

/// What an entry of the project is, for the icon and for what a
/// double-click does.
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
    /// The project lists it, but it is not on the disk.
    pub missing: bool,
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

/// Every schematic under `entries`, depth first.
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

/// What the project file holds.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
struct ProjectData {
    version: u32,
    /// The project's files, relative to the project file's folder (`/`).
    files: Vec<String>,
    /// Folders to show even with nothing in them (relative, `/`).
    folders: Vec<String>,
}

const FORMAT_VERSION: u32 = 1;

/// An open project: its file, and the tree of what it lists.
#[derive(Debug, Clone)]
pub struct Project {
    /// The project file.
    pub file: PathBuf,
    /// The folder the project file is in; listed paths are relative to it.
    pub root: PathBuf,
    data: ProjectData,
    pub entries: Vec<Entry>,
}

/// True if `path` is `base` or inside it, by reading the paths.
fn under(path: &Path, base: &Path) -> bool {
    normalize(path).starts_with(normalize(base))
}

impl Project {
    fn from_data(file: &Path, data: ProjectData) -> Self {
        let mut p = Self {
            file: file.to_path_buf(),
            root: file.parent().unwrap_or(Path::new("")).to_path_buf(),
            data,
            entries: Vec::new(),
        };
        p.refresh();
        p
    }

    /// Read a project file.
    pub fn open(file: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let data: ProjectData = serde_json::from_str(&text).map_err(|e| {
            format!(
                "{} is not a Dunbrec project file ({e})",
                file.file_name().map_or_else(
                    || file.display().to_string(),
                    |n| n.to_string_lossy().into_owned()
                )
            )
        })?;
        if data.version > FORMAT_VERSION {
            return Err(format!(
                "{} was made by a newer Dunbrec; update the program to open it",
                file.display()
            ));
        }
        Ok(Self::from_data(file, data))
    }

    /// Make a new project: write the project file `file` (`.dbrproj` is added
    /// if missing) and a first, empty circuit beside it, `Circuit 1.dbr` or the
    /// next name nothing there has. Returns the project and the circuit.
    pub fn create(file: &Path) -> Result<(Self, PathBuf), String> {
        let mut file = file.to_path_buf();
        if file
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case(EXTENSION))
        {
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned());
            if let Some(name) = name {
                file.set_file_name(with_extension(&name, EXTENSION));
            }
        }
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !valid_name(&name) {
            return Err(format!("“{name}” cannot be used as a file name"));
        }
        ensure_free(&file).map_err(|e| format!("{e}. Use Open project to open it."))?;
        let dir = file.parent().unwrap_or(Path::new("")).to_path_buf();
        let circuit = free_circuit_in(&dir);
        dunbrec_io::dbr::save(&Schematic::new(), &circuit).map_err(|e| e.to_string())?;
        let mut project = Self::from_data(
            &file,
            ProjectData {
                version: FORMAT_VERSION,
                ..ProjectData::default()
            },
        );
        project.add_file(&circuit)?;
        Ok((project, circuit))
    }

    /// Write the project file.
    pub fn save(&self) -> Result<(), String> {
        let mut data = self.data.clone();
        data.version = FORMAT_VERSION;
        let text = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
        std::fs::write(&self.file, text + "\n")
            .map_err(|e| format!("cannot write {}: {e}", self.file.display()))
    }

    /// The project's name: its file's name without the extension.
    pub fn name(&self) -> String {
        self.file.file_stem().map_or_else(
            || self.file.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Where a listed path really is.
    fn absolute(&self, listed: &str) -> PathBuf {
        normalize(&self.root.join(listed))
    }

    fn listed(&self, path: &Path) -> String {
        relative_to(&self.root, path)
    }

    /// Every listed file, as a path.
    pub fn files(&self) -> Vec<PathBuf> {
        self.data.files.iter().map(|f| self.absolute(f)).collect()
    }

    /// The listed `.dbr` schematics.
    pub fn schematics(&self) -> Vec<PathBuf> {
        schematics(&self.entries)
    }

    /// True if the project lists `path`.
    pub fn contains(&self, path: &Path) -> bool {
        let path = normalize(path);
        self.files().contains(&path)
    }

    /// List the files (those not listed yet). Returns how many were new.
    pub fn add_files(&mut self, paths: &[PathBuf]) -> Result<usize, String> {
        let mut added = 0;
        for path in paths {
            let listed = self.listed(path);
            if !self.data.files.contains(&listed) {
                self.data.files.push(listed);
                added += 1;
            }
        }
        if added > 0 {
            self.refresh();
            self.save()?;
        }
        Ok(added)
    }

    /// List one file. Returns true if it was new.
    pub fn add_file(&mut self, path: &Path) -> Result<bool, String> {
        Ok(self.add_files(&[path.to_path_buf()])? > 0)
    }

    /// Show a folder, even while nothing in it is listed.
    pub fn add_folder(&mut self, path: &Path) -> Result<(), String> {
        let listed = self.listed(path);
        if !self.data.folders.contains(&listed) {
            self.data.folders.push(listed);
            self.refresh();
            self.save()?;
        }
        Ok(())
    }

    /// Take `path` out of the project: the file, or the folder with
    /// everything listed under it. The disk is not touched. Returns how many
    /// listed files went.
    pub fn remove(&mut self, path: &Path) -> Result<usize, String> {
        let before = self.data.files.len();
        let (root, path) = (self.root.clone(), path.to_path_buf());
        let gone = |listed: &String| under(&root.join(listed), &path);
        self.data.files.retain(|f| !gone(f));
        self.data.folders.retain(|f| !gone(f));
        let removed = before - self.data.files.len();
        self.refresh();
        self.save()?;
        Ok(removed)
    }

    /// Follow a file or folder that was renamed or moved on the disk: what
    /// the project listed at or under `from` is now under `to`.
    pub fn moved(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        let (from, to) = (normalize(from), normalize(to));
        let root = self.root.clone();
        let remap = |listed: &mut String| {
            let abs = normalize(&root.join(&*listed));
            if let Ok(rest) = abs.strip_prefix(&from) {
                let new = if rest.as_os_str().is_empty() {
                    to.clone()
                } else {
                    to.join(rest)
                };
                *listed = relative_to(&root, &new);
            }
        };
        self.data.files.iter_mut().for_each(remap);
        self.data.folders.iter_mut().for_each(remap);
        self.refresh();
        self.save()
    }

    /// Rebuild the tree from the list (and look again at what is on disk).
    pub fn refresh(&mut self) {
        let mut entries = Vec::new();
        for folder in &self.data.folders {
            let parts: Vec<&str> = folder
                .split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .collect();
            insert(&mut entries, &self.root, &parts, false);
        }
        for file in &self.data.files {
            let parts: Vec<&str> = file
                .split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .collect();
            // A file outside the project's folder has no folders to sit in.
            if parts.contains(&"..") || Path::new(file).is_absolute() {
                let path = self.absolute(file);
                entries.push(Entry {
                    name: file.clone(),
                    kind: kind_of(file),
                    missing: !path.exists(),
                    path,
                    children: Vec::new(),
                });
            } else {
                insert(&mut entries, &self.root, &parts, true);
            }
        }
        sort_entries(&mut entries);
        self.entries = entries;
    }

    /// Read the project file again (it was edited by hand or by another
    /// program).
    pub fn reload(&mut self) -> Result<(), String> {
        *self = Self::open(&self.file)?;
        Ok(())
    }
}

/// Put the file or folder `parts` (below `dir`) into the tree.
fn insert(entries: &mut Vec<Entry>, dir: &Path, parts: &[&str], is_file: bool) {
    let Some((first, rest)) = parts.split_first() else {
        return;
    };
    let path = dir.join(first);
    if rest.is_empty() && is_file {
        entries.push(Entry {
            name: first.to_string(),
            kind: kind_of(first),
            missing: !path.exists(),
            path,
            children: Vec::new(),
        });
        return;
    }
    let at = match entries
        .iter()
        .position(|e| e.kind == EntryKind::Folder && e.name == *first)
    {
        Some(i) => i,
        None => {
            entries.push(Entry {
                name: first.to_string(),
                kind: EntryKind::Folder,
                missing: !path.is_dir(),
                path: path.clone(),
                children: Vec::new(),
            });
            entries.len() - 1
        }
    };
    if !rest.is_empty() {
        insert(&mut entries[at].children, &path, rest, is_file);
    }
}

/// Folders first, then files, each alphabetical without regard to case.
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let file = |e: &Entry| e.kind != EntryKind::Folder;
        file(a)
            .cmp(&file(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    for e in entries {
        sort_entries(&mut e.children);
    }
}

/// `Circuit 1.dbr`, or the next number, in `dir`: a path nothing is at.
fn free_circuit_in(dir: &Path) -> PathBuf {
    (1..)
        .map(|n| dir.join(format!("Circuit {n}.dbr")))
        .find(|p| !p.exists())
        .expect("a free number")
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

/// Rename `from` to `to` on the disk (in the same folder, or any other free
/// path), make the project list the new place, and make every schematic of
/// the project that referred to it, or to something in it, refer to the new
/// place. `open` is the schematic open in the app, which is left alone: the
/// caller updates it in memory with [`rewrite_refs`].
pub fn rename(
    project: &mut Project,
    from: &Path,
    to: &Path,
    open: Option<&Path>,
) -> Result<Renamed, String> {
    ensure_free(to)?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(from, to).map_err(|e| format!("cannot rename {}: {e}", from.display()))?;
    project.moved(from, to)?;
    let (from_n, to_n) = (normalize(from), normalize(to));
    let mut out = Renamed::default();
    for dbr_new in project.schematics() {
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
        let none = Path::new("");
        let (old_dir, new_dir) = (
            dbr_old.parent().unwrap_or(none).to_path_buf(),
            dbr_new.parent().unwrap_or(none).to_path_buf(),
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

/// Delete a file, or a folder with everything in it, from the disk, and take
/// it out of the project.
pub fn delete(project: &mut Project, path: &Path) -> Result<(), String> {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|e| format!("cannot delete {}: {e}", path.display()))?;
    project.remove(path)?;
    Ok(())
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

    /// A project in a scratch folder, with `Circuit 1` and the files named.
    fn project_in(root: &Path, files: &[&str]) -> Project {
        let file = root.join("Test.dbrproj");
        let (mut project, _) = Project::create(&file).unwrap();
        for name in files {
            let path = root.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            if !path.exists() {
                std::fs::write(&path, "").unwrap();
            }
            project.add_file(&path).unwrap();
        }
        project
    }

    #[test]
    fn a_new_project_is_a_file_with_a_first_circuit_beside_it() {
        let root = scratch("new");
        let file = root.join("Blinker");
        let (project, circuit) = Project::create(&file).unwrap();
        // The extension is added, and the circuit sits beside the project file.
        assert_eq!(project.file, root.join("Blinker.dbrproj"));
        assert_eq!(circuit, root.join("Circuit 1.dbr"));
        assert!(project.file.is_file() && circuit.is_file());
        assert_eq!(project.name(), "Blinker");
        assert!(
            dunbrec_io::dbr::load(&circuit)
                .unwrap()
                .components
                .is_empty()
        );
        assert_eq!(project.files(), std::slice::from_ref(&circuit));
        // Open again: the same list.
        let again = Project::open(&project.file).unwrap();
        assert_eq!(again.files(), project.files());
        // Never on top of an existing project, and never over other files in
        // the folder: the next circuit name is used.
        assert!(
            Project::create(&root.join("Blinker.dbrproj"))
                .unwrap_err()
                .contains("already exists")
        );
        let (_, second) = Project::create(&root.join("Second")).unwrap();
        assert_eq!(second, root.join("Circuit 2.dbr"));
        assert!(Project::create(&root.join("bad:name")).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_project_file_is_readable_and_checked() {
        let root = scratch("file");
        let (project, _) = Project::create(&root.join("P")).unwrap();
        let text = std::fs::read_to_string(&project.file).unwrap();
        assert!(
            text.contains("\"files\"") && text.contains("Circuit 1.dbr"),
            "{text}"
        );
        // Not a project file, and one from the future.
        std::fs::write(root.join("junk.dbrproj"), "hello").unwrap();
        assert!(
            Project::open(&root.join("junk.dbrproj"))
                .unwrap_err()
                .contains("not a Dunbrec project")
        );
        std::fs::write(root.join("new.dbrproj"), "{\"version\": 99}").unwrap();
        assert!(
            Project::open(&root.join("new.dbrproj"))
                .unwrap_err()
                .contains("newer")
        );
        assert!(
            Project::open(&root.join("missing.dbrproj"))
                .unwrap_err()
                .contains("cannot read")
        );
        // A project file with only some fields still opens.
        std::fs::write(root.join("min.dbrproj"), "{}").unwrap();
        assert!(
            Project::open(&root.join("min.dbrproj"))
                .unwrap()
                .files()
                .is_empty()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn only_listed_files_show_and_the_tree_follows_their_folders() {
        let root = scratch("tree");
        std::fs::write(root.join("not mine.txt"), "").unwrap();
        std::fs::write(root.join("other.dbr"), "").unwrap();
        let mut project = project_in(
            &root,
            &[
                "fw/blink.ino",
                "fw/blink.hex",
                "fw/util.h",
                "main.asm",
                "notes.txt",
                "Aaa.dbr",
            ],
        );
        let names: Vec<&str> = project.entries.iter().map(|e| e.name.as_str()).collect();
        // Folders first; the files the folder holds that are not listed are absent.
        assert_eq!(
            names,
            ["fw", "Aaa.dbr", "Circuit 1.dbr", "main.asm", "notes.txt"]
        );
        let inner: Vec<(&str, EntryKind)> = project.entries[0]
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
        assert_eq!(project.schematics().len(), 2);
        assert!(project.contains(&root.join("fw").join("blink.ino")));
        assert!(!project.contains(&root.join("other.dbr")));
        // A listed file that is gone shows as missing; a folder with
        // nothing listed shows when it was added.
        std::fs::remove_file(root.join("main.asm")).unwrap();
        std::fs::create_dir(root.join("empty")).unwrap();
        project.add_folder(&root.join("empty")).unwrap();
        let find = |p: &Project, name: &str| p.entries.iter().find(|e| e.name == name).cloned();
        assert!(find(&project, "main.asm").unwrap().missing);
        assert!(!find(&project, "notes.txt").unwrap().missing);
        assert_eq!(find(&project, "empty").unwrap().kind, EntryKind::Folder);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn files_are_added_and_removed_without_touching_the_disk() {
        let root = scratch("addremove");
        let mut project = project_in(&root, &["a.asm", "sub/b.asm", "sub/c.ino"]);
        assert_eq!(project.files().len(), 4);
        // Adding what is listed already changes nothing.
        assert!(!project.add_file(&root.join("a.asm")).unwrap());
        std::fs::write(root.join("new.asm"), "").unwrap();
        assert_eq!(
            project
                .add_files(&[root.join("a.asm"), root.join("new.asm")])
                .unwrap(),
            1
        );
        // Remove one file, then a folder with what is listed in it.
        assert_eq!(project.remove(&root.join("a.asm")).unwrap(), 1);
        assert!(root.join("a.asm").is_file(), "the file stays on disk");
        assert_eq!(project.remove(&root.join("sub")).unwrap(), 2);
        assert!(root.join("sub").join("b.asm").is_file());
        assert!(project.entries.iter().all(|e| e.name != "sub"));
        // The list is saved each time.
        let again = Project::open(&project.file).unwrap();
        assert_eq!(again.files(), project.files());
        // A file outside the project's folder is listed with a relative path.
        let outside = root.parent().unwrap().join("outside-dunbrec.asm");
        std::fs::write(&outside, "").unwrap();
        project.add_file(&outside).unwrap();
        assert!(project.contains(&outside));
        assert!(project.entries.iter().any(|e| e.name.starts_with("../")));
        std::fs::remove_file(&outside).unwrap();
        // Delete takes it off the disk too.
        delete(&mut project, &root.join("new.asm")).unwrap();
        assert!(!root.join("new.asm").exists() && !project.contains(&root.join("new.asm")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn renaming_a_source_keeps_the_project_and_every_schematic_pointing_at_it() {
        let root = scratch("rename");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("main.asm"), "nop\n").unwrap();
        // Schematics using main.asm: two beside it, one in a subfolder; plus
        // one that does not use it.
        let top = root.join("top.dbr");
        let other = root.join("other.dbr");
        let deep = root.join("sub").join("deep.dbr");
        let unrelated = root.join("unrelated.dbr");
        dunbrec_io::dbr::save(&schematic_with("main.asm"), &top).unwrap();
        dunbrec_io::dbr::save(&schematic_with("main.asm"), &other).unwrap();
        dunbrec_io::dbr::save(&schematic_with("../main.asm"), &deep).unwrap();
        dunbrec_io::dbr::save(&schematic_with("builtin:blink"), &unrelated).unwrap();
        let mut project = project_in(&root, &[]);
        let all = [&top, &other, &deep, &unrelated, &root.join("main.asm")];
        project.add_files(&all.map(|p| p.to_path_buf())).unwrap();

        let renamed = rename(
            &mut project,
            &root.join("main.asm"),
            &root.join("blink.asm"),
            Some(&top),
        )
        .unwrap();
        assert!(root.join("blink.asm").is_file() && !root.join("main.asm").exists());
        // The project lists the new name.
        assert!(
            project.contains(&root.join("blink.asm")) && !project.contains(&root.join("main.asm"))
        );
        assert!(
            Project::open(&project.file)
                .unwrap()
                .contains(&root.join("blink.asm"))
        );
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
    fn renaming_a_folder_updates_the_list_and_references_into_it() {
        let root = scratch("rename-dir");
        std::fs::create_dir_all(root.join("fw")).unwrap();
        std::fs::write(root.join("fw").join("blink.asm"), "nop\n").unwrap();
        std::fs::write(root.join("lib.asm"), "nop\n").unwrap();
        // One schematic outside the folder, one inside it using a file
        // outside the folder and one inside.
        let outside = root.join("a.dbr");
        let inside = root.join("fw").join("b.dbr");
        let mut both = schematic_with("blink.asm");
        both.add_component(mcu("../lib.asm"), Point::new(10, 0));
        dunbrec_io::dbr::save(&schematic_with("fw/blink.asm"), &outside).unwrap();
        dunbrec_io::dbr::save(&both, &inside).unwrap();
        let mut project = project_in(&root, &[]);
        project
            .add_files(&[
                outside.clone(),
                inside.clone(),
                root.join("fw").join("blink.asm"),
                root.join("lib.asm"),
            ])
            .unwrap();

        let r = rename(&mut project, &root.join("fw"), &root.join("firmware"), None).unwrap();
        assert_eq!(r.rewritten, std::slice::from_ref(&outside));
        assert_eq!(
            firmware_of(&dunbrec_io::dbr::load(&outside).unwrap()),
            "firmware/blink.asm"
        );
        // The list followed the folder.
        assert!(project.contains(&root.join("firmware").join("b.dbr")));
        assert!(!project.contains(&root.join("fw").join("b.dbr")));
        // Inside the folder both references still work unchanged.
        let moved = dunbrec_io::dbr::load(root.join("firmware").join("b.dbr")).unwrap();
        assert_eq!(
            referenced_files(&moved),
            BTreeSet::from(["blink.asm".to_string(), "../lib.asm".to_string()])
        );
        assert!(
            rename(
                &mut project,
                &root.join("a.dbr"),
                &root.join("firmware"),
                None
            )
            .is_err()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

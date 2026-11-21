//! Building Arduino C++ sketches with the `arduino-cli` command-line tool.
//!
//! The tool is an external program, never linked or bundled: Dunbrec finds
//! it, writes the sketch to a temporary folder, runs
//! `arduino-cli compile` for an Arduino Uno (the ATmega328P) and reads the
//! `.hex` it produces. Everything that touches the tool lives in this one
//! module.
//!
//! Setting the tool up on Windows:
//!
//! ```text
//! winget install ArduinoSA.CLI
//! arduino-cli core install arduino:avr
//! ```
//!
//! Built images are remembered by the sketch text ([`cached_image`]), so
//! running a circuit never waits for a compiler: a sketch is built on
//! request, and the simulation uses the last build of exactly that text.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

/// The board every sketch is built for.
pub const FQBN: &str = "arduino:avr:uno";

/// What to tell someone who has no tool, in plain words.
pub const SETUP_HELP: &str = "The Arduino command-line tool is needed to build sketches. \
Install it (on Windows: winget install ArduinoSA.CLI), then run: \
arduino-cli core install arduino:avr";

/// One problem the compiler reported in the sketch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Line in the sketch, from 1.
    pub line: usize,
    /// Column from 1; 0 when the compiler gave none.
    pub column: usize,
    /// False for a warning.
    pub error: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOutput {
    /// The flash image.
    pub image: Vec<u8>,
    /// Warnings, if the tool reported any.
    pub warnings: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// The tool, or the Arduino AVR core, is not installed.
    NoToolchain(String),
    /// The compiler rejected the sketch.
    Failed {
        diagnostics: Vec<Diagnostic>,
        /// The tool's own output, for what the diagnostics miss.
        log: String,
    },
    /// Something else went wrong (the tool would not run, no `.hex`...).
    Other(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::NoToolchain(m) | BuildError::Other(m) => f.write_str(m),
            BuildError::Failed { diagnostics, log } => match diagnostics.first() {
                Some(d) => write!(f, "line {}: {}", d.line, d.message),
                None => f.write_str(log.trim()),
            },
        }
    }
}

impl std::error::Error for BuildError {}

// ---------------------------------------------------------------------------
// Finding the tool

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "arduino-cli.exe"
    } else {
        "arduino-cli"
    }
}

/// Places the tool is usually installed that may not be on `PATH` (a
/// program started before the installer ran has an older `PATH`).
fn usual_places() -> Vec<PathBuf> {
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut out = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(p) = env(var) {
                out.push(p.join("Arduino CLI").join(exe_name()));
                out.push(
                    p.join("Arduino IDE")
                        .join("resources/app/lib/backend/resources")
                        .join(exe_name()),
                );
            }
        }
        if let Some(p) = env("LOCALAPPDATA") {
            out.push(
                p.join("Programs/Arduino IDE/resources/app/lib/backend/resources")
                    .join(exe_name()),
            );
        }
    } else {
        for dir in ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin"] {
            out.push(Path::new(dir).join(exe_name()));
        }
        if let Some(home) = env("HOME") {
            out.push(home.join(".local/bin").join(exe_name()));
            out.push(home.join("bin").join(exe_name()));
        }
    }
    out
}

/// Find `arduino-cli`: the `configured` path if it names a file, then
/// `PATH`, then the usual install places.
pub fn find_cli(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = configured
        && p.is_file()
    {
        return Some(p.to_path_buf());
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(exe_name());
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    usual_places().into_iter().find(|p| p.is_file())
}

// ---------------------------------------------------------------------------
// Compiler output

/// Pick the compiler's problems for the sketch file `file_name` (for
/// example `blink.ino`) out of the tool's output. Lines look like
/// `C:\\path\\blink\\blink.ino:5:3: error: 'x' was not declared in this scope`;
/// notes and lines about other files are skipped.
pub fn parse_diagnostics(output: &str, file_name: &str) -> Vec<Diagnostic> {
    let needle = format!("{file_name}:");
    let mut out = Vec::new();
    for raw in output.lines() {
        let Some(at) = raw.find(&needle) else {
            continue;
        };
        // The text before must end a path component, not a longer name.
        if raw[..at]
            .chars()
            .next_back()
            .is_some_and(|c| !matches!(c, '/' | '\\'))
        {
            continue;
        }
        let rest = &raw[at + needle.len()..];
        let mut parts = rest.splitn(3, ':');
        let Some(line) = parts.next().and_then(|s| s.trim().parse::<usize>().ok()) else {
            continue;
        };
        let second = parts.next().unwrap_or("");
        let (column, kind_and_message) = match second.trim().parse::<usize>() {
            Ok(col) => (col, parts.next().unwrap_or("")),
            // No column: `file.ino:5: error: message`.
            Err(_) => (0, rest.split_once(':').map_or("", |(_, r)| r)),
        };
        let kind_and_message = kind_and_message.trim();
        let (kind, message) = kind_and_message
            .split_once(':')
            .map_or((kind_and_message, ""), |(k, m)| (k.trim(), m.trim()));
        let error = match kind {
            "error" | "fatal error" => true,
            "warning" => false,
            _ => continue,
        };
        out.push(Diagnostic {
            line,
            column,
            error,
            message: message.to_string(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Building

/// A folder name the tool accepts for a sketch called `name`.
fn sketch_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    match cleaned.chars().next() {
        None => "sketch".to_string(),
        Some(c) if c.is_ascii_digit() => format!("sketch_{cleaned}"),
        Some(_) => cleaned,
    }
}

fn run(cli: &Path, args: &[&std::ffi::OsStr]) -> std::io::Result<(bool, String)> {
    let mut command = Command::new(cli);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window flashing up from a graphical program.
        command.creation_flags(0x0800_0000);
    }
    let output = command.output()?;
    let mut log = String::from_utf8_lossy(&output.stdout).into_owned();
    log.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((output.status.success(), log))
}

/// Build `source` as an Arduino sketch called `name` with the tool at
/// `cli`. Takes seconds (the first build after installing: longer), so call
/// it from a worker thread.
pub fn build(cli: &Path, name: &str, source: &str) -> Result<BuildOutput, BuildError> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    static BUILD_LOCK: Mutex<()> = Mutex::new(());
    let sketch = sketch_name(name);
    let root = std::env::temp_dir().join(format!(
        "dunbrec-build-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    // The tool shares a cache of compiled core files between runs, and two
    // builds at once can trip over each other in it: one at a time.
    let _one_at_a_time = BUILD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let result = build_in(cli, &sketch, source, &root);
    let _ = std::fs::remove_dir_all(&root);
    result
}

fn build_in(
    cli: &Path,
    sketch: &str,
    source: &str,
    root: &Path,
) -> Result<BuildOutput, BuildError> {
    let other = |what: &str, e: std::io::Error| BuildError::Other(format!("{what}: {e}"));
    let folder = root.join(sketch);
    let out = root.join("out");
    std::fs::create_dir_all(&folder).map_err(|e| other("cannot create a build folder", e))?;
    let file_name = format!("{sketch}.ino");
    std::fs::write(folder.join(&file_name), source)
        .map_err(|e| other("cannot write the sketch", e))?;

    let (ok, log) = run(
        cli,
        &[
            "compile".as_ref(),
            "--no-color".as_ref(),
            "--fqbn".as_ref(),
            FQBN.as_ref(),
            "--output-dir".as_ref(),
            out.as_os_str(),
            folder.as_os_str(),
        ],
    )
    .map_err(|e| {
        BuildError::NoToolchain(format!("cannot run {}: {e}. {SETUP_HELP}", cli.display()))
    })?;
    let diagnostics = parse_diagnostics(&log, &file_name);
    if !ok {
        if log.contains("platform not installed") || log.contains("Unknown FQBN") {
            return Err(BuildError::NoToolchain(format!(
                "the Arduino AVR core is not installed. {SETUP_HELP}"
            )));
        }
        return Err(BuildError::Failed { diagnostics, log });
    }
    let hex_path = out.join(format!("{file_name}.hex"));
    let text = std::fs::read_to_string(&hex_path).map_err(|e| {
        other(
            &format!(
                "the build finished but {} was not produced",
                hex_path.display()
            ),
            e,
        )
    })?;
    let image = crate::hex::parse(&text).map_err(|e| BuildError::Other(e.to_string()))?;
    Ok(BuildOutput {
        image,
        warnings: diagnostics,
    })
}

// ---------------------------------------------------------------------------
// Remembering builds

fn hash_of(source: &str) -> u64 {
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    h.finish()
}

static BUILT: Mutex<Option<HashMap<u64, Vec<u8>>>> = Mutex::new(None);

/// Remember the image built from `source`.
pub fn remember(source: &str, image: Vec<u8>) {
    BUILT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(hash_of(source), image);
}

/// The image last built from exactly this `source`, if any.
pub fn cached_image(source: &str) -> Option<Vec<u8>> {
    BUILT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .get(&hash_of(source))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLINK: &str = "void setup() { pinMode(LED_BUILTIN, OUTPUT); }\n\
                         void loop() { digitalWrite(LED_BUILTIN, HIGH); delay(500); \
                         digitalWrite(LED_BUILTIN, LOW); delay(500); }\n";

    #[test]
    fn compiler_lines_become_diagnostics() {
        let out = "\
C:\\Users\\me\\AppData\\Local\\Temp\\blink\\blink.ino: In function 'void setup()':
C:\\Users\\me\\AppData\\Local\\Temp\\blink\\blink.ino:3:5: error: 'foo' was not declared in this scope
    3 |     foo();
      |     ^~~
/tmp/blink/blink.ino:7:12: warning: unused variable 'x' [-Wunused-variable]
/tmp/blink/blink.ino:9: error: no column here
/tmp/blink/blink.ino:4:1: note: declared here
C:\\core\\Arduino.h:10:1: error: not in the sketch
/tmp/blink/other_blink.ino:2:2: error: a different file
/tmp/blink/blink.ino:12:3: fatal error: Missing.h: No such file or directory
Error during build: exit status 1
";
        assert_eq!(
            parse_diagnostics(out, "blink.ino"),
            [
                Diagnostic {
                    line: 3,
                    column: 5,
                    error: true,
                    message: "'foo' was not declared in this scope".into()
                },
                Diagnostic {
                    line: 7,
                    column: 12,
                    error: false,
                    message: "unused variable 'x' [-Wunused-variable]".into()
                },
                Diagnostic {
                    line: 9,
                    column: 0,
                    error: true,
                    message: "no column here".into()
                },
                Diagnostic {
                    line: 12,
                    column: 3,
                    error: true,
                    message: "Missing.h: No such file or directory".into()
                },
            ]
        );
        assert!(parse_diagnostics("all good\n", "blink.ino").is_empty());
    }

    #[test]
    fn sketch_folders_get_names_the_tool_accepts() {
        assert_eq!(sketch_name("main"), "main");
        assert_eq!(sketch_name("my sketch-2"), "my_sketch_2");
        assert_eq!(sketch_name("2fast"), "sketch_2fast");
        assert_eq!(sketch_name(""), "sketch");
    }

    #[test]
    fn a_configured_path_wins_and_a_missing_tool_is_explained() {
        let here = std::env::current_exe().unwrap();
        assert_eq!(find_cli(Some(&here)), Some(here));
        let err = build(Path::new("no/such/arduino-cli"), "x", BLINK).unwrap_err();
        assert!(matches!(err, BuildError::NoToolchain(_)), "{err:?}");
        assert!(err.to_string().contains("winget install"), "{err}");
    }

    #[test]
    fn built_images_are_remembered_by_their_text() {
        assert_eq!(cached_image("never built"), None);
        remember("some sketch", vec![1, 2, 3]);
        assert_eq!(cached_image("some sketch"), Some(vec![1, 2, 3]));
        assert_eq!(cached_image("some sketch "), None);
    }

    // The tests below run the real tool, and do nothing on a machine
    // without it.

    #[test]
    fn a_real_sketch_builds_and_runs_on_the_emulator() {
        let Some(cli) = find_cli(None) else {
            eprintln!("arduino-cli not installed: skipped");
            return;
        };
        let built = build(&cli, "blink", BLINK).expect("the sketch builds");
        assert!(built.image.len() > 500, "{} bytes", built.image.len());
        // The compiled program toggles the LED pin (PB5): run it for one
        // second of chip time and count the changes of PORTB.
        let mut chip = crate::Atmega328p::new(&built.image).unwrap();
        let mut changes = 0;
        let mut last = chip.pin_drives();
        let end = 16_000_000u64;
        while chip.cycle() < end {
            match chip.run(end) {
                crate::Stop::Pins => {
                    let now = chip.pin_drives();
                    if now != last {
                        changes += 1;
                        last = now;
                    }
                }
                crate::Stop::Reached => break,
                other => panic!("unexpected stop {other:?}"),
            }
        }
        // delay(500) twice per loop: about two changes a second.
        assert!((1..=3).contains(&changes), "{changes} pin changes in 1 s");
    }

    #[test]
    fn a_real_compile_error_is_reported_at_its_line() {
        let Some(cli) = find_cli(None) else {
            eprintln!("arduino-cli not installed: skipped");
            return;
        };
        let source = "void setup() {}\nvoid loop() {\n  nothing_here();\n}\n";
        let Err(BuildError::Failed { diagnostics, .. }) = build(&cli, "broken", source) else {
            panic!("the sketch should fail to build");
        };
        let first = diagnostics.iter().find(|d| d.error).expect("an error");
        assert_eq!(first.line, 3, "{diagnostics:?}");
        assert!(first.message.contains("nothing_here"), "{first:?}");
    }
}

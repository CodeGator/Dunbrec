# Microcontroller firmware development in Dunbrec

*Design document, written 2026-11-21. Each phase below is meant to be picked up on its own, in
order.*

## Progress

| Step | Status |
|---|---|
| A2 emulator debug API | **Done** (2026-11-21, uncommitted until the user says so): `Atmega328p::set_breakpoints`, `Stop::Breakpoint`, `step_instruction(until)` with `Stop::Stepped`, `snapshot() -> CpuSnapshot`, public `sp`, `data`, `flash`; `Asm::label_address`. Tests in `dunbrec-mcu/src/tests.rs` (section "debugging"), including a breakpoint stopping before an instruction without losing the "instruction after SEI runs before an interrupt" rule. |
| A3 simulation integration | **Done** (2026-11-21, uncommitted until the user says so). `McuSystem`/`Transient`: `set_breakpoints`, `step_mcu`, `mcu_snapshot`, `take_mcu_halt`, `mcu_parts`; halts are recorded when the run-ahead copy is adopted, so simulated time is exactly the stop's cycle time. Worker (`live.rs`): `Command::{SetBreakpoints, Step, Snapshot}`, `Event::{Halted, McuState}`, `LiveSim::start_debug(.., DebugSetup { breakpoints, start_paused })`, `LiveSim::mcus`. A halt pauses the worker; a step is an unpaced burst until its halt. Tests: `dunbrec-sim/src/mcu_tests.rs` ("debugging") and `live.rs`. The new commands, events and `mcus` carry a temporary `#[allow(dead_code)]` until A4 consumes them. |
| A1 disassembler | **Done** (2026-11-21, uncommitted until the user says so). `dunbrec-mcu/src/isa.rs`: the whole instruction set as one table (`OPS`, `lookup`, `Form`, SREG and branch alias names, `REGISTERS` / `register_name` / `io_name` for the ATmega328P). `disasm.rs`: `disassemble_at`, `disassemble`, `Line { address, words, mnemonic, operands, target }`; AVR syntax, jump targets as byte addresses, registers by name, `.word` for data. The table is checked against the emulator for **all 65,536 words** (about 7 s in a debug build), so it cannot drift. The text assembler (phase B) is meant to read the same table. |
| A4 debugger window, A5 docs | **Done** (2026-11-21, uncommitted until the user says so). `dunbrec-app/src/debugger_ui.rs` (a floating window, Instruments menu → MCU debugger): Continue / Pause / Step / Start paused at reset / Restart paused; PC, SP, SREG flags, cycle and simulated time, registers with changed ones highlighted; disassembly listing with a **clickable breakpoint gutter** (works before the circuit runs; breakpoints are kept by designator and handed to new simulations through `DebugSetup`); memory view with changes highlighted. A breakpoint halt opens the window. Parts are matched to the simulation by designator, not index. Pure helpers and a headless draw test are in the module's tests. **Not seen on screen yet:** screenshots were blank when I tried, so the layout needs the user's eye. README updated. |
| Phase A | **Complete** apart from visual polish found when the user tries it. |
| Phases B, C, D | Not started. |

Debug API contract (A2), for whoever builds A3:

- Breakpoints are **word addresses** (the program counter's units), set with
  `set_breakpoints(&[..])`, and travel with the chip when it is cloned.
- `run` returns `Stop::Breakpoint` *before* executing the instruction at a breakpoint. Calling
  `run` again executes it (the chip is "disarmed" for exactly that instruction), then
  breakpoints apply again. An interrupt entry re-arms, so a breakpoint on a handler's first
  instruction stops on every entry.
- `step_instruction(until)` executes one instruction or one interrupt entry. `Stepped`, `Pins`
  and `AdcSample` all mean it ran (the last two mean the circuit must hear about it);
  `Breakpoint` means nothing ran.
- Stopping at a breakpoint preserves the one-instruction interrupt delay after `SEI`/`RETI`.
- **A3 changed one A2 rule:** `step_instruction` now runs the instruction at the program counter
  even if a breakpoint is set there (stepping off a breakpoint must not stall); it never
  returns `Breakpoint`. Arriving at a breakpoint by `run` still stops before it.

A3 contract, for whoever builds A4:

- `Transient` has no pause of its own; the worker pauses by not calling `step()`. A halt is
  just a flag (`take_mcu_halt`) that the worker turns into a pause and an `Event::Halted`.
- The snapshot is of the **committed** chip (what the circuit has seen), never the run-ahead
  copy. A pin change at the halt's exact time has not yet reached the analog voltage (its edge
  starts at that moment), so the circuit reads as it did an instant before.
- A step requested before an input edge arrives never runs two instructions: replaying the
  chip to the edge counts as the step (`McuSystem::input_event`). A test checks it.
- Breakpoints live in the chip, so they are lost on Restart: send them again, or pass them in
  `DebugSetup` (applied before the first step; also how to catch the very first instruction).
- `Command::Step` on a sleeping chip waits for an interrupt to wake it, up to `STEP_LIMIT`
  simulation steps, then reports an error.

## Goal

Let the user work on the firmware side of a circuit inside Dunbrec, not only run a finished
`.hex` file. Three capabilities, agreed with the user:

1. **Step and inspect:** pause, single-step, breakpoints, registers, memory, flags, a
   disassembly listing and a cycle counter.
2. **Write and build Arduino C++** from a code panel in the app.
3. **Write AVR assembly text** in the same code panel.

The editor is a **code panel inside Dunbrec** (agreed), with source saved with the schematic.
Other chips are explicitly out of scope here (see Phase D).

## What exists today (verified 2026-11-21)

- `dunbrec-mcu`: a standalone ATmega328P emulator.
  - `avr.rs`: `Avr::new(image)`, `reset`, `run(until_cycle) -> Stop`, and read access to `pc`,
    `reg(r)`, `peek(addr)` (SRAM and I/O), `sreg`, `cycle`, `fault`, `is_sleeping`. Pin and ADC
    inputs, `pin_drives`, `take_serial`.
  - `periph.rs` (GPIO, timers 0/1/2, ADC, USART TX, interrupts), `hex.rs` (Intel HEX),
    `firmware.rs` (loads `builtin:<name>` or a `.hex` path relative to the schematic; reloaded
    on every Run), `programs.rs` (three built-in demos).
  - `asm.rs`: an assembler that is **only a Rust builder API** (`a.sbi(0x04, 5); a.label("x");
    a.rjmp("x")`). It cannot read assembly text.
  - There is **no disassembler.** (Single-step and breakpoints were added in step A2; see
  Progress.)
- `dunbrec-sim/src/mcu.rs`: `McuSystem` runs the chip *ahead* on a clone until its next event
  (pin change, ADC sample, horizon). An input crossing before that point discards the clone,
  replays the committed chip to the crossing, then applies the input. Timing is cycle-exact.
  Anything that observes or stops the chip must respect this design.
- `dunbrec-app/src/live.rs`: the simulation runs on a worker thread. The UI sends `Command`s
  (`Pause`, `Resume`, `SetSpeed`, ...) and receives `Event`s with samples.
- Serial output already reaches the Serial monitor window.
- **No compiler is installed** on the development machine (no `avr-gcc`, no `arduino-cli`), so
  Phase C needs one installed first. This also unlocks the open "real Arduino firmware golden
  tests" item in `HANDOFF.md`.

## Decisions already made

- Debugger first, then assembly text and the code panel, then Arduino C++ builds.
- Source is edited in a code panel in the app and stored with the schematic.
- Instruments stay floating windows that never dock (user decision). The debugger follows that.
- The toolchain is an **external process**, never linked or bundled, so its version and size stay
  out of the repo. (Under the original MIT licence this was also a licensing reason, as it was for
  rejecting simavr. The project is GPL-3.0 now, so bundling GPL tools would be allowed; there is
  just no need.)
- Existing `.hex` and `builtin:` firmware keeps working unchanged.

## Cross-cutting design

### One instruction table

Decoder, disassembler and assembler must not drift apart. Define the AVR instruction set once
in `dunbrec-mcu` (mnemonic, operand kinds, opcode pattern, mask, size) and drive all three from
it. The emulator's existing decoder can keep its fast path, but a test must prove the table and
the decoder agree for every opcode the emulator supports.

### Debug state crosses threads as snapshots

The chip lives on the simulation worker thread. The UI never touches it. When paused or
stepping, the worker sends an `McuSnapshot` event: `pc`, `sp`, `sreg`, 32 registers, cycle,
a window of SRAM/I-O, and the last stop reason. The UI sends `Step`, `RunToBreakpoint` and
`SetBreakpoints(addresses)` commands.

### Pause and step semantics

- **Pause** is the existing whole-simulation pause. The committed chip state at the paused time
  is what the debugger shows (never a run-ahead clone).
- **Step** advances the chip by one instruction. Simulation time advances by `cycles / clock`
  so the analog circuit and the chip stay in step. The scheduler lands exactly there.
- **Breakpoints** are events like pin changes: the run-ahead stops at the breakpoint address,
  the scheduler lands there, the committed chip is adopted, and the sim pauses.
- **Reset** restarts firmware and simulation time together.

### Source in the schematic

Add `Schematic.sources: BTreeMap<String, SourceFile>` (`#[serde(default)]`, so old `.dbr`
files load) where `SourceFile { language: Asm | Cpp, text: String }`. `Mcu.firmware` gains a
`source:<name>` form beside `builtin:<name>` and file paths. On Run, a `source:` firmware is
built in memory to an image, with errors reported by line. Document the file-format addition in
`HANDOFF.md`.

## Phases

Sizes: S = under a day, M = a few days, L = a week or more of focused work.

### Phase A: step and inspect (size L; no outside dependencies)

| Step | Work | Where |
|---|---|---|
| A1 | Instruction table; disassembler (`disassemble(image) -> listing`) | `dunbrec-mcu` new `isa.rs`, `disasm.rs` |
| A2 | Emulator debug API: single-step, breakpoint set, stop reasons, full state snapshot (incl. SP, stack, SRAM, I/O) | `avr.rs` |
| A3 | Simulation integration: breakpoint events in the run-ahead, step and run-to-breakpoint through the scheduler, worker commands and snapshot events | `dunbrec-sim/src/mcu.rs`, `transient.rs`, `dunbrec-app/src/live.rs` |
| A4 | Debugger window (floating): Step / Run / Pause / Reset, registers and flags, disassembly with a PC marker and a breakpoint gutter, memory view (SRAM, I/O), cycle counter and simulated time | `dunbrec-app` new `debugger_ui.rs` |
| A5 | Docs: README, HANDOFF | |

Tests: disassembler on every instruction the emulator supports, checked by assembling with the
existing `Asm` and disassembling back. Stepping lands on exact cycle counts. A breakpoint stops
the whole simulation at the right instruction, with the analog side at the matching time. Run-
ahead and replay (input crossing before a breakpoint) still give identical results.

Done when: the built-in `blink` demo can be paused, stepped instruction by instruction with
registers updating, and stopped at a breakpoint in its timer interrupt handler.

Main risk: stepping and breakpoints interacting with the run-ahead and replay design (A3). Do
A3 with tests before any UI.

### Phase B: assembly text and the code panel (size L)

| Step | Work | Where |
|---|---|---|
| B1 | Text assembler on the instruction table: labels, `.equ`, `.def`, `.db`/`.dw`, expressions, ATmega328P register and I/O names (`DDRB`, `PORTB`, ...), errors with line and column. Atmel-style syntax (`ldi r16, 0xFF`, `sbi PORTB, 5`), which is what tutorials use | `dunbrec-mcu` new `asm_text.rs` |
| B2 | `Schematic.sources` and `source:` firmware (see above); `.dbr` round trip test | `dunbrec-core`, `dunbrec-io`, `dunbrec-sim/firmware` |
| B3 | Code panel: line numbers, basic highlighting, error markers, a source list per microcontroller; builds on Run and shows errors without stopping the app | `dunbrec-app` new `code_ui.rs` |
| B4 | Source-line mapping: the assembler emits address to line; the debugger shows source lines, and breakpoints can be set by line | assembler, `debugger_ui.rs` |

Tests: assemble known programs and compare to the builder's output byte for byte; every error
kind reports the right line; the built-in demos re-expressed as text assemble to the same image.

Done when: a blink program typed into the panel runs in the circuit, errors show on the right
line, and the debugger shows the source.

### Phase C: Arduino C++ builds (size L; needs a toolchain installed)

Use **`arduino-cli`**, not bare `avr-gcc`: sketches need the Arduino core (`Arduino.h`,
`digitalWrite`, `Serial`, ...), which the CLI installs and compiles.

| Step | Work |
|---|---|
| C1 | Toolchain discovery (PATH, the Arduino IDE's install folder, a path in settings) and a plain-English "set up the compiler" message with the exact install command (`arduino-cli core install arduino:avr`) |
| C2 | Build runner on a worker thread: write the sketch to a temp folder (`<name>/<name>.ino`), run `arduino-cli compile --fqbn arduino:avr:uno --output-dir ...`, load the `.hex` that is not `with_bootloader`, capture output |
| C3 | Parse gcc errors (`file:line:col: error: ...`) into code-panel markers; show build output in a panel |
| C4 | C++ in the code panel (language switch, highlighting) and build on Run or on demand |
| C5 | Golden tests with real compiled sketches (blink, `analogWrite`, `analogRead`, `Serial.print`, `millis`): closes the open `HANDOFF.md` item 2 |

Debugging C++: first version shows disassembly only. Source-line mapping needs the build's
`.elf` and DWARF line info (for example via the toolchain's `avr-objdump -S`); treat it as a
later improvement, not part of the first cut.

Done when: an Arduino sketch typed in the panel builds, runs in the circuit, prints on the
serial monitor, and compile errors point at the right line.

Risks: the toolchain is a large download and a moving target (version flags, output folders);
keep every external call behind one small module with its own tests using a fake CLI.

### Phase D: more chips (out of scope for now)

Prerequisite when it comes up: a chip description table (pins, memory sizes, peripherals) and
`Mcu { chip, ... }`. Each new chip is a large separate piece of work.

## Open questions (decide when the phase starts)

1. **Where the code panel lives:** a dock tab beside the schematic (like an editor in an IDE),
   or a floating window like the instruments? Recommendation: a dock tab, so it can sit next
   to the schematic. The debugger is a floating window.
2. **One source file per microcontroller or several?** Start with one per microcontroller;
   add includes only if asked.
3. **Build on every Run, or only when the source changed?** Recommendation: only when it
   changed, with a Rebuild button.
4. **Assembly dialect:** Atmel syntax (recommended, matches tutorials) versus GNU `as` syntax
   (what the C++ toolchain emits).
5. **Editor quality:** the code panel is a basic editor. If it feels limiting later, "edit in
   my own editor and rebuild on save" is a cheap addition (watch the file).

## Order and cost summary

A (debugger) comes first: no dependencies, and the largest gap. B needs A's instruction table
and benefits from the debugger. C is mostly external-process plumbing and can start once a
toolchain is installed; it only needs B's code panel and source storage. D is deferred.

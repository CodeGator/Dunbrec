//! Live simulation on a background thread.
//!
//! The UI starts a [`LiveSim`] from a netlist. The worker thread owns the
//! [`Transient`] and steps it so that simulated time keeps pace with the
//! wall clock at the chosen speed (simulated seconds per real second). It
//! sends batches of samples back about 60 times a second and accepts
//! commands: pause, resume, speed, resolution, live parameter edits, and
//! the microcontroller debugger's breakpoints and single steps.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dunbrec_core::{Element, ModelLibrary, Netlist};
use dunbrec_mcu::CpuSnapshot;
use dunbrec_sim::{Circuit, McuHalt, McuInfo, Sample, SerialByte, Transient, TransientOptions};

pub enum Command {
    Pause,
    Resume,
    /// Simulated seconds per real second.
    SetSpeed(f64),
    SetMaxStep(f64),
    /// New parameters for elements (by netlist index).
    Update(Vec<(usize, Element)>, ModelLibrary),
    /// Stop microcontroller `part` before these word addresses, replacing
    /// its previous breakpoints.
    SetBreakpoints {
        part: usize,
        words: Vec<usize>,
    },
    /// Run microcontroller `part` for exactly one instruction. The
    /// simulation then stays paused, answered by [`Event::Halted`].
    Step {
        part: usize,
    },
    /// Ask for the state of microcontroller `part` ([`Event::McuState`]).
    Snapshot {
        part: usize,
    },
}

pub enum Event {
    Samples(Vec<Sample>),
    /// Bytes sent on microcontroller serial ports.
    Serial(Vec<SerialByte>),
    /// Progress: simulated time, and how fast it is running relative to the
    /// requested speed (1.0 = keeping up).
    Status {
        time: f64,
        keeping_up: f64,
    },
    /// A live edit changed the circuit's structure; restart required.
    NeedsRestart,
    /// A microcontroller stopped at a breakpoint or finished a step. The
    /// simulation is now paused, at exactly that moment.
    Halted {
        halt: McuHalt,
        snapshot: CpuSnapshot,
    },
    /// The state of a microcontroller, as asked for.
    McuState {
        part: usize,
        snapshot: CpuSnapshot,
    },
    Error(String),
}

/// Debugger settings a simulation starts with. Applied before the first
/// step, so a breakpoint on the very first instruction is not missed.
#[derive(Debug, Clone, Default)]
pub struct DebugSetup {
    /// Breakpoints (word addresses) per microcontroller index.
    pub breakpoints: Vec<(usize, Vec<usize>)>,
    /// Start paused, at time zero, so the first instruction can be stepped.
    pub start_paused: bool,
}

/// How often the worker sends samples to the UI.
const FLUSH_INTERVAL: Duration = Duration::from_millis(15);
/// Most steps taken between checks of the clock and command queue.
const STEPS_PER_SLICE: usize = 400;
/// Most simulation steps a single microcontroller step may take. A sleeping
/// chip only finishes a step when an interrupt wakes it, which can be far
/// off; this bounds how long that can hold up the worker.
const STEP_LIMIT: usize = 2_000_000;

pub struct LiveSim {
    commands: Sender<Command>,
    events: Receiver<Event>,
    worker: Option<JoinHandle<()>>,
    /// The netlist the simulation was built from; node and element indices
    /// in samples refer to it.
    pub netlist: Netlist,
    pub models: ModelLibrary,
    pub paused: bool,
    /// The circuit's microcontrollers, in the order the debugger commands
    /// index them.
    pub mcus: Vec<McuInfo>,
}

impl LiveSim {
    /// Build the circuit and initial operating point here (so errors are
    /// reported at once), then start the worker. Relative microcontroller
    /// firmware paths are resolved against `firmware_dir`.
    #[cfg(test)]
    pub fn start(
        netlist: Netlist,
        models: ModelLibrary,
        max_step: f64,
        speed: f64,
        firmware_dir: Option<PathBuf>,
    ) -> Result<Self, String> {
        Self::start_debug(
            netlist,
            models,
            max_step,
            speed,
            firmware_dir,
            DebugSetup::default(),
        )
    }

    /// [`start`](Self::start) with debugger settings applied before the
    /// first step.
    pub fn start_debug(
        netlist: Netlist,
        models: ModelLibrary,
        max_step: f64,
        speed: f64,
        firmware_dir: Option<PathBuf>,
        debug: DebugSetup,
    ) -> Result<Self, String> {
        let mut circuit = Circuit::from_netlist(&netlist, &models).map_err(|e| e.to_string())?;
        if let Some(dir) = firmware_dir {
            circuit.set_firmware_dir(dir);
        }
        let mut sim = Transient::new(circuit, TransientOptions::free_running(max_step))
            .map_err(|e| e.to_string())?;
        for (part, words) in &debug.breakpoints {
            sim.set_breakpoints(*part, words);
        }
        let mcus = sim.mcu_parts();
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let start_paused = debug.start_paused;
        let worker = thread::Builder::new()
            .name("dunbrec-sim".into())
            .spawn(move || run(sim, speed, start_paused, command_rx, event_tx))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            commands: command_tx,
            events: event_rx,
            worker: Some(worker),
            netlist,
            models,
            paused: start_paused,
            mcus,
        })
    }

    pub fn send(&mut self, command: Command) {
        match command {
            Command::Pause | Command::Step { .. } => self.paused = true,
            Command::Resume => self.paused = false,
            _ => {}
        }
        // A closed channel means the worker stopped; its error event
        // explains why.
        let _ = self.commands.send(command);
    }

    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

impl Drop for LiveSim {
    fn drop(&mut self) {
        // Dropping the command sender tells the worker to stop.
        let (dead, _) = mpsc::channel();
        self.commands = dead;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Send the samples and serial bytes collected so far. False if the
/// receiver is gone and the worker should stop.
fn flush(events: &Sender<Event>, batch: &mut Vec<Sample>, serial: &mut Vec<SerialByte>) -> bool {
    if !batch.is_empty() && events.send(Event::Samples(std::mem::take(batch))).is_err() {
        return false;
    }
    if !serial.is_empty() && events.send(Event::Serial(std::mem::take(serial))).is_err() {
        return false;
    }
    true
}

/// Report a halt: everything collected up to it, then the chip's state.
/// False if the receiver is gone.
fn report_halt(
    sim: &Transient,
    events: &Sender<Event>,
    batch: &mut Vec<Sample>,
    serial: &mut Vec<SerialByte>,
    halt: McuHalt,
) -> bool {
    if !flush(events, batch, serial) {
        return false;
    }
    match sim.mcu_snapshot(halt.part) {
        Some(snapshot) => events.send(Event::Halted { halt, snapshot }).is_ok(),
        None => true,
    }
}

fn run(
    mut sim: Transient,
    mut speed: f64,
    start_paused: bool,
    commands: Receiver<Command>,
    events: Sender<Event>,
) {
    let mut paused = start_paused;
    let mut wall_start = Instant::now();
    let mut sim_start = sim.time();
    let mut batch: Vec<Sample> = vec![sim.sample()];
    let mut serial: Vec<SerialByte> = Vec::new();
    let mut last_flush = Instant::now();
    let mut keeping_up = 1.0;

    loop {
        // Commands.
        loop {
            match commands.try_recv() {
                Ok(Command::Pause) => paused = true,
                Ok(Command::Resume) => {
                    paused = false;
                    wall_start = Instant::now();
                    sim_start = sim.time();
                }
                Ok(Command::SetSpeed(s)) => {
                    speed = s;
                    wall_start = Instant::now();
                    sim_start = sim.time();
                }
                Ok(Command::SetMaxStep(h)) => {
                    if let Err(e) = sim.set_max_step(h) {
                        let _ = events.send(Event::Error(e.to_string()));
                    }
                }
                Ok(Command::Update(elements, models)) => {
                    for (index, element) in elements {
                        match sim.update_element(index, &element, &models) {
                            Ok(true) => {}
                            Ok(false) => {
                                let _ = events.send(Event::NeedsRestart);
                            }
                            Err(e) => {
                                let _ = events.send(Event::Error(e.to_string()));
                                return;
                            }
                        }
                    }
                }
                Ok(Command::SetBreakpoints { part, words }) => sim.set_breakpoints(part, &words),
                Ok(Command::Snapshot { part }) => {
                    if let Some(snapshot) = sim.mcu_snapshot(part) {
                        let _ = events.send(Event::McuState { part, snapshot });
                    }
                }
                Ok(Command::Step { part }) => {
                    // Not paced against the wall clock: a step takes as much
                    // simulated time as it needs, however slow the speed.
                    paused = true;
                    sim.step_mcu(part);
                    let mut halted = None;
                    for _ in 0..STEP_LIMIT {
                        match sim.step() {
                            Ok(Some(step)) => batch.push(step.sample),
                            Ok(None) => break,
                            Err(e) => {
                                serial.extend(sim.take_serial());
                                let _ = flush(&events, &mut batch, &mut serial);
                                let _ = events.send(Event::Error(e.to_string()));
                                return;
                            }
                        }
                        if let Some(halt) = sim.take_mcu_halt() {
                            halted = Some(halt);
                            break;
                        }
                    }
                    serial.extend(sim.take_serial());
                    match halted {
                        Some(halt) => {
                            if !report_halt(&sim, &events, &mut batch, &mut serial, halt) {
                                return;
                            }
                        }
                        None => {
                            let _ = flush(&events, &mut batch, &mut serial);
                            let _ = events.send(Event::Error(
                                "the microcontroller did not finish the step (is it asleep?)"
                                    .into(),
                            ));
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if paused {
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        // Advance until simulated time catches up with the clock.
        let target = sim_start + speed * wall_start.elapsed().as_secs_f64();
        if sim.time() < target {
            let mut halted = None;
            for _ in 0..STEPS_PER_SLICE {
                if sim.time() >= target {
                    break;
                }
                match sim.step() {
                    Ok(Some(step)) => {
                        batch.push(step.sample);
                        if let Some(halt) = sim.take_mcu_halt() {
                            halted = Some(halt);
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        serial.extend(sim.take_serial());
                        let _ = events.send(Event::Samples(std::mem::take(&mut batch)));
                        let _ = events.send(Event::Serial(std::mem::take(&mut serial)));
                        let _ = events.send(Event::Error(e.to_string()));
                        return;
                    }
                }
            }
            serial.extend(sim.take_serial());
            if let Some(halt) = halted {
                // A breakpoint: pause here, exactly at the stop.
                paused = true;
                if !report_halt(&sim, &events, &mut batch, &mut serial, halt) {
                    return;
                }
                continue;
            }
            // If the circuit is too heavy to keep up, slow down gracefully
            // instead of building an ever-growing backlog.
            let lag = target - sim.time();
            if lag > 0.25 * speed {
                let elapsed = wall_start.elapsed().as_secs_f64();
                keeping_up = if elapsed > 0.0 {
                    ((sim.time() - sim_start) / (speed * elapsed)).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                wall_start = Instant::now();
                sim_start = sim.time();
            } else {
                keeping_up = 1.0;
            }
        } else {
            thread::sleep(Duration::from_millis(1));
        }

        if last_flush.elapsed() >= FLUSH_INTERVAL {
            last_flush = Instant::now();
            if !batch.is_empty()
                && events
                    .send(Event::Samples(std::mem::take(&mut batch)))
                    .is_err()
            {
                return;
            }
            if !serial.is_empty()
                && events
                    .send(Event::Serial(std::mem::take(&mut serial)))
                    .is_err()
            {
                return;
            }
            let status = Event::Status {
                time: sim.time(),
                keeping_up,
            };
            if events.send(status).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::{ComponentKind as K, NetlistBuilder, Waveform};
    use dunbrec_mcu::asm::{Asm, io};
    use dunbrec_sim::HaltReason;

    fn rc() -> Netlist {
        NetlistBuilder::new()
            .element(
                "V1",
                K::VoltageSource {
                    waveform: Waveform::Dc(1.0),
                },
                &["in", "0"],
            )
            .element("R1", K::Resistor { resistance: 1e3 }, &["in", "out"])
            .element(
                "C1",
                K::Capacitor {
                    capacitance: 1e-6,
                    initial_voltage: None,
                },
                &["out", "0"],
            )
            .build()
    }

    /// Collect samples until simulated time passes `t` (or give up).
    fn wait_for(live: &LiveSim, t: f64) -> Vec<Sample> {
        let mut all = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            for e in live.poll() {
                match e {
                    Event::Samples(s) => all.extend(s),
                    Event::Error(e) => panic!("{e}"),
                    _ => {}
                }
            }
            if all.last().is_some_and(|s| s.time >= t) {
                return all;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("simulation did not reach t = {t}");
    }

    #[test]
    fn runs_paced_and_accepts_live_edits() {
        // 0.1 s of simulated time per real second.
        let netlist = rc();
        let mut live =
            LiveSim::start(netlist.clone(), ModelLibrary::new(), 1e-5, 0.1, None).unwrap();
        let samples = wait_for(&live, 5e-3);
        assert!(samples.windows(2).all(|w| w[1].time > w[0].time));
        let out = netlist.node_by_name("out").unwrap().0;
        assert!((samples.last().unwrap().voltages[out] - 1.0).abs() < 1e-6);

        // Raise the source to 3 V while running.
        let mut v1 = netlist.elements[0].clone();
        v1.kind = K::VoltageSource {
            waveform: Waveform::Dc(3.0),
        };
        live.send(Command::Update(vec![(0, v1)], ModelLibrary::new()));
        let samples = wait_for(&live, 20e-3);
        let last = samples.last().unwrap();
        assert!(
            (last.voltages[out] - 3.0).abs() < 0.05,
            "{}",
            last.voltages[out]
        );
    }

    #[test]
    fn pause_stops_time_and_errors_are_reported_up_front() {
        let mut live = LiveSim::start(rc(), ModelLibrary::new(), 1e-5, 0.1, None).unwrap();
        wait_for(&live, 1e-3);
        live.send(Command::Pause);
        thread::sleep(Duration::from_millis(50));
        let before: Vec<Event> = live.poll();
        let t_paused = before
            .iter()
            .filter_map(|e| match e {
                Event::Samples(s) => s.last().map(|s| s.time),
                _ => None,
            })
            .fold(0.0, f64::max);
        thread::sleep(Duration::from_millis(100));
        let after_max = live
            .poll()
            .iter()
            .filter_map(|e| match e {
                Event::Samples(s) => s.last().map(|s| s.time),
                _ => None,
            })
            .fold(0.0, f64::max);
        assert!(after_max <= t_paused, "time advanced while paused");

        let bad = NetlistBuilder::new()
            .element("R1", K::Resistor { resistance: 0.0 }, &["a", "0"])
            .build();
        assert!(LiveSim::start(bad, ModelLibrary::new(), 1e-5, 1.0, None).is_err());
    }

    #[test]
    fn microcontroller_serial_output_arrives() {
        let pins: Vec<&str> = dunbrec_mcu::ARDUINO_NAMES.to_vec();
        let netlist = NetlistBuilder::new()
            .element(
                "V1",
                K::VoltageSource {
                    waveform: Waveform::Dc(2.5),
                },
                &["A0", "0"],
            )
            .element(
                "U1",
                K::Mcu {
                    firmware: "builtin:pot-pwm-serial".into(),
                    clock: 16e6,
                },
                &pins,
            )
            .build();
        let live = LiveSim::start(netlist, ModelLibrary::new(), 1e-4, 1.0, None).unwrap();
        let mut text = String::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        // Wait for the banner and one complete reading line.
        while Instant::now() < deadline && text.matches('\n').count() < 2 {
            for e in live.poll() {
                match e {
                    Event::Serial(bytes) => {
                        assert!(bytes.iter().all(|b| b.element == 1));
                        text.extend(bytes.iter().map(|b| b.byte as char));
                    }
                    Event::Error(e) => panic!("{e}"),
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(text.starts_with("Dunbrec AVR demo"), "{text:?}");
        assert!(text.contains("A0 = 05"), "{text:?}");
    }

    /// A microcontroller on its own, running `program` from a temporary
    /// .hex file. Returns the netlist and the folder holding the file.
    fn mcu_circuit(program: &Asm, tag: &str) -> (Netlist, PathBuf) {
        let dir = std::env::temp_dir().join(format!("dunbrec-live-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = program.finish().unwrap();
        std::fs::write(dir.join("fw.hex"), dunbrec_mcu::hex::write(&image)).unwrap();
        let pins: Vec<&str> = dunbrec_mcu::ARDUINO_NAMES.to_vec();
        let netlist = NetlistBuilder::new()
            .element(
                "U1",
                K::Mcu {
                    firmware: "fw.hex".into(),
                    clock: 16e6,
                },
                &pins,
            )
            .build();
        (netlist, dir)
    }

    /// Wait for the next `Halted` event.
    fn wait_halt(live: &LiveSim) -> (McuHalt, CpuSnapshot) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            for e in live.poll() {
                match e {
                    Event::Halted { halt, snapshot } => return (halt, snapshot),
                    Event::Error(e) => panic!("{e}"),
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("no halt");
    }

    #[test]
    fn breakpoints_pause_the_worker_and_steps_report_exact_state() {
        // D13 high for four cycles of an eight-cycle loop.
        let mut a = Asm::new();
        a.sbi(io::DDRB, 5);
        a.label("loop");
        a.sbi(io::PORTB, 5).nop().nop();
        a.label("late");
        a.cbi(io::PORTB, 5).rjmp("loop");
        let late = a.label_address("late").unwrap();
        let (netlist, dir) = mcu_circuit(&a, "debug");

        // Slow (1e-3 simulated seconds per real second), so the simulation
        // cannot race ahead of the commands this test sends.
        let mut live = LiveSim::start_debug(
            netlist,
            ModelLibrary::new(),
            1e-6,
            1e-3,
            Some(dir),
            DebugSetup {
                breakpoints: vec![(0, vec![late])],
                start_paused: false,
            },
        )
        .unwrap();
        assert_eq!(live.mcus.len(), 1);
        assert_eq!(live.mcus[0].designator, "U1");

        // The breakpoint pauses the simulation before its instruction:
        // SBI DDRB (2) + SBI PORTB (2) + two NOPs = cycle 6.
        let (halt, snap) = wait_halt(&live);
        assert_eq!(halt.reason, HaltReason::Breakpoint);
        assert_eq!((snap.pc, snap.cycle), (late, 6));
        // It stays paused: no further halt arrives on its own.
        thread::sleep(Duration::from_millis(150));
        assert!(
            live.poll()
                .iter()
                .all(|e| !matches!(e, Event::Halted { .. }))
        );

        // One step runs CBI (2 cycles) and halts again.
        live.send(Command::Step { part: 0 });
        assert!(live.paused);
        let (halt, snap) = wait_halt(&live);
        assert_eq!(halt.reason, HaltReason::Stepped);
        assert_eq!((snap.pc, snap.cycle), (late + 1, 8));

        // The state can be asked for without moving.
        live.send(Command::Snapshot { part: 0 });
        let deadline = Instant::now() + Duration::from_secs(5);
        let state = 'wait: loop {
            for e in live.poll() {
                if let Event::McuState { part, snapshot } = e {
                    break 'wait (part, snapshot);
                }
            }
            assert!(Instant::now() < deadline, "no state");
            thread::sleep(Duration::from_millis(5));
        };
        assert_eq!((state.0, state.1.cycle), (0, 8));

        // Resuming runs on to the next pass through the breakpoint.
        live.send(Command::Resume);
        let (halt, snap) = wait_halt(&live);
        assert_eq!(halt.reason, HaltReason::Breakpoint);
        assert_eq!((snap.pc, snap.cycle), (late, 14));
    }

    #[test]
    fn a_simulation_can_start_paused_and_step_from_the_first_instruction() {
        let mut a = Asm::new();
        a.ldi(16, 7).ldi(17, 9).label("end").rjmp("end");
        let (netlist, dir) = mcu_circuit(&a, "startpaused");
        let mut live = LiveSim::start_debug(
            netlist,
            ModelLibrary::new(),
            1e-6,
            1.0,
            Some(dir),
            DebugSetup {
                breakpoints: Vec::new(),
                start_paused: true,
            },
        )
        .unwrap();
        assert!(live.paused);
        // Nothing runs on its own.
        thread::sleep(Duration::from_millis(100));
        assert!(
            live.poll()
                .iter()
                .all(|e| !matches!(e, Event::Halted { .. }))
        );
        for (n, regs) in [(1u64, (7u8, 0u8)), (2, (7, 9))] {
            live.send(Command::Step { part: 0 });
            let (halt, snap) = wait_halt(&live);
            assert_eq!(halt.reason, HaltReason::Stepped);
            assert_eq!((snap.pc as u64, snap.cycle), (n, n));
            assert_eq!((snap.regs[16], snap.regs[17]), regs);
        }
    }
}

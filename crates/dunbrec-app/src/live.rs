//! Live simulation on a background thread.
//!
//! The UI starts a [`LiveSim`] from a netlist. The worker thread owns the
//! [`Transient`] and steps it so that simulated time keeps pace with the
//! wall clock at the chosen speed (simulated seconds per real second). It
//! sends batches of samples back about 60 times a second and accepts
//! commands: pause, resume, speed, resolution, and live parameter edits.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dunbrec_core::{Element, ModelLibrary, Netlist};
use dunbrec_sim::{Circuit, Sample, SerialByte, Transient, TransientOptions};

pub enum Command {
    Pause,
    Resume,
    /// Simulated seconds per real second.
    SetSpeed(f64),
    SetMaxStep(f64),
    /// New parameters for elements (by netlist index).
    Update(Vec<(usize, Element)>, ModelLibrary),
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
    Error(String),
}

/// How often the worker sends samples to the UI.
const FLUSH_INTERVAL: Duration = Duration::from_millis(15);
/// Most steps taken between checks of the clock and command queue.
const STEPS_PER_SLICE: usize = 400;

pub struct LiveSim {
    commands: Sender<Command>,
    events: Receiver<Event>,
    worker: Option<JoinHandle<()>>,
    /// The netlist the simulation was built from; node and element indices
    /// in samples refer to it.
    pub netlist: Netlist,
    pub models: ModelLibrary,
    pub paused: bool,
}

impl LiveSim {
    /// Build the circuit and initial operating point here (so errors are
    /// reported at once), then start the worker. Relative microcontroller
    /// firmware paths are resolved against `firmware_dir`.
    pub fn start(
        netlist: Netlist,
        models: ModelLibrary,
        max_step: f64,
        speed: f64,
        firmware_dir: Option<PathBuf>,
    ) -> Result<Self, String> {
        let mut circuit = Circuit::from_netlist(&netlist, &models).map_err(|e| e.to_string())?;
        if let Some(dir) = firmware_dir {
            circuit.set_firmware_dir(dir);
        }
        let sim = Transient::new(circuit, TransientOptions::free_running(max_step))
            .map_err(|e| e.to_string())?;
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("dunbrec-sim".into())
            .spawn(move || run(sim, speed, command_rx, event_tx))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            commands: command_tx,
            events: event_rx,
            worker: Some(worker),
            netlist,
            models,
            paused: false,
        })
    }

    pub fn send(&mut self, command: Command) {
        match command {
            Command::Pause => self.paused = true,
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

fn run(mut sim: Transient, mut speed: f64, commands: Receiver<Command>, events: Sender<Event>) {
    let mut paused = false;
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
            for _ in 0..STEPS_PER_SLICE {
                if sim.time() >= target {
                    break;
                }
                match sim.step() {
                    Ok(Some(step)) => batch.push(step.sample),
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
}

//! Kass's side of the dictation handshake, against a real responder, to
//! measure the round trip (docs/DICTATION_HANDSHAKE.md). Run the responder,
//! then this:
//!
//!   swift scripts/handshake-responder.swift &
//!   cargo run --example handshake_roundtrip -- [rounds]
//!
//! It waits for the responder to register, announces `rounds` takes, times
//! each `dictationReady` against Kass's timeout, and ends each take.

#[path = "../src/dictation_handshake/wire.rs"]
mod wire;

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use wire::Value;

/// Kass's own timeout (`dictation_handshake::READY_TIMEOUT`).
const READY_TIMEOUT: Duration = Duration::from_millis(150);

static EVENTS: OnceLock<Mutex<Sender<(String, i64)>>> = OnceLock::new();

fn on_notification(name: &str, pid: Option<i64>) {
    if let (Some(pid), Some(events)) = (pid, EVENTS.get()) {
        let _ = events.lock().unwrap().send((name.to_string(), pid));
    }
}

fn main() {
    let rounds: usize = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(20);
    let (tx, rx) = mpsc::channel();
    EVENTS.set(Mutex::new(tx)).unwrap();
    // Replies arrive on the main run loop, as in Kass; takes run elsewhere,
    // like Kass's hotkey dispatcher.
    wire::listen(&[wire::READY, wire::SUPPORTED], on_notification);
    std::thread::spawn(move || {
        run(rounds, rx);
        std::process::exit(0);
    });
    unsafe { core_foundation_sys::runloop::CFRunLoopRun() };
}

fn run(rounds: usize, rx: mpsc::Receiver<(String, i64)>) {
    println!("waiting for a responder to register…");
    let pid = loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok((name, pid)) if name == wire::SUPPORTED => break pid,
            Ok(_) => continue,
            Err(_) => {
                eprintln!("no handshakeSupported within 5s; is the responder running?");
                std::process::exit(1);
            }
        }
    };
    println!("pid {pid} registered");

    let mut latencies = Vec::new();
    let mut timeouts = 0;
    for round in 0..rounds {
        // Drop anything left over (repeated registrations).
        while rx.try_recv().is_ok() {}
        let mode = if round % 2 == 0 { "dictate" } else { "command" };
        let asked = Instant::now();
        wire::post(
            wire::WILL_BEGIN,
            &[("pid", Value::Int(pid)), ("mode", Value::Text(mode))],
        );
        let ready = loop {
            let left = READY_TIMEOUT.saturating_sub(asked.elapsed());
            match rx.recv_timeout(left) {
                Ok((name, from)) if name == wire::READY && from == pid => break true,
                Ok(_) => continue,
                Err(_) => break false,
            }
        };
        if ready {
            latencies.push(asked.elapsed());
        } else {
            timeouts += 1;
            // Let the late reply land before the next round, so it isn't
            // taken for that round's.
            std::thread::sleep(Duration::from_millis(500));
        }
        let outcome = ["inserted", "cancelled", "failed"][round % 3];
        wire::post(
            wire::DID_END,
            &[("pid", Value::Int(pid)), ("outcome", Value::Text(outcome))],
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    latencies.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "{rounds} rounds: {} ready, {timeouts} timed out",
        latencies.len()
    );
    if !latencies.is_empty() {
        println!(
            "willBegin → ready: min {:.2}ms, median {:.2}ms, max {:.2}ms",
            ms(latencies[0]),
            ms(latencies[latencies.len() / 2]),
            ms(*latencies.last().unwrap())
        );
    }
}

//! Real-app benchmark of every insertion method, for manual runs only: it
//! opens a TextEdit document, brings it to the front and types into it.
//!
//! ```text
//! cargo test --release insert_bench -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The terminal running it needs Accessibility permission. For each method
//! it reports how long the call took and how long until the text could be
//! read back from the field, then what each declining step costs, which is
//! the price of falling through to the next one.

use std::process::Command;
use std::time::{Duration, Instant};

use crate::insert_chain::{Attempt, Inserter, Request};

const RUNS: usize = 5;
const SHORT: &str = "Can we move the design review to Thursday afternoon instead";
const LONG: &str = "I looked at the numbers again this morning and I think we should hold \
the launch by a week. The onboarding flow still drops about a third of new users at the \
permissions step, and the fix is small but needs a proper review. If we ship the fix on \
Monday we can watch it for three days before the announcement goes out. That gives support \
time to update the help articles and gives us a clean baseline for the retention report. \
Let me know if that works for you and I will tell the rest of the team this afternoon.";
const MULTILINE: &str = "Thanks for the notes.\nI will send the revised draft tonight.\nTalk soon";

/// Open a blank plain-text document in TextEdit (no Automation permission
/// needed) and return TextEdit's pid.
fn textedit_pid() -> i32 {
    let doc = std::env::temp_dir().join("kass-insert-bench.txt");
    std::fs::write(&doc, "").expect("bench document");
    Command::new("open")
        .args(["-a", "TextEdit"])
        .arg(&doc)
        .status()
        .expect("open TextEdit");
    std::thread::sleep(Duration::from_secs(2));
    let out = Command::new("pgrep")
        .args(["-x", "TextEdit"])
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
        .expect("TextEdit pid")
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Median and max of `samples`, in ms.
fn stats(samples: &mut [Duration]) -> String {
    samples.sort();
    format!(
        "median {:>7.2} ms  max {:>7.2} ms",
        ms(samples[samples.len() / 2]),
        ms(*samples.last().unwrap())
    )
}

/// Time `step` inserting `text`: until the call returns, and until the text
/// reads back from the field.
fn measure(step: &dyn Inserter, pid: i32, text: &str) -> Option<(Duration, Duration)> {
    assert!(crate::text_insert::clear_focused(pid), "could not clear");
    std::thread::sleep(Duration::from_millis(150));
    let req = Request {
        pid,
        bundle_id: Some("com.apple.TextEdit"),
        role: Some("AXTextArea"),
        text,
    };
    let started = Instant::now();
    let attempt = step.attempt(&req);
    let returned = started.elapsed();
    if !matches!(attempt, Attempt::Inserted { .. }) {
        println!("    {:?}: {attempt:?}", step.method());
        return None;
    }
    let deadline = started + Duration::from_secs(3);
    loop {
        let value = crate::text_insert::focused_value(pid).unwrap_or_default();
        if value.replace('\r', "\n") == text {
            return Some((returned, started.elapsed()));
        }
        if Instant::now() > deadline {
            println!(
                "    {:?}: text never matched; field holds {:?}",
                step.method(),
                value.chars().take(80).collect::<String>()
            );
            return None;
        }
        std::thread::sleep(Duration::from_micros(500));
    }
}

fn report(name: &str, step: &dyn Inserter, pid: i32, text: &str) {
    let mut calls = Vec::new();
    let mut landed = Vec::new();
    for _ in 0..RUNS {
        match measure(step, pid, text) {
            Some((c, l)) => {
                calls.push(c);
                landed.push(l);
            }
            None => return,
        }
    }
    println!("  {name:<28} call {}", stats(&mut calls));
    println!("  {:<28} text {}", "", stats(&mut landed));
}

fn decline_cost(name: &str, step: &dyn Inserter, req: &Request) {
    let mut samples = Vec::new();
    for _ in 0..RUNS {
        let started = Instant::now();
        let attempt = step.attempt(req);
        samples.push(started.elapsed());
        assert!(
            matches!(attempt, Attempt::Declined(_)),
            "{name}: expected a decline, got {attempt:?}"
        );
    }
    println!("  {name:<40} {}", stats(&mut samples));
}

#[test]
#[ignore = "drives TextEdit; run by hand"]
fn insert_bench() {
    assert!(
        crate::accessibility::is_trusted(),
        "grant this terminal Accessibility"
    );
    let pid = textedit_pid();
    let ax = crate::text_insert::Accessibility;
    // The real step, minus the input-source check (see `assume_ascii`).
    let keys = crate::keystroke_insert::assume_ascii::KeystrokesAssumingAscii;
    let paste = crate::clipboard::Paste::new(None);

    for (label, text) in [("short", SHORT), ("long", LONG), ("multi-line", MULTILINE)] {
        println!(
            "\n{label} text ({} chars), {RUNS} runs:",
            text.chars().count()
        );
        report("Accessibility", &ax, pid, text);
        report("Keystrokes", &keys, pid, text);
        report("Clipboard + ⌘V", &paste, pid, text);
        // Let the last background clipboard restore run.
        std::thread::sleep(crate::clipboard::PASTE_CONSUME + Duration::from_millis(100));
    }

    println!("\nCost of a step that declines (the price of falling through):");
    let finder = Command::new("pgrep")
        .args(["-x", "Finder"])
        .output()
        .unwrap();
    let finder: i32 = String::from_utf8_lossy(&finder.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or(1);
    decline_cost(
        "Accessibility, no text field (Finder)",
        &ax,
        &Request {
            pid: finder,
            bundle_id: Some("com.apple.finder"),
            role: None,
            text: SHORT,
        },
    );
    decline_cost(
        "Accessibility, terminal",
        &ax,
        &Request {
            pid,
            bundle_id: Some("com.apple.Terminal"),
            role: None,
            text: SHORT,
        },
    );
    decline_cost(
        "Keystrokes, trigger character",
        &keys,
        &Request {
            pid,
            bundle_id: Some("com.apple.TextEdit"),
            role: None,
            text: "ping @sam about it",
        },
    );
    decline_cost(
        "Keystrokes, target not in front",
        &keys,
        &Request {
            pid: finder,
            bundle_id: Some("com.apple.finder"),
            role: None,
            text: SHORT,
        },
    );
}

/// What reading the text around the caret adds to each dictation, and that
/// it reads the real field (docs/plans/MID_SENTENCE_DICTATION.md).
#[test]
#[ignore = "drives TextEdit; run by hand"]
fn caret_context_bench() {
    assert!(
        crate::accessibility::is_trusted(),
        "grant this terminal Accessibility"
    );
    let pid = textedit_pid();
    assert!(crate::text_insert::clear_focused(pid), "could not clear");
    let ax = crate::text_insert::Accessibility;
    let setup = ax.attempt(&Request {
        pid,
        bundle_id: Some("com.apple.TextEdit"),
        role: Some("AXTextArea"),
        text: "I think we should",
    });
    assert!(matches!(setup, Attempt::Inserted { .. }), "{setup:?}");

    let mut samples = Vec::new();
    for _ in 0..RUNS {
        let started = Instant::now();
        let fitted =
            crate::text_insert::fit_to_focused(pid, Some("com.apple.TextEdit"), "move it.");
        samples.push(started.elapsed());
        assert_eq!(fitted, " move it.");
    }
    println!("\nReading the caret context: {}", stats(&mut samples));
}

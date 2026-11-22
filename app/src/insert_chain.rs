//! The ordered fallback chain that delivers dictated text into another app.
//!
//! Each [`Inserter`] is one way of getting text into the focused field:
//! Accessibility write, typed keystrokes, and clipboard + ⌘V. [`deliver`]
//! tries them in order and stops at the first one that inserted the text.
//!
//! The one rule every step must keep: fall through only when **nothing was
//! inserted**. A step that sent something it cannot confirm reports
//! [`Attempt::Uncertain`], and the chain stops there rather than risk the
//! text landing twice (it stays in Captures either way).
//!
//! Every step is timed, so the log shows how long each method took and what
//! each fallback cost.

use std::time::{Duration, Instant};

/// One way of inserting text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Accessibility,
    Keystrokes,
    Clipboard,
}

/// The result of one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// The text is in the field. `verified` is false when the step has no
    /// way to read the field back (keystrokes, a paste).
    Inserted { verified: bool },
    /// Nothing was inserted; the next method may try. The reason is logged.
    Declined(String),
    /// Something may have been inserted. The chain stops without trying
    /// anything else.
    Uncertain(String),
}

/// What a step needs to know about the dictation and its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request<'a> {
    pub pid: i32,
    pub bundle_id: Option<&'a str>,
    /// AX role of the element focused at key-down, when known.
    pub role: Option<&'a str>,
    pub text: &'a str,
}

pub trait Inserter {
    fn method(&self) -> Method;
    /// Try to insert `req.text`. Blocking.
    fn attempt(&self, req: &Request) -> Attempt;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    pub method: Method,
    pub attempt: Attempt,
    pub elapsed: Duration,
}

/// Everything that happened while delivering one dictation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub steps: Vec<StepReport>,
}

/// How the delivery ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Inserted {
        method: Method,
        verified: bool,
    },
    Uncertain {
        method: Method,
        message: String,
    },
    /// Every method declined.
    Exhausted,
}

impl Report {
    pub fn delivery(&self) -> Delivery {
        match self.steps.last() {
            Some(StepReport {
                method,
                attempt: Attempt::Inserted { verified },
                ..
            }) => Delivery::Inserted {
                method: *method,
                verified: *verified,
            },
            Some(StepReport {
                method,
                attempt: Attempt::Uncertain(message),
                ..
            }) => Delivery::Uncertain {
                method: *method,
                message: message.clone(),
            },
            _ => Delivery::Exhausted,
        }
    }

    pub fn total(&self) -> Duration {
        self.steps.iter().map(|s| s.elapsed).sum()
    }

    /// Time spent on methods that declined before the one that ended it.
    pub fn fallback_cost(&self) -> Duration {
        let n = self.steps.len().saturating_sub(1);
        self.steps[..n].iter().map(|s| s.elapsed).sum()
    }

    /// One log line: each step with its time, then the total.
    pub fn summary(&self) -> String {
        let steps: Vec<String> = self
            .steps
            .iter()
            .map(|s| {
                let what = match &s.attempt {
                    Attempt::Inserted { verified: true } => "inserted".to_string(),
                    Attempt::Inserted { verified: false } => "sent".to_string(),
                    Attempt::Declined(why) => format!("declined ({why})"),
                    Attempt::Uncertain(why) => format!("uncertain ({why})"),
                };
                format!("{:?} {what} {:.1} ms", s.method, ms(s.elapsed))
            })
            .collect();
        format!(
            "{} | total {:.1} ms, fallback {:.1} ms",
            steps.join(" → "),
            ms(self.total()),
            ms(self.fallback_cost())
        )
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Wraps a step that acts on the frontmost app (keystrokes, ⌘V) so the target is brought to the front first. `bring_front`
/// must be cheap when the target is already in front: it runs before every
/// wrapped step. If it fails, the step declines without running.
pub struct InFront<'a, F: Fn() -> Result<(), String>> {
    pub inner: &'a dyn Inserter,
    pub bring_front: &'a F,
}

impl<F: Fn() -> Result<(), String>> Inserter for InFront<'_, F> {
    fn method(&self) -> Method {
        self.inner.method()
    }

    fn attempt(&self, req: &Request) -> Attempt {
        match (self.bring_front)() {
            Ok(()) => self.inner.attempt(req),
            Err(e) => Attempt::Declined(format!("could not bring the target to the front: {e}")),
        }
    }
}

/// Try each inserter in order until one inserts the text or is uncertain.
pub fn deliver(chain: &[&dyn Inserter], req: &Request) -> Report {
    deliver_with_clock(chain, req, Instant::now)
}

/// [`deliver`] with an injectable clock, so tests can assert timings.
pub fn deliver_with_clock(
    chain: &[&dyn Inserter],
    req: &Request,
    mut now: impl FnMut() -> Instant,
) -> Report {
    let mut report = Report::default();
    for step in chain {
        let started = now();
        let attempt = step.attempt(req);
        let elapsed = now().saturating_duration_since(started);
        let stop = !matches!(attempt, Attempt::Declined(_));
        report.steps.push(StepReport {
            method: step.method(),
            attempt,
            elapsed,
        });
        if stop {
            break;
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// A scripted inserter that records being called and advances a fake
    /// clock by its cost.
    struct Fake<'c> {
        method: Method,
        result: Attempt,
        cost: Duration,
        clock: &'c Cell<Duration>,
        calls: RefCell<Vec<String>>,
    }

    impl<'c> Fake<'c> {
        fn new(method: Method, result: Attempt, cost_ms: u64, clock: &'c Cell<Duration>) -> Self {
            Self {
                method,
                result,
                cost: Duration::from_millis(cost_ms),
                clock,
                calls: RefCell::default(),
            }
        }
        fn called(&self) -> usize {
            self.calls.borrow().len()
        }
    }

    impl Inserter for Fake<'_> {
        fn method(&self) -> Method {
            self.method
        }
        fn attempt(&self, req: &Request) -> Attempt {
            self.calls.borrow_mut().push(req.text.to_string());
            self.clock.set(self.clock.get() + self.cost);
            self.result.clone()
        }
    }

    fn declined() -> Attempt {
        Attempt::Declined("no".into())
    }
    fn inserted() -> Attempt {
        Attempt::Inserted { verified: true }
    }
    fn uncertain() -> Attempt {
        Attempt::Uncertain("maybe".into())
    }

    const REQ: Request = Request {
        pid: 42,
        bundle_id: Some("com.example"),
        role: None,
        text: "hello",
    };

    fn run(chain: &[&dyn Inserter], clock: &Cell<Duration>) -> Report {
        let base = Instant::now();
        deliver_with_clock(chain, &REQ, || base + clock.get())
    }

    const ORDER: [Method; 3] = [Method::Accessibility, Method::Keystrokes, Method::Clipboard];

    /// Every combination of per-step results over the three-step chain:
    /// the chain stops at the first non-declined step, calls nothing after
    /// it, and reports that step.
    #[test]
    fn every_combination_stops_at_the_first_step_that_did_not_decline() {
        let options = [
            declined(),
            inserted(),
            uncertain(),
            Attempt::Inserted { verified: false },
        ];
        let mut cases = 0;
        for a in &options {
            for b in &options {
                for c in &options {
                    let clock = Cell::new(Duration::ZERO);
                    let results = [a, b, c];
                    let fakes: Vec<Fake> = ORDER
                        .iter()
                        .zip(results)
                        .enumerate()
                        .map(|(i, (m, r))| Fake::new(*m, r.clone(), 1 + i as u64, &clock))
                        .collect();
                    let chain: Vec<&dyn Inserter> =
                        fakes.iter().map(|f| f as &dyn Inserter).collect();
                    let report = run(&chain, &clock);

                    let stop = results
                        .iter()
                        .position(|r| !matches!(r, Attempt::Declined(_)));
                    let expected_len = stop.map_or(3, |i| i + 1);
                    assert_eq!(report.steps.len(), expected_len);
                    for (i, fake) in fakes.iter().enumerate() {
                        assert_eq!(fake.called(), usize::from(i < expected_len));
                    }
                    let delivery = report.delivery();
                    match stop.map(|i| (i, results[i])) {
                        None => assert_eq!(delivery, Delivery::Exhausted),
                        Some((i, Attempt::Inserted { verified })) => assert_eq!(
                            delivery,
                            Delivery::Inserted {
                                method: ORDER[i],
                                verified: *verified
                            }
                        ),
                        Some((i, Attempt::Uncertain(m))) => assert_eq!(
                            delivery,
                            Delivery::Uncertain {
                                method: ORDER[i],
                                message: m.clone()
                            }
                        ),
                        Some((_, Attempt::Declined(_))) => unreachable!(),
                    }
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 64);
    }

    #[test]
    fn falls_all_the_way_through_to_the_clipboard() {
        let clock = Cell::new(Duration::ZERO);
        let ax = Fake::new(Method::Accessibility, declined(), 3, &clock);
        let keys = Fake::new(Method::Keystrokes, declined(), 1, &clock);
        let paste = Fake::new(
            Method::Clipboard,
            Attempt::Inserted { verified: false },
            5,
            &clock,
        );
        let report = run(&[&ax, &keys, &paste], &clock);

        assert_eq!(
            report.delivery(),
            Delivery::Inserted {
                method: Method::Clipboard,
                verified: false
            }
        );
        assert_eq!(report.total(), Duration::from_millis(9));
        assert_eq!(report.fallback_cost(), Duration::from_millis(4));
        let methods: Vec<Method> = report.steps.iter().map(|s| s.method).collect();
        assert_eq!(methods, ORDER);
    }

    #[test]
    fn uncertain_never_falls_through() {
        let clock = Cell::new(Duration::ZERO);
        let ax = Fake::new(Method::Accessibility, uncertain(), 1, &clock);
        let paste = Fake::new(Method::Clipboard, inserted(), 1, &clock);
        let report = run(&[&ax, &paste], &clock);
        assert_eq!(paste.called(), 0);
        assert!(matches!(
            report.delivery(),
            Delivery::Uncertain {
                method: Method::Accessibility,
                ..
            }
        ));
    }

    #[test]
    fn first_success_costs_no_fallback() {
        let clock = Cell::new(Duration::ZERO);
        let ax = Fake::new(Method::Accessibility, inserted(), 4, &clock);
        let paste = Fake::new(Method::Clipboard, inserted(), 1, &clock);
        let report = run(&[&ax, &paste], &clock);
        assert_eq!(report.fallback_cost(), Duration::ZERO);
        assert_eq!(report.total(), Duration::from_millis(4));
    }

    #[test]
    fn in_front_brings_the_target_forward_before_the_step() {
        let clock = Cell::new(Duration::ZERO);
        let keys = Fake::new(Method::Keystrokes, inserted(), 1, &clock);
        let fronted = Cell::new(0);
        let bring = || {
            fronted.set(fronted.get() + 1);
            Ok(())
        };
        let wrapped = InFront {
            inner: &keys,
            bring_front: &bring,
        };
        let report = run(&[&wrapped], &clock);
        assert_eq!(fronted.get(), 1);
        assert_eq!(keys.called(), 1);
        assert_eq!(report.steps[0].method, Method::Keystrokes);
    }

    #[test]
    fn in_front_declines_without_running_when_activation_fails() {
        let clock = Cell::new(Duration::ZERO);
        let keys = Fake::new(Method::Keystrokes, inserted(), 1, &clock);
        let paste = Fake::new(Method::Clipboard, inserted(), 1, &clock);
        let bring = || Err("app quit".to_string());
        let keys_front = InFront {
            inner: &keys,
            bring_front: &bring,
        };
        let paste_front = InFront {
            inner: &paste,
            bring_front: &bring,
        };
        let report = run(&[&keys_front, &paste_front], &clock);
        assert_eq!(keys.called(), 0);
        assert_eq!(paste.called(), 0);
        assert_eq!(report.delivery(), Delivery::Exhausted);
    }

    #[test]
    fn empty_chain_is_exhausted() {
        let clock = Cell::new(Duration::ZERO);
        assert_eq!(run(&[], &clock).delivery(), Delivery::Exhausted);
    }

    #[test]
    fn every_step_sees_the_same_text() {
        let clock = Cell::new(Duration::ZERO);
        let fakes: Vec<Fake> = ORDER
            .iter()
            .map(|m| Fake::new(*m, declined(), 0, &clock))
            .collect();
        let chain: Vec<&dyn Inserter> = fakes.iter().map(|f| f as &dyn Inserter).collect();
        run(&chain, &clock);
        for f in &fakes {
            assert_eq!(*f.calls.borrow(), vec!["hello".to_string()]);
        }
    }

    #[test]
    fn summary_names_each_step_and_the_fallback_cost() {
        let clock = Cell::new(Duration::ZERO);
        let ax = Fake::new(
            Method::Accessibility,
            Attempt::Declined("not a text role".into()),
            2,
            &clock,
        );
        let keys = Fake::new(
            Method::Keystrokes,
            Attempt::Inserted { verified: false },
            7,
            &clock,
        );
        let line = run(&[&ax, &keys], &clock).summary();
        assert_eq!(
            line,
            "Accessibility declined (not a text role) 2.0 ms → Keystrokes sent 7.0 ms | total 9.0 ms, fallback 2.0 ms"
        );
    }
}

//! Opt-in request tracing. Each request partitions its elapsed handler time into
//! exclusive source-code phases. Thread-local buffered files avoid a shared lock.
//! Trace writing happens after the measured root interval and is profiling cost.
#[cfg(feature = "code-profile")]
mod enabled {
    use std::{
        cell::RefCell,
        fs::{File, OpenOptions},
        io::{BufWriter, Write},
        path::PathBuf,
        time::Instant,
    };

    struct Frame {
        name: &'static str,
        started: Instant,
        offset: u64,
        children: u64,
    }
    struct Trace {
        output: Option<BufWriter<File>>,
        stack: Vec<Frame>,
        phases: Vec<(&'static str, u64, u64)>,
        intervals: Vec<(&'static str, u64, u64)>,
        start_ns: u64,
    }
    impl Trace {
        fn new() -> Self {
            let output = std::env::var_os("SL7_CODE_PROFILE_DIR").map(|dir| {
                // SAFETY: gettid has no pointer arguments and cannot fail.
                let tid = unsafe { libc::gettid() };
                let path = PathBuf::from(dir).join(format!("{}-{tid}.tsv", std::process::id()));
                BufWriter::with_capacity(
                    256 * 1024,
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)
                        .expect("open code profile"),
                )
            });
            Self {
                output,
                stack: Vec::with_capacity(12),
                phases: Vec::with_capacity(32),
                intervals: Vec::with_capacity(16),
                start_ns: 0,
            }
        }
    }
    impl Drop for Trace {
        fn drop(&mut self) {
            if let Some(out) = &mut self.output {
                out.flush().expect("flush code profile");
            }
        }
    }
    thread_local! { static TRACE: RefCell<Trace> = RefCell::new(Trace::new()); }

    pub(super) struct Span {
        active: bool,
    }
    pub(super) fn span(name: &'static str, root: bool) -> Span {
        let active = TRACE.with(|trace| {
            let mut trace = trace.borrow_mut();
            if trace.output.is_none() || (!root && trace.stack.is_empty()) {
                return false;
            }
            if root {
                assert!(trace.stack.is_empty(), "nested profile root");
                trace.phases.clear();
                trace.intervals.clear();
                let mut ts = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                // SAFETY: ts is writable and CLOCK_MONOTONIC is supported on Linux.
                assert_eq!(
                    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) },
                    0
                );
                trace.start_ns = ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64;
            }
            let started = Instant::now();
            let offset = trace.stack.first().map_or(0, |root| {
                started.duration_since(root.started).as_nanos() as u64
            });
            trace.stack.push(Frame {
                name,
                started,
                offset,
                children: 0,
            });
            true
        });
        Span { active }
    }
    impl Drop for Span {
        fn drop(&mut self) {
            if !self.active {
                return;
            }
            TRACE.with(|trace| {
                let mut trace = trace.borrow_mut();
                let frame = trace.stack.pop().expect("profile stack");
                let elapsed = frame.started.elapsed().as_nanos() as u64;
                let exclusive = elapsed
                    .checked_sub(frame.children)
                    .expect("profile nesting");
                if let Some((_, value, count)) = trace
                    .phases
                    .iter_mut()
                    .find(|(name, _, _)| *name == frame.name)
                {
                    *value += exclusive;
                    *count += 1;
                } else {
                    trace.phases.push((frame.name, exclusive, 1));
                }
                if matches!(
                    frame.name,
                    "state_lock_wait"
                        | "cleanup_state_lock_wait"
                        | "state_locked_work"
                        | "cleanup_locked_work"
                        | "native_payload"
                        | "reconstruct"
                        | "reply"
                        | "inode_gate_wait"
                ) {
                    trace.intervals.push((frame.name, frame.offset, elapsed));
                }
                if let Some(parent) = trace.stack.last_mut() {
                    parent.children += elapsed;
                } else {
                    let Trace {
                        output,
                        phases,
                        intervals,
                        start_ns,
                        ..
                    } = &mut *trace;
                    let out = output.as_mut().unwrap();
                    write!(out, "{}\t{}\t{}", frame.name, start_ns, elapsed)
                        .expect("write code profile");
                    for (name, value, count) in phases {
                        write!(out, "\t{name}:{value}:{count}").expect("write code profile");
                    }
                    for (name, offset, elapsed) in intervals {
                        write!(out, "\t@{name}:{offset}:{elapsed}").expect("write code profile");
                    }
                    writeln!(out).expect("write code profile");
                }
            });
        }
    }
}

pub(super) struct Span {
    #[cfg(feature = "code-profile")]
    _inner: enabled::Span,
}

#[inline(always)]
pub(super) fn span(_name: &'static str) -> Span {
    Span {
        #[cfg(feature = "code-profile")]
        _inner: enabled::span(_name, false),
    }
}
#[inline(always)]
pub(super) fn request(_name: &'static str) -> Span {
    Span {
        #[cfg(feature = "code-profile")]
        _inner: enabled::span(_name, true),
    }
}
// A no-op Drop allows identical explicit scope endpoints in normal builds.
impl Drop for Span {
    #[inline(always)]
    fn drop(&mut self) {}
}

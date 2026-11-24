//! The other processor cores (threads). Limine starts them (the MP request) and hands each
//! one over here; they wait nearly asleep (MONITOR/MWAIT on the job counter, interrupts off)
//! until there's work. `run(n, f)` calls `f(0)` .. `f(n - 1)` spread over every core, the
//! caller's included, and returns when all of them are done.
//!
//! Only self-contained work goes to the other cores: decoding and scaling pictures and the
//! like. Drivers, disks, the screen and input stay on the first core, which also gets every
//! interrupt.
//!
//! A processor without MONITOR/MWAIT can't be woken without interrupts, so its other cores
//! are put to sleep for good (halted with interrupts off) and `run` uses the first core only.

use alloc::vec::Vec;
use core::arch::x86_64::__cpuid;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use limine::mp::Cpu;
use limine::response::MpResponse;
use spin::Mutex;

/// Cores taking work (not counting the first).
static WORKERS: AtomicUsize = AtomicUsize::new(0);
static STARTED: AtomicUsize = AtomicUsize::new(0);
static MWAIT: AtomicBool = AtomicBool::new(false);

/// Bumped for each new job: what the waiting cores watch.
static SEQ: AtomicU64 = AtomicU64::new(0);
/// The job: open while its parts are being handed out.
static OPEN: AtomicBool = AtomicBool::new(false);
/// Cores inside a job (they may be holding its function).
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static NEXT: AtomicUsize = AtomicUsize::new(0);
static COUNT: AtomicUsize = AtomicUsize::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
static JOB: Mutex<Option<Job>> = Mutex::new(None);
/// How many parts of the last job the calling core did itself (for the benchmark).
static LAST_OWN: AtomicUsize = AtomicUsize::new(0);
/// One job at a time (a job that starts another runs it on its own core).
static RUNNING: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
struct Job(*const (dyn Fn(usize) + Sync));
// The function is Sync, and `run` doesn't return while any core can still reach it.
unsafe impl Send for Job {}

/// How many cores take part in `run` (the first one included).
pub fn threads() -> usize {
    WORKERS.load(SeqCst) + 1
}

/// Take over the cores Limine started. Returns how many will take work.
pub fn init(mp: &MpResponse) -> usize {
    let mwait = __cpuid(1).ecx & (1 << 3) != 0;
    MWAIT.store(mwait, SeqCst);
    let bsp = mp.bsp_lapic_id();
    let others: Vec<&&Cpu> = mp.cpus().iter().filter(|c| c.lapic_id != bsp).collect();
    for c in &others {
        c.goto_address.write(if mwait { ap_main } else { ap_sleep });
    }
    if others.is_empty() {
        kprintln!("smp: one core");
        return 0;
    }
    if !mwait {
        kprintln!("smp: {} more cores, put to sleep (no MONITOR/MWAIT to wake them with)", others.len());
        return 0;
    }
    // Wait (up to a second) for them to say they're ready.
    let ticks = &crate::interrupts::TICKS;
    let start = ticks.load(SeqCst);
    while STARTED.load(SeqCst) < others.len() && ticks.load(SeqCst) < start + crate::interrupts::HZ {
        core::hint::spin_loop();
    }
    let n = STARTED.load(SeqCst);
    WORKERS.store(n, SeqCst);
    kprintln!("smp: {} of {} more cores ready for work", n, others.len());
    n
}

/// Put every other core to sleep for good (boot option "onecore").
pub fn park(mp: &MpResponse) {
    let bsp = mp.bsp_lapic_id();
    for c in mp.cpus().iter().filter(|c| c.lapic_id != bsp) {
        c.goto_address.write(ap_sleep);
    }
    kprintln!("smp: {} more cores, put to sleep (boot option onecore)", mp.cpus().len().saturating_sub(1));
}

/// A core with nothing to do, for good.
unsafe extern "C" fn ap_sleep(_cpu: &Cpu) -> ! {
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
    }
}

/// A worker core: set up, then wait for jobs and help with each one.
unsafe extern "C" fn ap_main(_cpu: &Cpu) -> ! {
    crate::interrupts::init_ap();
    // Note the job counter before saying "ready": the first job can only come after that, so
    // it can't be missed.
    let mut seen = SEQ.load(SeqCst);
    STARTED.fetch_add(1, SeqCst);
    loop {
        while SEQ.load(SeqCst) == seen {
            unsafe {
                // Sleep until the job counter's cache line is written.
                core::arch::asm!("monitor", in("rax") &SEQ as *const AtomicU64 as usize, in("ecx") 0, in("edx") 0, options(nostack));
                if SEQ.load(SeqCst) != seen {
                    break;
                }
                core::arch::asm!("mwait", in("eax") 0, in("ecx") 0, options(nostack));
            }
        }
        seen = SEQ.load(SeqCst);
        help();
    }
}

/// Take parts of the open job until there are none left. Returns how many this core did.
fn help() -> usize {
    let mut did = 0;
    ACTIVE.fetch_add(1, SeqCst);
    if OPEN.load(SeqCst) {
        // Copy the job out: a lock held in the `if let` would last through the whole job and
        // keep the other cores out.
        let job = *JOB.lock();
        if let Some(job) = job {
            let f = unsafe { &*job.0 };
            let n = COUNT.load(SeqCst);
            loop {
                let i = NEXT.fetch_add(1, SeqCst);
                if i >= n {
                    break;
                }
                f(i);
                did += 1;
                DONE.fetch_add(1, SeqCst);
            }
        }
    }
    ACTIVE.fetch_sub(1, SeqCst);
    did
}

/// The same work (arithmetic only, no memory) on one core and then on all of them, for the
/// log at start: a few milliseconds on a desktop processor.
pub fn bench() {
    if threads() < 2 {
        return;
    }
    let parts = threads().max(2) * 2;
    let work = |seed: usize| {
        let mut x = seed as u64 | 1;
        for _ in 0..2_000_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
        }
        core::hint::black_box(x);
    };
    let ticks = &crate::interrupts::TICKS;
    let t0 = ticks.load(SeqCst);
    (0..parts).for_each(work);
    let t1 = ticks.load(SeqCst);
    run(parts, &work);
    let t2 = ticks.load(SeqCst);
    let ms = |t: u64| t * 1000 / crate::interrupts::HZ;
    kprintln!(
        "smp: benchmark: {} parts took {} ms on one core, {} ms on {} cores (the first core did {} of them)",
        parts, ms(t1 - t0), ms(t2 - t1), threads(), LAST_OWN.load(SeqCst)
    );
}

/// Call `f(0)` .. `f(n - 1)`, spread over every core; returns when all are done.
pub fn run(n: usize, f: &(dyn Fn(usize) + Sync)) {
    let lock = if n > 1 && WORKERS.load(SeqCst) > 0 { RUNNING.try_lock() } else { None };
    let Some(_running) = lock else {
        (0..n).for_each(f);
        LAST_OWN.store(n, SeqCst);
        return;
    };
    // The workers only use `f` while `run` waits for them below.
    let f: &'static (dyn Fn(usize) + Sync) = unsafe { core::mem::transmute::<&(dyn Fn(usize) + Sync), &'static (dyn Fn(usize) + Sync)>(f) };
    *JOB.lock() = Some(Job(f as *const _));
    COUNT.store(n, SeqCst);
    NEXT.store(0, SeqCst);
    DONE.store(0, SeqCst);
    OPEN.store(true, SeqCst);
    SEQ.fetch_add(1, SeqCst);
    LAST_OWN.store(help(), SeqCst);
    while DONE.load(SeqCst) < n {
        core::hint::spin_loop();
    }
    // Close the job, then wait until no core can still be holding `f`.
    OPEN.store(false, SeqCst);
    while ACTIVE.load(SeqCst) != 0 {
        core::hint::spin_loop();
    }
    *JOB.lock() = None;
}

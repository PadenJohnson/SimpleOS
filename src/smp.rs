//! The other processor cores (threads). Limine starts them (the MP request) and hands each
//! one over here; they wait nearly asleep (MONITOR/MWAIT on the bell, interrupts off) until
//! there's work. Two kinds of work:
//!
//! - `run(n, f)` calls `f(0)` .. `f(n - 1)` spread over every free core, the caller's
//!   included, and returns when all of them are done. Several can run at once (the desktop's
//!   and a background task's), so one never waits for another.
//! - `spawn(task)` gives a long job a core of its own: decoding a video while it plays, so the
//!   first core stays free for the mouse, the sound and the screen. A task may use `run`, and
//!   waits for more work with `sleep_on` instead of spinning.
//!
//! Only self-contained work goes to the other cores: decoding and scaling pictures and the
//! like. Drivers, disks, the screen and input stay on the first core, which also gets every
//! interrupt.
//!
//! A processor without MONITOR/MWAIT can't be woken without interrupts, so its other cores
//! are put to sleep for good (halted with interrupts off) and everything runs on the first.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
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

/// Bumped for each new job or task: what the waiting cores watch.
static BELL: AtomicU64 = AtomicU64::new(0);
/// How many parts of the last job the calling core did itself (for the benchmark).
static LAST_OWN: AtomicUsize = AtomicUsize::new(0);

/// Background tasks waiting for a core, and how many cores are running one.
pub type Task = Box<dyn FnOnce() + Send>;
static TASKS: Mutex<VecDeque<Task>> = Mutex::new(VecDeque::new());
static IN_TASKS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct Job(*const (dyn Fn(usize) + Sync));
// The function is Sync, and `run` doesn't return while any core can still reach it.
unsafe impl Send for Job {}

/// One `run` in progress. Several can be (a background task's and the desktop's).
struct Slot {
    /// Taken by a `run` (from start to finish).
    claimed: AtomicBool,
    /// Open while its parts are being handed out.
    open: AtomicBool,
    job: Mutex<Option<Job>>,
    count: AtomicUsize,
    next: AtomicUsize,
    done: AtomicUsize,
    /// Cores inside the job (they may be holding its function).
    active: AtomicUsize,
}

impl Slot {
    const fn new() -> Slot {
        Slot {
            claimed: AtomicBool::new(false),
            open: AtomicBool::new(false),
            job: Mutex::new(None),
            count: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
        }
    }
}

const SLOTS: usize = 4;
static SLOT: [Slot; SLOTS] = [const { Slot::new() }; SLOTS];

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
        c.goto_address.write(if mwait { ap_entry } else { ap_sleep });
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

/// Where a worker core starts: SSE on and the stack lined up (as for the first core in
/// main.rs), then `ap_main`.
#[unsafe(naked)]
unsafe extern "C" fn ap_entry(_cpu: &Cpu) -> ! {
    core::arch::naked_asm!(
        "mov rax, cr0",
        "and rax, -13",
        "or rax, 2",
        "mov cr0, rax",
        "mov rax, cr4",
        "or rax, 0x600",
        "mov cr4, rax",
        "and rsp, -16",
        "call {main}",
        "ud2",
        main = sym ap_main,
    )
}

/// A worker core: set up, then wait for jobs and tasks and do them.
extern "C" fn ap_main(_cpu: &Cpu) -> ! {
    crate::cpu::enable_simd();
    crate::interrupts::init_ap();
    STARTED.fetch_add(1, SeqCst);
    loop {
        // Note the bell before looking for work: anything added after the look rings it,
        // so the sleep below can't miss it.
        let seen = BELL.load(SeqCst);
        let mut worked = false;
        for s in SLOT.iter() {
            if s.open.load(SeqCst) {
                worked |= help(s) > 0;
            }
        }
        let task = TASKS.lock().pop_front();
        if let Some(task) = task {
            IN_TASKS.fetch_add(1, SeqCst);
            task();
            IN_TASKS.fetch_sub(1, SeqCst);
            worked = true;
        }
        if !worked {
            sleep_on(&BELL, seen);
        }
    }
}

/// On a worker core: sleep until `word` is no longer `seen` (or a little longer than needed:
/// callers check again and may sleep again). Spins briefly without MONITOR/MWAIT.
pub fn sleep_on(word: &AtomicU64, seen: u64) {
    if !MWAIT.load(SeqCst) {
        for _ in 0..64 {
            core::hint::spin_loop();
        }
        return;
    }
    unsafe {
        // Sleep until the word's cache line is written.
        core::arch::asm!("monitor", in("rax") word as *const AtomicU64 as usize, in("ecx") 0, in("edx") 0, options(nostack));
        if word.load(SeqCst) != seen {
            return;
        }
        core::arch::asm!("mwait", in("eax") 0, in("ecx") 0, options(nostack));
    }
}

/// A core is free for a background task (`spawn` would take it).
pub fn can_spawn() -> bool {
    let workers = WORKERS.load(SeqCst);
    workers > 0 && IN_TASKS.load(SeqCst) + TASKS.lock().len() < workers
}

/// Run `task` on a core of its own (it may take as long as it likes). Gives it back if no
/// core is free for it (one core, or all of them running tasks already): the caller then
/// does the work itself.
pub fn spawn(task: Task) -> Result<(), Task> {
    let workers = WORKERS.load(SeqCst);
    let mut q = TASKS.lock();
    if workers == 0 || IN_TASKS.load(SeqCst) + q.len() >= workers {
        return Err(task);
    }
    q.push_back(task);
    drop(q);
    BELL.fetch_add(1, SeqCst);
    Ok(())
}

/// Take parts of a job until there are none left. Returns how many this core did.
fn help(s: &Slot) -> usize {
    let mut did = 0;
    s.active.fetch_add(1, SeqCst);
    if s.open.load(SeqCst) {
        // Copy the job out: a lock held in the `if let` would last through the whole job and
        // keep the other cores out.
        let job = *s.job.lock();
        if let Some(job) = job {
            let f = unsafe { &*job.0 };
            let n = s.count.load(SeqCst);
            loop {
                let i = s.next.fetch_add(1, SeqCst);
                if i >= n {
                    break;
                }
                f(i);
                did += 1;
                s.done.fetch_add(1, SeqCst);
            }
        }
    }
    s.active.fetch_sub(1, SeqCst);
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

/// Call `f(0)` .. `f(n - 1)`, spread over every free core; returns when all are done.
pub fn run(n: usize, f: &(dyn Fn(usize) + Sync)) {
    let slot = if n > 1 && WORKERS.load(SeqCst) > 0 { SLOT.iter().find(|s| s.claimed.compare_exchange(false, true, SeqCst, SeqCst).is_ok()) } else { None };
    let Some(s) = slot else {
        (0..n).for_each(f);
        LAST_OWN.store(n, SeqCst);
        return;
    };
    // The workers only use `f` while `run` waits for them below.
    let f: &'static (dyn Fn(usize) + Sync) = unsafe { core::mem::transmute::<&(dyn Fn(usize) + Sync), &'static (dyn Fn(usize) + Sync)>(f) };
    *s.job.lock() = Some(Job(f as *const _));
    s.count.store(n, SeqCst);
    s.next.store(0, SeqCst);
    s.done.store(0, SeqCst);
    s.open.store(true, SeqCst);
    BELL.fetch_add(1, SeqCst);
    LAST_OWN.store(help(s), SeqCst);
    while s.done.load(SeqCst) < n {
        core::hint::spin_loop();
    }
    // Close the job, then wait until no core can still be holding `f`.
    s.open.store(false, SeqCst);
    while s.active.load(SeqCst) != 0 {
        core::hint::spin_loop();
    }
    *s.job.lock() = None;
    s.claimed.store(false, SeqCst);
}

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::Result;
use crate::budget::{self, Class};

static LIVE: AtomicUsize = AtomicUsize::new(0);

pub fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Stop {
    stopped: AtomicBool,
    wakers: Mutex<Vec<(usize, Waker)>>,
    next: AtomicUsize,
}

#[derive(Clone, Default)]
pub struct Cancel(Arc<Stop>);

// wakes a waiter on its own condition when the work stops
pub struct Wake {
    cancel: Cancel,
    id: usize,
}

impl Drop for Wake {
    fn drop(&mut self) {
        locked(&self.cancel.0.wakers).retain(|(id, _)| *id != self.id);
    }
}

impl Cancel {
    pub fn stop(&self) {
        if self.0.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        let wakers: Vec<Waker> = locked(&self.0.wakers)
            .iter()
            .map(|(_, wake)| Arc::clone(wake))
            .collect();
        for wake in wakers {
            wake();
        }
    }

    pub fn stopped(&self) -> bool {
        self.0.stopped.load(Ordering::SeqCst)
    }

    pub fn same(&self, other: &Cancel) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn check(&self) -> Result<()> {
        match self.stopped() {
            true => Err("stopped".into()),
            false => Ok(()),
        }
    }

    // true when the time came before the work stopped
    pub fn pause(&self, until: Instant) -> bool {
        let gate = Arc::new((Mutex::new(()), Condvar::new()));
        let waker = Arc::clone(&gate);
        let _wake = self.wake(move || {
            let _held = locked(&waker.0);
            waker.1.notify_all();
        });
        let mut held = locked(&gate.0);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if self.stopped() {
                return false;
            }
            if left.is_zero() {
                return true;
            }
            held = gate
                .1
                .wait_timeout(held, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    // the waker locks before it notifies, so no stop is missed
    pub fn wake(&self, waker: impl Fn() + Send + Sync + 'static) -> Wake {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        locked(&self.0.wakers).push((id, Arc::new(waker)));
        Wake {
            cancel: self.clone(),
            id,
        }
    }
}

// follows the job that uses the work now, not the one that made it
#[derive(Clone, Default)]
pub struct Follow(Arc<Mutex<Cancel>>);

impl Follow {
    pub fn new(cancel: &Cancel) -> Self {
        Self(Arc::new(Mutex::new(cancel.clone())))
    }

    pub fn set(&self, cancel: &Cancel) {
        *locked(&self.0) = cancel.clone();
    }

    pub fn check(&self) -> Result<()> {
        locked(&self.0).check()
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub fn spawn<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> JoinHandle<T> {
    struct Live;
    impl Drop for Live {
        fn drop(&mut self) {
            LIVE.fetch_sub(1, Ordering::Relaxed);
        }
    }
    let class = budget::current();
    LIVE.fetch_add(1, Ordering::Relaxed);
    let thread = std::thread::Builder::new().name(name.into());
    let started = thread.spawn(move || {
        let _live = Live;
        budget::enter(class);
        work()
    });
    started.expect("a thread could not start")
}

type Job = (Cancel, Box<dyn FnOnce(&Cancel) + Send>);

struct Mail {
    job: Option<Job>,
    closed: bool,
}

pub struct Slot {
    class: Class,
    mail: Arc<(Mutex<Mail>, Condvar)>,
    cancel: Cancel,
    thread: Option<JoinHandle<()>>,
}

impl Slot {
    pub fn new(class: Class) -> Self {
        let mail = Mail {
            job: None,
            closed: false,
        };
        Self {
            class,
            mail: Arc::new((Mutex::new(mail), Condvar::new())),
            cancel: Cancel::default(),
            thread: None,
        }
    }

    pub fn submit(&mut self, job: impl FnOnce(&Cancel) + Send + 'static) {
        self.cancel.stop();
        self.cancel = Cancel::default();
        locked(&self.mail.0).job = Some((self.cancel.clone(), Box::new(job)));
        self.mail.1.notify_one();
        if self.thread.is_none() {
            let (mail, class) = (Arc::clone(&self.mail), self.class);
            self.thread = Some(spawn("slot", move || {
                budget::enter(class);
                loop {
                    let mut held = locked(&mail.0);
                    let (cancel, job) = loop {
                        if held.closed {
                            return;
                        }
                        match held.job.take() {
                            Some(job) => break job,
                            None => held = mail.1.wait(held).unwrap_or_else(|p| p.into_inner()),
                        }
                    };
                    drop(held);
                    let _ = catch_unwind(AssertUnwindSafe(|| job(&cancel)));
                }
            }));
        }
    }

    pub fn cancel(&mut self) {
        self.cancel.stop();
        locked(&self.mail.0).job = None;
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.cancel.stop();
        {
            let mut held = locked(&self.mail.0);
            (held.job, held.closed) = (None, true);
        }
        self.mail.1.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Task {
    key: String,
    cancel: Cancel,
    run: Box<dyn FnOnce(&Cancel) + Send>,
}

#[derive(Default)]
struct Tasks {
    queued: VecDeque<Task>,
    running: Vec<(String, Cancel)>,
    closed: bool,
}

pub struct Pool {
    tasks: Arc<(Mutex<Tasks>, Condvar)>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    pub fn new(class: Class) -> Self {
        let tasks = Arc::<(Mutex<Tasks>, Condvar)>::default();
        let count = budget::threads(class);
        let threads: Vec<_> = (0..count)
            .map(|_| {
                let tasks = Arc::clone(&tasks);
                spawn("pool", move || {
                    budget::enter(class);
                    let mut held = locked(&tasks.0);
                    while !held.closed {
                        let Some(task) = held.queued.pop_front() else {
                            held = tasks.1.wait(held).unwrap_or_else(|p| p.into_inner());
                            continue;
                        };
                        held.running.push((task.key, task.cancel.clone()));
                        drop(held);
                        let _ = catch_unwind(AssertUnwindSafe(|| (task.run)(&task.cancel)));
                        held = locked(&tasks.0);
                        held.running.retain(|running| !running.1.same(&task.cancel));
                    }
                })
            })
            .collect();
        Self { tasks, threads }
    }

    pub fn submit(&self, key: &str, run: impl FnOnce(&Cancel) + Send + 'static) {
        let mut held = locked(&self.tasks.0);
        let task = Task {
            key: key.to_owned(),
            cancel: Cancel::default(),
            run: Box::new(run),
        };
        let waiting = held.queued.iter().position(|queued| queued.key == key);
        match waiting {
            Some(index) => held.queued[index] = task,
            None if held.closed => return,
            None => held.queued.push_back(task),
        }
        drop(held);
        self.tasks.1.notify_one();
    }

    pub fn cancel(&self, key: &str) {
        let mut held = locked(&self.tasks.0);
        held.queued.retain(|queued| queued.key != key);
        for (_, cancel) in held.running.iter().filter(|running| running.0 == key) {
            cancel.stop();
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        {
            let mut held = locked(&self.tasks.0);
            held.closed = true;
            held.queued.clear();
            for (_, cancel) in &held.running {
                cancel.stop();
            }
        }
        self.tasks.1.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

struct Line<T> {
    items: VecDeque<T>,
    room: usize,
    closed: bool,
    reader: bool,
}

struct Lane<T> {
    line: Mutex<Line<T>>,
    readable: Condvar,
    writable: Condvar,
}

pub struct Sender<T>(Arc<Lane<T>>);
pub struct Receiver<T>(Arc<Lane<T>>);

// a bounded queue that ends when the work stops or the senders are gone
pub fn channel<T>(room: usize) -> (Sender<T>, Receiver<T>) {
    let line = Mutex::new(Line {
        items: VecDeque::new(),
        room,
        closed: false,
        reader: true,
    });
    let lane = Arc::new(Lane {
        line,
        readable: Condvar::new(),
        writable: Condvar::new(),
    });
    (Sender(Arc::clone(&lane)), Receiver(lane))
}

impl<T> Sender<T> {
    // gives the item back when the reader has left
    pub fn send(&self, item: T) -> std::result::Result<(), T> {
        let mut line = locked(&self.0.line);
        loop {
            if !line.reader {
                return Err(item);
            }
            if line.items.len() < line.room {
                line.items.push_back(item);
                self.0.readable.notify_one();
                return Ok(());
            }
            line = self
                .0
                .writable
                .wait(line)
                .unwrap_or_else(|p| p.into_inner());
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        locked(&self.0.line).closed = true;
        self.0.readable.notify_all();
    }
}

impl<T: Send + 'static> Receiver<T> {
    pub fn narrow(&self, room: usize) {
        let mut line = locked(&self.0.line);
        line.room = line.room.min(room);
    }

    pub fn try_recv(&self) -> Option<T> {
        let item = locked(&self.0.line).items.pop_front();
        self.0.writable.notify_one();
        item
    }

    // none when drained and the senders are gone
    pub fn recv(&self, cancel: &Cancel) -> Result<Option<T>> {
        Ok(self.recv_until(cancel, None)?.flatten())
    }

    pub fn recv_until(&self, cancel: &Cancel, until: Option<Instant>) -> Result<Option<Option<T>>> {
        let lane = Arc::clone(&self.0);
        let _wake = cancel.wake(move || {
            let _line = locked(&lane.line);
            lane.readable.notify_all();
        });
        let mut line = locked(&self.0.line);
        loop {
            if let Some(item) = line.items.pop_front() {
                self.0.writable.notify_one();
                return Ok(Some(Some(item)));
            }
            if line.closed {
                return Ok(Some(None));
            }
            cancel.check()?;
            line = match until.map(|until| until.saturating_duration_since(Instant::now())) {
                Some(left) if left.is_zero() => return Ok(None),
                Some(left) => {
                    let (line, _) = self
                        .0
                        .readable
                        .wait_timeout(line, left)
                        .unwrap_or_else(|p| p.into_inner());
                    line
                }
                None => self
                    .0
                    .readable
                    .wait(line)
                    .unwrap_or_else(|p| p.into_inner()),
            };
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let items = {
            let mut line = locked(&self.0.line);
            line.reader = false;
            std::mem::take(&mut line.items)
        };
        self.0.writable.notify_all();
        drop(items);
    }
}

static PARTIAL: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

// a file that is not finished, quit removes it
pub struct Partial(PathBuf);

pub fn partial(path: &Path) -> Partial {
    locked(&PARTIAL).push(path.to_owned());
    Partial(path.to_owned())
}

impl Drop for Partial {
    fn drop(&mut self) {
        let mut files = locked(&PARTIAL);
        if let Some(index) = files.iter().position(|file| *file == self.0) {
            files.swap_remove(index);
        }
    }
}

// skips the exit handlers, they tear down gpu libraries under running threads
pub fn quit(code: i32) -> ! {
    for file in locked(&PARTIAL).iter() {
        let _ = std::fs::remove_file(file);
    }
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn _exit(code: i32) -> !;
        }
        unsafe { _exit(code) }
    }
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetCurrentProcess() -> isize;
            fn TerminateProcess(process: isize, code: u32) -> i32;
        }
        unsafe { TerminateProcess(GetCurrentProcess(), code as u32) };
        unreachable!()
    }
}

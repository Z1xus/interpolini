use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::Result;
use crate::budget::{self, Class};

static LIVE: AtomicUsize = AtomicUsize::new(0);

pub fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
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
        // the jobs are short, so a few workers are enough
        let count = budget::threads(class).min(4);
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
                        let same =
                            |running: &(String, Cancel)| Arc::ptr_eq(&running.1.0, &task.cancel.0);
                        held.running.retain(|running| !same(running));
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

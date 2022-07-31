use std::cell::RefCell;
use std::future::Future;
use std::mem::ManuallyDrop;
use std::pin::Pin;
use std::task::{RawWaker, RawWakerVTable, Waker};

trait SharedWake {
    fn wake(&self, task_id: usize);
}

struct SharedWaker<W> {
    refs: RefCell<usize>,
    wake: *const W,
    task_id: usize,
}

impl<W> SharedWaker<W>
where
    W: SharedWake,
{
    // SAFETY: caller must ensure the wake field remains valid for as long as
    // the waker (and its clones) may be used
    unsafe fn as_std(&self) -> ManuallyDrop<Waker> {
        let rw = RawWaker::new(
            self as *const Self as *const (),
            &RawWakerVTable::new(Self::clone, Self::wake, Self::wake_by_ref, Self::drop),
        );

        ManuallyDrop::new(Waker::from_raw(rw))
    }

    unsafe fn clone(data: *const ()) -> RawWaker {
        let s = (data as *const Self).as_ref().unwrap();

        *s.refs.borrow_mut() += 1;

        RawWaker::new(
            data,
            &RawWakerVTable::new(Self::clone, Self::wake, Self::wake_by_ref, Self::drop),
        )
    }

    unsafe fn wake(data: *const ()) {
        Self::wake_by_ref(data);

        Self::drop(data);
    }

    unsafe fn wake_by_ref(data: *const ()) {
        let s = (data as *const Self).as_ref().unwrap();

        let wake = s.wake.as_ref().unwrap();

        wake.wake(s.task_id);
    }

    unsafe fn drop(data: *const ()) {
        let s = (data as *const Self).as_ref().unwrap();

        let refs = &mut *s.refs.borrow_mut();

        assert!(*refs > 1);

        *refs -= 1;
    }
}

pub type BoxFuture = Pin<Box<dyn Future<Output = ()>>>;

mod arg {
    use super::{SharedWake, SharedWaker};
    use crate::list;
    use slab::Slab;
    use std::cell::RefCell;
    use std::future::Future;
    use std::io;
    use std::mem::MaybeUninit;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct Task<W> {
        waker: SharedWaker<W>,
        awake: bool,
    }

    struct TasksData<F, W> {
        nodes: Slab<list::Node<Task<W>>>,
        next: list::List,
        futs: Vec<MaybeUninit<F>>,
    }

    struct Tasks<F> {
        data: RefCell<TasksData<F, Self>>,
    }

    impl<F> Tasks<F>
    where
        F: Future<Output = ()>,
    {
        fn new(tasks_max: usize) -> Self {
            let mut data = TasksData {
                nodes: Slab::with_capacity(tasks_max),
                next: list::List::default(),
                futs: Vec::with_capacity(tasks_max),
            };

            unsafe { data.futs.set_len(tasks_max) };

            Self {
                data: RefCell::new(data),
            }
        }

        fn is_empty(&self) -> bool {
            self.data.borrow().nodes.is_empty()
        }

        fn add<S>(&self, get_fut_fn: S) -> Result<(), ()>
        where
            S: FnOnce(&mut MaybeUninit<F>),
        {
            let data = &mut *self.data.borrow_mut();

            if data.nodes.len() == data.nodes.capacity() {
                return Err(());
            }

            let entry = data.nodes.vacant_entry();
            let key = entry.key();

            let waker = SharedWaker {
                refs: RefCell::new(1),
                wake: self,
                task_id: key,
            };

            let task = Task { waker, awake: true };

            entry.insert(list::Node::new(task));

            data.next.push_back(&mut data.nodes, key);

            get_fut_fn(&mut data.futs[key]);

            Ok(())
        }

        fn wake(&self, task_id: usize) {
            let data = &mut *self.data.borrow_mut();

            let task = &mut data.nodes[task_id].value;

            if !task.awake {
                task.awake = true;

                data.next.remove(&mut data.nodes, task_id);
                data.next.push_back(&mut data.nodes, task_id);
            }
        }

        fn process_next(&self) {
            loop {
                let (nkey, task_ptr, fut_ptr) = {
                    let tasks = &mut *self.data.borrow_mut();

                    let nkey = match tasks.next.head {
                        Some(nkey) => nkey,
                        None => break,
                    };

                    tasks.next.remove(&mut tasks.nodes, nkey);

                    let task = &mut tasks.nodes[nkey].value;

                    task.awake = false;

                    let fut = unsafe { tasks.futs[nkey].assume_init_mut() };

                    (nkey, task as *mut Task<Self>, fut as *mut F)
                };

                // SAFETY: task won't move/drop while this pointer is in use.
                // we don't allow inserting into the slab beyond its capacity,
                // therefore its items never move. and the only place we remove
                // the pointed-to item is at the end of this function, after we
                // are no longer using the pointer
                let task = unsafe { task_ptr.as_mut().unwrap() };

                // SAFETY: fut never moves, and won't drop while this pointer
                // is in use. we don't add or remove items to/from the vec
                // after its initialization. drop is done in-place, after we
                // are no longer using the pointer
                let mut fut = unsafe { Pin::new_unchecked(fut_ptr.as_mut().unwrap()) };

                let done = {
                    let w = unsafe { task.waker.as_std() };

                    let mut cx = Context::from_waker(&w);

                    match fut.as_mut().poll(&mut cx) {
                        Poll::Ready(_) => true,
                        Poll::Pending => false,
                    }
                };

                if done {
                    let tasks = &mut *self.data.borrow_mut();

                    unsafe { tasks.futs[nkey].assume_init_drop() };

                    let task = &mut tasks.nodes[nkey].value;

                    assert_eq!(*task.waker.refs.borrow(), 1);

                    tasks.next.remove(&mut tasks.nodes, nkey);
                    tasks.nodes.remove(nkey);
                }
            }
        }
    }

    impl<F> SharedWake for Tasks<F>
    where
        F: Future<Output = ()>,
    {
        fn wake(&self, task_id: usize) {
            Tasks::wake(self, task_id);
        }
    }

    struct SpawnerData<A> {
        ctx: *const (),
        spawn_fn: unsafe fn(*const (), A) -> Result<(), ()>,
    }

    pub struct Spawner<A> {
        data: RefCell<Option<SpawnerData<A>>>,
    }

    impl<A> Spawner<A> {
        pub fn new() -> Self {
            Self {
                data: RefCell::new(None),
            }
        }

        pub fn spawn(&self, arg: A) -> Result<(), ()> {
            match &*self.data.borrow() {
                Some(data) => unsafe { (data.spawn_fn)(data.ctx, arg) },
                None => Err(()),
            }
        }
    }

    pub struct ArgExecutor<'a, F, A, S> {
        tasks: Tasks<F>,
        spawn_fn: S,
        spawner: RefCell<Option<&'a Spawner<A>>>,
    }

    impl<'a, F, A: 'a, S> ArgExecutor<'a, F, A, S>
    where
        F: Future<Output = ()>,
        S: Fn(A, &mut MaybeUninit<F>),
    {
        pub fn new(tasks_max: usize, spawn_fn: S) -> Self {
            Self {
                tasks: Tasks::new(tasks_max),
                spawn_fn,
                spawner: RefCell::new(None),
            }
        }

        pub fn spawn(&self, arg: A) -> Result<(), ()> {
            self.tasks.add(|dest| (self.spawn_fn)(arg, dest))
        }

        pub fn set_spawner(&self, spawner: &'a Spawner<A>) {
            *self.spawner.borrow_mut() = Some(spawner);

            let mut spawner = self.spawner.borrow_mut();
            let spawner = spawner.as_mut().unwrap();

            *spawner.data.borrow_mut() = Some(SpawnerData {
                ctx: self as *const Self as *const (),
                spawn_fn: Self::spawn_by_arg_fn,
            });
        }

        unsafe fn spawn_by_arg_fn(ctx: *const (), arg: A) -> Result<(), ()> {
            let executor = { (ctx as *const Self).as_ref().unwrap() };

            executor.spawn(arg)
        }

        pub fn run<P>(&self, park: P)
        where
            P: Fn() -> Result<(), io::Error>,
        {
            loop {
                self.tasks.process_next();

                if self.tasks.is_empty() {
                    break;
                }

                park().unwrap();
            }
        }
    }

    impl<'a, F, A: 'a, S> Drop for ArgExecutor<'a, F, A, S> {
        fn drop(&mut self) {
            if let Some(spawner) = &mut *self.spawner.borrow_mut() {
                *spawner.data.borrow_mut() = None;
            }
        }
    }
}

mod bx {
    use super::BoxFuture;
    use super::{SharedWake, SharedWaker};
    use crate::list;
    use slab::Slab;
    use std::cell::RefCell;
    use std::future::Future;
    use std::io;
    use std::rc::Rc;
    use std::task::{Context, Poll};

    struct Task<W> {
        fut: Option<BoxFuture>,
        waker: SharedWaker<W>,
        awake: bool,
    }

    struct TasksData<W> {
        nodes: Slab<list::Node<Task<W>>>,
        next: list::List,
    }

    struct Tasks {
        data: RefCell<TasksData<Self>>,
    }

    impl Tasks {
        fn new(tasks_max: usize) -> Rc<Self> {
            let data = TasksData {
                nodes: Slab::with_capacity(tasks_max),
                next: list::List::default(),
            };

            Rc::new(Self {
                data: RefCell::new(data),
            })
        }

        fn is_empty(&self) -> bool {
            self.data.borrow().nodes.is_empty()
        }

        fn add(&self, f: BoxFuture) -> Result<(), ()> {
            let data = &mut *self.data.borrow_mut();

            if data.nodes.len() == data.nodes.capacity() {
                return Err(());
            }

            let entry = data.nodes.vacant_entry();
            let key = entry.key();

            let waker = SharedWaker {
                refs: RefCell::new(1),
                wake: self,
                task_id: key,
            };

            let task = Task {
                fut: Some(f),
                waker,
                awake: true,
            };

            entry.insert(list::Node::new(task));

            data.next.push_back(&mut data.nodes, key);

            Ok(())
        }

        fn wake(&self, task_id: usize) {
            let data = &mut *self.data.borrow_mut();

            let task = &mut data.nodes[task_id].value;

            if !task.awake {
                task.awake = true;

                data.next.remove(&mut data.nodes, task_id);
                data.next.push_back(&mut data.nodes, task_id);
            }
        }

        fn process_next(&self) {
            loop {
                let (nkey, task_ptr) = {
                    let tasks = &mut *self.data.borrow_mut();

                    let nkey = match tasks.next.head {
                        Some(nkey) => nkey,
                        None => break,
                    };

                    tasks.next.remove(&mut tasks.nodes, nkey);

                    let task = &mut tasks.nodes[nkey].value;

                    task.awake = false;

                    (nkey, task as *mut Task<Self>)
                };

                // SAFETY: task won't move/drop while this pointer is in use.
                // we don't allow inserting into the slab beyond its capacity,
                // therefore its items never move. and the only place we remove
                // the pointed-to item is at the end of this function, after we
                // are no longer using the pointer
                let task = unsafe { task_ptr.as_mut().unwrap() };

                let done = {
                    let fut: &mut BoxFuture = task.fut.as_mut().unwrap();

                    let w = unsafe { task.waker.as_std() };

                    let mut cx = Context::from_waker(&w);

                    match fut.as_mut().poll(&mut cx) {
                        Poll::Ready(_) => true,
                        Poll::Pending => false,
                    }
                };

                if done {
                    task.fut = None;

                    assert_eq!(*task.waker.refs.borrow(), 1);

                    let tasks = &mut *self.data.borrow_mut();

                    tasks.next.remove(&mut tasks.nodes, nkey);
                    tasks.nodes.remove(nkey);
                }
            }
        }
    }

    impl SharedWake for Tasks {
        fn wake(&self, task_id: usize) {
            Tasks::wake(self, task_id);
        }
    }

    pub struct BoxExecutor {
        tasks: Rc<Tasks>,
    }

    impl BoxExecutor {
        pub fn new(tasks_max: usize) -> Self {
            Self {
                tasks: Tasks::new(tasks_max),
            }
        }

        pub fn spawn<F>(&self, f: F) -> Result<(), ()>
        where
            F: Future<Output = ()> + 'static,
        {
            self.tasks.add(Box::pin(f))
        }

        pub fn spawn_boxed(&self, f: BoxFuture) -> Result<(), ()> {
            self.tasks.add(f)
        }

        pub fn run<P>(&self, park: P)
        where
            P: Fn() -> Result<(), io::Error>,
        {
            loop {
                self.tasks.process_next();

                if self.tasks.is_empty() {
                    break;
                }

                park().unwrap();
            }
        }
    }
}

mod boxrc {
    use super::BoxFuture;
    use crate::list;
    use crate::waker::{CheckedLocalWake, LocalWake, WakerFactory};
    use slab::Slab;
    use std::cell::RefCell;
    use std::future::Future;
    use std::io;
    use std::rc::{Rc, Weak};
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread::{self, ThreadId};

    struct TaskWaker {
        tasks: Weak<Tasks>,
        task_id: usize,
        thread_id: ThreadId,
    }

    // SAFETY: we promise to not send wakers across threads
    unsafe impl Send for TaskWaker {}
    unsafe impl Sync for TaskWaker {}

    impl LocalWake for TaskWaker {
        fn wake(self: Rc<Self>) {
            LocalWake::wake_by_ref(&self);
        }

        fn wake_by_ref(self: &Rc<Self>) {
            if let Some(tasks) = self.tasks.upgrade() {
                tasks.wake(self.task_id);
            }
        }
    }

    impl CheckedLocalWake for TaskWaker {
        fn thread_id(self: &Rc<Self>) -> ThreadId {
            self.thread_id
        }

        fn wake(self: Rc<Self>) {
            CheckedLocalWake::wake_by_ref(&self);
        }

        fn wake_by_ref(self: &Rc<Self>) {
            if let Some(tasks) = self.tasks.upgrade() {
                tasks.wake(self.task_id);
            }
        }
    }

    impl Wake for TaskWaker {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            if let Some(tasks) = self.tasks.upgrade() {
                tasks.wake(self.task_id);
            }
        }
    }

    struct Task {
        fut: Option<BoxFuture>,
        awake: bool,
    }

    struct TasksData {
        nodes: Slab<list::Node<Task>>,
        next: list::List,
    }

    struct Tasks {
        data: RefCell<TasksData>,
        wakers: Vec<Waker>,
        waker_strong_counts: Vec<Box<dyn Fn() -> usize>>,
    }

    impl Tasks {
        fn new<W>(tasks_max: usize, waker_factory: W) -> Rc<Self>
        where
            W: WakerFactory,
        {
            let data = TasksData {
                nodes: Slab::with_capacity(tasks_max),
                next: list::List::default(),
            };

            let tasks = {
                let tasks = Rc::new(Self {
                    data: RefCell::new(data),
                    wakers: Vec::new(),
                    waker_strong_counts: Vec::new(),
                });

                let mut wakers = Vec::with_capacity(tasks_max);
                let mut strong_counts = Vec::with_capacity(tasks_max);

                for task_id in 0..wakers.capacity() {
                    let (waker, c) = waker_factory.new_waker(TaskWaker {
                        tasks: Rc::downgrade(&tasks),
                        task_id,
                        thread_id: thread::current().id(),
                    });

                    wakers.push(waker);
                    strong_counts.push(c);
                }

                // SAFETY: we can modify the content of the Rc here because
                // nothing is accessing it yet. we only just constructed the Rc
                // above, and the TaskWakers take refs but they don't access
                // the Rc content at rest
                unsafe {
                    let tasks = Rc::into_raw(tasks) as *mut Tasks;
                    (*tasks).wakers = wakers;
                    (*tasks).waker_strong_counts = strong_counts;

                    Rc::from_raw(tasks)
                }
            };

            tasks
        }

        fn is_empty(&self) -> bool {
            self.data.borrow().nodes.is_empty()
        }

        fn add(&self, f: BoxFuture) -> Result<(), ()> {
            let data = &mut *self.data.borrow_mut();

            if data.nodes.len() == data.nodes.capacity() {
                return Err(());
            }

            let entry = data.nodes.vacant_entry();
            let key = entry.key();

            let task = Task {
                fut: Some(f),
                awake: true,
            };

            entry.insert(list::Node::new(task));

            data.next.push_back(&mut data.nodes, key);

            Ok(())
        }

        fn wake(&self, task_id: usize) {
            let data = &mut *self.data.borrow_mut();

            let task = &mut data.nodes[task_id].value;

            if !task.awake {
                task.awake = true;

                data.next.remove(&mut data.nodes, task_id);
                data.next.push_back(&mut data.nodes, task_id);
            }
        }

        fn process_next<'a>(&'a self) {
            loop {
                let (nkey, task_ptr) = {
                    let tasks = &mut *self.data.borrow_mut();

                    let nkey = match tasks.next.head {
                        Some(nkey) => nkey,
                        None => break,
                    };

                    tasks.next.remove(&mut tasks.nodes, nkey);

                    let task = &mut tasks.nodes[nkey].value;

                    task.awake = false;

                    (nkey, task as *mut Task)
                };

                // SAFETY: task won't move/drop while this pointer is in use.
                // we don't allow inserting into the slab beyond its capacity,
                // therefore its items never move. and the only place we remove
                // the pointed-to item is at the end of this function, after we
                // are no longer using the pointer
                let task = unsafe { task_ptr.as_mut().unwrap() };

                let done = {
                    let fut: &mut BoxFuture = task.fut.as_mut().unwrap();

                    let mut cx = Context::from_waker(&self.wakers[nkey]);

                    match fut.as_mut().poll(&mut cx) {
                        Poll::Ready(_) => true,
                        Poll::Pending => false,
                    }
                };

                if done {
                    task.fut = None;

                    assert_eq!((self.waker_strong_counts[nkey])(), 1);

                    let tasks = &mut *self.data.borrow_mut();

                    tasks.next.remove(&mut tasks.nodes, nkey);
                    tasks.nodes.remove(nkey);
                }
            }
        }
    }

    pub struct BoxRcExecutor {
        tasks: Rc<Tasks>,
    }

    impl BoxRcExecutor {
        pub fn new<W>(tasks_max: usize, waker_factory: W) -> Self
        where
            W: WakerFactory,
        {
            Self {
                tasks: Tasks::new(tasks_max, waker_factory),
            }
        }

        pub fn spawn<F>(&self, f: F) -> Result<(), ()>
        where
            F: Future<Output = ()> + 'static,
        {
            self.tasks.add(Box::pin(f))
        }

        pub fn run<P>(&self, park: P)
        where
            P: Fn() -> Result<(), io::Error>,
        {
            loop {
                self.tasks.process_next();

                if self.tasks.is_empty() {
                    break;
                }

                park().unwrap();
            }
        }
    }
}

pub use arg::{ArgExecutor, Spawner};
pub use boxrc::BoxRcExecutor;
pub use bx::BoxExecutor;

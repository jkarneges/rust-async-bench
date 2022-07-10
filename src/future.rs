use crate::fakeio;
use crate::fakeio::{Evented, FakeListener, FakeStream, Stats, READABLE, WRITABLE};
use crate::list;
use slab::Slab;
use std::cell::RefCell;
use std::future::Future;
use std::io;
use std::io::{Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

pub trait Reactor {
    fn poll(&self) -> Result<(), io::Error>;
}

pub struct RegistrationHandle<'a, 's> {
    reactor: &'a FakeReactor<'s>,
    key: usize,
}

impl RegistrationHandle<'_, '_> {
    fn is_ready(&self) -> bool {
        let data = &*self.reactor.data.borrow();

        let event_reg = &data.registrations[self.key];

        event_reg.ready
    }

    fn set_ready(&self, ready: bool) {
        let data = &mut *self.reactor.data.borrow_mut();

        let event_reg = &mut data.registrations[self.key];

        event_reg.ready = ready;
    }

    fn bind_waker(&self, waker: Waker) {
        let data = &mut *self.reactor.data.borrow_mut();

        let event_reg = &mut data.registrations[self.key];

        event_reg.waker = Some(waker);
    }

    fn unbind_waker(&self) {
        let data = &mut *self.reactor.data.borrow_mut();

        let event_reg = &mut data.registrations[self.key];

        event_reg.waker = None;
    }
}

impl Drop for RegistrationHandle<'_, '_> {
    fn drop(&mut self) {
        let data = &mut *self.reactor.data.borrow_mut();

        data.registrations.remove(self.key);
    }
}

struct EventRegistration {
    ready: bool,
    waker: Option<Waker>,
}

struct FakeReactorData {
    registrations: Slab<EventRegistration>,
    events: Slab<(usize, u8)>,
}

pub struct FakeReactor<'s> {
    data: RefCell<FakeReactorData>,
    poll: fakeio::Poll<'s>,
}

impl<'s> FakeReactor<'s> {
    pub fn new(registrations_max: usize, stats: &'s Stats) -> Self {
        let data = FakeReactorData {
            registrations: Slab::with_capacity(registrations_max),
            events: Slab::with_capacity(128),
        };

        Self {
            data: RefCell::new(data),
            poll: fakeio::Poll::new(128, stats),
        }
    }

    fn register<'a, E: Evented>(
        &'a self,
        handle: &E,
        interest: u8,
    ) -> Result<RegistrationHandle<'a, 's>, io::Error> {
        let data = &mut *self.data.borrow_mut();

        if data.registrations.len() == data.registrations.capacity() {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }

        let key = data.registrations.insert(EventRegistration {
            ready: false,
            waker: None,
        });

        self.poll.register(handle, key, interest);

        Ok(RegistrationHandle { reactor: self, key })
    }

    fn unregister<E: Evented>(&self, handle: &E) {
        self.poll.unregister(handle);
    }
}

impl Reactor for FakeReactor<'_> {
    fn poll(&self) -> Result<(), io::Error> {
        let data = &mut *self.data.borrow_mut();

        self.poll.poll(&mut data.events);

        for (_, (key, _)) in data.events.iter() {
            if let Some(event_reg) = data.registrations.get_mut(*key) {
                event_reg.ready = true;

                if let Some(waker) = event_reg.waker.take() {
                    waker.wake();
                }
            }
        }

        Ok(())
    }
}

struct SharedWaker<'r, R, F, A, S> {
    executor: *const Executor<'r, R, F, A, S>,
    task_id: usize,
}

impl<'r, R: 'r, F, A: 'r, S> SharedWaker<'r, R, F, A, S>
where
    R: Reactor,
    F: Future<Output = ()>,
    S: Fn(A) -> F,
{
    unsafe fn as_std_waker(&self) -> Waker {
        let executor = self.executor.as_ref().unwrap();

        executor.add_waker_ref(self.task_id);

        let rw = RawWaker::new(self as *const Self as *const (), Self::vtable());

        Waker::from_raw(rw)
    }

    unsafe fn clone(data: *const ()) -> RawWaker {
        let s = (data as *const Self).as_ref().unwrap();

        let executor = s.executor.as_ref().unwrap();

        executor.add_waker_ref(s.task_id);

        RawWaker::new(data, &Self::vtable())
    }

    unsafe fn wake(data: *const ()) {
        Self::wake_by_ref(data);

        Self::drop(data);
    }

    unsafe fn wake_by_ref(data: *const ()) {
        let s = (data as *const Self).as_ref().unwrap();

        let executor = s.executor.as_ref().unwrap();

        executor.wake(s.task_id);
    }

    unsafe fn drop(data: *const ()) {
        let s = (data as *const Self).as_ref().unwrap();

        let executor = s.executor.as_ref().unwrap();

        executor.remove_waker_ref(s.task_id);
    }

    fn vtable() -> &'static RawWakerVTable {
        &RawWakerVTable::new(Self::clone, Self::wake, Self::wake_by_ref, Self::drop)
    }
}

struct Task<'r, R, F, A, S> {
    fut: Option<F>,
    waker: SharedWaker<'r, R, F, A, S>,
    waker_refs: usize,
    awake: bool,
}

struct Tasks<'r, R, F, A, S> {
    nodes: Slab<list::Node<Task<'r, R, F, A, S>>>,
    next: list::List,
    spawn_fn: S,
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

pub struct Executor<'r, R, F, A, S> {
    reactor: &'r R,
    tasks: RefCell<Tasks<'r, R, F, A, S>>,
    spawner: RefCell<Option<&'r Spawner<A>>>,
}

impl<'r, R: 'r, F, A: 'r, S> Executor<'r, R, F, A, S>
where
    R: Reactor,
    F: Future<Output = ()>,
    S: Fn(A) -> F,
{
    pub fn new(reactor: &'r R, tasks_max: usize, spawn_fn: S) -> Self {
        Self {
            reactor,
            tasks: RefCell::new(Tasks {
                nodes: Slab::with_capacity(tasks_max),
                next: list::List::default(),
                spawn_fn,
            }),
            spawner: RefCell::new(None),
        }
    }

    pub fn spawn(&self, f: F) -> Result<(), ()> {
        let key = self.create_task()?;

        let tasks = &mut *self.tasks.borrow_mut();

        tasks.nodes[key].value.fut = Some(f);

        Ok(())
    }

    pub fn spawn_by_arg(&self, arg: A) -> Result<(), ()> {
        let key = self.create_task()?;

        let tasks = &mut *self.tasks.borrow_mut();

        tasks.nodes[key].value.fut = Some((tasks.spawn_fn)(arg));

        Ok(())
    }

    pub fn set_spawner(&self, spawner: &'r Spawner<A>) {
        *self.spawner.borrow_mut() = Some(spawner);

        let mut spawner = self.spawner.borrow_mut();
        let spawner = spawner.as_mut().unwrap();

        *spawner.data.borrow_mut() = Some(SpawnerData {
            ctx: self as *const Self as *const (),
            spawn_fn: Self::spawn_by_arg_fn,
        });
    }

    unsafe fn spawn_by_arg_fn(ctx: *const (), arg: A) -> Result<(), ()> {
        let executor = { (ctx as *const Executor<R, F, A, S>).as_ref().unwrap() };

        executor.spawn_by_arg(arg)
    }

    pub fn exec(&self) {
        loop {
            loop {
                let (nkey, task_ptr) = {
                    let tasks = &mut *self.tasks.borrow_mut();

                    let nkey = match tasks.next.head {
                        Some(nkey) => nkey,
                        None => break,
                    };

                    tasks.next.remove(&mut tasks.nodes, nkey);

                    let task = &mut tasks.nodes[nkey].value;

                    task.awake = false;

                    (nkey, task as *mut Task<R, F, A, S>)
                };

                // task won't move/drop while this pointer is in use
                let task = unsafe { task_ptr.as_mut().unwrap() };

                let done = {
                    let mut p = unsafe { Pin::new_unchecked(task.fut.as_mut().unwrap()) };

                    let w = unsafe { task.waker.as_std_waker() };

                    let mut cx = Context::from_waker(&w);

                    match p.as_mut().poll(&mut cx) {
                        Poll::Ready(_) => true,
                        Poll::Pending => false,
                    }
                };

                if done {
                    task.fut = None;

                    let tasks = &mut *self.tasks.borrow_mut();

                    let task = &mut tasks.nodes[nkey].value;

                    assert_eq!(task.waker_refs, 0);

                    tasks.next.remove(&mut tasks.nodes, nkey);
                    tasks.nodes.remove(nkey);
                }
            }

            {
                let tasks = &*self.tasks.borrow();

                if tasks.nodes.is_empty() {
                    break;
                }
            }

            self.reactor.poll().unwrap();
        }
    }

    fn create_task(&self) -> Result<usize, ()> {
        let tasks = &mut *self.tasks.borrow_mut();

        if tasks.nodes.len() == tasks.nodes.capacity() {
            return Err(());
        }

        let entry = tasks.nodes.vacant_entry();
        let key = entry.key();

        let waker = SharedWaker {
            executor: self,
            task_id: key,
        };

        let task = Task {
            fut: None,
            waker,
            waker_refs: 0,
            awake: true,
        };

        entry.insert(list::Node::new(task));

        tasks.next.push_back(&mut tasks.nodes, key);

        Ok(key)
    }

    fn add_waker_ref(&self, task_id: usize) {
        let tasks = &mut *self.tasks.borrow_mut();

        let task = &mut tasks.nodes[task_id].value;

        task.waker_refs += 1;
    }

    fn remove_waker_ref(&self, task_id: usize) {
        let tasks = &mut *self.tasks.borrow_mut();

        let task = &mut tasks.nodes[task_id].value;

        assert!(task.waker_refs > 0);

        task.waker_refs -= 1;
    }

    fn wake(&self, task_id: usize) {
        let tasks = &mut *self.tasks.borrow_mut();

        let task = &mut tasks.nodes[task_id].value;

        if !task.awake {
            task.awake = true;

            tasks.next.remove(&mut tasks.nodes, task_id);
            tasks.next.push_back(&mut tasks.nodes, task_id);
        }
    }
}

impl<'r, R: 'r, F, A: 'r, S> Drop for Executor<'r, R, F, A, S> {
    fn drop(&mut self) {
        if let Some(spawner) = &mut *self.spawner.borrow_mut() {
            *spawner.data.borrow_mut() = None;
        }
    }
}

pub struct AsyncFakeStream<'r, 's> {
    inner: FakeStream<'s>,
    handle: RegistrationHandle<'r, 's>,
}

impl<'r, 's: 'r> AsyncFakeStream<'r, 's> {
    pub fn new(s: FakeStream<'s>, reactor: &'r FakeReactor<'s>) -> Self {
        let handle = reactor.register(&s, READABLE | WRITABLE).unwrap();

        handle.set_ready(true);

        Self { inner: s, handle }
    }

    pub fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> ReadFuture<'a, 'r, 's> {
        ReadFuture { s: self, buf }
    }

    pub fn write<'a>(&'a mut self, buf: &'a [u8]) -> WriteFuture<'a, 'r, 's> {
        WriteFuture { s: self, buf }
    }
}

impl Drop for AsyncFakeStream<'_, '_> {
    fn drop(&mut self) {
        self.handle.reactor.unregister(&self.inner);
    }
}

pub struct AsyncFakeListener<'r, 's> {
    inner: FakeListener<'s>,
    handle: RegistrationHandle<'r, 's>,
}

impl<'r, 's: 'r> AsyncFakeListener<'r, 's> {
    pub fn new(reactor: &'r FakeReactor<'s>, stats: &'s Stats) -> Self {
        let l = FakeListener::new(stats);

        let handle = reactor.register(&l, READABLE).unwrap();

        handle.set_ready(true);

        Self { inner: l, handle }
    }

    pub fn accept<'a>(&'a self) -> AcceptFuture<'a, 'r, 's> {
        AcceptFuture { l: self }
    }
}

impl Drop for AsyncFakeListener<'_, '_> {
    fn drop(&mut self) {
        self.handle.reactor.unregister(&self.inner);
    }
}

pub struct ReadFuture<'a, 'r, 's> {
    s: &'a mut AsyncFakeStream<'r, 's>,
    buf: &'a mut [u8],
}

impl<'a, 'r, 's: 'a> Future for ReadFuture<'a, 'r, 's> {
    type Output = Result<usize, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let f = &mut *self;

        f.s.handle.bind_waker(cx.waker().clone());

        if !f.s.handle.is_ready() {
            return Poll::Pending;
        }

        match f.s.inner.read(f.buf) {
            Ok(size) => Poll::Ready(Ok(size)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                f.s.handle.set_ready(false);

                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl Drop for ReadFuture<'_, '_, '_> {
    fn drop(&mut self) {
        self.s.handle.unbind_waker();
    }
}

pub struct WriteFuture<'a, 'r, 's> {
    s: &'a mut AsyncFakeStream<'r, 's>,
    buf: &'a [u8],
}

impl<'a, 'r, 's: 'a> Future for WriteFuture<'a, 'r, 's> {
    type Output = Result<usize, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let f = &mut *self;

        f.s.handle.bind_waker(cx.waker().clone());

        if !f.s.handle.is_ready() {
            return Poll::Pending;
        }

        match f.s.inner.write(f.buf) {
            Ok(size) => Poll::Ready(Ok(size)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                f.s.handle.set_ready(false);

                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl Drop for WriteFuture<'_, '_, '_> {
    fn drop(&mut self) {
        self.s.handle.unbind_waker();
    }
}

pub struct AcceptFuture<'a, 'r, 's> {
    l: &'a AsyncFakeListener<'r, 's>,
}

impl<'a, 'r, 's: 'a> Future for AcceptFuture<'a, 'r, 's> {
    type Output = Result<AsyncFakeStream<'r, 's>, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let f = &mut *self;

        f.l.handle.bind_waker(cx.waker().clone());

        if !f.l.handle.is_ready() {
            return Poll::Pending;
        }

        match f.l.inner.accept() {
            Ok(stream) => Poll::Ready(Ok(AsyncFakeStream::new(stream, f.l.handle.reactor))),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                f.l.handle.set_ready(false);

                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl Drop for AcceptFuture<'_, '_, '_> {
    fn drop(&mut self) {
        self.l.handle.unbind_waker();
    }
}

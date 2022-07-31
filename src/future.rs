use crate::fakeio;
use crate::fakeio::{Evented, FakeListener, FakeStream, Stats, READABLE, WRITABLE};
use slab::Slab;
use std::cell::RefCell;
use std::future::Future;
use std::io;
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

pub trait FakeReactorRef<T>: Clone
where
    T: Stats,
{
    fn get(&self) -> &FakeReactor<T>;

    fn register<'a, E: Evented>(
        &'a self,
        handle: &E,
        interest: u8,
    ) -> Result<RegistrationHandle<T, Self>, io::Error> {
        let r = self.get();

        let data = &mut *r.data.borrow_mut();

        if data.registrations.len() == data.registrations.capacity() {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }

        let key = data.registrations.insert(EventRegistration {
            ready: false,
            waker: None,
        });

        r.poll.register(handle, key, interest);

        Ok(RegistrationHandle {
            reactor: self.clone(),
            key,
            _marker: PhantomData,
        })
    }
}

pub struct RegistrationHandle<T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    reactor: R,
    key: usize,
    _marker: PhantomData<T>,
}

impl<T, R> RegistrationHandle<T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    fn is_ready(&self) -> bool {
        let data = &*self.reactor.get().data.borrow();

        let event_reg = &data.registrations[self.key];

        event_reg.ready
    }

    fn set_ready(&self, ready: bool) {
        let data = &mut *self.reactor.get().data.borrow_mut();

        let event_reg = &mut data.registrations[self.key];

        event_reg.ready = ready;
    }

    fn bind_waker(&self, waker: &Waker) {
        let data = &mut *self.reactor.get().data.borrow_mut();

        let event_reg = &mut data.registrations[self.key];

        if let Some(current_waker) = &event_reg.waker {
            if current_waker.will_wake(waker) {
                // keep the current waker
                return;
            }
        }

        event_reg.waker = Some(waker.clone());
    }

    fn unbind_waker(&self) {
        let data = &mut *self.reactor.get().data.borrow_mut();

        let event_reg = &mut data.registrations[self.key];

        event_reg.waker = None;
    }
}

impl<T, R> Drop for RegistrationHandle<T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    fn drop(&mut self) {
        let data = &mut *self.reactor.get().data.borrow_mut();

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

pub struct FakeReactor<T> {
    data: RefCell<FakeReactorData>,
    poll: fakeio::Poll<T>,
}

impl<T> FakeReactor<T>
where
    T: Stats,
{
    pub fn new(registrations_max: usize, stats: T) -> Self {
        let data = FakeReactorData {
            registrations: Slab::with_capacity(registrations_max),
            events: Slab::with_capacity(128),
        };

        Self {
            data: RefCell::new(data),
            poll: fakeio::Poll::new(128, stats),
        }
    }

    pub fn poll(&self) -> Result<(), io::Error> {
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

    fn unregister<E: Evented>(&self, handle: &E) {
        self.poll.unregister(handle);
    }
}

pub struct AsyncFakeStream<T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    inner: FakeStream<T>,
    handle: RegistrationHandle<T, R>,
}

impl<T, R> AsyncFakeStream<T, R>
where
    T: Stats + Clone,
    R: FakeReactorRef<T>,
{
    pub fn new(s: FakeStream<T>, reactor: R) -> Self {
        let handle = reactor.register(&s, READABLE | WRITABLE).unwrap();

        handle.set_ready(true);

        Self { inner: s, handle }
    }

    pub fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> ReadFuture<'a, T, R> {
        ReadFuture { s: self, buf }
    }

    pub fn write<'a>(&'a mut self, buf: &'a [u8]) -> WriteFuture<'a, T, R> {
        WriteFuture { s: self, buf }
    }
}

impl<T, R> Drop for AsyncFakeStream<T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    fn drop(&mut self) {
        self.handle.reactor.get().unregister(&self.inner);
    }
}

pub struct AsyncFakeListener<T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    inner: FakeListener<T>,
    handle: RegistrationHandle<T, R>,
}

impl<T, R> AsyncFakeListener<T, R>
where
    T: Stats + Clone,
    R: FakeReactorRef<T>,
{
    pub fn new(reactor: R, stats: T) -> Self {
        let l = FakeListener::new(stats);

        let handle = reactor.register(&l, READABLE).unwrap();

        handle.set_ready(true);

        Self { inner: l, handle }
    }

    pub fn accept<'a>(&'a self) -> AcceptFuture<'a, T, R> {
        AcceptFuture { l: self }
    }
}

impl<T, R> Drop for AsyncFakeListener<T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    fn drop(&mut self) {
        self.handle.reactor.get().unregister(&self.inner);
    }
}

pub struct ReadFuture<'a, T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    s: &'a mut AsyncFakeStream<T, R>,
    buf: &'a mut [u8],
}

impl<'a, T, R> Future for ReadFuture<'a, T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    type Output = Result<usize, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let f = &mut *self;

        f.s.handle.bind_waker(cx.waker());

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

impl<T, R> Drop for ReadFuture<'_, T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    fn drop(&mut self) {
        self.s.handle.unbind_waker();
    }
}

pub struct WriteFuture<'a, T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    s: &'a mut AsyncFakeStream<T, R>,
    buf: &'a [u8],
}

impl<'a, T, R> Future for WriteFuture<'a, T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    type Output = Result<usize, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let f = &mut *self;

        f.s.handle.bind_waker(cx.waker());

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

impl<T, R> Drop for WriteFuture<'_, T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    fn drop(&mut self) {
        self.s.handle.unbind_waker();
    }
}

pub struct AcceptFuture<'a, T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    l: &'a AsyncFakeListener<T, R>,
}

impl<'a, T, R> Future for AcceptFuture<'a, T, R>
where
    T: Stats + Clone,
    R: FakeReactorRef<T>,
{
    type Output = Result<AsyncFakeStream<T, R>, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let f = &mut *self;

        f.l.handle.bind_waker(cx.waker());

        if !f.l.handle.is_ready() {
            return Poll::Pending;
        }

        match f.l.inner.accept() {
            Ok(stream) => Poll::Ready(Ok(AsyncFakeStream::new(stream, f.l.handle.reactor.clone()))),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                f.l.handle.set_ready(false);

                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl<T, R> Drop for AcceptFuture<'_, T, R>
where
    T: Stats,
    R: FakeReactorRef<T>,
{
    fn drop(&mut self) {
        self.l.handle.unbind_waker();
    }
}

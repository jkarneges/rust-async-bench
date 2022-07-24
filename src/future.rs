use crate::fakeio;
use crate::fakeio::{Evented, FakeListener, FakeStream, Stats, READABLE, WRITABLE};
use slab::Slab;
use std::cell::RefCell;
use std::future::Future;
use std::io;
use std::io::{Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

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

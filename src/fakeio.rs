use slab::Slab;
use std::cell::{Cell, RefCell};
use std::io;

pub enum StatsType {
    Register,
    Unregister,
    Poll,
    Accept,
    Read,
    Write,
}

pub trait Stats {
    fn inc(&self, t: StatsType);
}

pub trait Evented {
    fn set_poll_index(&self, index: Option<usize>);
    fn get_poll_index(&self) -> Option<usize>;
}

pub struct FakeStream<T>
where
    T: Stats,
{
    poll_index: Cell<Option<usize>>,
    stats: T,
    calls: usize,
}

impl<T> FakeStream<T>
where
    T: Stats,
{
    fn new(stats: T) -> Self {
        Self {
            poll_index: Cell::new(None),
            stats,
            calls: 0,
        }
    }
}

impl<T> io::Read for FakeStream<T>
where
    T: Stats,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        self.stats.inc(StatsType::Read);

        self.calls += 1;

        if self.calls % 2 == 1 {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        } else {
            let data = &b"hello world\n"[..];

            assert!(buf.len() >= data.len());

            buf[..data.len()].copy_from_slice(&data);

            Ok(data.len())
        }
    }
}

impl<T> io::Write for FakeStream<T>
where
    T: Stats,
{
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        self.stats.inc(StatsType::Write);

        self.calls += 1;

        if self.calls % 2 == 1 {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        } else {
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        Ok(())
    }
}

impl<T> Evented for FakeStream<T>
where
    T: Stats,
{
    fn set_poll_index(&self, index: Option<usize>) {
        self.poll_index.set(index);
    }

    fn get_poll_index(&self) -> Option<usize> {
        self.poll_index.get()
    }
}

pub struct FakeListener<T> {
    poll_index: Cell<Option<usize>>,
    stats: T,
    calls: RefCell<usize>,
}

impl<T> FakeListener<T>
where
    T: Stats + Clone,
{
    pub fn new(stats: T) -> Self {
        Self {
            poll_index: Cell::new(None),
            stats,
            calls: RefCell::new(0),
        }
    }

    pub fn accept(&self) -> Result<FakeStream<T>, io::Error> {
        self.stats.inc(StatsType::Accept);

        *self.calls.borrow_mut() += 1;

        if *self.calls.borrow() % 2 == 1 {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        } else {
            Ok(FakeStream::new(self.stats.clone()))
        }
    }
}

impl<T> Evented for FakeListener<T> {
    fn set_poll_index(&self, index: Option<usize>) {
        self.poll_index.set(index);
    }

    fn get_poll_index(&self) -> Option<usize> {
        self.poll_index.get()
    }
}

pub const READABLE: u8 = 1;
pub const WRITABLE: u8 = 2;

pub struct Poll<T> {
    stats: T,
    items: RefCell<Slab<(u8, usize)>>,
}

impl<T> Poll<T>
where
    T: Stats,
{
    pub fn new(capacity: usize, stats: T) -> Self {
        Self {
            stats,
            items: RefCell::new(Slab::with_capacity(capacity)),
        }
    }

    pub fn register<E: Evented>(&self, handle: &E, key: usize, interest: u8) {
        self.stats.inc(StatsType::Register);

        let index = self.items.borrow_mut().insert((interest, key));

        handle.set_poll_index(Some(index));
    }

    pub fn unregister<E: Evented>(&self, handle: &E) {
        self.stats.inc(StatsType::Unregister);

        if let Some(index) = handle.get_poll_index() {
            self.items.borrow_mut().remove(index);

            handle.set_poll_index(None);
        }
    }

    pub fn poll(&self, events: &mut Slab<(usize, u8)>) {
        self.stats.inc(StatsType::Poll);

        events.clear();

        for (_, (interest, key)) in self.items.borrow().iter() {
            events.insert((*key, *interest));
        }
    }
}

impl<T> Drop for Poll<T> {
    fn drop(&mut self) {
        // confirm all i/o objects were unregistered
        assert!(self.items.borrow().is_empty());
    }
}

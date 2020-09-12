use slab::Slab;
use std::cell::{Cell, RefCell};
use std::fmt;
use std::io;

#[derive(Debug)]
struct StatsData {
    register: usize,
    unregister: usize,
    poll: usize,
    accept: usize,
    read: usize,
    write: usize,
}

pub struct Stats {
    data: RefCell<StatsData>,
}

impl Stats {
    pub fn new() -> Self {
        Self {
            data: RefCell::new(StatsData {
                register: 0,
                unregister: 0,
                poll: 0,
                accept: 0,
                read: 0,
                write: 0,
            }),
        }
    }

    fn inc_register(&self) {
        self.data.borrow_mut().register += 1;
    }

    fn inc_unregister(&self) {
        self.data.borrow_mut().unregister += 1;
    }

    fn inc_poll(&self) {
        self.data.borrow_mut().poll += 1;
    }

    fn inc_accept(&self) {
        self.data.borrow_mut().accept += 1;
    }

    fn inc_read(&self) {
        self.data.borrow_mut().read += 1;
    }

    fn inc_write(&self) {
        self.data.borrow_mut().write += 1;
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", *self.data.borrow())
    }
}

pub trait Evented {
    fn set_poll_index(&self, index: Option<usize>);
    fn get_poll_index(&self) -> Option<usize>;
}

pub struct FakeStream<'s> {
    poll_index: Cell<Option<usize>>,
    stats: &'s Stats,
    calls: usize,
}

impl<'s> FakeStream<'s> {
    fn new(stats: &'s Stats) -> Self {
        Self {
            poll_index: Cell::new(None),
            stats,
            calls: 0,
        }
    }
}

impl io::Read for FakeStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        self.stats.inc_read();

        self.calls += 1;

        if self.calls % 2 == 1 {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        } else {
            let data = &b"hello world\n"[..];

            assert!(buf.len() >= data.len());

            &mut buf[..data.len()].copy_from_slice(&data);

            Ok(data.len())
        }
    }
}

impl io::Write for FakeStream<'_> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        self.stats.inc_write();

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

impl Evented for FakeStream<'_> {
    fn set_poll_index(&self, index: Option<usize>) {
        self.poll_index.set(index);
    }

    fn get_poll_index(&self) -> Option<usize> {
        self.poll_index.get()
    }
}

pub struct FakeListener<'s> {
    poll_index: Cell<Option<usize>>,
    stats: &'s Stats,
    calls: RefCell<usize>,
}

impl<'s> FakeListener<'s> {
    pub fn new(stats: &'s Stats) -> Self {
        Self {
            poll_index: Cell::new(None),
            stats,
            calls: RefCell::new(0),
        }
    }

    pub fn accept(&self) -> Result<FakeStream, io::Error> {
        self.stats.inc_accept();

        *self.calls.borrow_mut() += 1;

        if *self.calls.borrow() % 2 == 1 {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        } else {
            Ok(FakeStream::new(&self.stats))
        }
    }
}

impl Evented for FakeListener<'_> {
    fn set_poll_index(&self, index: Option<usize>) {
        self.poll_index.set(index);
    }

    fn get_poll_index(&self) -> Option<usize> {
        self.poll_index.get()
    }
}

pub const READABLE: u8 = 1;
pub const WRITABLE: u8 = 2;

pub struct Poll<'s> {
    stats: &'s Stats,
    items: RefCell<Slab<(u8, usize)>>,
}

impl<'s> Poll<'s> {
    pub fn new(capacity: usize, stats: &'s Stats) -> Self {
        Self {
            stats,
            items: RefCell::new(Slab::with_capacity(capacity)),
        }
    }

    pub fn register<E: Evented>(&self, handle: &E, key: usize, interest: u8) {
        self.stats.inc_register();

        let index = self.items.borrow_mut().insert((interest, key));

        handle.set_poll_index(Some(index));
    }

    pub fn unregister<E: Evented>(&self, handle: &E) {
        self.stats.inc_unregister();

        if let Some(index) = handle.get_poll_index() {
            self.items.borrow_mut().remove(index);

            handle.set_poll_index(None);
        }
    }

    pub fn poll(&self, events: &mut Slab<(usize, u8)>) {
        self.stats.inc_poll();

        events.clear();

        for (_, (interest, key)) in self.items.borrow().iter() {
            events.insert((*key, *interest));
        }
    }
}

impl Drop for Poll<'_> {
    fn drop(&mut self) {
        // confirm all i/o objects were unregistered
        assert!(self.items.borrow().is_empty());
    }
}

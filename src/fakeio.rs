use slab::Slab;
use std::cell::{Cell, RefCell};
use std::fmt;
use std::io;

#[derive(Debug)]
struct StatsMetrics {
    register: usize,
    unregister: usize,
    poll: usize,
    accept: usize,
    read: usize,
    write: usize,
}

struct StatsData {
    metrics: StatsMetrics,
    pipe_fds: Option<[libc::c_int; 2]>,
}

impl StatsData {
    fn new(syscalls: bool) -> Self {
        let pipe_fds = if syscalls {
            let mut pipe_fds: [libc::c_int; 2] = [0; 2];

            let ret = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
            assert_eq!(ret, 0);

            let ret = unsafe { libc::fcntl(pipe_fds[0], libc::F_SETFL, libc::O_NONBLOCK) };
            assert_eq!(ret, 0);

            Some(pipe_fds)
        } else {
            None
        };

        Self {
            metrics: StatsMetrics {
                register: 0,
                unregister: 0,
                poll: 0,
                accept: 0,
                read: 0,
                write: 0,
            },
            pipe_fds,
        }
    }

    fn do_call(&mut self) {
        if let Some(fds) = &self.pipe_fds {
            let mut dest: [u8; 1] = [0; 1];

            let ret = unsafe { libc::read(fds[0], dest.as_mut_ptr() as *mut libc::c_void, 1) };
            assert_eq!(ret, -1);
        }
    }
}

impl Drop for StatsData {
    fn drop(&mut self) {
        if let Some(fds) = &self.pipe_fds {
            let ret = unsafe { libc::close(fds[0]) };
            assert_eq!(ret, 0);

            let ret = unsafe { libc::close(fds[1]) };
            assert_eq!(ret, 0);
        }
    }
}

pub struct Stats {
    data: RefCell<StatsData>,
}

impl Stats {
    pub fn new(syscalls: bool) -> Self {
        Self {
            data: RefCell::new(StatsData::new(syscalls)),
        }
    }

    fn inc_register(&self) {
        let data = &mut *self.data.borrow_mut();

        data.do_call();
        data.metrics.register += 1;
    }

    fn inc_unregister(&self) {
        let data = &mut *self.data.borrow_mut();

        data.do_call();
        data.metrics.unregister += 1;
    }

    fn inc_poll(&self) {
        let data = &mut *self.data.borrow_mut();

        data.do_call();
        data.metrics.poll += 1;
    }

    fn inc_accept(&self) {
        let data = &mut *self.data.borrow_mut();

        data.do_call();
        data.metrics.accept += 1;
    }

    fn inc_read(&self) {
        let data = &mut *self.data.borrow_mut();

        data.do_call();
        data.metrics.read += 1;
    }

    fn inc_write(&self) {
        let data = &mut *self.data.borrow_mut();

        data.do_call();
        data.metrics.write += 1;
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = &self.data.borrow().metrics;

        write!(
            f,
            "register={} unregister={} poll={} accept={} read={} write={}",
            m.register, m.unregister, m.poll, m.accept, m.read, m.write
        )
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

use crate::fakeio::{FakeListener, FakeStream, Poll, Stats, READABLE, WRITABLE};
use crate::future::{AsyncFakeListener, AsyncFakeStream, Executor, FakeReactor, Reactor};
use crate::list;
use futures::executor::{LocalPool, LocalSpawner};
use futures::task::LocalSpawnExt;
use slab::Slab;
use std::cell::RefCell;
use std::io;
use std::io::{Read, Write};
use std::mem;
use std::rc::Rc;

const CONNS_MAX: usize = 32;

enum ConnectionState {
    ReceivingRequest,
    SendingResponse,
}

struct Connection {
    stream: FakeStream,
    can_read: bool,
    can_write: bool,
    state: ConnectionState,
    buf: [u8; 128],
    buf_len: usize,
    sent: usize,
}

impl Connection {
    fn process(&mut self) -> bool {
        loop {
            match self.state {
                ConnectionState::ReceivingRequest => {
                    let size = match self.stream.read(&mut self.buf[self.buf_len..]) {
                        Ok(size) => size,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            self.can_read = false;
                            return false;
                        }
                        Err(_) => unreachable!(),
                    };

                    self.buf_len += size;

                    if (&self.buf[..self.buf_len]).contains(&b'\n') {
                        self.state = ConnectionState::SendingResponse;
                    }
                }
                ConnectionState::SendingResponse => {
                    let size = match self.stream.write(&self.buf[self.sent..self.buf_len]) {
                        Ok(size) => size,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            self.can_write = false;
                            return false;
                        }
                        Err(_) => unreachable!(),
                    };

                    self.sent += size;

                    if self.sent >= self.buf_len {
                        return true;
                    }
                }
            }
        }
    }
}

pub fn run_sync(stats: &Rc<Stats>) {
    let poll = Poll::new(CONNS_MAX + 1, stats.clone());

    let mut events = Slab::with_capacity(CONNS_MAX + 1);

    let listener = FakeListener::new(stats.clone());

    poll.register(&listener, 0, READABLE);

    let mut conns: Slab<list::Node<Connection>> = Slab::with_capacity(CONNS_MAX);
    let mut needs_process = list::List::default();

    let mut can_accept = true;

    let mut accept_left = CONNS_MAX;

    loop {
        while accept_left > 0 && can_accept {
            match listener.accept() {
                Ok(stream) => {
                    accept_left -= 1;

                    let key = conns.insert(list::Node::new(Connection {
                        stream,
                        can_read: true,
                        can_write: true,
                        state: ConnectionState::ReceivingRequest,
                        buf: [0; 128],
                        buf_len: 0,
                        sent: 0,
                    }));

                    let c = &mut conns[key].value;

                    poll.register(&c.stream, key + 1, READABLE | WRITABLE);

                    needs_process.push_back(&mut conns, key);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => can_accept = false,
                _ => unreachable!(),
            }
        }

        let mut next = needs_process.head;

        while let Some(key) = next {
            needs_process.remove(&mut conns, key);

            let c = &mut conns[key].value;
            if c.process() {
                poll.unregister(&c.stream);

                conns.remove(key);
            }

            next = needs_process.head;
        }

        if accept_left == 0 && conns.len() == 0 {
            break;
        }

        poll.poll(&mut events);

        for (_, &(key, state)) in events.iter() {
            if key == 0 {
                can_accept = true;
            } else {
                let key = key - 1;

                let c = &mut conns[key].value;

                let mut do_process = false;

                if state & READABLE != 0 {
                    c.can_read = true;
                    do_process = true;
                }

                if state & WRITABLE != 0 {
                    c.can_write = true;
                    do_process = true;
                }

                if do_process {
                    needs_process.remove(&mut conns, key);
                    needs_process.push_back(&mut conns, key);
                }
            }
        }
    }

    poll.unregister(&listener);
}

pub struct AsyncPrealloc {
    pub stats: Rc<Stats>,
    pub reactor: Rc<FakeReactor>,
    pub task_count: Rc<RefCell<usize>>,
}

impl AsyncPrealloc {
    pub fn new(stats_syscalls: bool) -> Self {
        let stats = Rc::new(Stats::new(stats_syscalls));
        let reactor = Rc::new(FakeReactor::new(CONNS_MAX + 1, &stats));
        let task_count = Rc::new(RefCell::new(0));

        Self {
            stats,
            reactor,
            task_count,
        }
    }
}

async fn listen(
    task_count: Rc<RefCell<usize>>,
    spawn: unsafe fn(*const (), *const (), usize) -> Result<(), ()>,
    ctx: *const (),
    reactor: &Rc<FakeReactor>,
    stats: &Rc<Stats>,
) -> Result<(), io::Error> {
    let listener = AsyncFakeListener::new(reactor, stats.clone());

    for _ in 0..CONNS_MAX {
        let stream = listener.accept().await?;

        *task_count.borrow_mut() += 1;

        let f = do_async(
            task_count.clone(),
            spawn,
            ctx,
            reactor.clone(),
            stats.clone(),
            AsyncInvoke::Connection(stream),
        );

        unsafe { spawn(ctx, &f as *const _ as *const (), mem::size_of_val(&f)).unwrap() };

        mem::forget(f);
    }

    *task_count.borrow_mut() -= 1;

    Ok(())
}

async fn connection(
    task_count: Rc<RefCell<usize>>,
    mut stream: AsyncFakeStream,
) -> Result<(), io::Error> {
    let mut buf = [0; 128];
    let mut buf_len = 0;

    while !(&buf[..buf_len]).contains(&b'\n') {
        let size = stream.read(&mut buf[buf_len..]).await?;
        buf_len += size;
    }

    let mut sent = 0;

    while sent < buf_len {
        let size = stream.write(&buf[sent..buf_len]).await?;
        sent += size;
    }

    *task_count.borrow_mut() -= 1;

    Ok(())
}

enum AsyncInvoke {
    Listen,
    Connection(AsyncFakeStream),
}

async fn do_async(
    task_count: Rc<RefCell<usize>>,
    spawn: unsafe fn(*const (), *const (), usize) -> Result<(), ()>,
    ctx: *const (),
    reactor: Rc<FakeReactor>,
    stats: Rc<Stats>,
    invoke: AsyncInvoke,
) {
    match invoke {
        AsyncInvoke::Listen => listen(task_count, spawn, ctx, &reactor, &stats)
            .await
            .unwrap(),
        AsyncInvoke::Connection(stream) => connection(task_count, stream).await.unwrap(),
    }
}

pub fn run_async(prealloc: &AsyncPrealloc) {
    let executor = Executor::new(&*prealloc.reactor, CONNS_MAX + 1);

    let ctx = &executor as *const Executor<_, _> as *const ();

    *prealloc.task_count.borrow_mut() += 1;

    executor
        .spawn(do_async(
            prealloc.task_count.clone(),
            executor.get_spawn_blob(),
            ctx,
            prealloc.reactor.clone(),
            prealloc.stats.clone(),
            AsyncInvoke::Listen,
        ))
        .unwrap();

    executor.exec();

    assert_eq!(*prealloc.task_count.borrow(), 0);
}

async fn listen2(
    task_count: Rc<RefCell<usize>>,
    spawner: &LocalSpawner,
    reactor: &Rc<FakeReactor>,
    stats: &Rc<Stats>,
) -> Result<(), io::Error> {
    let listener = AsyncFakeListener::new(reactor, stats.clone());

    for _ in 0..CONNS_MAX {
        let stream = listener.accept().await?;

        *task_count.borrow_mut() += 1;

        spawner
            .spawn_local(do_async_frs(
                task_count.clone(),
                spawner.clone(),
                reactor.clone(),
                stats.clone(),
                AsyncInvoke2::Connection(stream),
            ))
            .unwrap();
    }

    *task_count.borrow_mut() -= 1;

    Ok(())
}

enum AsyncInvoke2 {
    Listen,
    Connection(AsyncFakeStream),
}

async fn do_async_frs(
    task_count: Rc<RefCell<usize>>,
    spawner: LocalSpawner,
    reactor: Rc<FakeReactor>,
    stats: Rc<Stats>,
    invoke: AsyncInvoke2,
) {
    match invoke {
        AsyncInvoke2::Listen => listen2(task_count, &spawner, &reactor, &stats)
            .await
            .unwrap(),
        AsyncInvoke2::Connection(stream) => connection(task_count, stream).await.unwrap(),
    }
}

pub fn run_async_frs(prealloc: &AsyncPrealloc) {
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();

    *prealloc.task_count.borrow_mut() += 1;

    spawner
        .spawn_local(do_async_frs(
            prealloc.task_count.clone(),
            spawner.clone(),
            prealloc.reactor.clone(),
            prealloc.stats.clone(),
            AsyncInvoke2::Listen,
        ))
        .unwrap();

    loop {
        pool.run_until_stalled();

        if *prealloc.task_count.borrow() == 0 {
            // all tasks claim to be finished. let's blocking run to be sure
            pool.run();

            break;
        }

        prealloc.reactor.poll().unwrap();
    }
}

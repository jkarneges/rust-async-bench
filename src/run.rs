use crate::fakeio::{FakeListener, FakeStream, Poll, Stats, READABLE, WRITABLE};
use crate::future::{AsyncFakeListener, AsyncFakeStream, Executor, FakeReactor};
use crate::list;
use slab::Slab;
use std::io;
use std::io::{Read, Write};
use std::mem;

const CONNS_MAX: usize = 32;

enum ConnectionState {
    ReceivingRequest,
    SendingResponse,
}

struct Connection<'a> {
    stream: FakeStream<'a>,
    can_read: bool,
    can_write: bool,
    state: ConnectionState,
    buf: [u8; 128],
    buf_len: usize,
    sent: usize,
}

impl Connection<'_> {
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

pub fn run_sync(stats: &Stats) {
    let poll = Poll::new(CONNS_MAX + 1, &stats);

    let mut events = Slab::with_capacity(CONNS_MAX + 1);

    let listener = FakeListener::new(&stats);

    poll.register(&listener, 0, READABLE);

    let mut conns: Slab<list::Node<Connection>> = Slab::with_capacity(CONNS_MAX);
    let mut needs_process = list::List::default();

    let mut can_accept = true;

    let mut accept_left = 10;

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

async fn listen(
    spawn: fn(*const (), *const (), usize) -> Result<(), ()>,
    ctx: *const (),
    reactor: &FakeReactor<'_>,
    stats: &Stats,
) -> Result<(), io::Error> {
    let listener = AsyncFakeListener::new(reactor, stats);

    for _ in 0..10 {
        let stream = listener.accept().await?;

        let f = do_async(spawn, ctx, reactor, stats, AsyncInvoke::Connection(stream));
        spawn(ctx, &f as *const _ as *const (), mem::size_of_val(&f)).unwrap();
        mem::forget(f);
    }

    Ok(())
}

async fn connection(mut stream: AsyncFakeStream<'_, '_>) -> Result<(), io::Error> {
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

    Ok(())
}

enum AsyncInvoke<'r, 's> {
    Listen,
    Connection(AsyncFakeStream<'r, 's>),
}

async fn do_async(
    spawn: fn(*const (), *const (), usize) -> Result<(), ()>,
    ctx: *const (),
    reactor: &FakeReactor<'_>,
    stats: &Stats,
    invoke: AsyncInvoke<'_, '_>,
) {
    match invoke {
        AsyncInvoke::Listen => listen(spawn, ctx, reactor, stats).await.unwrap(),
        AsyncInvoke::Connection(stream) => connection(stream).await.unwrap(),
    }
}

pub fn run_async(stats: &Stats) {
    let reactor = FakeReactor::new(CONNS_MAX + 1, &stats);

    let executor = Executor::new(&reactor, CONNS_MAX + 1);

    let ctx = &executor as *const Executor<_, _> as *const ();

    executor
        .spawn(do_async(
            executor.get_spawn_blob(),
            ctx,
            &reactor,
            &stats,
            AsyncInvoke::Listen,
        ))
        .unwrap();

    executor.exec();
}

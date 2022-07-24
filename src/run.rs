use crate::fakeio::{FakeListener, FakeStream, Poll, Stats, READABLE, WRITABLE};
use crate::future::{AsyncFakeListener, AsyncFakeStream, Executor, FakeReactor, Spawner};
use crate::list;
use slab::Slab;
use std::io;
use std::io::{Read, Write};

pub const CONNS_MAX: usize = 32;

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

pub struct RunSync<'s, 'a> {
    stats: &'s Stats,
    poll: Poll<'s>,
    events: Slab<(usize, u8)>,
    conns: Slab<list::Node<Connection<'a>>>,
}

impl<'s: 'a, 'a> RunSync<'s, 'a> {
    pub fn new(stats: &'s Stats) -> Self {
        let poll = Poll::new(CONNS_MAX + 1, stats);
        let events = Slab::with_capacity(CONNS_MAX + 1);
        let conns = Slab::with_capacity(CONNS_MAX);

        Self {
            stats,
            poll,
            events,
            conns,
        }
    }

    pub fn run(&mut self) {
        let poll = &mut self.poll;
        let events = &mut self.events;
        let conns = &mut self.conns;

        let mut needs_process = list::List::default();

        let listener = FakeListener::new(self.stats);

        poll.register(&listener, 0, READABLE);

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

                        needs_process.push_back(conns, key);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => can_accept = false,
                    _ => unreachable!(),
                }
            }

            let mut next = needs_process.head;

            while let Some(key) = next {
                needs_process.remove(conns, key);

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

            poll.poll(events);

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
                        needs_process.remove(conns, key);
                        needs_process.push_back(conns, key);
                    }
                }
            }
        }

        poll.unregister(&listener);
    }
}

pub fn run_sync(stats: &Stats) {
    RunSync::new(stats).run()
}

async fn listen<'r, 's: 'r>(
    spawner: &'r Spawner<AsyncInvoke<'r, 's>>,
    reactor: &'r FakeReactor<'s>,
    stats: &'s Stats,
) -> Result<(), io::Error> {
    let listener = AsyncFakeListener::new(reactor, stats);

    for _ in 0..CONNS_MAX {
        let stream = listener.accept().await?;

        spawner.spawn(AsyncInvoke::Connection(stream)).unwrap();
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

pub enum AsyncInvoke<'r, 's> {
    Listen,
    Connection(AsyncFakeStream<'r, 's>),
}

pub async fn do_async<'r, 's: 'r>(
    spawner: &'r Spawner<AsyncInvoke<'r, 's>>,
    reactor: &'r FakeReactor<'s>,
    stats: &'s Stats,
    invoke: AsyncInvoke<'r, 's>,
) {
    match invoke {
        AsyncInvoke::Listen => listen(spawner, reactor, stats).await.unwrap(),
        AsyncInvoke::Connection(stream) => connection(stream).await.unwrap(),
    }
}

pub fn run_async(stats: &Stats) {
    let reactor = FakeReactor::new(CONNS_MAX + 1, stats);

    let spawner = Spawner::new();

    let executor = Executor::new(&reactor, CONNS_MAX + 1, |invoke| {
        do_async(&spawner, &reactor, stats, invoke)
    });

    executor.set_spawner(&spawner);

    spawner.spawn(AsyncInvoke::Listen).unwrap();

    executor.exec();
}

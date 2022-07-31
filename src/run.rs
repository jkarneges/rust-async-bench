use crate::executor::{ArgExecutor, BoxExecutor, BoxRcExecutor, Spawner};
use crate::fakeio;
use crate::fakeio::{FakeListener, FakeStream, Poll, READABLE, WRITABLE};
use crate::future::{AsyncFakeListener, AsyncFakeStream, FakeReactor, FakeReactorRef};
use crate::list;
use crate::waker::{ArcWakerFactory, CheckedRcWakerFactory, RcWakerFactory};
use slab::Slab;
use std::cell::RefCell;
use std::fmt;
use std::io;
use std::io::{Read, Write};
use std::rc::Rc;

pub const CONNS_MAX: usize = 32;
pub const SMALL_BUFSIZE: usize = 128;
pub const LARGE_BUFSIZE: usize = 16_384;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct StatsMetrics {
    register: u32,
    unregister: u32,
    poll: u32,
    accept: u32,
    read: u32,
    write: u32,
}

impl fmt::Display for StatsMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "register={} unregister={} poll={} accept={} read={} write={}",
            self.register, self.unregister, self.poll, self.accept, self.read, self.write
        )
    }
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

    fn get(&self) -> StatsMetrics {
        self.data.borrow().metrics
    }
}

impl fakeio::Stats for Stats {
    fn inc(&self, t: fakeio::StatsType) {
        let data = &mut *self.data.borrow_mut();

        data.do_call();

        match t {
            fakeio::StatsType::Register => data.metrics.register += 1,
            fakeio::StatsType::Unregister => data.metrics.unregister += 1,
            fakeio::StatsType::Poll => data.metrics.poll += 1,
            fakeio::StatsType::Accept => data.metrics.accept += 1,
            fakeio::StatsType::Read => data.metrics.read += 1,
            fakeio::StatsType::Write => data.metrics.write += 1,
        }
    }
}

impl fakeio::Stats for &Stats {
    fn inc(&self, t: fakeio::StatsType) {
        Stats::inc(*self, t)
    }
}

impl fakeio::Stats for Rc<Stats> {
    fn inc(&self, t: fakeio::StatsType) {
        Stats::inc(self.as_ref(), t)
    }
}

impl<'s> FakeReactorRef<&'s Stats> for &FakeReactor<&'s Stats> {
    fn get<'a>(&'a self) -> &'a FakeReactor<&'s Stats> {
        *self
    }
}

impl FakeReactorRef<Rc<Stats>> for Rc<FakeReactor<Rc<Stats>>> {
    fn get(&self) -> &FakeReactor<Rc<Stats>> {
        self.as_ref()
    }
}

enum ConnectionState {
    ReceivingRequest,
    SendingResponse,
}

struct Connection<T>
where
    T: fakeio::Stats,
{
    stream: FakeStream<T>,
    can_read: bool,
    can_write: bool,
    state: ConnectionState,
    buf: [u8; SMALL_BUFSIZE],
    buf_len: usize,
    sent: usize,
}

impl<T> Connection<T>
where
    T: fakeio::Stats,
{
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

pub struct RunManual<'s> {
    stats: &'s Stats,
    poll: Poll<&'s Stats>,
    events: Slab<(usize, u8)>,
    conns: Slab<list::Node<Connection<&'s Stats>>>,
}

impl<'s> RunManual<'s> {
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

async fn listen<'r, 's: 'r>(
    spawner: &'r Spawner<AsyncInvoke<'r, 's>>,
    reactor: &'r FakeReactor<&'s Stats>,
    stats: &'s Stats,
) -> Result<(), io::Error> {
    let listener = AsyncFakeListener::new(reactor, stats);

    for _ in 0..CONNS_MAX {
        let stream = listener.accept().await?;

        spawner.spawn(AsyncInvoke::Connection(stream)).unwrap();
    }

    Ok(())
}

pub async fn listen_box<const N: usize>(
    executor: Rc<BoxExecutor>,
    reactor: Rc<FakeReactor<Rc<Stats>>>,
    stats: Rc<Stats>,
) -> Result<(), io::Error> {
    let listener = AsyncFakeListener::new(reactor, stats);

    for _ in 0..CONNS_MAX {
        let stream = listener.accept().await?;

        executor
            .spawn(async { connection_box::<N>(stream).await.unwrap() })
            .unwrap();
    }

    Ok(())
}

pub async fn listen_box_callerbox<const N: usize>(
    executor: Rc<BoxExecutor>,
    reactor: Rc<FakeReactor<Rc<Stats>>>,
    stats: Rc<Stats>,
) -> Result<(), io::Error> {
    let listener = AsyncFakeListener::new(reactor, stats);

    for _ in 0..CONNS_MAX {
        let stream = listener.accept().await?;

        executor
            .spawn_boxed(Box::pin(async {
                connection_box::<N>(stream).await.unwrap()
            }))
            .unwrap();
    }

    Ok(())
}

pub async fn listen_rc(
    executor: Rc<BoxRcExecutor>,
    reactor: Rc<FakeReactor<Rc<Stats>>>,
    stats: Rc<Stats>,
) -> Result<(), io::Error> {
    let listener = AsyncFakeListener::new(reactor, stats);

    for _ in 0..CONNS_MAX {
        let stream = listener.accept().await?;

        executor
            .spawn(async { connection_box::<SMALL_BUFSIZE>(stream).await.unwrap() })
            .unwrap();
    }

    Ok(())
}

async fn connection<'r, 's, const N: usize>(
    mut stream: AsyncFakeStream<&'s Stats, &'r FakeReactor<&'s Stats>>,
) -> Result<(), io::Error> {
    let mut buf = [0; N];
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

async fn connection_box<const N: usize>(
    mut stream: AsyncFakeStream<Rc<Stats>, Rc<FakeReactor<Rc<Stats>>>>,
) -> Result<(), io::Error> {
    let mut buf = [0; N];
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
    Connection(AsyncFakeStream<&'s Stats, &'r FakeReactor<&'s Stats>>),
}

pub async fn server_task<'r, 's: 'r, const N: usize>(
    spawner: &'r Spawner<AsyncInvoke<'r, 's>>,
    reactor: &'r FakeReactor<&'s Stats>,
    stats: &'s Stats,
    invoke: AsyncInvoke<'r, 's>,
) {
    match invoke {
        AsyncInvoke::Listen => listen(spawner, reactor, stats).await.unwrap(),
        AsyncInvoke::Connection(stream) => connection::<N>(stream).await.unwrap(),
    }
}

pub fn run_manual<R>(syscalls: bool, mut run_fn: R) -> StatsMetrics
where
    R: FnMut(&mut dyn FnMut()),
{
    let stats = Stats::new(syscalls);
    let mut r = RunManual::new(&stats);

    run_fn(&mut || r.run());

    stats.get()
}

pub fn run_nonbox<R>(syscalls: bool, mut run_fn: R) -> StatsMetrics
where
    R: FnMut(&mut dyn FnMut()),
{
    let stats = Stats::new(syscalls);
    let reactor = FakeReactor::new(CONNS_MAX + 1, &stats);
    let spawner = Spawner::new();
    let executor = ArgExecutor::new(CONNS_MAX + 1, |invoke, dest| {
        dest.write(server_task::<SMALL_BUFSIZE>(
            &spawner, &reactor, &stats, invoke,
        ));
    });

    executor.set_spawner(&spawner);

    run_fn(&mut || {
        spawner.spawn(AsyncInvoke::Listen).unwrap();
        executor.run(|| reactor.poll());
    });

    stats.get()
}

pub fn run_callerbox<R>(syscalls: bool, mut run_fn: R) -> StatsMetrics
where
    R: FnMut(&mut dyn FnMut()),
{
    let stats = Stats::new(syscalls);
    let reactor = FakeReactor::new(CONNS_MAX + 1, &stats);
    let spawner = Spawner::new();
    let executor = ArgExecutor::new(CONNS_MAX + 1, |invoke, dest| {
        dest.write(Box::pin(server_task::<SMALL_BUFSIZE>(
            &spawner, &reactor, &stats, invoke,
        )));
    });

    executor.set_spawner(&spawner);

    run_fn(&mut || {
        spawner.spawn(AsyncInvoke::Listen).unwrap();
        executor.run(|| reactor.poll());
    });

    stats.get()
}

pub fn run_large_nonbox<R>(syscalls: bool, mut run_fn: R) -> StatsMetrics
where
    R: FnMut(&mut dyn FnMut()),
{
    let stats = Stats::new(syscalls);
    let reactor = FakeReactor::new(CONNS_MAX + 1, &stats);
    let spawner = Spawner::new();
    let executor = ArgExecutor::new(CONNS_MAX + 1, |invoke, dest| {
        dest.write(server_task::<LARGE_BUFSIZE>(
            &spawner, &reactor, &stats, invoke,
        ));
    });

    executor.set_spawner(&spawner);

    run_fn(&mut || {
        spawner.spawn(AsyncInvoke::Listen).unwrap();
        executor.run(|| reactor.poll());
    });

    stats.get()
}

pub fn run_box<R>(syscalls: bool, mut run_fn: R) -> StatsMetrics
where
    R: FnMut(&mut dyn FnMut()),
{
    let stats = Rc::new(Stats::new(syscalls));
    let reactor = Rc::new(FakeReactor::new(CONNS_MAX + 1, stats.clone()));
    let executor = Rc::new(BoxExecutor::new(CONNS_MAX + 1));

    run_fn(&mut || {
        {
            let stats = stats.clone();
            let reactor = reactor.clone();
            let executor_copy = executor.clone();

            executor
                .spawn(async {
                    listen_box::<SMALL_BUFSIZE>(executor_copy, reactor, stats)
                        .await
                        .unwrap()
                })
                .unwrap();
        }

        executor.run(|| reactor.poll());
    });

    stats.get()
}

pub fn run_box_callerbox<R>(syscalls: bool, mut run_fn: R) -> StatsMetrics
where
    R: FnMut(&mut dyn FnMut()),
{
    let stats = Rc::new(Stats::new(syscalls));
    let reactor = Rc::new(FakeReactor::new(CONNS_MAX + 1, stats.clone()));
    let executor = Rc::new(BoxExecutor::new(CONNS_MAX + 1));

    run_fn(&mut || {
        {
            let stats = stats.clone();
            let reactor = reactor.clone();
            let executor_copy = executor.clone();

            executor
                .spawn_boxed(Box::pin(async {
                    listen_box_callerbox::<SMALL_BUFSIZE>(executor_copy, reactor, stats)
                        .await
                        .unwrap()
                }))
                .unwrap();
        }

        executor.run(|| reactor.poll());
    });

    stats.get()
}

pub fn run_large_box<R>(syscalls: bool, mut run_fn: R) -> StatsMetrics
where
    R: FnMut(&mut dyn FnMut()),
{
    let stats = Rc::new(Stats::new(syscalls));
    let reactor = Rc::new(FakeReactor::new(CONNS_MAX + 1, stats.clone()));
    let executor = Rc::new(BoxExecutor::new(CONNS_MAX + 1));

    run_fn(&mut || {
        {
            let stats = stats.clone();
            let reactor = reactor.clone();
            let executor_copy = executor.clone();

            executor
                .spawn(async {
                    listen_box::<LARGE_BUFSIZE>(executor_copy, reactor, stats)
                        .await
                        .unwrap()
                })
                .unwrap();
        }

        executor.run(|| reactor.poll());
    });

    stats.get()
}

pub enum BoxRcMode {
    RcWaker,
    CheckedRcWaker,
    ArcWaker,
}

pub fn run_box_rc<R>(syscalls: bool, mode: BoxRcMode, mut run_fn: R) -> StatsMetrics
where
    R: FnMut(&mut dyn FnMut()),
{
    let stats = Rc::new(Stats::new(syscalls));
    let reactor = Rc::new(FakeReactor::new(CONNS_MAX + 1, stats.clone()));

    let executor = Rc::new(match mode {
        BoxRcMode::RcWaker => BoxRcExecutor::new(CONNS_MAX + 1, RcWakerFactory::default()),
        BoxRcMode::CheckedRcWaker => {
            BoxRcExecutor::new(CONNS_MAX + 1, CheckedRcWakerFactory::default())
        }
        BoxRcMode::ArcWaker => BoxRcExecutor::new(CONNS_MAX + 1, ArcWakerFactory::default()),
    });

    run_fn(&mut || {
        {
            let stats = stats.clone();
            let reactor = reactor.clone();
            let executor_copy = executor.clone();

            executor
                .spawn(async { listen_rc(executor_copy, reactor, stats).await.unwrap() })
                .unwrap();
        }

        executor.run(|| reactor.poll());
    });

    stats.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_STATS: StatsMetrics = StatsMetrics {
        register: 33,
        unregister: 33,
        poll: 34,
        accept: 64,
        read: 64,
        write: 64,
    };

    #[test]
    fn test_manual() {
        assert_eq!(run_manual(false, |r| r()), EXPECTED_STATS);
    }

    #[test]
    fn test_nonbox() {
        assert_eq!(run_nonbox(false, |r| r()), EXPECTED_STATS);
    }

    #[test]
    fn test_callerbox() {
        assert_eq!(run_callerbox(false, |r| r()), EXPECTED_STATS);
    }

    #[test]
    fn test_large_nonbox() {
        assert_eq!(run_large_nonbox(false, |r| r()), EXPECTED_STATS);
    }

    #[test]
    fn test_box() {
        assert_eq!(run_box(false, |r| r()), EXPECTED_STATS);
    }

    #[test]
    fn test_box_callerbox() {
        assert_eq!(run_box_callerbox(false, |r| r()), EXPECTED_STATS);
    }

    #[test]
    fn test_large_box() {
        assert_eq!(run_large_box(false, |r| r()), EXPECTED_STATS);
    }

    #[test]
    fn test_box_rc() {
        assert_eq!(
            run_box_rc(false, BoxRcMode::RcWaker, |r| r()),
            EXPECTED_STATS
        );
    }

    #[test]
    fn test_box_chkrc() {
        assert_eq!(
            run_box_rc(false, BoxRcMode::CheckedRcWaker, |r| r()),
            EXPECTED_STATS
        );
    }

    #[test]
    fn test_box_arc() {
        assert_eq!(
            run_box_rc(false, BoxRcMode::ArcWaker, |r| r()),
            EXPECTED_STATS
        );
    }
}

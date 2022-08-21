// adapted from alloc::task::Wake

use std::cell::{Cell, RefCell};
use std::mem::{ManuallyDrop, MaybeUninit};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Wake;
use std::task::{RawWaker, RawWakerVTable, Waker};
use std::thread::{self, ThreadId};

pub trait EmbedWake {
    fn wake(&self, task_id: usize);
}

pub struct EmbedWaker<'a, W> {
    refs: Cell<usize>,
    wake: &'a W,
    task_id: usize,
}

impl<'a, W> EmbedWaker<'a, W>
where
    W: EmbedWake + 'a,
{
    pub fn new(wake: &'a W, task_id: usize) -> Self {
        Self {
            refs: Cell::new(1),
            wake,
            task_id,
        }
    }

    pub fn ref_count(&self) -> usize {
        self.refs.get()
    }

    pub fn as_std<'out>(
        self: Pin<&mut Self>,
        output_mem: &'out mut MaybeUninit<Waker>,
    ) -> &'out Waker {
        let s = &*self;

        let rw = RawWaker::new(
            s as *const Self as *const (),
            &RawWakerVTable::new(Self::clone, Self::wake, Self::wake_by_ref, Self::drop),
        );

        output_mem.write(unsafe { Waker::from_raw(rw) });

        unsafe { output_mem.assume_init_mut() }
    }

    unsafe fn clone(data: *const ()) -> RawWaker {
        let s = (data as *const Self).as_ref().unwrap();

        s.refs.set(s.refs.get() + 1);

        RawWaker::new(
            data,
            &RawWakerVTable::new(Self::clone, Self::wake, Self::wake_by_ref, Self::drop),
        )
    }

    unsafe fn wake(data: *const ()) {
        Self::wake_by_ref(data);

        Self::drop(data);
    }

    unsafe fn wake_by_ref(data: *const ()) {
        let s = (data as *const Self).as_ref().unwrap();

        s.wake.wake(s.task_id);
    }

    unsafe fn drop(data: *const ()) {
        let s = (data as *const Self).as_ref().unwrap();

        let refs = s.refs.get();

        assert!(refs > 1);

        s.refs.set(refs - 1);
    }
}

pub trait LocalWake {
    fn wake(self: Rc<Self>);

    fn wake_by_ref(self: &Rc<Self>) {
        self.clone().wake();
    }
}

#[inline(always)]
fn is_current_thread(id: ThreadId) -> bool {
    thread_local! {
        static CURRENT_THREAD: RefCell<Option<ThreadId>> = RefCell::new(None);
    }

    CURRENT_THREAD.with(|v| id == *v.borrow_mut().get_or_insert_with(|| thread::current().id()))
}

pub trait CheckedLocalWake {
    fn thread_id(self: &Rc<Self>) -> ThreadId;

    fn wake(self: Rc<Self>);

    fn wake_by_ref(self: &Rc<Self>) {
        self.clone().wake();
    }
}

pub fn local_wake_into_std<W: LocalWake>(waker: Rc<W>) -> Waker {
    // SAFETY: This is safe because raw_waker safely constructs
    // a RawWaker from Rc<W>.
    unsafe { Waker::from_raw(raw_waker(waker)) }
}

pub fn checked_local_wake_into_std<W: CheckedLocalWake>(waker: Rc<W>) -> Waker {
    // SAFETY: This is safe because raw_waker safely constructs
    // a RawWaker from Rc<W>.
    unsafe { Waker::from_raw(checked_raw_waker(waker)) }
}

#[inline(always)]
fn raw_waker<W: LocalWake>(waker: Rc<W>) -> RawWaker {
    // Increment the reference count of the rc to clone it.
    unsafe fn clone_waker<W: LocalWake>(waker: *const ()) -> RawWaker {
        Rc::increment_strong_count(waker as *const W);
        RawWaker::new(
            waker as *const (),
            &RawWakerVTable::new(
                clone_waker::<W>,
                wake::<W>,
                wake_by_ref::<W>,
                drop_waker::<W>,
            ),
        )
    }

    // Wake by value, moving the Rc into the Wake::wake function
    unsafe fn wake<W: LocalWake>(waker: *const ()) {
        let waker = Rc::from_raw(waker as *const W);
        <W as LocalWake>::wake(waker);
    }

    // Wake by reference, wrap the waker in ManuallyDrop to avoid dropping it
    unsafe fn wake_by_ref<W: LocalWake>(waker: *const ()) {
        let waker = ManuallyDrop::new(Rc::from_raw(waker as *const W));
        <W as LocalWake>::wake_by_ref(&waker);
    }

    // Decrement the reference count of the Rc on drop
    unsafe fn drop_waker<W: LocalWake>(waker: *const ()) {
        Rc::decrement_strong_count(waker as *const W);
    }

    RawWaker::new(
        Rc::into_raw(waker) as *const (),
        &RawWakerVTable::new(
            clone_waker::<W>,
            wake::<W>,
            wake_by_ref::<W>,
            drop_waker::<W>,
        ),
    )
}

#[inline(always)]
fn checked_raw_waker<W: CheckedLocalWake>(waker: Rc<W>) -> RawWaker {
    #[inline(always)]
    fn check_thread<W: CheckedLocalWake>(waker: &Rc<W>) {
        if !is_current_thread(waker.thread_id()) {
            panic!("local waker used from another thread");
        }
    }

    // Increment the reference count of the rc to clone it.
    unsafe fn clone_waker<W: CheckedLocalWake>(waker: *const ()) -> RawWaker {
        let waker = ManuallyDrop::new(Rc::from_raw(waker as *const W));

        check_thread(&waker);

        let waker = Rc::clone(&waker);

        RawWaker::new(
            Rc::into_raw(waker) as *const (),
            &RawWakerVTable::new(
                clone_waker::<W>,
                wake::<W>,
                wake_by_ref::<W>,
                drop_waker::<W>,
            ),
        )
    }

    // Wake by value, moving the Rc into the Wake::wake function
    unsafe fn wake<W: CheckedLocalWake>(waker: *const ()) {
        let waker = Rc::from_raw(waker as *const W);

        check_thread(&waker);

        <W as CheckedLocalWake>::wake(waker);
    }

    // Wake by reference, wrap the waker in ManuallyDrop to avoid dropping it
    unsafe fn wake_by_ref<W: CheckedLocalWake>(waker: *const ()) {
        let waker = ManuallyDrop::new(Rc::from_raw(waker as *const W));

        check_thread(&waker);

        <W as CheckedLocalWake>::wake_by_ref(&waker);
    }

    // Decrement the reference count of the Rc on drop
    unsafe fn drop_waker<W: CheckedLocalWake>(waker: *const ()) {
        let waker = Rc::from_raw(waker as *const W);

        check_thread(&waker);
    }

    RawWaker::new(
        Rc::into_raw(waker) as *const (),
        &RawWakerVTable::new(
            clone_waker::<W>,
            wake::<W>,
            wake_by_ref::<W>,
            drop_waker::<W>,
        ),
    )
}

pub trait WakerFactory {
    fn new_waker<T>(&self, inner: T) -> (Waker, Box<dyn Fn() -> usize>)
    where
        T: Send + Sync + Wake + LocalWake + CheckedLocalWake + 'static;
}

#[derive(Default)]
pub struct RcWakerFactory {}

impl WakerFactory for RcWakerFactory {
    fn new_waker<T>(&self, inner: T) -> (Waker, Box<dyn Fn() -> usize>)
    where
        T: Send + Sync + Wake + LocalWake + CheckedLocalWake + 'static,
    {
        let r = Rc::new(inner);

        let strong_count = {
            let r = Rc::downgrade(&r);

            Box::new(move || std::rc::Weak::strong_count(&r))
        };

        (local_wake_into_std(r), strong_count)
    }
}

#[derive(Default)]
pub struct CheckedRcWakerFactory {}

impl WakerFactory for CheckedRcWakerFactory {
    fn new_waker<T>(&self, inner: T) -> (Waker, Box<dyn Fn() -> usize>)
    where
        T: Send + Sync + Wake + CheckedLocalWake + 'static,
    {
        let r = Rc::new(inner);

        let strong_count = {
            let r = Rc::downgrade(&r);

            Box::new(move || std::rc::Weak::strong_count(&r))
        };

        (checked_local_wake_into_std(r), strong_count)
    }
}

#[derive(Default)]
pub struct ArcWakerFactory {}

impl WakerFactory for ArcWakerFactory {
    fn new_waker<T>(&self, inner: T) -> (Waker, Box<dyn Fn() -> usize>)
    where
        T: Send + Sync + Wake + LocalWake + CheckedLocalWake + 'static,
    {
        let r = Arc::new(inner);

        let strong_count = {
            let r = Arc::downgrade(&r);

            Box::new(move || std::sync::Weak::strong_count(&r))
        };

        (r.into(), strong_count)
    }
}

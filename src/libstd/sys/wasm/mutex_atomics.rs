// Copyright 2018 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use arch::wasm32::atomic;
use cell::UnsafeCell;
use mem;
use sync::atomic::{AtomicUsize, AtomicU32, Ordering::SeqCst};
use sys::thread;

pub struct Mutex {
    locked: AtomicUsize,
}

// Mutexes have a pretty simple implementation where they contain an `i32`
// internally that is 0 when unlocked and 1 when the mutex is locked.
// Acquisition has a fast path where it attempts to cmpxchg the 0 to a 1, and
// if it fails it then waits for a notification. Releasing a lock is then done
// by swapping in 0 and then notifying any waiters, if present.

impl Mutex {
    pub const fn new() -> Mutex {
        Mutex { locked: AtomicUsize::new(0) }
    }

    #[inline]
    pub unsafe fn init(&mut self) {
        // nothing to do
    }

    pub unsafe fn lock(&self) {
        while !self.try_lock() {
            let val = atomic::wait_i32(
                self.ptr(),
                1,  // we expect our mutex is locked
                -1, // wait infinitely
            );
            // we should have either woke up (0) or got a not-equal due to a
            // race (1). We should never time out (2)
            debug_assert!(val == 0 || val == 1);
        }
    }

    pub unsafe fn unlock(&self) {
        let prev = self.locked.swap(0, SeqCst);
        debug_assert_eq!(prev, 1);
        atomic::wake(self.ptr(), 1); // wake up one waiter, if any
    }

    #[inline]
    pub unsafe fn try_lock(&self) -> bool {
        self.locked.compare_exchange(0, 1, SeqCst, SeqCst).is_ok()
    }

    #[inline]
    pub unsafe fn destroy(&self) {
        // nothing to do
    }

    #[inline]
    fn ptr(&self) -> *mut i32 {
        assert_eq!(mem::size_of::<usize>(), mem::size_of::<i32>());
        &self.locked as *const AtomicUsize as *mut isize as *mut i32
    }
}

pub struct ReentrantMutex {
    owner: AtomicU32,
    recursions: UnsafeCell<u32>,
}

unsafe impl Send for ReentrantMutex {}
unsafe impl Sync for ReentrantMutex {}

// Reentrant mutexes are similarly implemented to mutexs above except that
// instead of "1" meaning unlocked we use the id of a thread to represent
// whether it has locked a mutex. That way we have an atomic counter which
// always holds the id of the thread that currently holds the lock (or 0 if the
// lock is unlocked).
//
// Once a thread acquires a lock recursively, which it detects by looking at
// the value that's already there, it will update a local `recursions` counter
// in a nonatomic fashion (as we hold the lock). The lock is then fully
// released when this recursion counter reaches 0.

impl ReentrantMutex {
    pub unsafe fn uninitialized() -> ReentrantMutex {
        ReentrantMutex {
            owner: AtomicU32::new(0),
            recursions: UnsafeCell::new(0),
        }
    }

    pub unsafe fn init(&mut self) {
        // nothing to do...
    }

    pub unsafe fn lock(&self) {
        let me = thread::my_id();
        while let Err(owner) = self._try_lock(me) {
            let val = atomic::wait_i32(self.ptr(), owner as i32, -1);
            debug_assert!(val == 0 || val == 1);
        }
    }

    #[inline]
    pub unsafe fn try_lock(&self) -> bool {
        self._try_lock(thread::my_id()).is_ok()
    }

    #[inline]
    unsafe fn _try_lock(&self, id: u32) -> Result<(), u32> {
        let id = id.checked_add(1).unwrap(); // make sure `id` isn't 0
        match self.owner.compare_exchange(0, id, SeqCst, SeqCst) {
            // we transitioned from unlocked to locked
            Ok(_) => {
                debug_assert_eq!(*self.recursions.get(), 0);
                Ok(())
            }

            // we currently own this lock, so let's update our count and return
            // true.
            Err(n) if n == id => {
                *self.recursions.get() += 1;
                Ok(())
            }

            // Someone else owns the lock, let our caller take care of it
            Err(other) => Err(other),
        }
    }

    pub unsafe fn unlock(&self) {
        // If we didn't ever recursively lock the lock then we fully unlock the
        // mutex and wake up a waiter, if any. Otherwise we decrement our
        // recursive counter and let some one else take care of the zero.
        match *self.recursions.get() {
            0 => {
                self.owner.swap(0, SeqCst);
                atomic::wake(self.ptr() as *mut i32, 1); // wake up one waiter, if any
            }
            ref mut n => *n -= 1,
        }
    }

    pub unsafe fn destroy(&self) {
        // nothing to do...
    }

    #[inline]
    fn ptr(&self) -> *mut i32 {
        &self.owner as *const AtomicU32 as *mut i32
    }
}

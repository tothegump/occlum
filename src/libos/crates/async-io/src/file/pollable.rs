use std::fmt::Debug;
use std::ops::Deref;

use futures::future::{self, BoxFuture};
use futures::prelude::*;

use crate::file::{AccessMode, SeekFrom, StatusFlags};
use crate::poll::{Events, Poller};
use crate::prelude::*;

/// An abstract for file APIs.
///
/// An implementation for this trait should make sure all read and write APIs
/// are non-blocking.
pub trait PollableFile: Debug + Sync + Send {
    fn read(&self, _buf: &mut [u8]) -> Result<usize> {
        return_errno!(EBADF, "not support read");
    }

    fn readv(&self, bufs: &mut [&mut [u8]]) -> Result<usize> {
        for buf in bufs {
            if buf.len() > 0 {
                return self.read(buf);
            }
        }
        Ok(0)
    }

    fn write(&self, _buf: &[u8]) -> Result<usize> {
        return_errno!(EBADF, "not support write");
    }

    fn writev(&self, bufs: &[&[u8]]) -> Result<usize> {
        for buf in bufs {
            if buf.len() > 0 {
                return self.write(buf);
            }
        }
        Ok(0)
    }

    fn poll_by(&self, mask: Events, poller: Option<&mut Poller>) -> Events;

    /*
        fn ioctl(&self, cmd: &mut IoctlCmd) -> Result<i32> {
            return_op_unsupported_error!("ioctl")
        }
    */

    fn status_flags(&self) -> StatusFlags {
        StatusFlags::empty()
    }

    fn set_status_flags(&self, new_status: StatusFlags) -> Result<()> {
        return_errno!(ENOSYS, "not support setting status flags");
    }
}

/// A wrapper type that extends a `PollableFile` object with async APIs.
pub struct Async<T>(T);

impl<F: PollableFile + ?Sized, T: Deref<Target = F>> Async<T> {
    pub fn new(file: T) -> Self {
        Self(file)
    }

    pub async fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let is_nonblocking = self.is_nonblocking();

        // Fast path
        let res = self.0.read(buf);
        if Self::should_io_return(&res, is_nonblocking) {
            return res;
        }

        // Slow path
        let mask = Events::IN;
        let mut poller = Poller::new();
        loop {
            let events = self.poll_by(mask, Some(&mut poller));
            if events.contains(Events::IN) {
                let res = self.0.read(buf);
                if Self::should_io_return(&res, is_nonblocking) {
                    return res;
                }
            }
            poller.wait().await;
        }
    }

    pub async fn readv(&self, bufs: &mut [&mut [u8]]) -> Result<usize> {
        let is_nonblocking = self.is_nonblocking();

        // Fast path
        let res = self.0.readv(bufs);
        if Self::should_io_return(&res, is_nonblocking) {
            return res;
        }

        // Slow path
        let mask = Events::IN;
        let mut poller = Poller::new();
        loop {
            let events = self.poll_by(mask, Some(&mut poller));
            if events.contains(Events::IN) {
                let res = self.0.readv(bufs);
                if Self::should_io_return(&res, is_nonblocking) {
                    return res;
                }
            }
            poller.wait().await;
        }
    }

    pub async fn write(&self, buf: &[u8]) -> Result<usize> {
        let is_nonblocking = self.is_nonblocking();

        // Fast path
        let res = self.0.write(buf);
        if Self::should_io_return(&res, is_nonblocking) {
            return res;
        }

        // Slow path
        let mask = Events::OUT;
        let mut poller = Poller::new();
        loop {
            let events = self.poll_by(mask, Some(&mut poller));
            if events.contains(Events::OUT) {
                let res = self.0.write(buf);
                if Self::should_io_return(&res, is_nonblocking) {
                    return res;
                }
            }
            poller.wait().await;
        }
    }

    pub async fn writev(&self, bufs: &[&[u8]]) -> Result<usize> {
        let is_nonblocking = self.is_nonblocking();

        // Fast path
        let res = self.0.writev(bufs);
        if Self::should_io_return(&res, is_nonblocking) {
            return res;
        }

        // Slow path
        let mask = Events::OUT;
        let mut poller = Poller::new();
        loop {
            let events = self.poll_by(mask, Some(&mut poller));
            if events.contains(Events::OUT) {
                let res = self.0.writev(bufs);
                if Self::should_io_return(&res, is_nonblocking) {
                    return res;
                }
            }
            poller.wait().await;
        }
    }

    #[inline]
    pub fn poll_by(&self, mask: Events, poller: Option<&mut Poller>) -> Events {
        self.0.poll_by(mask, poller)
    }

    #[inline]
    pub fn status_flags(&self) -> StatusFlags {
        self.0.status_flags()
    }

    #[inline]
    pub fn set_status_flags(&self, new_status: StatusFlags) -> Result<()> {
        self.0.set_status_flags(new_status)
    }

    #[inline]
    pub fn inner(&self) -> &T {
        &self.0
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.0
    }

    fn should_io_return(res: &Result<usize>, is_nonblocking: bool) -> bool {
        is_nonblocking || !res.has_errno(EAGAIN)
    }

    fn is_nonblocking(&self) -> bool {
        let flags = self.status_flags();
        flags.contains(StatusFlags::O_NONBLOCK)
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Async<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Async").field("0", &self.0).finish()
    }
}

impl<F: PollableFile + ?Sized, T: Deref<Target = F> + Clone> Clone for Async<T> {
    fn clone(&self) -> Self {
        Self::new(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::fmt::Debug;
    use std::sync::Arc;

    use super::*;
    use dummy_files::*;

    #[test]
    fn with_arc_dyn() {
        let foo = Arc::new(FooFile::new()) as Arc<dyn PollableFile>;
        let bar = Arc::new(BarFile::new()) as Arc<dyn PollableFile>;
        let async_foo = Async::new(foo);
        let async_bar = Async::new(bar);
        println!("foo file = {:?}", &async_foo);
        println!("bar file = {:?}", &async_bar);
    }

    mod dummy_files {
        use super::*;
        use crate::poll::Pollee;

        #[derive(Debug)]
        pub struct FooFile {
            pollee: Pollee,
        }

        impl FooFile {
            pub fn new() -> Self {
                Self {
                    pollee: Pollee::new(Events::empty()),
                }
            }
        }

        impl PollableFile for FooFile {
            fn poll_by(&self, mask: Events, poller: Option<&mut Poller>) -> Events {
                self.pollee.poll_by(mask, poller)
            }
        }

        #[derive(Debug)]
        pub struct BarFile {
            pollee: Pollee,
        }

        impl BarFile {
            pub fn new() -> Self {
                Self {
                    pollee: Pollee::new(Events::empty()),
                }
            }
        }

        impl PollableFile for BarFile {
            fn poll_by(&self, mask: Events, poller: Option<&mut Poller>) -> Events {
                self.pollee.poll_by(mask, poller)
            }
        }
    }
}

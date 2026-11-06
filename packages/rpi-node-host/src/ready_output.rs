//! Bounded, cancellable Ready JSONL publication, without background writers.
//!
//! Only a complete LF-terminated frame means ready. A timeout or stop may leave
//! a prefix in the pipe; consumers must discard that incomplete frame.
use std::{future::Future, io, pin::Pin, time::Duration};
use tokio::time::{Instant, sleep, sleep_until};

const DEADLINE: Duration = Duration::from_secs(15);
const RETRY: Duration = Duration::from_millis(10);

/// Best-effort fatal diagnostic: at most one nonblocking pipe write, no flush,
/// retry, or background writer. A full/closed stderr may lose the diagnostic;
/// the caller's nonzero exit status is authoritative. Caller sanitizes content.
/// As with Ready, synchronous filesystem/device stalls are outside this pipe
/// backpressure guarantee. Intended for exit paths, not routine logging.
pub fn report_error(message: &str) {
    if let Ok(mut output) = platform::Output::stderr() {
        let mut line: String = message.chars().take(2048).collect();
        line.push('\n');
        let _ = output.write(line.as_bytes());
    }
}

/// The same pinned stop future supervises publication and the ready lifetime.
/// Native calls are nonblocking for pipes. Console/disk writes are synchronous;
/// filesystem/device I/O stalls are outside the pipe-backpressure guarantee.
pub(crate) async fn publish_and_wait<S>(line: &[u8], mut stop: Pin<&mut S>) -> Result<(), String>
where
    S: Future<Output = Result<(), String>>,
{
    let deadline = Instant::now() + DEADLINE;
    let mut output: Option<platform::Output> = None;
    let published = write_frame(
        line,
        |bytes| {
            if let Some(writer) = output.as_mut() {
                writer.write(bytes)
            } else {
                // Preparing a full Windows pipe can itself return WouldBlock.
                // Supervise configuration retries exactly like partial writes.
                let mut writer = platform::Output::stdout()?;
                let result = writer.write(bytes);
                output = Some(writer);
                result
            }
        },
        stop.as_mut(),
        deadline,
    )
    .await?;
    // Restore the original mode before waiting for the service lifetime to end.
    // No task/thread retains the writer, including on error or cancellation.
    drop(output);
    if published { stop.await } else { Ok(()) }
}

/// `false` means stop won; never retry after stop, deadline, or a write error.
async fn write_frame<S>(
    line: &[u8],
    mut write: impl FnMut(&[u8]) -> io::Result<usize>,
    mut stop: Pin<&mut S>,
    deadline: Instant,
) -> Result<bool, String>
where
    S: Future<Output = Result<(), String>>,
{
    let mut offset = 0;
    while offset < line.len() {
        let result = tokio::select! {
            biased;
            result = stop.as_mut() => { result?; return Ok(false); }
            () = sleep_until(deadline) => return Err("Timed out writing ready status".into()),
            () = std::future::ready(()) => {
                // Tokio timers round to clock ticks; also check the exact
                // deadline immediately before every native write attempt.
                if Instant::now() >= deadline {
                    return Err("Timed out writing ready status".into());
                }
                write(&line[offset..])
            },
        };
        match result {
            Ok(written) if written > 0 && written <= line.len() - offset => {
                offset += written;
            }
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            _ => return Err("Unable to write ready status".into()),
        }
        if offset < line.len() {
            tokio::select! {
                biased;
                result = stop.as_mut() => { result?; return Ok(false); }
                () = sleep_until(deadline) => return Err("Timed out writing ready status".into()),
                () = sleep(RETRY) => {}
            }
        }
    }
    Ok(true)
}

#[cfg(unix)]
mod platform {
    use std::io;

    // Borrows only this process's standard descriptors; never closes any fd.
    pub(super) struct Output {
        fd: libc::c_int,
        flags: libc::c_int,
    }

    impl Output {
        pub(super) fn stdout() -> io::Result<Self> {
            Self::standard(libc::STDOUT_FILENO)
        }

        pub(super) fn stderr() -> io::Result<Self> {
            Self::standard(libc::STDERR_FILENO)
        }

        #[allow(unsafe_code)]
        fn standard(fd: libc::c_int) -> io::Result<Self> {
            // SAFETY: F_GETFL queries only a process standard fd, with no pointers.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags == -1 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: F_SETFL sets valid flags on a borrowed process standard fd;
            // the guard restores them. No competing service output writer exists.
            if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { fd, flags })
        }

        #[allow(unsafe_code)]
        pub(super) fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            // SAFETY: bytes is readable for its length for this synchronous call;
            // the standard descriptor is borrowed, not closed, and O_NONBLOCK.
            let written = unsafe { libc::write(self.fd, bytes.as_ptr().cast(), bytes.len()) };
            if written < 0 {
                Err(io::Error::last_os_error())
            } else {
                usize::try_from(written).map_err(|_| io::Error::other("Invalid write count"))
            }
        }
    }

    impl Drop for Output {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: Restore saved standard-descriptor flags without closing it.
            let _ = unsafe { libc::fcntl(self.fd, libc::F_SETFL, self.flags) };
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::{io, ptr};
    use windows_sys::Win32::{
        Foundation::{ERROR_NO_DATA, ERROR_PIPE_BUSY, HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType, WriteFile},
        System::{
            Console::{GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE},
            Pipes::{
                GetNamedPipeHandleStateW, PIPE_NOWAIT, PIPE_READMODE_MESSAGE,
                SetNamedPipeHandleState,
            },
        },
    };

    // Borrowed process standard handles only; raw handles never leave this
    // module and are never closed. No background or blocking-pool writer exists.
    pub(super) struct Output {
        handle: HANDLE,
        pipe_mode: Option<u32>,
    }

    impl Output {
        pub(super) fn stdout() -> io::Result<Self> {
            Self::standard(STD_OUTPUT_HANDLE)
        }

        pub(super) fn stderr() -> io::Result<Self> {
            Self::standard(STD_ERROR_HANDLE)
        }

        #[allow(unsafe_code)]
        fn standard(which: STD_HANDLE) -> io::Result<Self> {
            // SAFETY: Query a process standard handle; no pointers are passed.
            let handle = unsafe { GetStdHandle(which) };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::other("No standard output handle"));
            }
            // SAFETY: Only queries the borrowed standard handle.
            let is_pipe = unsafe { GetFileType(handle) } == FILE_TYPE_PIPE;
            let pipe_mode = if is_pipe {
                let mut state = 0;
                // SAFETY: state is a valid output pointer; all optional outputs
                // are null and username length is zero. Works for anonymous pipes.
                if unsafe {
                    GetNamedPipeHandleStateW(
                        handle,
                        &raw mut state,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        0,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                // Only these bits are writable; other returned state describes
                // pipe type/server end and must not be passed to Set...State.
                let original = state & (PIPE_NOWAIT | PIPE_READMODE_MESSAGE);
                let mode = original | PIPE_NOWAIT;
                // SAFETY: mode is a valid input pointer for this synchronous call;
                // all optional collection settings are null. Restore in Drop.
                if unsafe {
                    SetNamedPipeHandleState(handle, &raw const mode, ptr::null(), ptr::null())
                } == 0
                {
                    let error = io::Error::last_os_error();
                    if error
                        .raw_os_error()
                        .and_then(|code| u32::try_from(code).ok())
                        == Some(ERROR_PIPE_BUSY)
                    {
                        // A full pipe may refuse the mode change. Never fall
                        // back to a blocking write; retry under stop/deadline.
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                    return Err(error);
                }
                Some(original)
            } else {
                None
            };
            Ok(Self { handle, pipe_mode })
        }

        #[allow(unsafe_code)]
        pub(super) fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = u32::try_from(bytes.len())
                .map_err(|_| io::Error::other("Ready frame too large"))?;
            let mut written = 0;
            // SAFETY: The borrowed standard handle stays valid; bytes and written
            // are valid buffers for this synchronous call. No OVERLAPPED or
            // deferred write can retain them. Pipes were set to PIPE_NOWAIT.
            let ok = unsafe {
                WriteFile(
                    self.handle,
                    bytes.as_ptr(),
                    count,
                    &raw mut written,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error
                    .raw_os_error()
                    .and_then(|code| u32::try_from(code).ok())
                    == Some(ERROR_NO_DATA)
                {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                return Err(error);
            }
            // A full anonymous pipe can succeed with zero bytes. CRT write
            // converts this to ENOSPC, so use native WriteFile instead.
            if written == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            Ok(written as usize)
        }
    }

    impl Drop for Output {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            if let Some(mode) = self.pipe_mode {
                // SAFETY: Restore only saved writable pipe mode bits on the
                // borrowed standard handle; never close or flush the pipe.
                let _ = unsafe {
                    SetNamedPipeHandleState(self.handle, &raw const mode, ptr::null(), ptr::null())
                };
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::io;
    pub(super) struct Output;
    impl Output {
        pub(super) fn stdout() -> io::Result<Self> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub(super) fn stderr() -> io::Result<Self> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub(super) fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    #[tokio::test]
    async fn retries_keep_partial_offset_until_complete_lf() {
        let line = b"{\"ready\":true}\n";
        let mut calls = 0;
        let mut bytes = Vec::new();
        let stop = std::future::pending::<Result<(), String>>();
        tokio::pin!(stop);
        let published = write_frame(
            line,
            |remaining| {
                calls += 1;
                match calls {
                    1 => {
                        bytes.extend_from_slice(&remaining[..3]);
                        Ok(3)
                    }
                    2 => Err(io::ErrorKind::WouldBlock.into()),
                    3 => Ok(0),
                    4 => Err(io::ErrorKind::Interrupted.into()),
                    _ => {
                        bytes.extend_from_slice(remaining);
                        Ok(remaining.len())
                    }
                }
            },
            stop.as_mut(),
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(published);
        assert_eq!(calls, 5);
        assert_eq!(bytes, line);
    }

    #[tokio::test]
    async fn full_pipe_times_out_without_late_writes() {
        let calls = Rc::new(Cell::new(0));
        let observed = calls.clone();
        let stop = std::future::pending::<Result<(), String>>();
        tokio::pin!(stop);
        let result = write_frame(
            b"ready\n",
            move |_| {
                observed.set(observed.get() + 1);
                Err(io::ErrorKind::WouldBlock.into())
            },
            stop.as_mut(),
            Instant::now() + Duration::from_millis(25),
        )
        .await;
        assert_eq!(result.unwrap_err(), "Timed out writing ready status");
        let ended = calls.get();
        assert!(ended > 0);
        sleep(Duration::from_millis(30)).await;
        assert_eq!(calls.get(), ended);
    }

    #[tokio::test]
    async fn stop_after_partial_frame_never_retries_or_emits_lf() {
        let calls = Rc::new(Cell::new(0));
        let observed = calls.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        let mut send = Some(send);
        let stop = async {
            receive.await.unwrap();
            Ok(())
        };
        tokio::pin!(stop);
        let mut bytes = Vec::new();
        let published = write_frame(
            b"ready\n",
            |remaining| {
                observed.set(observed.get() + 1);
                bytes.extend_from_slice(&remaining[..2]);
                send.take().unwrap().send(()).unwrap();
                Ok(2)
            },
            stop.as_mut(),
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(!published);
        assert_eq!(bytes, b"re");
        sleep(Duration::from_millis(30)).await;
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn stop_and_expired_deadline_precede_any_write() {
        let stop = std::future::ready(Ok(()));
        tokio::pin!(stop);
        assert!(
            !write_frame(
                b"ready\n",
                |_| panic!("write after stop"),
                stop.as_mut(),
                Instant::now(),
            )
            .await
            .unwrap()
        );
        let stop = std::future::pending::<Result<(), String>>();
        tokio::pin!(stop);
        assert!(
            write_frame(
                b"ready\n",
                |_| panic!("write after deadline"),
                stop.as_mut(),
                Instant::now(),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn dropping_publication_has_no_background_writer() {
        let calls = Rc::new(Cell::new(0));
        let observed = calls.clone();
        let stop = std::future::pending::<Result<(), String>>();
        tokio::pin!(stop);
        {
            let publish = write_frame(
                b"ready\n",
                move |_| {
                    observed.set(observed.get() + 1);
                    Ok(0)
                },
                stop.as_mut(),
                Instant::now() + Duration::from_secs(1),
            );
            tokio::pin!(publish);
            tokio::select! {
                biased;
                result = &mut publish => panic!("unexpected completion: {result:?}"),
                () = sleep(Duration::from_millis(15)) => {}
            }
        }
        let ended = calls.get();
        assert!(ended > 0);
        sleep(Duration::from_millis(30)).await;
        assert_eq!(calls.get(), ended);
    }

    #[tokio::test]
    async fn same_pinned_stop_survives_publication_into_wait_phase() {
        let polls = Cell::new(0);
        let written = Cell::new(false);
        let stop = std::future::poll_fn(|_| {
            polls.set(polls.get() + 1);
            if written.get() {
                std::task::Poll::Ready(Err("stop monitor error".into()))
            } else {
                std::task::Poll::Pending
            }
        });
        tokio::pin!(stop);
        assert!(
            write_frame(
                b"ready\n",
                |bytes| {
                    written.set(true);
                    Ok(bytes.len())
                },
                stop.as_mut(),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap()
        );
        assert_eq!(polls.get(), 1);
        let result: Result<(), String> = stop.await;
        assert_eq!(result.unwrap_err(), "stop monitor error");
        assert_eq!(polls.get(), 2);
    }

    #[tokio::test]
    async fn output_error_is_sanitized_and_not_retried() {
        let stop = std::future::pending::<Result<(), String>>();
        tokio::pin!(stop);
        let mut calls = 0;
        let result = write_frame(
            b"ready\n",
            |_| {
                calls += 1;
                Err(io::Error::other("secret diagnostic"))
            },
            stop.as_mut(),
            Instant::now() + Duration::from_secs(1),
        )
        .await;
        assert_eq!(result.unwrap_err(), "Unable to write ready status");
        assert_eq!(calls, 1);
    }
}

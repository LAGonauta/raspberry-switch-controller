//! Thread priority utilities for realtime scheduling.

use std::io;

/// Set the current thread to use SCHED_FIFO realtime scheduling with the given priority.
/// Priority range is 1-99 (1 is lowest realtime, 99 is highest).
/// Returns Ok(()) on success, or an error if the syscall fails.
pub fn set_realtime_priority(priority: i32) -> io::Result<()> {
    unsafe {
        let param = libc::sched_param {
            sched_priority: priority,
        };
        let result = libc::sched_setscheduler(0, libc::SCHED_FIFO, &param);
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

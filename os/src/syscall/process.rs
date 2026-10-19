//! Process management syscalls
use crate::{
    config::MEMORY_END,
    task::{current_syscall_count, exit_current_and_run_next, suspend_current_and_run_next},
    timer::get_time_us,
};

/// `trace` request: read a byte from user memory
const TRACE_READ: usize = 0;
/// `trace` request: write a byte into user memory
const TRACE_WRITE: usize = 1;
/// `trace` request: query how many times a syscall has been called
const TRACE_SYSCALL: usize = 2;

/// Lowest address which belongs to the RAM the kernel can touch directly.
const MEMORY_START: usize = 0x8000_0000;

/// Is `addr` inside the RAM, i.e. is it safe for the kernel to dereference?
///
/// In chapter 3 the kernel runs without paging and every app shares the same
/// address space, so the whole physical memory is directly accessible and the
/// only kind of address we must reject is one outside of RAM.
fn is_valid_user_addr(addr: usize) -> bool {
    (MEMORY_START..MEMORY_END).contains(&addr)
}

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("[kernel] Application exited with code {}", exit_code);
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// get time with second and microsecond
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    unsafe {
        *ts = TimeVal {
            sec: us / 1_000_000,
            usec: us % 1_000_000,
        };
    }
    0
}

/// Trace the execution of the current task.
///
/// * `Read`: read one byte at user address `id` and return it, `-1` on failure.
/// * `Write`: write `data` (its lowest byte) to user address `id`, `0` on
///   success and `-1` on failure.
/// * `Syscall`: return how many times syscall `id` has been called by the
///   current task. Since the counters are updated on syscall entry, the count
///   already includes this very `trace` call.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    match trace_request {
        TRACE_READ => {
            if !is_valid_user_addr(id) {
                return -1;
            }
            unsafe { core::ptr::read_volatile(id as *const u8) as isize }
        }
        TRACE_WRITE => {
            if !is_valid_user_addr(id) {
                return -1;
            }
            unsafe {
                core::ptr::write_volatile(id as *mut u8, data as u8);
            }
            0
        }
        TRACE_SYSCALL => current_syscall_count(id) as isize,
        _ => -1,
    }
}

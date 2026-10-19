//! Process management syscalls
use crate::config::PAGE_SIZE;
use crate::mm::{
    translated_byte_buffer_checked, MapPermission, UserAccess, VirtAddr, USER_ADDR_LIMIT,
};
use crate::task::{
    change_program_brk, current_syscall_count, current_user_token, exit_current_and_run_next, mmap,
    munmap, suspend_current_and_run_next,
};
use crate::timer::get_time_us;
use core::mem::size_of;

/// `trace` request: read a byte from user memory
const TRACE_READ: usize = 0;
/// `trace` request: write a byte into user memory
const TRACE_WRITE: usize = 1;
/// `trace` request: query how many times a syscall has been called
const TRACE_SYSCALL: usize = 2;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
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
///
/// `ts` is a user virtual address, so the value can not be written directly:
/// it must go through the page table of the current task, and the struct may
/// be splitted by two pages.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    let time_val = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    let buffers = match translated_byte_buffer_checked(
        current_user_token(),
        ts as *const u8,
        size_of::<TimeVal>(),
        UserAccess::Write,
    ) {
        Some(buffers) => buffers,
        None => return -1,
    };
    let src = unsafe {
        core::slice::from_raw_parts(
            &time_val as *const TimeVal as *const u8,
            size_of::<TimeVal>(),
        )
    };
    let mut copied = 0;
    for buffer in buffers {
        let end = copied + buffer.len();
        buffer.copy_from_slice(&src[copied..end]);
        copied = end;
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
    trace!("kernel: sys_trace");
    match trace_request {
        TRACE_READ => match read_user_byte(id) {
            Some(byte) => byte as isize,
            None => -1,
        },
        TRACE_WRITE => {
            if write_user_byte(id, data as u8) {
                0
            } else {
                -1
            }
        }
        TRACE_SYSCALL => current_syscall_count(id) as isize,
        _ => -1,
    }
}

/// Read one byte from the user address `addr` through the page table.
fn read_user_byte(addr: usize) -> Option<u8> {
    let buffers =
        translated_byte_buffer_checked(current_user_token(), addr as *const u8, 1, UserAccess::Read)?;
    buffers.first().map(|buffer| buffer[0])
}

/// Write one byte to the user address `addr` through the page table.
fn write_user_byte(addr: usize, value: u8) -> bool {
    let mut buffers = match translated_byte_buffer_checked(
        current_user_token(),
        addr as *const u8,
        1,
        UserAccess::Write,
    ) {
        Some(buffers) => buffers,
        None => return false,
    };
    match buffers.first_mut() {
        Some(buffer) => {
            buffer[0] = value;
            true
        }
        None => false,
    }
}

/// Map a virtual memory area with `prot` permissions.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel: sys_mmap");
    // `start` must be page aligned; `len` is rounded up to whole pages.
    if start % PAGE_SIZE != 0 || len == 0 {
        return -1;
    }
    // Only the lowest three bits are valid permissions and at least one of
    // them must be requested.
    if prot & !0b111 != 0 || prot & 0b111 == 0 {
        return -1;
    }
    match start.checked_add(len) {
        Some(end) if end <= USER_ADDR_LIMIT => {}
        _ => return -1,
    }
    let mut permission = MapPermission::U;
    if prot & 0b001 != 0 {
        permission |= MapPermission::R;
    }
    if prot & 0b010 != 0 {
        permission |= MapPermission::W;
    }
    if prot & 0b100 != 0 {
        permission |= MapPermission::X;
    }
    if mmap(VirtAddr::from(start), len, permission) {
        0
    } else {
        -1
    }
}

/// Unmap a virtual memory area. Both `start` and `len` must be page aligned.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap");
    if start % PAGE_SIZE != 0 || len % PAGE_SIZE != 0 || len == 0 {
        return -1;
    }
    match start.checked_add(len) {
        Some(end) if end <= USER_ADDR_LIMIT => {}
        _ => return -1,
    }
    if munmap(VirtAddr::from(start), len) {
        0
    } else {
        -1
    }
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

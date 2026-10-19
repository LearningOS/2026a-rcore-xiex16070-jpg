//! File and filesystem-related syscalls

use crate::mm::{translated_byte_buffer_checked, UserAccess};
use crate::task::current_user_token;

const FD_STDOUT: usize = 1;

/// write buf of length `len`  to a file with `fd`
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel: sys_write");
    match fd {
        FD_STDOUT => {
            let buffers = match translated_byte_buffer_checked(
                current_user_token(),
                buf,
                len,
                UserAccess::Read,
            ) {
                Some(buffers) => buffers,
                None => return -1,
            };
            for buffer in buffers {
                print!("{}", core::str::from_utf8(buffer).unwrap());
            }
            len as isize
        }
        _ => {
            panic!("Unsupported fd in sys_write!");
        }
    }
}

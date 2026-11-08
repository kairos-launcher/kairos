//! Give memory back to the system while the launcher sits hidden.
//!
//! Most of a hidden launcher's RSS is clean pages of its own executable and libraries (code that
//! ran at startup), plus heap that was freed but not returned. `release_while_hidden` asks the
//! kernel to drop the first (`MADV_PAGEOUT`; they are re-read from the page cache or disk when
//! needed again) and glibc to return the second (`malloc_trim`). Linux with glibc only.

/// Trim the heap and page out clean file-backed mappings. Call once the launcher has been
/// hidden for a while; the next show faults the needed pages back in.
pub fn release_while_hidden() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    sys::trim_heap();
    #[cfg(target_os = "linux")]
    page_out_file_mappings();
}

/// One line of `/proc/self/maps` that is worth paging out.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
struct Mapping {
    start: usize,
    len: usize,
}

/// Parse a maps line; `Some` only for read-only file mappings (code and read-only data).
/// Writable private file mappings may hold changed pages, so they are left alone.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn clean_file_mapping(line: &str) -> Option<Mapping> {
    // address perms offset dev inode path
    let mut fields = line.split_whitespace();
    let (range, perms) = (fields.next()?, fields.next()?);
    let path = fields.nth(3)?;
    if !path.starts_with('/') || perms.contains('w') {
        return None;
    }
    let (a, b) = range.split_once('-')?;
    let start = usize::from_str_radix(a, 16).ok()?;
    let end = usize::from_str_radix(b, 16).ok()?;
    Some(Mapping { start, len: end.checked_sub(start)? })
}

#[cfg(target_os = "linux")]
fn page_out_file_mappings() {
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else { return };
    for m in maps.lines().filter_map(clean_file_mapping) {
        sys::page_out(&m);
    }
}

/// The only `unsafe` in this module: two advisory libc calls.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod sys {
    use super::Mapping;

    #[cfg(target_env = "gnu")]
    pub(super) fn trim_heap() {
        // SAFETY: malloc_trim only returns free heap pages to the kernel; it has no
        // preconditions and does not touch live allocations.
        unsafe {
            libc::malloc_trim(0);
        }
    }

    pub(super) fn page_out(m: &Mapping) {
        // SAFETY: the range comes from /proc/self/maps, so it is mapped in this process.
        // MADV_PAGEOUT is advisory and never changes a mapping's contents; on a race with
        // an unmap the kernel just returns an error, which is ignored.
        unsafe {
            libc::madvise(m.start as *mut libc::c_void, m.len, libc::MADV_PAGEOUT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_read_only_file_mappings_are_paged_out() {
        let code = "7f00-7f10 r-xp 00000000 08:01 123  /usr/lib/libc.so.6";
        assert_eq!(clean_file_mapping(code), Some(Mapping { start: 0x7f00, len: 0x10 }));
        assert_eq!(clean_file_mapping("7f00-7f10 rw-p 00000000 08:01 123  /usr/lib/libc.so.6"), None);
        assert_eq!(clean_file_mapping("7f00-7f10 rw-p 00000000 00:00 0  [heap]"), None);
        assert_eq!(clean_file_mapping("7f00-7f10 r--p 00000000 00:00 0"), None, "anonymous");
        assert_eq!(clean_file_mapping("7f10-7f00 r--p 00000000 08:01 1 /x"), None, "bad range");
    }
}

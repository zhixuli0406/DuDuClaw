//! Read-only Windows physical identity, with no dependency on nightly MetadataExt.
use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    io,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
};

const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_ID_INFO_CLASS: i32 = 18;

// FILE_ID_INFO: volume serial number followed by the complete FILE_ID_128.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileId {
    pub(super) volume: u64,
    pub(super) id: [u8; 16],
}
const _: () = assert!(std::mem::size_of::<FileId>() == 24);

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "GetFileInformationByHandleEx"]
    fn get_file_information_by_handle_ex(
        file: *mut c_void,
        information_class: i32,
        information: *mut c_void,
        size: u32,
    ) -> i32;
}

fn query_id(handle: *mut c_void) -> io::Result<FileId> {
    let mut value = FileId {
        volume: 0,
        id: [0; 16],
    };
    // SAFETY: the only production caller borrows a live File handle; the
    // output has the FILE_ID_INFO layout and exactly its advertised size.
    let ok = unsafe {
        get_file_information_by_handle_ex(
            handle,
            FILE_ID_INFO_CLASS,
            (&mut value as *mut FileId).cast(),
            std::mem::size_of::<FileId>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if value.id == [0; 16] {
        return Err(io::Error::other("Windows file identity is unavailable"));
    }
    Ok(value)
}

pub(super) fn file_id(file: &File) -> io::Result<FileId> {
    use std::os::windows::fs::MetadataExt;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other("Source identity is a reparse point"));
    }
    query_id(file.as_raw_handle())
}

pub(super) fn open_identity(path: &Path, directory: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(io::Error::other("Unexpected source identity type"));
    }
    Ok(file)
}

pub(super) fn configure_read(options: &mut OpenOptions) {
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_file_id_query_failure_never_yields_identity() {
        // A null handle is invalid, not an owned or foreign file object.
        assert!(super::query_id(std::ptr::null_mut()).is_err());
    }
}

use std::{
    fs::{self, File, FileTimes, OpenOptions, TryLockError},
    io::{self, ErrorKind, IoSlice, IoSliceMut, Read, Seek, SeekFrom, Write},
    iter,
    os::molecular,
    path::Path,
    thread,
};

use components::into_std_handle;
use filesystem_abi::{FileSystemError, NAME_MAX, PATH_MAX};
use kernel_objects::Port;

const IMAGE_FILE: &str = "/boot/testrunner/sample";
const IMAGE_FILE_BODY: &str = "hello from the userland image\n";
const IMAGE_DIRECTORY: &str = "/boot/testrunner";

const LISTINGS: usize = 128;

const FILE_SYSTEM_ERRORS: [(FileSystemError, ErrorKind); 17] = [
    (FileSystemError::NotFound, ErrorKind::NotFound),
    (FileSystemError::NotADirectory, ErrorKind::NotADirectory),
    (FileSystemError::IsADirectory, ErrorKind::IsADirectory),
    (FileSystemError::NotEmpty, ErrorKind::DirectoryNotEmpty),
    (FileSystemError::AlreadyExists, ErrorKind::AlreadyExists),
    (FileSystemError::InvalidPath, ErrorKind::InvalidFilename),
    (FileSystemError::BadDescriptor, ErrorKind::InvalidData),
    (
        FileSystemError::TooManyOpenFiles,
        ErrorKind::TooManyOpenFiles,
    ),
    (FileSystemError::ReadOnly, ErrorKind::ReadOnlyFilesystem),
    (FileSystemError::NoSpace, ErrorKind::StorageFull),
    (FileSystemError::CrossMount, ErrorKind::CrossesDevices),
    (FileSystemError::NoSession, ErrorKind::NotConnected),
    (FileSystemError::NoBuffer, ErrorKind::InvalidData),
    (FileSystemError::BufferTooSmall, ErrorKind::InvalidData),
    (FileSystemError::NotSupported, ErrorKind::Unsupported),
    (FileSystemError::Unresponsive, ErrorKind::TimedOut),
    (FileSystemError::WrongMode, ErrorKind::PermissionDenied),
];

fn kind_of<T>(result: io::Result<T>) -> Option<ErrorKind> {
    result.err().map(|error| error.kind())
}

fn written(path: &str, body: &[u8]) -> File {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .unwrap_or_else(|error| panic!("{path} opens: {error}"));
    file.write_all(body).expect("the body is written");
    file
}

fn contents(path: &str) -> Vec<u8> {
    let mut body = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut body))
        .unwrap_or_else(|error| panic!("{path} reads: {error}"));
    body
}

pub(crate) fn a_path_before_the_root_is_not_connected() {
    assert_eq!(
        kind_of(File::open("/tmp/std-selftest-before-root")),
        Some(ErrorKind::NotConnected)
    );
}

pub(crate) fn the_root_is_installed(root: Port) {
    molecular::install_filesystem_root(into_std_handle(root)).expect("the first root is installed");
}

pub(crate) fn a_second_root_is_handed_back() {
    let spare = into_std_handle(Port::create().expect("the port is created"));
    let raw = spare.as_raw();

    let returned =
        molecular::install_filesystem_root(spare).expect_err("a second root replaces the first");

    assert_eq!(returned.as_raw(), raw);
}

pub(crate) fn open_options_follow_their_flags() {
    const CREATED: &str = "/tmp/std-selftest-created";

    drop(written(CREATED, b"abcdef"));
    assert_eq!(
        kind_of(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(CREATED)
        ),
        Some(ErrorKind::AlreadyExists)
    );

    let exclusive = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open("/tmp/std-selftest-exclusive")
        .expect("an exclusive create of a new name");
    assert_eq!(exclusive.metadata().expect("metadata").len(), 0);

    assert_eq!(
        kind_of(File::open("/tmp/std-selftest-absent")),
        Some(ErrorKind::NotFound)
    );

    drop(
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(CREATED)
            .expect("a truncating open"),
    );
    assert_eq!(contents(CREATED), b"");
    for part in [b"xy".as_slice(), b"z"] {
        OpenOptions::new()
            .append(true)
            .open(CREATED)
            .and_then(|mut file| file.write_all(part))
            .expect("an append");
    }
    assert_eq!(contents(CREATED), b"xyz");

    for refused in [
        OpenOptions::new(),
        OpenOptions::new().read(true).create(true).clone(),
        OpenOptions::new().read(true).truncate(true).clone(),
        OpenOptions::new().append(true).truncate(true).clone(),
    ] {
        assert_eq!(
            kind_of(refused.open(CREATED)),
            Some(ErrorKind::InvalidInput),
            "{refused:?}"
        );
    }
}

pub(crate) fn reads_writes_and_seeks_share_one_position() {
    const BODY: &str = "/tmp/std-selftest-body";

    let file = written(BODY, b"0123456789");
    let mut shared = &file;
    shared.seek(SeekFrom::Start(2)).expect("seek");
    let mut three = [0u8; 3];
    shared.read_exact(&mut three).expect("read through &File");
    assert_eq!(&three, b"234");
    assert_eq!(shared.stream_position().expect("tell"), 5);

    let (mut empty, mut first, mut second) = ([0u8; 0], [0u8; 2], [0u8; 2]);
    let read = shared
        .read_vectored(&mut [
            IoSliceMut::new(&mut empty),
            IoSliceMut::new(&mut first),
            IoSliceMut::new(&mut second),
        ])
        .expect("vectored read");
    assert_eq!((read, &first, &second), (2, b"56", &[0, 0]));
    assert!(!shared.is_read_vectored());

    shared.rewind().expect("rewind");
    let wrote = shared
        .write_vectored(&[IoSlice::new(b""), IoSlice::new(b"ab"), IoSlice::new(b"cd")])
        .expect("vectored write");
    assert_eq!(wrote, 2);
    assert!(!shared.is_write_vectored());
    assert_eq!(contents(BODY), b"ab23456789");

    for refused in [SeekFrom::End(-11), SeekFrom::Current(i64::MIN)] {
        assert_eq!(kind_of(shared.seek(refused)), Some(ErrorKind::InvalidInput));
        assert_eq!(shared.stream_position().expect("tell"), 2);
    }

    assert_eq!(
        shared.seek(SeekFrom::Start(20)).expect("seek past the end"),
        20
    );
    assert_eq!(shared.seek(SeekFrom::End(0)).expect("seek to the end"), 10);

    file.set_len(4).expect("shrink");
    file.set_len(6).expect("extend");
    assert_eq!(file.metadata().expect("metadata").len(), 6);
    assert_eq!(shared.stream_position().expect("tell"), 10);
    assert_eq!(contents(BODY), b"ab23\0\0");
    shared.flush().expect("flush");
}

pub(crate) fn an_append_lands_at_the_end_in_one_piece() {
    const APPENDED: &str = "/tmp/std-selftest-appended";

    let other = written(APPENDED, b"head");
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(APPENDED)
        .expect("an append open");

    let long = vec![b'x'; 16 * 1024];
    let first = (&file).write(&long).expect("an append write");
    assert!(0 < first && first < long.len(), "{first} of {}", long.len());
    (&file).write_all(&long[first..]).expect("the rest appends");

    let clone = file.try_clone().expect("clone");
    (&other).seek(SeekFrom::End(0)).expect("seek to the end");
    (&other)
        .write_all(b"-other")
        .expect("the other writer extends the file");
    (&clone).write_all(b"!").expect("the clone appends");

    let body = contents(APPENDED);
    assert_eq!(body.len(), 4 + long.len() + 7);
    assert!(body.starts_with(b"head"));
    assert!(body.ends_with(b"x-other!"));
}

pub(crate) fn a_clone_shares_the_position() {
    let file = written("/tmp/std-selftest-cloned", b"abcdef");
    let clone = file.try_clone().expect("clone");

    (&file).seek(SeekFrom::Start(3)).expect("seek");
    let mut rest = String::new();
    (&clone)
        .read_to_string(&mut rest)
        .expect("read through the clone");

    assert_eq!(rest, "def");
    assert_eq!((&file).stream_position().expect("tell"), 6);
}

pub(crate) fn two_files_in_two_threads_stay_apart() {
    let workers: Vec<_> = ["/tmp/std-selftest-thread-a", "/tmp/std-selftest-thread-b"]
        .into_iter()
        .map(|path| {
            thread::spawn(move || {
                let mut file = written(path, path.as_bytes());
                for _ in 0..32 {
                    file.rewind().expect("rewind");
                    let mut back = vec![0u8; path.len()];
                    file.read_exact(&mut back).expect("read back");
                    assert_eq!(back, path.as_bytes());
                }
            })
        })
        .collect();

    for worker in workers {
        worker.join().expect("worker runs to its end");
    }
}

pub(crate) fn metadata_holds_only_the_kind_and_the_size() {
    let file = written("/tmp/std-selftest-metadata", b"four");
    let metadata = file.metadata().expect("metadata");

    assert!(metadata.is_file() && !metadata.is_dir() && !metadata.is_symlink());
    assert_eq!(metadata.len(), 4);
    for time in [metadata.modified(), metadata.accessed(), metadata.created()] {
        assert_eq!(kind_of(time), Some(ErrorKind::Unsupported));
    }

    let mut permissions = metadata.permissions();
    assert!(!permissions.readonly());
    permissions.set_readonly(true);
    assert!(permissions.readonly());
    assert_eq!(
        kind_of(file.set_permissions(permissions)),
        Some(ErrorKind::Unsupported)
    );

    let directory = File::open(IMAGE_DIRECTORY).expect("a directory opens");
    let metadata = directory.metadata().expect("directory metadata");

    assert!(metadata.is_dir() && !metadata.is_file() && !metadata.is_symlink());
}

pub(crate) fn the_image_reads_and_refuses_writes() {
    let mut body = String::new();
    File::open(IMAGE_FILE)
        .and_then(|mut file| file.read_to_string(&mut body))
        .expect("an image file reads");

    assert_eq!(body, IMAGE_FILE_BODY);
    assert_eq!(
        kind_of(OpenOptions::new().write(true).open(IMAGE_FILE)),
        Some(ErrorKind::ReadOnlyFilesystem)
    );
}

pub(crate) fn file_system_errors_keep_their_meaning() {
    for (error, kind) in FILE_SYSTEM_ERRORS {
        let path = format!("/failing/{error:?}/file");
        assert_eq!(kind_of(File::open(&path)), Some(kind), "{error:?}");
    }
}

pub(crate) fn unsupported_file_operations_say_so() {
    let file = written("/tmp/std-selftest-unsupported", b"");

    for refused in [
        file.sync_all(),
        file.sync_data(),
        file.lock(),
        file.lock_shared(),
        file.unlock(),
        file.set_times(FileTimes::new()),
    ] {
        assert_eq!(kind_of(refused), Some(ErrorKind::Unsupported));
    }

    for refused in [file.try_lock(), file.try_lock_shared()] {
        assert!(
            matches!(&refused, Err(TryLockError::Error(error)) if error.kind() == ErrorKind::Unsupported),
            "{refused:?}"
        );
    }
}

pub(crate) fn malformed_paths_are_refused() {
    let long_path = format!("/tmp/{}", "p/".repeat(PATH_MAX / 2));
    let long_name = format!("/tmp/{}", "n".repeat(NAME_MAX + 1));

    for path in [
        "tmp/std-selftest-relative",
        "/tmp/./std-selftest-dot",
        "/tmp/../std-selftest-dot-dot",
        "/tmp//std-selftest-empty",
        &long_path,
        &long_name,
    ] {
        assert_eq!(
            kind_of(File::open(path)),
            Some(ErrorKind::InvalidFilename),
            "{path}"
        );
    }

    assert_eq!(
        kind_of(File::open("/std-selftest-uncovered/file")),
        Some(ErrorKind::TimedOut)
    );
}

pub(crate) fn a_listing_outlives_other_calls() {
    const DIRECTORY: &str = "/tmp/std-selftest-listing";

    fs::create_dir(DIRECTORY).expect("the directory is created");
    for name in ["a", "b"] {
        drop(written(&format!("{DIRECTORY}/{name}"), name.as_bytes()));
    }
    fs::create_dir(format!("{DIRECTORY}/nested")).expect("the nested directory is created");

    let mut listing = fs::read_dir(DIRECTORY).expect("the directory lists");
    let first = listing.next().expect("the first entry");
    assert_eq!(contents(IMAGE_FILE), IMAGE_FILE_BODY.as_bytes());

    let mut entries: Vec<_> = iter::once(first)
        .chain(listing)
        .map(|entry| {
            let entry = entry.expect("the entry reads");
            let name = entry.file_name().into_string().expect("a UTF-8 name");
            assert_eq!(entry.path(), Path::new(DIRECTORY).join(&name));
            let is_dir = entry.file_type().expect("the entry kind").is_dir();
            let metadata = entry.metadata().expect("the entry metadata");
            (name, is_dir, metadata.is_dir())
        })
        .collect();
    entries.sort();

    assert_eq!(
        entries,
        [
            ("a".to_owned(), false, false),
            ("b".to_owned(), false, false),
            ("nested".to_owned(), true, true),
        ]
    );
}

pub(crate) fn a_dropped_listing_closes_its_session() {
    for _ in 0..LISTINGS {
        let mut listing = fs::read_dir(IMAGE_DIRECTORY).expect("the directory lists");
        assert!(listing.next().is_some());
    }
}

pub(crate) fn create_dir_refuses_an_existing_directory() {
    const NESTED: &str = "/tmp/std-selftest-tree/one/two";

    assert_eq!(kind_of(fs::create_dir(NESTED)), Some(ErrorKind::NotFound));
    fs::create_dir_all(NESTED).expect("the chain is created");
    fs::create_dir_all(NESTED).expect("an existing chain is accepted");

    assert_eq!(
        kind_of(fs::create_dir(NESTED)),
        Some(ErrorKind::AlreadyExists)
    );
    assert!(fs::metadata(NESTED).expect("metadata").is_dir());
}

pub(crate) fn rename_stays_within_one_mount() {
    const FROM: &str = "/tmp/std-selftest-rename-from";
    const TO: &str = "/tmp/std-selftest-rename-to";

    drop(written(FROM, b"moved"));
    fs::rename(FROM, TO).expect("a rename inside one mount");

    assert_eq!(contents(TO), b"moved");
    assert_eq!(kind_of(fs::metadata(FROM)), Some(ErrorKind::NotFound));
    assert_eq!(
        kind_of(fs::rename(TO, "/boot/std-selftest-renamed")),
        Some(ErrorKind::CrossesDevices)
    );
    assert_eq!(contents(TO), b"moved");
}

pub(crate) fn removal_checks_the_node_kind() {
    const DIRECTORY: &str = "/tmp/std-selftest-remove";
    const FILE: &str = "/tmp/std-selftest-remove/file";

    fs::create_dir(DIRECTORY).expect("the directory is created");
    drop(written(FILE, b"x"));

    assert_eq!(
        kind_of(fs::remove_file(DIRECTORY)),
        Some(ErrorKind::IsADirectory)
    );
    assert_eq!(
        kind_of(fs::remove_dir(FILE)),
        Some(ErrorKind::NotADirectory)
    );
    assert_eq!(
        kind_of(fs::remove_dir(DIRECTORY)),
        Some(ErrorKind::DirectoryNotEmpty)
    );
    assert!(fs::metadata(DIRECTORY).expect("metadata").is_dir());

    fs::remove_file(FILE).expect("the file is removed");
    fs::remove_dir(DIRECTORY).expect("the empty directory is removed");
    assert_eq!(kind_of(fs::metadata(DIRECTORY)), Some(ErrorKind::NotFound));
}

pub(crate) fn remove_dir_all_takes_the_subtree() {
    const TREE: &str = "/tmp/std-selftest-subtree";

    fs::create_dir_all(format!("{TREE}/a/b")).expect("the tree is created");
    drop(written(&format!("{TREE}/a/file"), b"1"));
    drop(written(&format!("{TREE}/a/b/file"), b"2"));

    fs::remove_dir_all(TREE).expect("the tree is removed");

    assert_eq!(kind_of(fs::metadata(TREE)), Some(ErrorKind::NotFound));
}

pub(crate) fn copy_moves_the_bytes() {
    const SOURCE: &str = "/tmp/std-selftest-copy-source";
    const COPY: &str = "/tmp/std-selftest-copy";

    let body = vec![b'c'; 10 * 1024];
    drop(written(SOURCE, &body));

    assert_eq!(fs::copy(SOURCE, COPY).expect("a copy"), body.len() as u64);
    assert_eq!(contents(COPY), body);
    assert_eq!(
        fs::copy(IMAGE_FILE, COPY).expect("a copy across mounts"),
        IMAGE_FILE_BODY.len() as u64
    );
    assert_eq!(contents(COPY), IMAGE_FILE_BODY.as_bytes());
    assert_eq!(
        kind_of(fs::copy(IMAGE_DIRECTORY, COPY)),
        Some(ErrorKind::IsADirectory)
    );
    assert_eq!(contents(COPY), IMAGE_FILE_BODY.as_bytes());
}

pub(crate) fn exists_hides_only_not_found() {
    assert!(fs::exists(IMAGE_FILE).expect("an existing file"));
    assert!(!fs::exists("/tmp/std-selftest-absent").expect("an absent file"));
    assert_eq!(
        kind_of(fs::exists("/failing/NotADirectory/file")),
        Some(ErrorKind::NotADirectory)
    );
}

pub(crate) fn canonicalize_normalizes_lexically() {
    assert_eq!(
        fs::canonicalize("/boot//./testrunner/../testrunner/sample").expect("a canonical path"),
        Path::new(IMAGE_FILE)
    );
    assert_eq!(
        fs::canonicalize("/../../boot/testrunner").expect("the parent of the root"),
        Path::new(IMAGE_DIRECTORY)
    );
    assert_eq!(
        kind_of(fs::canonicalize("/tmp/a/../std-selftest-absent")),
        Some(ErrorKind::NotFound)
    );
    assert_eq!(
        kind_of(fs::canonicalize("tmp")),
        Some(ErrorKind::InvalidFilename)
    );
}

#[expect(deprecated)]
pub(crate) fn unsupported_path_operations_say_so() {
    const TARGET: &str = "/tmp/std-selftest-link-target";
    const LINK: &str = "/tmp/std-selftest-link";

    drop(written(TARGET, b""));
    let permissions = fs::metadata(TARGET).expect("metadata").permissions();

    for refused in [
        fs::soft_link(TARGET, LINK),
        fs::hard_link(TARGET, LINK),
        fs::read_link(TARGET).map(drop),
        fs::set_permissions(TARGET, permissions.clone()),
        fs::set_permissions_nofollow(TARGET, permissions),
        fs::set_times(TARGET, FileTimes::new()),
        fs::set_times_nofollow(TARGET, FileTimes::new()),
    ] {
        assert_eq!(kind_of(refused), Some(ErrorKind::Unsupported));
    }
    assert_eq!(kind_of(fs::metadata(LINK)), Some(ErrorKind::NotFound));
}

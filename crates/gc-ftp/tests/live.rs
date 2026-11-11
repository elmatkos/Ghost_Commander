//! Round trip against a real FTP server. Skipped unless `GC_FTP_TEST_URL` is set, e.g.
//! `GC_FTP_TEST_URL=ftp://user:pass@127.0.0.1:2121/ cargo test -p gc-ftp --test live`
//! (the folder must be writable; a `gc-test-<pid>` folder is created and removed again).

use std::fs;
use std::sync::atomic::Ordering;

use gc_core::jobs::Progress;
use gc_ftp::{RemoteItem, Target};

#[test]
fn upload_list_download_delete() {
    let Ok(url) = std::env::var("GC_FTP_TEST_URL") else {
        eprintln!("GC_FTP_TEST_URL not set, skipping");
        return;
    };
    let target = Target::from_url(&url).unwrap();
    let (conn, start, _) = gc_ftp::connect_and_list(target).unwrap();

    let work = std::env::temp_dir().join(format!("gc-ftp-live-{}", std::process::id()));
    let src = work.join("src");
    fs::create_dir_all(src.join("inner")).unwrap();
    fs::write(src.join("a.txt"), "alpha").unwrap();
    fs::write(src.join("inner").join("b.bin"), vec![7u8; 300_000]).unwrap();

    let folder = gc_ftp::join(&start, &format!("gc-test-{}", std::process::id()));
    gc_ftp::mkdir(&conn, &folder).unwrap();

    // upload a folder tree
    let p = Progress::default();
    gc_ftp::upload(&conn, &[src.clone()], &folder, false, &p);
    assert!(p.errors().is_empty(), "{:?}", p.errors());
    assert_eq!(p.done.load(Ordering::Relaxed), 300_005);

    // list it
    let top = gc_ftp::list(&conn, &gc_ftp::join(&folder, "src")).unwrap();
    let names: Vec<_> = top.iter().map(|e| (e.name.as_str(), e.is_dir)).collect();
    assert!(names.contains(&("a.txt", false)) && names.contains(&("inner", true)), "{names:?}");

    // download it again, elsewhere
    let back = work.join("back");
    fs::create_dir_all(&back).unwrap();
    let p = Progress::default();
    gc_ftp::download(&conn, &[RemoteItem { path: gc_ftp::join(&folder, "src"), is_dir: true }], &back, false, &p);
    assert!(p.errors().is_empty(), "{:?}", p.errors());
    assert_eq!(fs::read_to_string(back.join("src").join("a.txt")).unwrap(), "alpha");
    assert_eq!(fs::read(back.join("src").join("inner").join("b.bin")).unwrap().len(), 300_000);

    // view copy
    let scratch = gc_ftp::download_to_scratch(&conn, &gc_ftp::join(&folder, "src/a.txt")).unwrap();
    assert_eq!(fs::read_to_string(scratch).unwrap(), "alpha");

    // delete the whole test folder
    let p = Progress::default();
    gc_ftp::delete(&conn, &[RemoteItem { path: folder.clone(), is_dir: true }], &p);
    assert!(p.errors().is_empty(), "{:?}", p.errors());
    assert!(gc_ftp::list(&conn, &start).unwrap().iter().all(|e| !e.name.starts_with("gc-test-")));

    gc_ftp::cleanup_scratch();
    let _ = fs::remove_dir_all(work);
    gc_ftp::disconnect(&conn);
}
